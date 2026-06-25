import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { openUrl } from "@tauri-apps/plugin-opener";
import * as api from "./api";
import { useUi, READER_FONTS } from "./store";
import type { DarkShade } from "./store";
import { useArticleActions } from "./hooks/articleActions";
import { readCurrentItems } from "./lib/currentList";
import { checkForUpdates } from "./lib/updater";
import { useToasts, toast as toastApi, reportError } from "./toast";
import type { ArticleQuery, ArticleSummary, Feed } from "./types";
import Sidebar from "./components/Sidebar";
import ArticleList from "./components/ArticleList";
import Reader from "./components/Reader";
import CommandPalette, { type CommandAction } from "./components/CommandPalette";
import SettingsDialog from "./components/SettingsDialog";
import AddFeedDialog from "./components/AddFeedDialog";
import ExploreDialog from "./components/ExploreDialog";
import PromptDialog from "./components/PromptDialog";
import PlayerBar from "./components/PlayerBar";
import ResizeHandle from "./components/ResizeHandle";
import Icon from "./components/Icon";
import { PANEL_BOUNDS } from "./store";

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

// Native window backing per dark-shade. The webview is made non-opaque in
// lib.rs (to kill the white resize flash), so a resize exposes THIS colour in
// the strip the webview hasn't repainted yet. Use each shade's `--reader`
// (the widest pane, 1fr, and the right/bottom edge a resize is dragged from)
// rather than the darker `--paper` floor — otherwise the exposed strip flashes
// a shade darker than the reader content it sits next to. Mirrors `--reader`
// in styles.css.
const DARK_BACKING: Record<DarkShade, string> = {
  default: "#25201F",
  dimmer: "#1C1715",
  black: "#15100F",
};

