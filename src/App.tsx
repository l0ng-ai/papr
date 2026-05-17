import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import * as api from "./api";
import { useUi } from "./store";
import { useArticleActions } from "./hooks/articleActions";
import { readCurrentItems } from "./lib/currentList";
import { errorText } from "./lib/errors";
import type { ArticleQuery, ArticleSummary, Feed } from "./types";
import Sidebar from "./components/Sidebar";
import ArticleList from "./components/ArticleList";
import Reader from "./components/Reader";
import CommandPalette, { type CommandAction } from "./components/CommandPalette";
import SettingsDialog from "./components/SettingsDialog";
import AddFeedDialog from "./components/AddFeedDialog";
import PromptDialog from "./components/PromptDialog";
import PlayerBar from "./components/PlayerBar";

// Accent palettes — ported from the design prototype (app.jsx ACCENTS).
const ACCENTS: Record<
  string,
  { accent: string; soft: string; ink: string; dAccent: string; dSoft: string; dInk: string }
> = {
  clay: { accent: "oklch(0.60 0.13 38)", soft: "oklch(0.94 0.04 50)", ink: "oklch(0.42 0.10 38)", dAccent: "oklch(0.74 0.13 45)", dSoft: "oklch(0.32 0.06 40)", dInk: "oklch(0.80 0.10 45)" },
  pine: { accent: "oklch(0.50 0.10 165)", soft: "oklch(0.94 0.04 160)", ink: "oklch(0.38 0.08 165)", dAccent: "oklch(0.72 0.11 170)", dSoft: "oklch(0.30 0.05 165)", dInk: "oklch(0.80 0.08 170)" },
  indigo: { accent: "oklch(0.52 0.14 268)", soft: "oklch(0.94 0.04 270)", ink: "oklch(0.40 0.12 268)", dAccent: "oklch(0.74 0.13 270)", dSoft: "oklch(0.30 0.06 268)", dInk: "oklch(0.82 0.10 270)" },
  ink: { accent: "oklch(0.30 0.02 50)", soft: "oklch(0.92 0.005 50)", ink: "oklch(0.20 0.01 50)", dAccent: "oklch(0.86 0.005 50)", dSoft: "oklch(0.30 0.005 50)", dInk: "oklch(0.92 0.005 50)" },
};

// `id` is stamped once per toast so the render key is stable across unrelated
// re-renders — keying on `Date.now()` would remount and replay the animation.
type Toast = { text: string; kbd?: string; id: number };

