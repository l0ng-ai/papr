import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { openUrl } from "@tauri-apps/plugin-opener";
import * as api from "../api";
import { useUi } from "../store";
import { useArticleActions } from "../hooks/articleActions";
import { feedAvatar, feedColor, relTime } from "../lib/feedMeta";
import { errorText } from "../lib/errors";
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
  const actions = useArticleActions(onToast);
  const query = useUi((s) => s.query);
  const queryLabel = useUi((s) => s.queryLabel);
  const unreadOnly = useUi((s) => s.unreadOnly);
  const toggleUnreadOnly = useUi((s) => s.toggleUnreadOnly);
  const sortOldest = useUi((s) => s.sortOldest);
  const toggleSort = useUi((s) => s.toggleSort);
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

  const browse = useInfiniteQuery({
    queryKey: ["articles", query, unreadOnly, sortOldest],
    initialPageParam: 0,
    queryFn: ({ pageParam }) =>
      api.listArticles(query, unreadOnly, null, sortOldest, PAGE, pageParam as number),
    getNextPageParam: (last, all) =>
      last.length < PAGE ? undefined : all.length * PAGE,
  });

  const items: ArticleSummary[] = useMemo(
    () => browse.data?.pages.flat() ?? [],
    [browse.data],
  );

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

  // Load the next page as the end approaches.
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
  }, [virt.getVirtualItems(), items.length, browse]);

  // Keep the keyboard-selected article visible.
  useEffect(() => {
    if (selectedId == null) return;
    const i = items.findIndex((a) => a.id === selectedId);
    if (i >= 0) virt.scrollToIndex(i, { align: "auto" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId]);

  useEffect(() => () => window.clearTimeout(hoverTimer.current), []);

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
      onToast(errorText(e));
    }
  };

  const onHover = (a: ArticleSummary, e: React.MouseEvent) => {
    window.clearTimeout(hoverTimer.current);
    const rect = e.currentTarget.getBoundingClientRect();
    hoverTimer.current = window.setTimeout(() => {
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
            shortcut: "⌘O",
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
      <div className="list-header" data-tauri-drag-region>
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
              const color = feedColor(a.feedId);
              return (
                <div
                  key={a.id}
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
                      <CardThumb article={a} color={color} />
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

/** Card-view thumbnail: the article image, falling back to a generated
 *  pattern both when there is no image and when the image fails to load. */
function CardThumb({
  article,
  color,
}: {
  article: ArticleSummary;
  color: string;
}) {
  const [broken, setBroken] = useState(false);
  // The virtualizer recycles this instance across rows — clear the error
  // flag whenever the image URL changes.
  useEffect(() => setBroken(false), [article.imageUrl]);

  return (
    <div className="art-thumb">
      {article.imageUrl && !broken ? (
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
      ) : (
        <svg viewBox="0 0 200 112" preserveAspectRatio="xMidYMid slice">
          <defs>
            <pattern
              id={`p-${article.id}`}
              width="8"
              height="8"
              patternUnits="userSpaceOnUse"
              patternTransform="rotate(135)"
            >
              <line
                x1="0"
                y1="0"
                x2="0"
                y2="8"
                stroke={color}
                strokeWidth="1.4"
                opacity="0.18"
              />
            </pattern>
          </defs>
          <rect width="200" height="112" fill={`url(#p-${article.id})`} />
          <text
            x="100"
            y="64"
            textAnchor="middle"
            fontSize="32"
            fontWeight="700"
            fill={color}
            opacity="0.55"
            fontFamily="Inter Tight, sans-serif"
          >
            {feedAvatar(article.feedTitle)}
          </text>
        </svg>
      )}
    </div>
  );
}

function HoverPreview({
  article,
  top,
  left,
  feedTitle,
}: Hover & { feedTitle: string }) {
  const adjLeft = Math.min(left, window.innerWidth - 360);
  const adjTop = Math.min(top, window.innerHeight - 200);
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
