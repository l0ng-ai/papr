import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { useUi } from "../store";
import { useArticleActions } from "../hooks/articleActions";
import { withUndo, reportError } from "../toast";
import { isMac, modCombo } from "../lib/platform";
import { tagColor, TAG_PALETTE } from "../lib/tagColors";
import type { ArticleQuery, Feed, Folder, Tag } from "../types";
import Icon, { type IconName } from "./Icon";
import ContextMenu, { type MenuEntry } from "./ContextMenu";
import FeedAvatar from "./FeedAvatar";
import PromptDialog from "./PromptDialog";

interface Props {
  onAddFeed: () => void;
  /** Opens the Add-feed dialog on its Explore tab. */
  onExplore: () => void;
  onOpenSettings: (section?: string) => void;
  onSearchClick: () => void;
  onRefresh: () => void;
  refreshing: boolean;
  onToast: (msg: string) => void;
}

const sameQuery = (a: ArticleQuery, b: ArticleQuery) =>
  JSON.stringify(a) === JSON.stringify(b);

/** Enter / Space activator for a div that behaves as a button — gives the
 *  sidebar's clickable rows keyboard parity with their onClick. */
const onActivate = (fn: () => void) => (e: React.KeyboardEvent) => {
  if (e.key === "Enter" || e.key === " ") {
    e.preventDefault();
    fn();
  }
};

type Menu =
  | { x: number; y: number; kind: "feed"; feed: Feed }
  | { x: number; y: number; kind: "folder"; folder: Folder }
  | { x: number; y: number; kind: "tag"; tag: Tag };

type Prompt = {
  title: string;
  initial: string;
  placeholder: string;
  onSubmit: (v: string) => void;
};

function SbItem({
  icon,
  label,
  count,
  active,
  onClick,
}: {
  icon: IconName;
  label: string;
  count?: number;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <div
      className={`sb-item ${active ? "active" : ""}`}
      role="button"
      tabIndex={0}
      aria-current={active || undefined}
      onClick={onClick}
      onKeyDown={onActivate(onClick)}
    >
      <span className="sb-ico">
        <Icon name={icon} size={15} />
      </span>
      <span className="sb-label">{label}</span>
      {count != null && count > 0 && <span className="sb-count">{count}</span>}
    </div>
  );
}