export default function App() {
  const { t } = useTranslation();
  const qc = useQueryClient();

  const theme = useUi((s) => s.theme);
  const accent = useUi((s) => s.accent);
  const density = useUi((s) => s.density);
  const useSerif = useUi((s) => s.useSerif);
  const readerSize = useUi((s) => s.readerSize);
  const readerLeading = useUi((s) => s.readerLeading);
  const readerWidth = useUi((s) => s.readerWidth);
  const reduceMotion = useUi((s) => s.prefs.reduceMotion);
  const focusMode = useUi((s) => s.focusMode);

  const [toast, setToast] = useState<Toast | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [cpOpen, setCpOpen] = useState(false);
  const [settings, setSettings] = useState<{ open: boolean; section?: string }>({
    open: false,
  });
  const [addFeed, setAddFeed] = useState(false);
  const [newFolder, setNewFolder] = useState(false);
  const toastTimer = useRef<number | undefined>(undefined);

  // ── apply appearance to the document root ──
  useEffect(() => {
    const root = document.documentElement;
    root.dataset.theme = theme;
    root.dataset.density = density;
    const a = ACCENTS[accent] ?? ACCENTS.clay;
    const dark = theme === "dark";
    root.style.setProperty("--accent", dark ? a.dAccent : a.accent);
    root.style.setProperty("--accent-soft", dark ? a.dSoft : a.soft);
    root.style.setProperty("--accent-ink", dark ? a.dInk : a.ink);
  }, [theme, accent, density]);

  // ── dismiss the boot splash once the app shell has mounted ──
  useEffect(() => {
    const el = document.getElementById("app-loading");
    if (!el) return;
    el.classList.add("hide");
    const timer = window.setTimeout(() => el.remove(), 360);
    return () => window.clearTimeout(timer);
  }, []);

  useEffect(() => {
    document.documentElement.dataset.reduceMotion = String(reduceMotion);
  }, [reduceMotion]);

  // Apply the startup view preference once, on first mount.
  useEffect(() => {
    const { startupView, hideReadOnStartup } = useUi.getState().prefs;
    const labels: Record<string, string> = {
      all: t("smart.all"),
      unread: t("smart.unread"),
      starred: t("smart.starred"),
    };
    if (startupView !== "last" && labels[startupView]) {
      useUi
        .getState()
        .select({ kind: startupView } as ArticleQuery, labels[startupView]);
    } else if (startupView === "last") {
      // Restore the view that was open when the app last closed.
      try {
        const raw = localStorage.getItem("lastView");
        if (raw) {
          const saved = JSON.parse(raw) as { query?: ArticleQuery; label?: string };
          if (saved.query?.kind) {
            useUi.getState().select(saved.query, saved.label ?? "");
          }
        }
      } catch {
        /* ignore a corrupt persisted value */
      }
    }
    if (hideReadOnStartup && !useUi.getState().unreadOnly) {
      useUi.getState().toggleUnreadOnly();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const root = document.documentElement.style;
    root.setProperty("--reader-size", `${readerSize}px`);
    root.setProperty("--reader-leading", String(readerLeading / 100));
    root.setProperty("--reader-width", `${readerWidth}px`);
  }, [readerSize, readerLeading, readerWidth, useSerif]);

  // ── toast ──
  const showToast = useCallback((text: string, kbd?: string) => {
    setToast({ text, kbd, id: Date.now() });
    window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 1900);
  }, []);

  // Declared after showToast so a failed mark action surfaces a toast.
  const actions = useArticleActions(showToast);

  // ── background refresh events from the Rust scheduler ──
  useEffect(() => {
    const un = listen("feeds-updated", () => {
      qc.invalidateQueries({ queryKey: ["feeds"] });
      qc.invalidateQueries({ queryKey: ["counts"] });
      qc.invalidateQueries({ queryKey: ["articles"] });
    });
    return () => {
      un.then((f) => f());
    };
  }, [qc]);

  // ── "Settings…" from the menu-bar tray ──
  useEffect(() => {
    const un = listen("tray-open-settings", () => setSettings({ open: true }));
    return () => {
      un.then((f) => f());
    };
  }, []);

  const doRefresh = useCallback(() => {
    setRefreshing((busy) => {
      if (busy) return busy;
      showToast(t("app.refreshing"));
      api
        .refreshFeeds()
        .then(async (n) => {
          await qc.invalidateQueries();
          showToast(n > 0 ? t("app.foundNew", { count: n }) : t("app.upToDate"));
        })
        .catch((e) => showToast(errorText(e)))
        .finally(() => setRefreshing(false));
      return true;
    });
  }, [qc, showToast, t]);

  const markAllRead = useCallback(async () => {
    try {
      const n = await api.markAllRead(useUi.getState().query);
      actions.refreshAfterBulk();
      showToast(n > 0 ? t("app.markedRead", { count: n }) : t("app.nothingToMark"));
    } catch (e) {
      showToast(errorText(e));
    }
  }, [actions, showToast, t]);

  const openSettings = (section?: string) => setSettings({ open: true, section });

  // ── command-palette actions ──
  const handleCommand = (action: CommandAction) => {
    switch (action) {
      case "mark-all-read": markAllRead(); break;
      case "toggle-theme":
        useUi.getState().setTheme(theme === "light" ? "dark" : "light");
        break;
      case "toggle-focus":
        useUi.getState().setFocusMode(!useUi.getState().focusMode);
        break;
      case "toggle-ai":
        if (useUi.getState().selectedArticleId != null)
          useUi.getState().setAiOpen(!useUi.getState().aiOpen);
        break;
      case "refresh": doRefresh(); break;
      case "add-feed": setAddFeed(true); break;
      case "new-folder": setNewFolder(true); break;
      case "opml": openSettings("subscriptions"); break;
      case "open-settings": openSettings(); break;
    }
  };

  const navigateFeed = (feed: Feed) => {
    useUi.getState().select({ kind: "feed", value: feed.id }, feed.title);
  };
  const navigateArticle = (a: ArticleSummary) => {
    useUi.getState().select({ kind: "feed", value: a.feedId }, a.feedTitle);
    useUi.getState().openArticle(a.id);
  };

  // ── global keyboard shortcuts (design app.jsx parity) ──
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;
      const mod = e.metaKey || e.ctrlKey;

      if (mod && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setCpOpen((o) => !o);
        return;
      }
      if (mod && e.key === ",") {
        e.preventDefault();
        setSettings((s) => ({ open: !s.open }));
        return;
      }
      if (mod && e.key.toLowerCase() === "r") {
        e.preventDefault();
        doRefresh();
        return;
      }
      if (mod) return;

      // Skip list/reader shortcuts while any overlay owns the keyboard.
      if (
        document.querySelector(
          ".cp-backdrop, .settings-backdrop, .modal-backdrop, .ctx-menu, .tag-picker",
        )
      )
        return;

      const st = useUi.getState();

      const items = readCurrentItems(qc);
      const idx = items.findIndex((a) => a.id === st.selectedArticleId);
      const sel = idx >= 0 ? items[idx] : undefined;
      const go = (delta: number) => {
        if (items.length === 0) return;
        const next = items[Math.min(items.length - 1, Math.max(0, idx + delta))];
        if (next) st.openArticle(next.id);
      };

      switch (e.key.toLowerCase()) {
        case "j": e.preventDefault(); go(idx < 0 ? 0 : 1); break;
        case "k": e.preventDefault(); go(-1); break;
        case "o":
          if (sel?.url) { e.preventDefault(); openUrl(sel.url).catch(() => {}); }
          break;
        case "s":
          if (sel) {
            e.preventDefault();
            actions.setStarred(sel.id, !sel.isStarred);
            showToast(sel.isStarred ? t("app.starRemoved") : t("app.starred"), "S");
          }
          break;
        case "b":
          if (sel) {
            e.preventDefault();
            actions.setReadLater(sel.id, !sel.readLater);
            showToast(sel.readLater ? t("app.readLaterRemoved") : t("app.readLaterAdded"), "B");
          }
          break;
        case "u":
          if (sel) { e.preventDefault(); actions.setRead(sel.id, !sel.isRead); }
          break;
        case "i":
          if (st.selectedArticleId != null) {
            e.preventDefault();
            st.setAiOpen(!st.aiOpen);
          }
          break;
        case "f": e.preventDefault(); st.setFocusMode(!st.focusMode); break;
        case "v": e.preventDefault(); st.toggleUnreadOnly(); break;
        case "a":
          if (e.shiftKey) { e.preventDefault(); markAllRead(); }
          else { e.preventDefault(); setAddFeed(true); }
          break;
        case "d":
          if (e.shiftKey) {
            e.preventDefault();
            st.setTheme(st.theme === "light" ? "dark" : "light");
          }
          break;
        case "escape":
          st.setFocusMode(false);
          st.setAiOpen(false);
          break;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // `t` is listed so the shortcut toasts re-bind after a language change.
  }, [qc, actions, cpOpen, doRefresh, markAllRead, showToast, t]);

  return (
    <>
      <div className="app-shell">
        <div className={`window ${focusMode ? "focus" : ""}`}>
          <Sidebar
            onAddFeed={() => setAddFeed(true)}
            onOpenSettings={openSettings}
            onSearchClick={() => setCpOpen(true)}
            onRefresh={doRefresh}
            refreshing={refreshing}
            onToast={showToast}
          />
          <ArticleList onToast={showToast} />
          <Reader onToast={showToast} />
        </div>
        <PlayerBar />
      </div>

      <CommandPalette
        open={cpOpen}
        onClose={() => setCpOpen(false)}
        onAction={handleCommand}
        onNavigateFeed={navigateFeed}
        onNavigateArticle={navigateArticle}
      />

      {settings.open && (
        <SettingsDialog
          onClose={() => setSettings({ open: false })}
          onToast={showToast}
          initialSection={settings.section}
          onAddFeed={() => {
            setSettings({ open: false });
            setAddFeed(true);
          }}
        />
      )}

      {addFeed && (
        <AddFeedDialog onClose={() => setAddFeed(false)} onToast={showToast} />
      )}

      {newFolder && (
        <PromptDialog
          title={t("app.newFolderTitle")}
          placeholder={t("app.folderNamePlaceholder")}
          onSubmit={(v) =>
            api
              .createFolder(v)
              .then(() => {
                qc.invalidateQueries({ queryKey: ["folders"] });
                showToast(t("app.folderCreated"));
              })
              .catch((e) => showToast(errorText(e)))
          }
          onClose={() => setNewFolder(false)}
        />
      )}

      {toast && (
        <div className="toast" key={toast.id}>
          {toast.text}
          {toast.kbd && <kbd>{toast.kbd}</kbd>}
        </div>
      )}
    </>
  );
}
