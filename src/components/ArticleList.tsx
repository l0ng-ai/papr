import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { openUrl } from "@tauri-apps/plugin-opener";
import * as api from "../api";
import { useUi } from "../store";
import { useArticleActions } from "../hooks/articleActions";
import { relTime } from "../lib/feedMeta";
import { isMac, modCombo } from "../lib/platform";
import { reportError, toast } from "../toast";
import { clampToViewport } from "../lib/viewport";
import type { ArticleSummary, Feed } from "../types";
import Icon from "./Icon";
import ContextMenu, { type MenuEntry } from "./ContextMenu";

const PAGE = 60;

interface Props {
  onToast: (msg: string) => void;
}

interface Hover {
  article: ArticleSummary;
  top: number;
  left: number;
}

export default function ArticleList({ onToast }: Props) {
  const { t } = useTranslation();
  const actions = useArticleActions(toast.error);
  const query = useUi((s) => s.query);
  const queryLabel = useUi((s) => s.queryLabel);
  const unreadOnly = useUi((s) => s.unreadOnly);
  const toggleUnreadOnly = useUi((s) => s.toggleUnreadOnly);
  const sortOldest = useUi((s) => s.sortOldest);
  const toggleSort = useUi((s) => s.toggleSort);
  const listAnchor = useUi((s) => s.listAnchor);
  const viewMode = useUi((s) => s.viewMode);
  const density = useUi((s) => s.density);
  const showCardThumbs = useUi((s) => s.prefs.showCardThumbs);
  const selectedId = useUi((s) => s.selectedArticleId);
  const openArticle = useUi((s) => s.openArticle);

  const feeds = useQuery({ queryKey: ["feeds"], queryFn: api.listFeeds });
  const feedById = useMemo(() => {
    const m: Record<number, Feed> = {};
    for (const f of feeds.data ?? []) m[f.id] = f;
    return m;
  }, [feeds.data]);

  const [menu, setMenu] = useState<{
    x: number;
    y: number;
    article: ArticleSummary;
  } | null>(null);
  const [hover, setHover] = useState<Hover | null>(null);
  const hoverTimer = useRef<number | undefined>(undefined);

  // Offset-anchored, *bidirectional* paging. `pageParam` is the row offset of a
  // page, so any page can be the starting one (`initialPageParam: listAnchor`).
  // Opening a deep article from search anchors here so the list loads only that
  // article's page; the user can then page newer (up) or older (down) from it.
  const browse = useInfiniteQuery({
    queryKey: ["articles", query, unreadOnly, sortOldest, listAnchor],
    initialPageParam: listAnchor,
    queryFn: ({ pageParam }) =>
      api.listArticles(query, unreadOnly, null, sortOldest, PAGE, pageParam as number),
    getNextPageParam: (last, _all, lastParam) =>
      last.length < PAGE ? undefined : (lastParam as number) + PAGE,
    getPreviousPageParam: (_first, _all, firstParam) =>
      (firstParam as number) > 0 ? Math.max(0, (firstParam as number) - PAGE) : undefined,
  });

  const items: ArticleSummary[] = useMemo(
    () => browse.data?.pages.flat() ?? [],
    [browse.data],
  );
  // Global row offset of `items[0]` — the param of the earliest loaded page
  // (which can sit below the anchor once the user pages upward). Global index
  // of `items[k]` is `baseOffset + k`, the bridge between the virtual list's
  // local indices and the backend's absolute positions (`article_index`).
  const baseOffset = (browse.data?.pageParams?.[0] as number | undefined) ?? listAnchor;

  const scrollRef = useRef<HTMLDivElement>(null);
  const rowEstimate =
    viewMode === "card"
      ? 320
      : density === "compact"
        ? 78
        : density === "spacious"
          ? 122
          : 98;
  const virt = useVirtualizer({
    count: items.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => rowEstimate,
    overscan: 8,
  });

  // Load the next page as the end approaches. Keyed on the last visible index
  // (a primitive) rather than `getVirtualItems()` — which returns a fresh
  // array every render and would re-run this effect unconditionally.
  useEffect(() => {
    const last = virt.getVirtualItems().at(-1);
    if (
      last &&
      last.index >= items.length - 6 &&
      browse.hasNextPage &&
      !browse.isFetchingNextPage
    ) {
      browse.fetchNextPage();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [virt.range?.endIndex, items.length, browse.hasNextPage, browse.isFetchingNextPage]);

  // Load the previous (newer) page as the top approaches — only ever non-empty
  // when the list is anchored mid-feed (an article opened from search), so a
  // normal newest-first browse never triggers it.
  useEffect(() => {
    const first = virt.getVirtualItems()[0];
    if (
      first &&
      first.index <= 5 &&
      browse.hasPreviousPage &&
      !browse.isFetchingPreviousPage
    ) {
      browse.fetchPreviousPage();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [virt.range?.startIndex, browse.hasPreviousPage, browse.isFetchingPreviousPage]);

  // Prepending a newer page shifts every loaded row down by a page, so the
  // viewport would jump. Before paint, nudge the scroll position down by the
  // newly prepended rows' height to keep the article under the user's eyes put.
  // Estimate-based (exact for the uniform list rows; a hair off for cards).
  // Guarded by the list signature so a *new* list (feed switch, filter, anchor
  // jump) — which also moves `baseOffset` — isn't mistaken for a prepend.
  const prevBaseRef = useRef(baseOffset);
  const listSigRef = useRef("");
  useLayoutEffect(() => {
    const sig = `${JSON.stringify(query)}|${unreadOnly}|${sortOldest}|${listAnchor}`;
    const prev = prevBaseRef.current;
    prevBaseRef.current = baseOffset;
    if (sig !== listSigRef.current) {
      listSigRef.current = sig;
      return; // new list context — not a prepend, don't touch the scroll
    }
    if (baseOffset < prev) {
      const el = scrollRef.current;
      if (el) el.scrollTop += (prev - baseOffset) * rowEstimate;
    }
  }, [baseOffset, query, unreadOnly, sortOldest, listAnchor, rowEstimate]);

  // The article we're keeping in view. A one-shot scroll lands on *estimated*
  // row heights when the list hasn't measured yet — fatal for long rows (an
  // article-as-a-site page), which settle far from their estimate, leaving the
  // target off-screen. So instead of scrolling once and locking, we keep the
  // article in view, re-checking every time the total size shifts (rows
  // measuring, images loading) until it stops moving. `align: "auto"` only
  // scrolls when the row isn't already fully visible, so *clicking* a visible
  // row never jolts the list — it just opens.
  const [reveal, setReveal] = useState<number | null>(null);
  const totalSize = virt.getTotalSize();
  useEffect(() => {
    if (reveal == null) return;
    const i = items.findIndex((a) => a.id === reveal);
    if (i < 0) return; // not loaded into the window yet — wait
    virt.scrollToIndex(i, { align: "auto" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reveal, items, totalSize]);

  // Hand control back the instant the user drives the list themselves, so we
  // never fight their scroll while a long page is still settling. A safety
  // deadline releases too, in case the size never stops changing.
  useEffect(() => {
    if (reveal == null) return;
    const el = scrollRef.current;
    if (!el) return;
    const release = () => setReveal(null);
    el.addEventListener("wheel", release, { passive: true });
    el.addEventListener("pointerdown", release, { passive: true });
    el.addEventListener("keydown", release);
    const timer = window.setTimeout(release, 3000);
    return () => {
      el.removeEventListener("wheel", release);
      el.removeEventListener("pointerdown", release);
      el.removeEventListener("keydown", release);
      window.clearTimeout(timer);
    };
  }, [reveal]);

  // Reveal the selected article. The common case (keyboard nav, clicking a row)
  // finds it already loaded and just scrolls. The hard case is an article
  // opened from search: selecting it also switches feed, and it may live far
  // below the first loaded page — or be hidden by the "unread only" filter. We
  // ask the backend for its position under the current filters and page the
  // virtual list down to it (see the locate effect below); if it's filtered out
  // (null), drop the unread filter so it rejoins the list and locate again.
  const revealedForRef = useRef<number | null>(null);
  const lookupRef = useRef<number | null>(null);
  const [locate, setLocate] = useState<{ id: number; rank: number } | null>(null);
  useEffect(() => {
    if (selectedId == null) return;
    if (revealedForRef.current === selectedId) return;
    const i = items.findIndex((a) => a.id === selectedId);
    if (i >= 0) {
      revealedForRef.current = selectedId;
      setLocate(null);
      setReveal(selectedId);
      return;
    }
    // Not in the loaded window. Locate read-agnostically: an "unread only" view
    // can't reliably hold the article we're opening (it may already be read, or
    // get marked read on open, and then the page query filters it right back
    // out), so switch to "all" first, then page to it. Only reached for an
    // article below the loaded window — a recent one is already in view.
    if (lookupRef.current === selectedId) return;
    if (unreadOnly) {
      toggleUnreadOnly();
      return;
    }
    lookupRef.current = selectedId;
    const target = selectedId;
    api
      .articleIndex(query, false, sortOldest, target)
      .then((rank) => {
        if (useUi.getState().selectedArticleId !== target) return;
        if (rank != null) setLocate({ id: target, rank });
      })
      .catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId, items]);

  // Drive the located article into view. If it's outside the loaded window,
  // jump the paging anchor to its page so the list reloads *only* there (rather
  // than every page above it); once that page is in, Effect above scrolls to it.
  useEffect(() => {
    if (!locate) return;
    if (locate.id !== selectedId) {
      setLocate(null);
      return;
    }
    const local = locate.rank - baseOffset;
    if (local >= 0 && local < items.length) {
      revealedForRef.current = locate.id;
      setReveal(locate.id);
      setLocate(null);
      return;
    }
    const targetAnchor = Math.floor(locate.rank / PAGE) * PAGE;
    if (listAnchor !== targetAnchor) useUi.getState().setListAnchor(targetAnchor);
    // else: anchor already at the target page, still loading — wait for `items`.
  }, [locate, selectedId, items, baseOffset, listAnchor]);

  // A new query or filter rebuilds the list, so any in-flight locate is stale —
  // clear it and allow a fresh lookup under the new filters.
  useEffect(() => {
    lookupRef.current = null;
    setLocate(null);
  }, [query, unreadOnly, sortOldest]);

  useEffect(() => () => window.clearTimeout(hoverTimer.current), []);

  // Dismiss any hover preview (shown or still pending) when the list contents
  // change — switching feed/folder/tag, or toggling the unread / sort filters.
  // The hovered row is unmounted by the re-render without firing `mouseleave`,
  // so without this the preview lingers over the new list (or a pending timer
  // fires later and measures a now-detached row, placing the preview at 0,0).
  useEffect(() => {
    window.clearTimeout(hoverTimer.current);
    setHover(null);
  }, [query, unreadOnly, sortOldest]);

  // Jump back to the top of the list whenever the sidebar selection changes.
  // The scroll container stays mounted across the query swap, so without this
  // a new feed/folder/tag opens scrolled to wherever the *previous* list was
  // left — burying its newest articles below the fold. `scrollToOffset(0)`
  // also resets the virtualizer's internal offset, keeping its rendered window
  // in sync with the DOM scroll position. Skipped when the query change came
  // from opening a specific article (search → feed + article in one step), so
  // the selected-article scroll above isn't overridden back to the top.
  useEffect(() => {
    if (selectedId == null) virt.scrollToOffset(0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query]);

  const markAll = async () => {
    try {
      const n = await api.markAllRead(query);
      actions.refreshAfterBulk();
      onToast(
        n > 0
          ? t("articleList.markedReadToast", { count: n })
          : t("articleList.nothingToMark"),
      );
    } catch (e) {
      reportError(e);
    }
  };

  const onHover = (a: ArticleSummary, e: React.MouseEvent) => {
    window.clearTimeout(hoverTimer.current);
    // Hold the row element, not a rect snapshot: the preview only appears
    // 650ms later, and the list is scrollable — measuring at hover time would
    // anchor the preview to where the row *was*, so a scroll during the delay
    // (a common "scroll, then pause on a row" gesture) leaves it floating over
    // unrelated rows or off-screen. Re-measure inside the timer instead, when
    // the preview actually opens, so it tracks the row's live position.
    const row = e.currentTarget;
    hoverTimer.current = window.setTimeout(() => {
      const rect = row.getBoundingClientRect();
      setHover({ article: a, top: rect.top + 4, left: rect.right + 12 });
    }, 650);
  };
  const leaveHover = () => {
    window.clearTimeout(hoverTimer.current);
    setHover(null);
  };

  const articleMenu = (a: ArticleSummary): MenuEntry[] => [
    { icon: "open", label: t("articleList.menuOpen"), shortcut: "⏎", onClick: () => openArticle(a.id) },
    ...(a.url
      ? ([
          {
            icon: "globe",
            label: t("articleList.menuOpenInBrowser"),
            shortcut: modCombo("O"),
            onClick: () => openUrl(a.url!).catch(() => {}),
          },
        ] as MenuEntry[])
      : []),
    { separator: true },
    {
      icon: a.isStarred ? "star-fill" : "star",
      label: a.isStarred ? t("articleList.menuUnstar") : t("articleList.menuStar"),
      shortcut: "S",
      onClick: () => actions.setStarred(a.id, !a.isStarred),
    },
    {
      icon: a.readLater ? "bookmark-fill" : "bookmark",
      label: a.readLater
        ? t("articleList.menuRemoveReadLater")
        : t("articleList.menuAddReadLater"),
      shortcut: "B",
      onClick: () => actions.setReadLater(a.id, !a.readLater),
    },
    {
      icon: a.isRead ? "circle" : "check",
      label: a.isRead ? t("articleList.menuMarkUnread") : t("articleList.menuMarkRead"),
      shortcut: "U",
      onClick: () => actions.setRead(a.id, !a.isRead),
    },
    ...(a.url
      ? ([
          { separator: true },
          {
            icon: "copy",
            label: t("articleList.menuCopyLink"),
            onClick: () =>
              navigator.clipboard
                .writeText(a.url!)
                .then(() => onToast(t("articleList.linkCopied")), () => {}),
          },
        ] as MenuEntry[])
      : []),
  ];

  const vItems = virt.getVirtualItems();
  const showCount = t("articleList.countArticles", {
    count: items.length,
    suffix: browse.hasNextPage ? "+" : "",
  });

  // Arrow-key navigation for the listbox (in addition to the global j/k).
  const onListKeyDown = (e: React.KeyboardEvent) => {
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
    if (items.length === 0) return;
    e.preventDefault();
    const cur = items.findIndex((x) => x.id === selectedId);
    const next =
      e.key === "Home"
        ? 0
        : e.key === "End"
          ? items.length - 1
          : e.key === "ArrowDown"
            ? Math.min(items.length - 1, cur < 0 ? 0 : cur + 1)
            : Math.max(0, cur < 0 ? 0 : cur - 1);
    openArticle(items[next].id);
  };

  return (
    <div className="list" role="region" aria-labelledby="article-list-title">
      <div className="list-header" {...(isMac && { "data-tauri-drag-region": true })}>
        <h1 className="list-title" id="article-list-title">
          {/* Smart views re-translate live; feed/folder/tag keep their own title. */}
          {query.kind === "feed" ||
          query.kind === "folder" ||
          query.kind === "tag"
            ? queryLabel
            : t(`smart.${query.kind}`)}
          <span className="count">{browse.isLoading ? t("common.loading") : showCount}</span>
        </h1>
        <div className="list-meta">
          <button
            className={`list-meta-btn ${!sortOldest ? "on" : ""}`}
            onClick={toggleSort}
            title={t("articleList.sort")}
          >
            <Icon name={sortOldest ? "arrow-up" : "arrow-down"} size={12} />
            {sortOldest ? t("articleList.oldestFirst") : t("articleList.newestFirst")}
          </button>
          <button
            className={`list-meta-btn ${unreadOnly ? "on" : ""}`}
            onClick={toggleUnreadOnly}
            title={t("articleList.hideRead")}
          >
            <Icon name={unreadOnly ? "eye-off" : "eye"} size={12} />
            {unreadOnly ? t("articleList.unreadOnly") : t("smart.all")}
          </button>
          <div style={{ flex: 1 }} />
          <button
            className="list-meta-btn"
            onClick={markAll}
            title={t("articleList.markAllRead")}
          >
            <Icon name="check-all" size={12} />
            {t("articleList.markRead")}
          </button>
        </div>
      </div>

      <div className="list-scroll" ref={scrollRef}>
        {browse.isLoading && (
          <div>
            {Array.from({ length: 7 }).map((_, i) => (
              <div className="sk-art" key={i}>
                <div className="sk-line" style={{ width: "40%" }} />
                <div className="sk-line" style={{ width: "92%", height: 12 }} />
                <div className="sk-line" style={{ width: "70%" }} />
              </div>
            ))}
          </div>
        )}

        {/* A failed fetch must not masquerade as "all caught up". */}
        {!browse.isLoading && browse.isError && items.length === 0 && (
          <div className="empty" style={{ height: 240 }}>
            <div className="glyph">
              <Icon name="alert" size={22} />
            </div>
            <div>{t("articleList.loadError")}</div>
            <button
              className="empty-retry"
              onClick={() => browse.refetch()}
              disabled={browse.isFetching}
            >
              <Icon name="refresh" size={12} />
              {t("common.retry")}
            </button>
          </div>
        )}

        {!browse.isLoading && !browse.isError && items.length === 0 && (
          <div className="empty" style={{ height: 240 }}>
            <div className="glyph">
              <Icon name="check" size={22} />
            </div>
            <div>{t("articleList.emptyState")}</div>
          </div>
        )}

        {!browse.isLoading && items.length > 0 && (
          <div
            role="listbox"
            tabIndex={0}
            aria-labelledby="article-list-title"
            aria-activedescendant={
              selectedId != null ? `option-article-${selectedId}` : undefined
            }
            onKeyDown={onListKeyDown}
            style={{
              height: virt.getTotalSize(),
              position: "relative",
              width: "100%",
            }}
          >
            {vItems.map((vi) => {
              const a = items[vi.index];
              const feed = feedById[a.feedId];
              return (
                // Key by the virtual slot, not the article id. The window of
                // rendered rows is a fixed band that slides as you scroll, so
                // keying by index lets React keep the same ~dozen DOM nodes
                // mounted and just swap their content + transform. Keying by
                // `a.id` instead remounts a node every time the window slides,
                // restarting `measureElement` from its estimate each time — a
                // freshly mounted row briefly reports the 98px estimate before
                // the real ~130px is measured, so the row below it renders too
                // high and overlaps. (It also collides when offset pagination
                // returns the same article on two pages.)
                <div
                  key={vi.key}
                  data-index={vi.index}
                  ref={virt.measureElement}
                  style={{
                    position: "absolute",
                    top: 0,
                    left: 0,
                    width: "100%",
                    transform: `translateY(${vi.start}px)`,
                  }}
                >
                  <div
                    className={`art ${viewMode === "card" ? "card" : ""} ${
                      selectedId === a.id ? "active" : ""
                    } ${a.isRead ? "read" : ""}`}
                    role="option"
                    id={`option-article-${a.id}`}
                    aria-selected={selectedId === a.id}
                    onClick={() => openArticle(a.id)}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      setMenu({ x: e.clientX, y: e.clientY, article: a });
                    }}
                    onMouseEnter={(e) => onHover(a, e)}
                    onMouseLeave={leaveHover}
                  >
                    {viewMode === "card" && showCardThumbs && (
                      <CardThumb article={a} />
                    )}
                    <div className="art-head">
                      {!a.isRead && <span className="art-dot" />}
                      <span className="art-feed">{a.feedTitle}</span>
                      {feed && feed.sourceType !== "rss" && (
                        <span className="src-badge">{feed.sourceType}</span>
                      )}
                      <span className="art-sep">·</span>
                      <span className="art-time">{relTime(a.publishedAt)}</span>
                      {a.isStarred && (
                        <span className="art-star">
                          <Icon name="star-fill" size={12} />
                        </span>
                      )}
                      {a.readLater && !a.isStarred && (
                        <span className="art-star">
                          <Icon name="bookmark-fill" size={12} />
                        </span>
                      )}
                    </div>
                    <h3 className="art-title">{a.title}</h3>
                    {a.snippet && <p className="art-snippet">{a.snippet}</p>}
                  </div>
                </div>
              );
            })}
          </div>
        )}
        <div style={{ height: 60 }} />
      </div>

      {hover && <HoverPreview {...hover} feedTitle={hover.article.feedTitle} />}

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={articleMenu(menu.article)}
          onClose={() => setMenu(null)}
        />
      )}

    </div>
  );
}

/** Card-view thumbnail: the article image, or nothing. When a card has no
 *  usable image — none supplied and none extractable from the body, or the
 *  image fails to load — the card simply renders without a thumbnail rather
 *  than showing a generic placeholder. */
function CardThumb({ article }: { article: ArticleSummary }) {
  const [broken, setBroken] = useState(false);
  // The virtualizer recycles this instance across rows — clear the error
  // flag whenever the image URL changes.
  useEffect(() => setBroken(false), [article.imageUrl]);

  if (!article.imageUrl || broken) return null;

  return (
    <div className="art-thumb">
      <img
        src={article.imageUrl}
        alt=""
        loading="lazy"
        onError={() => setBroken(true)}
        style={{
          position: "absolute",
          inset: 0,
          width: "100%",
          height: "100%",
          objectFit: "cover",
        }}
      />
    </div>
  );
}

function HoverPreview({
  article,
  top,
  left,
  feedTitle,
}: Hover & { feedTitle: string }) {
  // Clamp the preview inside the viewport. The card is a fixed 340px wide;
  // the 192px height below pairs with the 8px margin to keep the historical
  // `innerHeight - 200` bottom pull-back. The shared helper bounds both edges
  // so a narrow/short window can't shove the preview off the top-left corner.
  const { left: adjLeft, top: adjTop } = clampToViewport({
    x: left,
    y: top,
    width: 340,
    height: 192,
    margin: 8,
  });
  return (
    <div className="hover-preview" style={{ top: adjTop, left: adjLeft }}>
      <div className="hp-feed">{feedTitle}</div>
      <div className="hp-title">{article.title}</div>
      {article.snippet && <div className="hp-body">{article.snippet}</div>}
      <div className="hp-meta">
        {[article.author, relTime(article.publishedAt)].filter(Boolean).join(" · ")}
      </div>
    </div>
  );
}