export default function Sidebar({
  onAddFeed,
  onExplore,
  onOpenSettings,
  onSearchClick,
  onRefresh,
  refreshing,
  onToast,
}: Props) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const actions = useArticleActions();
  const query = useUi((s) => s.query);
  const select = useUi((s) => s.select);
  const showCounts = useUi((s) => s.prefs.showSidebarCounts);

  const feeds = useQuery({ queryKey: ["feeds"], queryFn: api.listFeeds });
  const folders = useQuery({ queryKey: ["folders"], queryFn: api.listFolders });
  const counts = useQuery({ queryKey: ["counts"], queryFn: api.smartCounts });
  const tags = useQuery({ queryKey: ["tags"], queryFn: api.listTags });

  const [collapsed, setCollapsed] = useState<Record<number, boolean>>(() => {
    try {
      return JSON.parse(localStorage.getItem("collapsedFolders") || "{}");
    } catch {
      return {};
    }
  });
  useEffect(() => {
    localStorage.setItem("collapsedFolders", JSON.stringify(collapsed));
  }, [collapsed]);

  const [menu, setMenu] = useState<Menu | null>(null);
  const [prompt, setPrompt] = useState<Prompt | null>(null);
  const [dragId, setDragId] = useState<number | null>(null);
  const [dropFolder, setDropFolder] = useState<number | "none" | null>(null);
  const [tagDragId, setTagDragId] = useState<number | null>(null);
  const [tagOverId, setTagOverId] = useState<number | null>(null);

  // Feed/folder/tag mutations only touch the article-bearing caches — a bare
  // invalidateQueries() would also refetch AI summaries, settings and storage
  // stats. refreshAfterBulk() invalidates just the relevant keys.
  const guard = (p: Promise<unknown>, ok: string) =>
    p
      .then(() => {
        actions.refreshAfterBulk();
        onToast(ok);
      })
      .catch((e) => reportError(e));

  // Destructive feed/folder/tag deletes run behind an Undo window: the row
  // disappears at once, but the irreversible backend call is deferred ~6s so
  // a misclick can be taken back. `makeDelete` is a thunk — the backend call
  // must not start until the window actually closes.
  //
  // The optimistic removal also resets a now-dangling selection: the active
  // view, or the open article, may point at the entity that just vanished.
  // Deleting a feed cascade-deletes its articles, so a still-open article
  // from it is closed too (the reader would otherwise show a load error).
  const guardDelete = (
    makeDelete: () => Promise<unknown>,
    toastText: string,
    kind: "feed" | "folder" | "tag",
    id: number,
  ) => {
    const cacheKey =
      kind === "feed" ? "feeds" : kind === "folder" ? "folders" : "tags";
    // Snapshots taken before the optimistic edit, restored verbatim on undo
    // (or if the eventual delete fails).
    const prevList = qc.getQueryData<{ id: number }[]>([cacheKey]);
    const prevFeeds =
      kind === "folder" ? qc.getQueryData<Feed[]>(["feeds"]) : undefined;
    const restore = () => {
      if (prevList) qc.setQueryData([cacheKey], prevList);
      if (prevFeeds) qc.setQueryData(["feeds"], prevFeeds);
    };

    withUndo({
      text: toastText,
      apply: () => {
        qc.setQueryData<{ id: number }[]>([cacheKey], (old) =>
          old?.filter((x) => x.id !== id),
        );
        // Deleting a folder orphans its feeds to "uncategorized" (the DB FK
        // is ON DELETE SET NULL) — mirror that so they don't briefly vanish
        // from the tree during the undo window.
        if (kind === "folder") {
          qc.setQueryData<Feed[]>(["feeds"], (old) =>
            old?.map((f) =>
              f.folderId === id ? { ...f, folderId: null } : f,
            ),
          );
        }
        const st = useUi.getState();
        if (st.query.kind === kind && st.query.value === id) {
          st.select({ kind: "all" }, t("smart.all"));
        } else if (kind === "feed" && st.selectedArticleId != null) {
          const open = qc.getQueryData<{ feedId: number }>([
            "article",
            st.selectedArticleId,
          ]);
          if (open?.feedId === id) st.openArticle(null);
        }
      },
      commit: () => {
        makeDelete()
          .then(() => actions.refreshAfterBulk())
          .catch((e) => {
            restore();
            reportError(e);
          });
      },
      revert: restore,
    });
  };

  const allFeeds = feeds.data ?? [];
  const allFolders = folders.data ?? [];
  const allTags = tags.data ?? [];
  const ungrouped = allFeeds.filter((f) => f.folderId == null);
  const isActive = (q: ArticleQuery) => sameQuery(q, query);

  // ── drag to move a feed between folders ──
  const handleDrop = (target: number | null) => {
    const feed = allFeeds.find((f) => f.id === dragId);
    setDragId(null);
    setDropFolder(null);
    if (!feed || feed.folderId === target) return;
    const folderName =
      target == null
        ? t("sidebar.uncategorized")
        : allFolders.find((f) => f.id === target)?.name ?? "";
    guard(
      api.moveFeed(feed.id, target),
      t("sidebar.toastMoved", { feed: feed.title, folder: folderName }),
    );
  };

  // ── feed / folder context menus ──
  const feedMenu = (f: Feed): MenuEntry[] => {
    const moves: MenuEntry[] = allFolders
      .filter((fo) => fo.id !== f.folderId)
      .map((fo) => ({
        icon: "folder" as const,
        label: t("sidebar.moveToFolder", { folder: fo.name }),
        onClick: () =>
          guard(
            api.moveFeed(f.id, fo.id),
            t("sidebar.toastMovedTo", { folder: fo.name }),
          ),
      }));
    if (f.folderId != null)
      moves.push({
        icon: "folder",
        label: t("sidebar.moveOutOfFolder"),
        onClick: () =>
          guard(api.moveFeed(f.id, null), t("sidebar.toastMovedOut")),
      });
    return [
      {
        icon: "check-all",
        label: t("sidebar.markAllRead"),
        onClick: () =>
          guard(
            api.markAllRead({ kind: "feed", value: f.id }),
            t("sidebar.toastMarkedAllRead"),
          ),
      },
      {
        icon: "settings",
        label: t("sidebar.renameMenu"),
        onClick: () =>
          setPrompt({
            title: t("sidebar.renameFeedTitle"),
            initial: f.title,
            placeholder: t("sidebar.feedNamePlaceholder"),
            onSubmit: (v) =>
              guard(api.renameFeed(f.id, v), t("sidebar.toastRenamed")),
          }),
      },
      ...(moves.length ? [{ separator: true } as MenuEntry, ...moves] : []),
      { separator: true },
      {
        icon: "trash",
        label: t("sidebar.unsubscribe"),
        danger: true,
        onClick: () =>
          guardDelete(
            () => api.deleteFeed(f.id),
            t("sidebar.toastUnsubscribed", { feed: f.title }),
            "feed",
            f.id,
          ),
      },
    ];
  };

  const folderMenu = (folder: Folder): MenuEntry[] => [
    {
      icon: "check-all",
      label: t("sidebar.markAllRead"),
      onClick: () =>
        guard(
          api.markAllRead({ kind: "folder", value: folder.id }),
          t("sidebar.toastMarkedAllRead"),
        ),
    },
    {
      icon: "settings",
      label: t("sidebar.renameMenu"),
      onClick: () =>
        setPrompt({
          title: t("sidebar.renameFolderTitle"),
          initial: folder.name,
          placeholder: t("sidebar.folderNamePlaceholder"),
          onSubmit: (v) =>
            guard(api.renameFolder(folder.id, v), t("sidebar.toastRenamed")),
        }),
    },
    { separator: true },
    {
      icon: "trash",
      label: t("sidebar.deleteFolder"),
      danger: true,
      onClick: () =>
        guardDelete(
          () => api.deleteFolder(folder.id),
          t("sidebar.toastFolderDeleted"),
          "folder",
          folder.id,
        ),
    },
  ];

  const tagMenu = (tag: Tag): MenuEntry[] => {
    const idx = allTags.findIndex((tg) => tg.id === tag.id);
    return [
    // Drag-reorder isn't reachable by keyboard — these give it parity.
    ...(idx > 0
      ? ([
          {
            icon: "arrow-up",
            label: t("sidebar.moveTagUp"),
            onClick: () => moveTag(tag.id, -1),
          },
        ] as MenuEntry[])
      : []),
    ...(idx >= 0 && idx < allTags.length - 1
      ? ([
          {
            icon: "arrow-down",
            label: t("sidebar.moveTagDown"),
            onClick: () => moveTag(tag.id, 1),
          },
        ] as MenuEntry[])
      : []),
    ...(allTags.length > 1 ? [{ separator: true } as MenuEntry] : []),
    {
      icon: "settings",
      label: t("sidebar.renameMenu"),
      onClick: () =>
        setPrompt({
          title: t("sidebar.renameTagTitle"),
          initial: tag.name,
          placeholder: t("sidebar.tagNamePlaceholder"),
          onSubmit: (v) =>
            guard(api.renameTag(tag.id, v), t("sidebar.toastRenamed")),
        }),
    },
    {
      swatches: Object.entries(TAG_PALETTE).map(([value, color]) => ({
        value,
        color,
      })),
      current: tag.color,
      // The recolour is instantly visible on the dot, so no toast — just
      // refresh the caches that embed the tag colour.
      onPick: (color) =>
        api
          .setTagColor(tag.id, color)
          .then(() => actions.refreshAfterBulk())
          .catch((e) => reportError(e)),
    },
    { separator: true },
    {
      icon: "trash",
      label: t("sidebar.deleteTag"),
      danger: true,
      onClick: () =>
        guardDelete(
          () => api.deleteTag(tag.id),
          t("sidebar.toastTagDeleted"),
          "tag",
          tag.id,
        ),
    },
    ];
  };

  const createTag = () =>
    setPrompt({
      title: t("sidebar.newTagTitle"),
      initial: "",
      placeholder: t("sidebar.tagNamePlaceholder"),
      onSubmit: (v) => guard(api.createTag(v), t("sidebar.toastTagCreated")),
    });

  // Creating a folder only touches the folders list, which `refreshAfterBulk`
  // (and thus `guard`) doesn't cover — invalidate it explicitly.
  const createFolder = () =>
    setPrompt({
      title: t("app.newFolderTitle"),
      initial: "",
      placeholder: t("app.folderNamePlaceholder"),
      onSubmit: (v) =>
        api
          .createFolder(v)
          .then(() => {
            qc.invalidateQueries({ queryKey: ["folders"] });
            onToast(t("app.folderCreated"));
          })
          .catch((e) => reportError(e)),
    });

  // Optimistically apply a new tag order, then persist; reconcile on either
  // outcome. Shared by the drag-reorder and the menu's move up/down.
  const persistTagOrder = (next: Tag[]) => {
    qc.setQueryData(["tags"], next);
    api
      .reorderTags(next.map((tg) => tg.id))
      .catch((e) => reportError(e))
      .finally(() => qc.invalidateQueries({ queryKey: ["tags"] }));
  };

  // ── drag to reorder tags ──
  const dropTag = (targetId: number) => {
    const from = allTags.findIndex((tg) => tg.id === tagDragId);
    const to = allTags.findIndex((tg) => tg.id === targetId);
    setTagDragId(null);
    setTagOverId(null);
    if (from < 0 || to < 0 || from === to) return;
    const next = [...allTags];
    const [moved] = next.splice(from, 1);
    // The `drop-above` indicator marks an insertion point *before* the target
    // tag. After removing the dragged item, every index past `from` shifts
    // down by one — so a downward drag must insert at `to - 1` to land the
    // tag above the target, not below it.
    const insertAt = from < to ? to - 1 : to;
    next.splice(insertAt, 0, moved);
    persistTagOrder(next);
  };

  // Keyboard-reachable counterpart to the drag-reorder: swap a tag with its
  // neighbour. `dir` is -1 (up) or 1 (down).
  const moveTag = (tagId: number, dir: -1 | 1) => {
    const i = allTags.findIndex((tg) => tg.id === tagId);
    const j = i + dir;
    if (i < 0 || j < 0 || j >= allTags.length) return;
    const next = [...allTags];
    [next[i], next[j]] = [next[j], next[i]];
    persistTagOrder(next);
  };

  // ── feed row ──
  const feedRow = (f: Feed) => (
    <div
      key={f.id}
      className={`sb-item ${
        isActive({ kind: "feed", value: f.id }) ? "active" : ""
      } ${dragId === f.id ? "dragging" : ""}`}
      role="button"
      tabIndex={0}
      aria-current={isActive({ kind: "feed", value: f.id }) || undefined}
      draggable
      onDragStart={() => setDragId(f.id)}
      onDragEnd={() => {
        setDragId(null);
        setDropFolder(null);
      }}
      onClick={() => select({ kind: "feed", value: f.id }, f.title)}
      onKeyDown={onActivate(() => select({ kind: "feed", value: f.id }, f.title))}
      onContextMenu={(e) => {
        e.preventDefault();
        setMenu({ x: e.clientX, y: e.clientY, kind: "feed", feed: f });
      }}
      title={f.fetchError ?? f.title}
    >
      <FeedAvatar title={f.title} faviconUrl={f.faviconUrl} seed={f.id} />
      <span className="sb-label">{f.title}</span>
      {f.fetchError && (
        <span className="sb-warn" role="img" aria-label={t("sidebar.feedError")}>
          !
        </span>
      )}
      {showCounts && f.unreadCount > 0 && (
        <span className="sb-count">{f.unreadCount}</span>
      )}
    </div>
  );

  return (
    <div className="sidebar" role="navigation">
      {isMac && <div className="titlebar" data-tauri-drag-region />}

      {isMac && (
        <div className="sb-brand">
          <img className="sb-brand-mark" src="/papr.svg" alt="" />
          <span className="sb-brand-name">Papr</span>
        </div>
      )}

      <div
        className="sidebar-search"
        role="button"
        tabIndex={0}
        onClick={onSearchClick}
        onKeyDown={onActivate(onSearchClick)}
      >
        <Icon name="search" size={13} />
        <span>{t("sidebar.searchArticles")}</span>
        <kbd>{modCombo("K")}</kbd>
      </div>

      <div className="sidebar-scroll">
        <div className="sb-section-title">
          <span>{t("sidebar.library")}</span>
        </div>
        <SbItem
          icon="inbox"
          label={t("smart.all")}
          active={isActive({ kind: "all" })}
          onClick={() => select({ kind: "all" }, t("smart.all"))}
        />
        <SbItem
          icon="unread"
          label={t("smart.unread")}
          count={showCounts ? counts.data?.unread : undefined}
          active={isActive({ kind: "unread" })}
          onClick={() => select({ kind: "unread" }, t("smart.unread"))}
        />
        <SbItem
          icon="star"
          label={t("smart.starred")}
          count={showCounts ? counts.data?.starred : undefined}
          active={isActive({ kind: "starred" })}
          onClick={() => select({ kind: "starred" }, t("smart.starred"))}
        />
        <SbItem
          icon="bookmark"
          label={t("smart.readLater")}
          count={showCounts ? counts.data?.readLater : undefined}
          active={isActive({ kind: "readLater" })}
          onClick={() => select({ kind: "readLater" }, t("smart.readLater"))}
        />

        <div className="sb-section-title">
          <span>{t("sidebar.feeds")}</span>
          <span className="sb-section-actions">
            <button
              onClick={createFolder}
              title={t("app.newFolderTitle")}
              aria-label={t("app.newFolderTitle")}
            >
              <Icon name="folder" size={12} />
            </button>
            <button
              onClick={onAddFeed}
              title={t("sidebar.addFeed")}
              aria-label={t("sidebar.addFeed")}
            >
              <Icon name="plus" size={12} />
            </button>
          </span>
        </div>

        {allFeeds.length === 0 && (
          <div
            style={{
              padding: "10px 12px",
              fontSize: 12,
              color: "var(--muted)",
              lineHeight: 1.5,
            }}
          >
            {t("sidebar.emptyHint")}
          </div>
        )}

        {/* Ungrouped feeds — also the drop zone for "move out of folder".
            Rendered whenever there are ungrouped feeds, OR while a feed drag
            is in progress: without the latter, a drag-to-ungroup would have
            no target to land on once every feed lives inside a folder. */}
        {(ungrouped.length > 0 ||
          (dragId != null &&
            allFeeds.find((f) => f.id === dragId)?.folderId != null)) && (
          <div
            onDragOver={(e) => {
              if (dragId != null) {
                e.preventDefault();
                setDropFolder("none");
              }
            }}
            onDrop={() => handleDrop(null)}
            style={
              dropFolder === "none"
                ? { outline: "2px solid var(--accent)", borderRadius: 8 }
                : undefined
            }
          >
            {ungrouped.length > 0 ? (
              ungrouped.map(feedRow)
            ) : (
              <div className="sb-drop-hint">{t("sidebar.dropUngroup")}</div>
            )}
          </div>
        )}

        {allFolders.map((folder) => {
          const inFolder = allFeeds.filter((f) => f.folderId === folder.id);
          const isCollapsed = collapsed[folder.id];
          // Aggregate unread for the folder. Shown only while collapsed —
          // expanded, the per-feed badges already carry the same signal, and a
          // header total would just duplicate them.
          const folderUnread = inFolder.reduce((n, f) => n + f.unreadCount, 0);
          return (
            <div
              key={folder.id}
              onDragOver={(e) => {
                if (dragId != null) {
                  e.preventDefault();
                  setDropFolder(folder.id);
                }
              }}
              onDrop={() => handleDrop(folder.id)}
              style={
                dropFolder === folder.id
                  ? { outline: "2px solid var(--accent)", borderRadius: 8 }
                  : undefined
              }
            >
              <div
                className={`sb-folder ${isCollapsed ? "collapsed" : ""}`}
                role="button"
                tabIndex={0}
                aria-expanded={!isCollapsed}
                onClick={() =>
                  setCollapsed((s) => ({ ...s, [folder.id]: !isCollapsed }))
                }
                onKeyDown={onActivate(() =>
                  setCollapsed((s) => ({ ...s, [folder.id]: !isCollapsed })),
                )}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setMenu({ x: e.clientX, y: e.clientY, kind: "folder", folder });
                }}
              >
                <Icon name="chevron-down" size={11} />
                <span className="sb-folder-name">{folder.name}</span>
                {showCounts && isCollapsed && folderUnread > 0 && (
                  <span className="sb-count">{folderUnread}</span>
                )}
              </div>
              {!isCollapsed && inFolder.map(feedRow)}
            </div>
          );
        })}

        <div className="sb-section-title">
          <span>{t("sidebar.tags")}</span>
          <button
            onClick={createTag}
            title={t("sidebar.newTagTitle")}
            aria-label={t("sidebar.newTagTitle")}
          >
            <Icon name="plus" size={12} />
          </button>
        </div>
        {allTags.length === 0 && (
          <div
            style={{
              padding: "4px 12px 2px",
              fontSize: 11.5,
              color: "var(--muted)",
              lineHeight: 1.5,
            }}
          >
            {t("sidebar.tagsEmptyHint")}
          </div>
        )}
        {allTags.map((tag) => (
          <div
            key={tag.id}
            className={`sb-item ${
              isActive({ kind: "tag", value: tag.id }) ? "active" : ""
            } ${tagDragId === tag.id ? "dragging" : ""} ${
              tagOverId === tag.id ? "drop-above" : ""
            }`}
            role="button"
            tabIndex={0}
            aria-current={isActive({ kind: "tag", value: tag.id }) || undefined}
            draggable
            onDragStart={() => setTagDragId(tag.id)}
            onDragEnd={() => {
              setTagDragId(null);
              setTagOverId(null);
            }}
            onDragOver={(e) => {
              if (tagDragId != null && tagDragId !== tag.id) {
                e.preventDefault();
                setTagOverId(tag.id);
              }
            }}
            onDrop={() => dropTag(tag.id)}
            onClick={() => select({ kind: "tag", value: tag.id }, tag.name)}
            onKeyDown={onActivate(() =>
              select({ kind: "tag", value: tag.id }, tag.name),
            )}
            onContextMenu={(e) => {
              e.preventDefault();
              setMenu({ x: e.clientX, y: e.clientY, kind: "tag", tag });
            }}
          >
            <span className="sb-ico">
              <span
                className="tag-dot"
                style={{ background: tagColor(tag.color) }}
              />
            </span>
            <span className="sb-label">{tag.name}</span>
            {showCounts && tag.articleCount > 0 && (
              <span className="sb-count">{tag.articleCount}</span>
            )}
          </div>
        ))}

        <div style={{ height: 30 }} />
      </div>

      <div className="sb-footer">
        <button
          title={t("sidebar.addFeedShortcut")}
          aria-label={t("sidebar.addFeedShortcut")}
          onClick={onAddFeed}
        >
          <Icon name="plus" size={14} />
        </button>
        <button
          title={t("sidebar.refreshAll")}
          aria-label={t("sidebar.refreshAll")}
          onClick={onRefresh}
          disabled={refreshing}
          className={refreshing ? "spinning" : ""}
        >
          <Icon name="refresh" size={14} />
        </button>
        <button
          title={t("sidebar.explore")}
          aria-label={t("sidebar.explore")}
          onClick={onExplore}
        >
          <Icon name="globe" size={14} />
        </button>
        <div className="spacer" />
        <button
          title={t("sidebar.settings")}
          aria-label={t("sidebar.settings")}
          onClick={() => onOpenSettings()}
        >
          <Icon name="settings" size={14} />
        </button>
      </div>

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={
            menu.kind === "feed"
              ? feedMenu(menu.feed)
              : menu.kind === "folder"
                ? folderMenu(menu.folder)
                : tagMenu(menu.tag)
          }
          onClose={() => setMenu(null)}
        />
      )}
      {prompt && (
        <PromptDialog
          title={prompt.title}
          initialValue={prompt.initial}
          placeholder={prompt.placeholder}
          onSubmit={prompt.onSubmit}
          onClose={() => setPrompt(null)}
        />
      )}
    </div>
  );
}
