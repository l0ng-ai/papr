import { useMutation, useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { useArticleActions } from "../hooks/articleActions";
import { useFocusTrap } from "../hooks/useFocusTrap";
import { reportError } from "../toast";
import { NO_AUTOCORRECT } from "../lib/inputProps";
import FeedAvatar from "./FeedAvatar";
import Icon from "./Icon";

interface Props {
  onClose: () => void;
  onToast: (msg: string) => void;
}

/**
 * A favicon URL for a directory entry, derived from its site URL via the
 * same Google s2 service `add_feed` uses on the backend. `FeedAvatar`
 * gracefully falls back to a coloured letter when the icon fails to load.
 */
function faviconFor(siteUrl: string | null | undefined): string | null {
  if (!siteUrl) return null;
  try {
    const host = new URL(siteUrl).hostname;
    return `https://www.google.com/s2/favicons?domain=${host}&sz=64`;
  } catch {
    return null;
  }
}

/**
 * The Explore dialog — a marketplace-style gallery of the bundled curated
 * feed directory. The whole directory is fetched once (`search_feed_directory("")`
 * returns every entry) and filtered in-memory, so the category chips and the
 * search box react instantly without round-tripping the backend. Subscribing
 * keeps the dialog open so the user can add several feeds in one session.
 */
export default function ExploreDialog({ onClose, onToast }: Props) {
  const { t, i18n } = useTranslation();
  const actions = useArticleActions();
  const dialogRef = useRef<HTMLDivElement>(null);
  useFocusTrap(dialogRef);

  const [search, setSearch] = useState("");
  const [category, setCategory] = useState<string | null>(null);
  const [folderId, setFolderId] = useState<number | null>(null);

  const directory = useQuery({
    // Keyed by language: switching the UI locale fetches that locale's slice.
    queryKey: ["directory", i18n.language],
    queryFn: () => api.searchFeedDirectory("", i18n.language),
    // The directory is a compile-time constant bundled into the binary, so
    // it never goes stale within a session.
    staleTime: Infinity,
  });
  const folders = useQuery({ queryKey: ["folders"], queryFn: api.listFolders });
  const feeds = useQuery({ queryKey: ["feeds"], queryFn: api.listFeeds });

  // Already-subscribed feed URLs — entries the user has added flip to a
  // disabled "Subscribed" state on their card.
  const subscribedUrls = useMemo(
    () => new Set((feeds.data ?? []).map((f) => f.feedUrl)),
    [feeds.data],
  );

  // `refreshAfterBulk` invalidates the `feeds` query, so the card just added
  // re-renders as "Subscribed" without any extra bookkeeping here.
  const add = useMutation({
    mutationFn: (feedUrl: string) => api.addFeed(feedUrl, folderId),
    onSuccess: (feed) => {
      actions.refreshAfterBulk();
      onToast(t("explore.subscribedToast", { title: feed.title }));
    },
    onError: (e) => reportError(e),
  });
  const pendingUrl = add.isPending ? add.variables ?? null : null;

  const entries = directory.data ?? [];

  // Categories in the directory's authored order, de-duplicated.
  const categories = useMemo(() => {
    const seen: string[] = [];
    for (const e of entries) {
      if (e.category && !seen.includes(e.category)) seen.push(e.category);
    }
    return seen;
  }, [entries]);

  // Filter by the selected category chip and a free-text search over the
  // title, description and category.
  const filtered = useMemo(() => {
    const needle = search.trim().toLowerCase();
    return entries.filter((e) => {
      if (category && e.category !== category) return false;
      if (!needle) return true;
      return (
        e.title.toLowerCase().includes(needle) ||
        (e.description ?? "").toLowerCase().includes(needle) ||
        (e.category ?? "").toLowerCase().includes(needle)
      );
    });
  }, [entries, category, search]);

  // Escape closes the dialog from anywhere inside it.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal modal-explore"
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="explore-dialog-title"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="explore-header">
          <div className="explore-header-text">
            <h2 id="explore-dialog-title">{t("explore.title")}</h2>
            <p className="explore-sub">
              {directory.isSuccess
                ? t("explore.feedCount", { n: entries.length })
                : t("explore.subtitle")}
            </p>
          </div>
          <button
            className="explore-close"
            onClick={onClose}
            aria-label={t("common.close")}
          >
            <Icon name="x" size={16} />
          </button>
        </div>

        <div className="explore-toolbar">
          <div className="explore-search">
            <Icon name="search" size={14} />
            <input
              type="text"
              autoFocus
              placeholder={t("explore.searchPlaceholder")}
              aria-label={t("explore.searchLabel")}
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              {...NO_AUTOCORRECT}
            />
          </div>
          <div
            className="explore-chips"
            role="tablist"
            aria-label={t("explore.categories")}
          >
            <button
              role="tab"
              aria-selected={category === null}
              className={`explore-chip${category === null ? " active" : ""}`}
              onClick={() => setCategory(null)}
            >
              {t("explore.all")}
            </button>
            {categories.map((c) => (
              <button
                key={c}
                role="tab"
                aria-selected={category === c}
                className={`explore-chip${category === c ? " active" : ""}`}
                onClick={() => setCategory(c)}
              >
                {c}
              </button>
            ))}
          </div>
        </div>

        <div className="explore-grid">
          {directory.isLoading && (
            <div className="discover-empty">{t("explore.loading")}</div>
          )}
          {directory.isError && (
            <div className="discover-empty">{t("explore.error")}</div>
          )}
          {directory.isSuccess && filtered.length === 0 && (
            <div className="discover-empty">{t("explore.noResults")}</div>
          )}
          {filtered.map((r) => {
            const subscribed = subscribedUrls.has(r.feedUrl);
            const pending = pendingUrl === r.feedUrl;
            return (
              <div className="explore-card" key={r.feedUrl}>
                <div className="explore-card-head">
                  <FeedAvatar
                    title={r.title}
                    faviconUrl={faviconFor(r.siteUrl)}
                    seed={r.feedUrl}
                    style={{
                      width: 34,
                      height: 34,
                      borderRadius: 9,
                      fontSize: 14,
                    }}
                  />
                  <div className="explore-card-meta">
                    <span className="explore-card-title">{r.title}</span>
                    {r.category && (
                      <span className="explore-card-cat">{r.category}</span>
                    )}
                  </div>
                </div>
                {r.description && (
                  <p className="explore-card-desc">{r.description}</p>
                )}
                <button
                  className={`s-btn explore-card-add${
                    subscribed ? "" : " primary"
                  }`}
                  disabled={subscribed || pendingUrl !== null}
                  onClick={() => add.mutate(r.feedUrl)}
                  aria-label={`${t("explore.add")} — ${r.title}`}
                >
                  {subscribed ? (
                    <>
                      <Icon name="check" size={12} />
                      {t("explore.subscribed")}
                    </>
                  ) : (
                    <>
                      <Icon name="plus" size={12} />
                      {pending ? t("explore.adding") : t("explore.add")}
                    </>
                  )}
                </button>
              </div>
            );
          })}
        </div>

        <div className="explore-footer">
          {(folders.data?.length ?? 0) > 0 ? (
            <select
              className="s-select"
              aria-label={t("explore.folderLabel")}
              value={folderId ?? ""}
              onChange={(e) =>
                setFolderId(e.target.value ? Number(e.target.value) : null)
              }
            >
              <option value="">{t("explore.noFolder")}</option>
              {folders.data!.map((f) => (
                <option key={f.id} value={f.id}>
                  {f.name}
                </option>
              ))}
            </select>
          ) : (
            <span />
          )}
          <button className="s-btn primary" onClick={onClose}>
            {t("common.done")}
          </button>
        </div>
      </div>
    </div>
  );
}
