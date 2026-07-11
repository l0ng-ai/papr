import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useUi } from "../store";
import Sidebar from "./Sidebar";
import ArticleList from "./ArticleList";
import Reader from "./Reader";

interface Props {
  onAddFeed: () => void;
  onExplore: () => void;
  onOpenSettings: (section?: string) => void;
  onSearchClick: () => void;
  onRefresh: (scope?: { feedId?: number; folderId?: number }) => void;
  refreshing: boolean;
  onToast: (msg: string) => void;
}

/** The three navigation levels, stacked. The current level is *derived* from
 *  the store rather than tracked in a parallel router: opening an article
 *  (`selectedArticleId != null`) is the reader level; otherwise a pushed list
 *  flag distinguishes the list level from the sidebar (home) level. That keeps
 *  a single source of truth — the same selection state the desktop three-pane
 *  layout reads — so nothing can drift out of sync. */
const SIDEBAR = 0;
const LIST = 1;
const READER = 2;

/** Fraction of the pane width a back-swipe must cross to commit the pop; below
 *  it the pane springs back. Mirrors the iOS interactive-pop feel. */
const SWIPE_COMMIT = 0.4;
/** Only a touch starting within this many px of the left edge begins an
 *  edge-swipe — the same "screen-edge pan" gesture iOS uses for back, so a
 *  normal in-content drag/scroll is never hijacked. */
const EDGE_ZONE = 28;

/**
 * Mobile (iOS) shell: a stacked single-column navigation over the *same*
 * Sidebar / ArticleList / Reader the desktop renders — only the layout differs.
 * All three panes are always mounted (absolutely positioned, full-width) and
 * slid horizontally by a transform keyed off the current level, so React state
 * and each pane's scroll position survive a push/pop. Selecting a view pushes
 * the list; opening an article pushes the reader; the top-left chevron (and an
 * iOS-style left-edge back-swipe) pops one level.
 */
export default function MobileShell({
  onAddFeed,
  onExplore,
  onOpenSettings,
  onSearchClick,
  onRefresh,
  refreshing,
  onToast,
}: Props) {
  const { t } = useTranslation();
  const selectedArticleId = useUi((s) => s.selectedArticleId);
  const openArticle = useUi((s) => s.openArticle);

  // The one bit of navigation state that isn't already derivable from the
  // selection: whether the article list has been pushed over the sidebar. The
  // reader level is derived from `selectedArticleId`, so no separate flag.
  const [listOpen, setListOpen] = useState(false);

  const level = selectedArticleId != null ? READER : listOpen ? LIST : SIDEBAR;

  // An article can be opened without going through the sidebar (command
  // palette, a deep link) — keep the list beneath the reader so a back-swipe or
  // chevron lands on the list rather than skipping straight home.
  useEffect(() => {
    if (selectedArticleId != null) setListOpen(true);
  }, [selectedArticleId]);

  const back = () => {
    if (level === READER) openArticle(null);
    else if (level === LIST) setListOpen(false);
  };

  // ── interactive left-edge back-swipe ──
  // A lightweight touch handler (no gesture lib): follow the finger with a live
  // translateX while dragging, commit the pop past a threshold, else spring
  // back. `drag` is the horizontal offset in px while a swipe is in progress,
  // or null when idle. The `.dragging` class suspends the pane transition so
  // the follow is 1:1; releasing re-enables it to animate to rest. When the
  // reduce-motion preference (or the OS setting) is on, the global rule in
  // styles.css collapses that transition to ~0ms, so the pop is instant — the
  // gesture still works, it just doesn't animate.
  const [drag, setDrag] = useState<number | null>(null);
  const startX = useRef(0);
  const startY = useRef(0);
  const width = useRef(1);
  const active = useRef(false);

  const onTouchStart = (e: React.TouchEvent) => {
    if (level === SIDEBAR) return; // nothing to pop back to
    const tch = e.touches[0];
    if (tch.clientX > EDGE_ZONE) return;
    startX.current = tch.clientX;
    startY.current = tch.clientY;
    width.current = e.currentTarget.clientWidth || window.innerWidth;
    active.current = true;
  };
  const onTouchMove = (e: React.TouchEvent) => {
    if (!active.current) return;
    const tch = e.touches[0];
    const dx = tch.clientX - startX.current;
    const dy = tch.clientY - startY.current;
    // A clearly vertical first move is a scroll, not a back-swipe — bail so we
    // never fight the pane's own scrolling.
    if (drag == null && Math.abs(dy) > Math.abs(dx) && Math.abs(dy) > 10) {
      active.current = false;
      return;
    }
    if (dx > 0) setDrag(dx);
  };
  const onTouchEnd = () => {
    if (!active.current) return;
    active.current = false;
    if ((drag ?? 0) > width.current * SWIPE_COMMIT) back();
    setDrag(null);
  };

  // Resting transform (in %) for a pane at `index`, given the active level and
  // any in-progress swipe. The active pane sits at 0; deeper panes are off to
  // the right (100%); the pane one level up sits slightly left (parallax, the
  // iOS "card behind" look). A swipe drags the active pane right and pulls the
  // pane beneath it back toward 0.
  const paneTransform = (index: number): string => {
    let pct: number;
    if (index < level) pct = -22; // beneath the stack, parallaxed left
    else if (index === level) pct = 0; // active
    else pct = 100; // not yet reached, off-screen right
    if (drag != null && level > SIDEBAR) {
      const f = Math.min(1, Math.max(0, drag / width.current));
      if (index === level) pct = f * 100;
      else if (index === level - 1) pct = -22 + f * 22;
    }
    return `translateX(${pct}%)`;
  };

  const backLabel = t("common.back");

  return (
    <div
      className={`mobile-shell${drag != null ? " dragging" : ""}`}
      data-level={level}
      onTouchStart={onTouchStart}
      onTouchMove={onTouchMove}
      onTouchEnd={onTouchEnd}
      onTouchCancel={onTouchEnd}
    >
      <div
        className="ms-pane ms-sidebar"
        style={{ transform: paneTransform(SIDEBAR) }}
        aria-hidden={level !== SIDEBAR}
      >
        <Sidebar
          onAddFeed={onAddFeed}
          onExplore={onExplore}
          onOpenSettings={onOpenSettings}
          onSearchClick={onSearchClick}
          onRefresh={onRefresh}
          refreshing={refreshing}
          onToast={onToast}
          onSelectView={() => setListOpen(true)}
        />
      </div>
      <div
        className="ms-pane ms-list"
        style={{ transform: paneTransform(LIST) }}
        aria-hidden={level !== LIST}
      >
        <ArticleList onToast={onToast} onBack={back} backLabel={backLabel} />
      </div>
      <div
        className="ms-pane ms-reader"
        style={{ transform: paneTransform(READER) }}
        aria-hidden={level !== READER}
      >
        <Reader onToast={onToast} onBack={back} backLabel={backLabel} />
      </div>
    </div>
  );
}
