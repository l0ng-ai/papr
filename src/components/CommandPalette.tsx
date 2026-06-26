import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { useUi } from "../store";
import { feedHost, relTime } from "../lib/feedMeta";
import { modCombo } from "../lib/platform";
import { NO_AUTOCORRECT } from "../lib/inputProps";
import type { ArticleSummary, Feed } from "../types";
import Icon, { type IconName } from "./Icon";
import FeedAvatar from "./FeedAvatar";
import { useFocusTrap } from "../hooks/useFocusTrap";

export type CommandAction =
  | "mark-all-read"
  | "toggle-theme"
  | "toggle-focus"
  | "toggle-ai"
  | "refresh"
  | "add-feed"
  | "new-folder"
  | "opml"
  | "open-settings";

interface Props {
  open: boolean;
  onClose: () => void;
  onAction: (action: CommandAction) => void;
  onNavigateFeed: (feed: Feed) => void;
  onNavigateArticle: (article: ArticleSummary) => void;
}

interface Item {
  id: string;
  group: "action" | "feed" | "article";
  icon: IconName | null;
  feed?: Feed;
  label: string;
  hint?: string;
  run: () => void;
}

const ACTIONS: { icon: IconName; labelKey: string; hint: string; action: CommandAction }[] = [
  { icon: "check-all", labelKey: "commandPalette.actionMarkAllRead", hint: "⇧A", action: "mark-all-read" },
  { icon: "globe", labelKey: "commandPalette.actionToggleTheme", hint: "⇧D", action: "toggle-theme" },
  { icon: "focus", labelKey: "commandPalette.actionToggleFocus", hint: "F", action: "toggle-focus" },
  { icon: "sparkle", labelKey: "commandPalette.actionToggleAi", hint: "I", action: "toggle-ai" },
  { icon: "refresh", labelKey: "commandPalette.actionRefresh", hint: modCombo("R"), action: "refresh" },
  { icon: "plus", labelKey: "commandPalette.actionAddFeed", hint: "A", action: "add-feed" },
  { icon: "folder", labelKey: "commandPalette.actionNewFolder", hint: "", action: "new-folder" },
  { icon: "open", labelKey: "commandPalette.actionOpml", hint: "", action: "opml" },
  { icon: "settings", labelKey: "commandPalette.actionOpenSettings", hint: modCombo(","), action: "open-settings" },
];