export default function App() {
  const { t } = useTranslation();
  const qc = useQueryClient();

  const theme = useUi((s) => s.theme);
  const darkShade = useUi((s) => s.darkShade);
  const accent = useUi((s) => s.accent);
  const density = useUi((s) => s.density);
  const readerFont = useUi((s) => s.readerFont);
  const readerSize = useUi((s) => s.readerSize);
  const readerLeading = useUi((s) => s.readerLeading);
  const readerWidth = useUi((s) => s.readerWidth);
  const sidebarWidth = useUi((s) => s.sidebarWidth);
  const listWidth = useUi((s) => s.listWidth);
  const aiWidth = useUi((s) => s.aiWidth);
  const reduceMotion = useUi((s) => s.prefs.reduceMotion);
  const focusMode = useUi((s) => s.focusMode);

  const activeToast = useToasts((s) => s.current);
  const dismissToast = useToasts((s) => s.dismiss);
  const [refreshing, setRefreshing] = useState(false);
  const [cpOpen, setCpOpen] = useState(false);
  const [settings, setSettings] = useState<{ open: boolean; section?: string }>({
    open: false,
  });
  const [addFeed, setAddFeed] = useState(false);
  // Feed URL handed over by a `papr://subscribe` deep link (browser extension).
  const [addFeedUrl, setAddFeedUrl] = useState<string | undefined>(undefined);
  // The standalone Explore (curated-directory marketplace) dialog.
  const [explore, setExplore] = useState(false);
  const [newFolder, setNewFolder] = useState(false);

  // Mirror "any covering modal is open" into the store. The reader's
  // original-page view is a native child webview that floats above the whole
  // DOM, so it would occlude a dialog opened over the reader (issue #54). The
  // reader watches this flag and tears the view down while a modal is up.
  const setModalOpen = useUi((s) => s.setModalOpen);
  useEffect(() => {
    setModalOpen(cpOpen || settings.open || addFeed || explore || newFolder);
  }, [cpOpen, settings.open, addFeed, explore, newFolder, setModalOpen]);

  // ── apply appearance to the document root ──
  useEffect(() => {
    const root = document.documentElement;
    root.dataset.theme = theme;
    root.dataset.darkShade = darkShade;
    root.dataset.density = density;
    const a = ACCENTS[accent] ?? ACCENTS.clay;
    const dark = theme === "dark";
    root.style.setProperty("--accent", dark ? a.dAccent : a.accent);
    root.style.setProperty("--accent-soft", dark ? a.dSoft : a.soft);
    root.style.setProperty("--accent-ink", dark ? a.dInk : a.ink);
    // Keep the native window backing on the themed reader colour. The webview is
    // non-opaque on macOS (see lib.rs), so a live resize exposes the NSWindow
    // background in the strip the webview hasn't repainted yet — use --reader so
    // that strip blends with the reader pane it sits next to. (On Win/Linux the
    // webview is opaque, so setBackgroundColor here mainly covers their own
    // resize/overscroll; harmless on macOS where it's the NSWindow colour.)
    const backing = dark ? DARK_BACKING[darkShade] : "#FBF9F3";
    getCurrentWindow().setBackgroundColor(backing).catch(() => {});
    getCurrentWebview().setBackgroundColor(backing).catch(() => {});
  }, [theme, darkShade, accent, density]);

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
    // Smart-view header labels in the *current* UI language. Smart-view
    // selections persist a translated label into `lastView`; re-deriving it
    // here keeps the header correct after a language switch (a feed/folder/tag
    // label is a proper name, so that case keeps the persisted value).
    const labels: Record<string, string> = {
      all: t("smart.all"),
      unread: t("smart.unread"),
      starred: t("smart.starred"),
      readLater: t("smart.readLater"),
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
            // The persisted label was captured in whatever language was
            // active when the view was last selected — for a smart view it
            // would now be stale if the user has since changed languages, so
            // re-translate it from the current locale.
            const label = labels[saved.query.kind] ?? saved.label ?? "";
            useUi.getState().select(saved.query, label);
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
    const font = READER_FONTS[readerFont];
    root.setProperty("--reader-font", font.stack);
    root.setProperty("--reader-font-adjust", font.adjust);
    root.setProperty("--reader-size", `${readerSize}px`);
    root.setProperty("--reader-leading", String(readerLeading / 100));
    root.setProperty("--reader-width", `${readerWidth}px`);
  }, [readerFont, readerSize, readerLeading, readerWidth]);

  // ── apply the draggable pane widths as the grid/drawer CSS variables ──
  // These drive `.window`'s grid columns and the AI drawer's width; the resize
  // handles write to the store, the store persists, and this mirrors the value
  // back onto the document root.
  useEffect(() => {
    const root = document.documentElement.style;
    root.setProperty("--col-sidebar", `${sidebarWidth}px`);
    root.setProperty("--col-list", `${listWidth}px`);
    root.setProperty("--ai-width", `${aiWidth}px`);
  }, [sidebarWidth, listWidth, aiWidth]);

  // ── toast ──
  // The store owns the queue; App owns only the dwell timer and the render.
  const showToast = toastApi.show;
  useEffect(() => {
    if (!activeToast) return;
    const timer = window.setTimeout(
      () => dismissToast(activeToast.id),
      activeToast.duration,
    );
    return () => window.clearTimeout(timer);
  }, [activeToast, dismissToast]);

  // Article-action failures route to an error toast, not a silent default one.
  const actions = useArticleActions(toastApi.error);

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

  // ── papr://subscribe deep links from the browser extension (F6) ──
  useEffect(() => {
    const un = listen<string>("deep-link-subscribe", (e) => {
      setAddFeedUrl(e.payload);
      setAddFeed(true);
    });
    // A cold-start link arrives during the backend's `setup()` — before this
    // listener exists — so the `emit` above is dropped. The backend buffers
    // that URL; drain it once on mount so a launch-by-deep-link still opens
    // the Add-feed dialog.
    api
      .takePendingDeepLink()
      .then((url) => {
        if (url) {
          setAddFeedUrl(url);
          setAddFeed(true);
        }
      })
      .catch(() => {});
    return () => {
      un.then((f) => f());
    };
  }, []);

  // ── Auto-update: one quiet check shortly after launch ──
  // Delayed so it doesn't compete with the first feed refresh for bandwidth;
  // `silent` keeps a missing release feed (or a dev build) from raising noise.
  useEffect(() => {
    const id = window.setTimeout(() => {
      void checkForUpdates({ silent: true });
    }, 4000);
    return () => window.clearTimeout(id);
  }, []);

  // A ref — not the `refreshing` state — is the concurrency guard: it must be
  // read-and-set synchronously, and the kick-off has side effects (a network
  // refresh, a toast). A setState updater must stay pure; React invokes it
  // twice under StrictMode, which previously fired the refresh twice in dev.
  // `refreshing` state is kept purely to drive the sidebar spinner.
  const refreshingRef = useRef(false);
  const doRefresh = useCallback((scope?: { feedId?: number; folderId?: number }) => {
    if (refreshingRef.current) return;
    refreshingRef.current = true;
    setRefreshing(true);
    showToast(t("app.refreshing"));
    api
      .refreshFeeds(undefined, scope)
      .then((n) => {
        // Refresh only the caches a feed fetch can actually change — a bare
        // `invalidateQueries()` would also refetch unrelated queries (rules,
        // FreshRSS status, the open feed-discovery search).
        actions.refreshAfterFetch();
        showToast(n > 0 ? t("app.foundNew", { count: n }) : t("app.upToDate"));
      })
      .catch(reportError)
      .finally(() => {
        refreshingRef.current = false;
        setRefreshing(false);
      });
  }, [actions, showToast, t]);

  const markAllRead = useCallback(async () => {
    try {
      const n = await api.markAllRead(useUi.getState().query);
      actions.refreshAfterBulk();
      showToast(n > 0 ? t("app.markedRead", { count: n }) : t("app.nothingToMark"));
    } catch (e) {
      reportError(e);
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
      const inField = tag === "INPUT" || tag === "TEXTAREA";
      const mod = e.metaKey || e.ctrlKey;

      // The modifier-key shortcuts (⌘K / ⌘, / ⌘R) are *application-global* —
      // they must fire regardless of where focus sits. The INPUT/TEXTAREA
      // guard below only suppresses the single-key list/reader shortcuts so a
      // plain "j" typed into a search box doesn't navigate; it must not block
      // a ⌘-combo. Crucially, the command palette and Settings each own a
      // focused text field, so gating these on focus would make ⌘K / ⌘, fail
      // to *close* their own dialog — the one path Escape isn't the only key
      // for.

      // ⌘K / ⌘, open their own modal. Firing them while another modal is
      // already open would stack a second dialog on top — two focus traps
      // then fight over the keyboard, and dismissing the inner one drops
      // focus to nowhere. So suppress the *open* half when a blocking modal
      // is up; the *close* (toggle-off) half stays live so ⌘K still shuts
      // the command palette and ⌘, still shuts Settings.
      if (mod && e.key.toLowerCase() === "k") {
        e.preventDefault();
        const cpOpen = !!document.querySelector(".cp-backdrop");
        if (
          !cpOpen &&
          document.querySelector(
            ".settings-backdrop, .modal-backdrop, .tag-picker, .hl-popover",
          )
        )
          return;
        setCpOpen((o) => !o);
        return;
      }
      if (mod && e.key === ",") {
        e.preventDefault();
        const settingsOpen = !!document.querySelector(".settings-backdrop");
        if (
          !settingsOpen &&
          document.querySelector(
            ".cp-backdrop, .modal-backdrop, .tag-picker, .hl-popover",
          )
        )
          return;
        setSettings((s) => ({ open: !s.open }));
        return;
      }
      if (mod && e.key.toLowerCase() === "r") {
        e.preventDefault();
        doRefresh();
        return;
      }
      if (mod) return;

      // Past this point only the single-key list/reader shortcuts remain —
      // a bare "j" / "s" / "a" etc. Those must never fire while the user is
      // typing into a text field, so bail once the modifier combos above
      // have had their chance.
      if (inField) return;

      // Skip list/reader shortcuts while any overlay owns the keyboard.
      // `.hl-popover` is the highlight edit dialog inside the reader and
      // `.hl-toolbar` is the floating colour toolbar shown when text is
      // selected: without them here, j/k would navigate away (destroying the
      // overlay — and, for the toolbar, the live selection the user was about
      // to highlight), s/u/b would act on the article, and Escape would close
      // the AI drawer instead of just the overlay.
      if (
        document.querySelector(
          ".cp-backdrop, .settings-backdrop, .modal-backdrop, .ctx-menu, .tag-picker, .hl-popover, .hl-toolbar",
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
    // `cpOpen` is intentionally absent — the handler only ever calls
    // setCpOpen (a functional update), so it doesn't depend on the value;
    // listing it would needlessly re-bind the listener on every ⌘K.
  }, [qc, actions, doRefresh, markAllRead, showToast, t]);

  return (
    <>
      <div className="app-shell">
        <div className={`window ${focusMode ? "focus" : ""}`}>
          <Sidebar
            onAddFeed={() => setAddFeed(true)}
            onExplore={() => setExplore(true)}
            onOpenSettings={openSettings}
            onSearchClick={() => setCpOpen(true)}
            onRefresh={doRefresh}
            refreshing={refreshing}
            onToast={showToast}
          />
          <ArticleList onToast={showToast} />
          <Reader onToast={showToast} />
          {/* Pane resize handles. Hidden in focus mode (the sidebar + list are
              hidden then, collapsing the grid to a single reader column). They
              sit at the column boundaries via the `left` offset below. */}
          {!focusMode && (
            <>
              <div
                className="resize-handle-slot"
                style={{ left: "var(--col-sidebar)" }}
              >
                <ResizeHandle
                  width={sidebarWidth}
                  side="right"
                  min={PANEL_BOUNDS.sidebar.min}
                  max={PANEL_BOUNDS.sidebar.max}
                  onResize={(w) => useUi.getState().setPanel({ sidebarWidth: w })}
                  label={t("app.resizeSidebar")}
                />
              </div>
              <div
                className="resize-handle-slot"
                style={{ left: "calc(var(--col-sidebar) + var(--col-list))" }}
              >
                <ResizeHandle
                  width={listWidth}
                  side="right"
                  min={PANEL_BOUNDS.list.min}
                  max={PANEL_BOUNDS.list.max}
                  onResize={(w) => useUi.getState().setPanel({ listWidth: w })}
                  label={t("app.resizeList")}
                />
              </div>
            </>
          )}
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
        <AddFeedDialog
          onClose={() => {
            setAddFeed(false);
            setAddFeedUrl(undefined);
          }}
          onToast={showToast}
          initialUrl={addFeedUrl}
        />
      )}

      {explore && (
        <ExploreDialog
          onClose={() => setExplore(false)}
          onToast={showToast}
        />
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
              .catch(reportError)
          }
          onClose={() => setNewFolder(false)}
        />
      )}

      {/* A live region so screen readers announce each toast; the toast
          itself is position: fixed, so the wrapper adds no layout. */}
      <div role="status" aria-live="polite">
        {activeToast && (
          <div
            className={`toast${activeToast.tone === "error" ? " toast-error" : ""}`}
            key={activeToast.id}
          >
            {activeToast.tone === "error" && (
              <span className="toast-ico" aria-hidden="true">
                <Icon name="alert" size={14} />
              </span>
            )}
            <span className="toast-text">{activeToast.text}</span>
            {activeToast.kbd && <kbd aria-hidden="true">{activeToast.kbd}</kbd>}
            {activeToast.action && (
              <button
                className="toast-action"
                onClick={() => {
                  activeToast.action!.run();
                  dismissToast(activeToast.id);
                }}
              >
                {activeToast.action.label}
              </button>
            )}
            {(activeToast.tone === "error" || activeToast.action) && (
              <button
                className="toast-dismiss"
                aria-label={t("common.close")}
                onClick={() => dismissToast(activeToast.id)}
              >
                <Icon name="x" size={13} />
              </button>
            )}
          </div>
        )}
      </div>
    </>
  );
}