export default function CommandPalette({
  open,
  onClose,
  onAction,
  onNavigateFeed,
  onNavigateArticle,
}: Props) {
  const { t } = useTranslation();
  // The "Toggle AI summary" action only does anything while an article is
  // open (its handler in App.tsx no-ops otherwise). Listing it unconditionally
  // would let the user pick a command that silently does nothing — so hide it
  // when there is no article to summarise.
  const hasArticle = useUi((s) => s.selectedArticleId != null);
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const cpRef = useRef<HTMLDivElement>(null);
  // True while the selection just moved by keyboard. Mouse hover also sets
  // `active`, but scrolling the list to a hovered row yanks content under
  // the cursor — and can land a different row under it, cascading more hover
  // events. So the scroll-into-view effect below only fires for keyboard nav.
  const keyboardNav = useRef(false);

  useFocusTrap(cpRef, open);

  useEffect(() => {
    if (!open) return;
    // Return focus to whatever was focused when the palette opened (the
    // ⌘K trigger lives nowhere in particular), so closing it doesn't drop
    // the user on <body>.
    const trigger = document.activeElement as HTMLElement | null;
    setQuery("");
    setDebounced("");
    setActive(0);
    const id = setTimeout(() => inputRef.current?.focus(), 30);
    return () => {
      clearTimeout(id);
      trigger?.focus?.();
    };
  }, [open]);

  useEffect(() => {
    const t = setTimeout(() => setDebounced(query.trim()), 180);
    return () => clearTimeout(t);
  }, [query]);

  const feeds = useQuery({
    queryKey: ["feeds"],
    queryFn: api.listFeeds,
    enabled: open,
  });

  const articleResults = useQuery({
    queryKey: ["cp-search", debounced],
    queryFn: () =>
      api.listArticles({ kind: "all" }, false, debounced, false, 8, 0),
    enabled: open && debounced.length > 0,
  });

  const items: Item[] = useMemo(() => {
    const q = debounced.toLowerCase();
    const out: Item[] = [];

    for (const a of ACTIONS) {
      // Skip article-scoped actions when nothing is open — they would no-op.
      if (a.action === "toggle-ai" && !hasArticle) continue;
      const label = t(a.labelKey);
      if (q && !label.toLowerCase().includes(q)) continue;
      out.push({
        id: `act-${a.action}`,
        group: "action",
        icon: a.icon,
        label,
        hint: a.hint,
        run: () => onAction(a.action),
      });
    }

    const matchedFeeds = (feeds.data ?? [])
      .filter(
        (f) =>
          !q ||
          f.title.toLowerCase().includes(q) ||
          feedHost(f).toLowerCase().includes(q),
      )
      .slice(0, 6);
    for (const f of matchedFeeds) {
      out.push({
        id: `feed-${f.id}`,
        group: "feed",
        icon: null,
        feed: f,
        label: f.title,
        hint: feedHost(f),
        run: () => onNavigateFeed(f),
      });
    }

    if (q) {
      for (const a of articleResults.data ?? []) {
        out.push({
          id: `article-${a.id}`,
          group: "article",
          icon: a.isStarred ? "star-fill" : "rss",
          label: a.title,
          hint: relTime(a.publishedAt),
          run: () => onNavigateArticle(a),
        });
      }
    }

    return out;
  }, [debounced, hasArticle, feeds.data, articleResults.data, onAction, onNavigateFeed, onNavigateArticle, t]);

  useEffect(() => {
    if (active >= items.length) setActive(0);
  }, [items.length, active]);

  // A new query reselects the first row — scroll the list back to the top so
  // that row is visible (the `keyboardNav`-gated effect below deliberately
  // ignores this non-keyboard `active` reset).
  useEffect(() => {
    if (listRef.current) listRef.current.scrollTop = 0;
  }, [debounced]);

  // Keep the keyboard-selected row visible when arrowing past the fold.
  // Skipped for mouse-driven selection changes — see `keyboardNav`.
  useEffect(() => {
    if (!keyboardNav.current) return;
    keyboardNav.current = false;
    listRef.current
      ?.querySelector<HTMLElement>(`[data-cp-index="${active}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [active]);

  if (!open) return null;

  const run = (it: Item) => {
    it.run();
    onClose();
  };

  const handleKey = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      keyboardNav.current = true;
      setActive((i) => (i + 1) % Math.max(items.length, 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      keyboardNav.current = true;
      setActive((i) => (i - 1 + items.length) % Math.max(items.length, 1));
    } else if (e.key === "Enter" && !e.nativeEvent.isComposing) {
      // `isComposing` skips the Enter that only confirms an IME candidate
      // (CJK search input) — without it the first result fires mid-typing.
      e.preventDefault();
      const it = items[active];
      if (it) run(it);
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  };

  let flat = -1;
  const renderGroup = (key: Item["group"], title: string) => {
    const list = items.filter((i) => i.group === key);
    if (list.length === 0) return null;
    return (
      <div key={key} role="group" aria-label={title}>
        <div className="cp-group-title" aria-hidden="true">
          {title}
        </div>
        {list.map((it) => {
          flat++;
          const idx = flat;
          return (
            <div
              key={it.id}
              data-cp-index={idx}
              id={`cp-option-${idx}`}
              role="option"
              aria-selected={idx === active}
              className={`cp-item ${idx === active ? "active" : ""}`}
              onMouseEnter={() => setActive(idx)}
              onClick={() => run(it)}
            >
              <span className="cp-ico">
                {it.feed ? (
                  <FeedAvatar
                    title={it.feed.title}
                    faviconUrl={it.feed.faviconUrl}
                    seed={it.feed.id}
                  />
                ) : (
                  <Icon name={it.icon ?? "rss"} size={15} />
                )}
              </span>
              <span className="cp-label">{it.label}</span>
              {it.hint && <span className="cp-hint">{it.hint}</span>}
            </div>
          );
        })}
      </div>
    );
  };

  return (
    <div className="cp-backdrop" onClick={onClose}>
      <div className="cp" ref={cpRef} onClick={(e) => e.stopPropagation()}>
        <div className="cp-input">
          <Icon name="search" size={16} />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={handleKey}
            placeholder={t("commandPalette.searchPlaceholder")}
            aria-label={t("commandPalette.searchPlaceholder")}
            {...NO_AUTOCORRECT}
            role="combobox"
            aria-expanded={items.length > 0}
            aria-controls="cp-listbox"
            aria-activedescendant={
              items.length > 0 ? `cp-option-${active}` : undefined
            }
            aria-autocomplete="list"
          />
          <span className="cp-esc">ESC</span>
        </div>
        <div className="cp-list" id="cp-listbox" role="listbox" ref={listRef}>
          {items.length === 0 ? (
            <div className="cp-empty">
              {articleResults.isFetching
                ? t("commandPalette.searching")
                : articleResults.isError && debounced.length > 0
                  ? t("commandPalette.searchError")
                  : t("commandPalette.noResults")}
            </div>
          ) : (
            <>
              {renderGroup("action", t("commandPalette.groupActions"))}
              {renderGroup("feed", t("commandPalette.groupFeeds"))}
              {renderGroup("article", t("commandPalette.groupArticles"))}
              {/* The article search failing must not be masked just because an
                  action or feed still matched the query — `items.length` would
                  then be non-zero and the empty-state error branch above never
                  shows. Surface the failure as a trailing notice instead, so a
                  partial result list still tells the user article search broke
                  rather than silently omitting matches. */}
              {articleResults.isError && debounced.length > 0 && (
                <div className="cp-notice" role="status">
                  <Icon name="alert" size={13} />
                  {t("commandPalette.searchError")}
                </div>
              )}
            </>
          )}
        </div>
        <div className="cp-footer">
          <span>
            <kbd>↑</kbd>
            <kbd>↓</kbd> {t("commandPalette.footerSelect")}
          </span>
          <span>
            <kbd>⏎</kbd> {t("commandPalette.footerOpen")}
          </span>
          <span>
            <kbd>esc</kbd> {t("commandPalette.footerClose")}
          </span>
          <div style={{ flex: 1 }} />
          <span>{t("commandPalette.footerHint")}</span>
        </div>
      </div>
    </div>
  );
}
