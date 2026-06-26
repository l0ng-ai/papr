import { useMutation, useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { useArticleActions } from "../hooks/articleActions";
import { useFocusTrap } from "../hooks/useFocusTrap";
import { errorText } from "../lib/errors";
import { NO_AUTOCORRECT } from "../lib/inputProps";
import type { DiscoveryResult } from "../types";
import Icon from "./Icon";

interface Props {
  onClose: () => void;
  onToast: (msg: string) => void;
  /** Feed URL to prefill the feed tab with — set by a `papr://` deep link. */
  initialUrl?: string;
}

/** Which kind of source the dialog is currently configuring. */
type Tab = "feed" | "newsletter";

/** The IMAP port `add_newsletter_source` falls back to (implicit-TLS IMAP). */
const DEFAULT_IMAP_PORT = 993;

/**
 * Coerce the free-text port field into a valid TCP port. The backend command
 * argument is typed `u16`, so a value the user typed that is out of the
 * 1–65535 range (an extra digit) or non-integer (`993.5`) would otherwise fail
 * Tauri's argument deserialization with a cryptic, non-localised error before
 * `add_newsletter_source` even runs. Anything invalid falls back to 993.
 */
function parsePort(raw: string): number {
  const n = Number(raw.trim());
  if (!Number.isInteger(n) || n < 1 || n > 65535) return DEFAULT_IMAP_PORT;
  return n;
}

/** Subscribe to a new source — feed URL or an IMAP newsletter mailbox. */
export default function AddFeedDialog({ onClose, onToast, initialUrl }: Props) {
  const { t, i18n } = useTranslation();
  const actions = useArticleActions();
  const dialogRef = useRef<HTMLDivElement>(null);
  useFocusTrap(dialogRef);
  const [tab, setTab] = useState<Tab>("feed");

  // ── feed tab state ──
  const [url, setUrl] = useState(initialUrl ?? "");
  const [folderId, setFolderId] = useState<number | null>(null);
  const folders = useQuery({ queryKey: ["folders"], queryFn: api.listFolders });

  // A `papr://subscribe` deep link can arrive while the dialog is already
  // open (the user opened it manually first). `useState(initialUrl)` only
  // reads the prop on mount, so without this the new feed URL would be
  // silently dropped. Sync prop changes into the input and surface the feed
  // tab so the prefilled URL is visible.
  useEffect(() => {
    if (initialUrl) {
      setUrl(initialUrl);
      setTab("feed");
    }
  }, [initialUrl]);

  // ── discovery (feature F6): debounced search of the curated directory
  // plus a live page scrape when the query looks like a URL. ──
  const [debounced, setDebounced] = useState("");
  useEffect(() => {
    const handle = window.setTimeout(() => setDebounced(url.trim()), 280);
    return () => window.clearTimeout(handle);
  }, [url]);

  const discovery = useQuery({
    queryKey: ["discover", debounced, i18n.language],
    queryFn: () => api.searchFeedDirectory(debounced, i18n.language),
    // Only search once there's a meaningful query; the scrape can be slow.
    enabled: tab === "feed" && debounced.length >= 2,
    staleTime: 60_000,
  });

  // Group directory results by category; live page-scrape results (no
  // category) are kept in their own leading group.
  const grouped = useMemo(() => {
    const results = discovery.data ?? [];
    const scraped: DiscoveryResult[] = [];
    const byCategory = new Map<string, DiscoveryResult[]>();
    for (const r of results) {
      if (!r.fromDirectory || !r.category) {
        scraped.push(r);
      } else {
        const list = byCategory.get(r.category) ?? [];
        list.push(r);
        byCategory.set(r.category, list);
      }
    }
    return { scraped, byCategory };
  }, [discovery.data]);

  // ── newsletter tab state ──
  const [nlTitle, setNlTitle] = useState("");
  const [nlHost, setNlHost] = useState("");
  const [nlPort, setNlPort] = useState("993");
  const [nlUser, setNlUser] = useState("");
  const [nlPass, setNlPass] = useState("");
  const [nlFolder, setNlFolder] = useState("INBOX");

  const add = useMutation({
    mutationFn: (target: string) => api.addFeed(target, folderId),
    onSuccess: (feed) => {
      // Adding a feed touches only the article-bearing caches — refreshing
      // unrelated ones (AI summaries, settings, storage) is wasted work.
      actions.refreshAfterBulk();
      onToast(t("addFeed.subscribed", { title: feed.title }));
      onClose();
    },
  });

  const addNewsletter = useMutation({
    mutationFn: () =>
      api.addNewsletterSource({
        title: nlTitle.trim() || null,
        host: nlHost.trim(),
        port: parsePort(nlPort),
        username: nlUser.trim(),
        password: nlPass,
        folder: nlFolder.trim() || "INBOX",
      }),
    onSuccess: (feed) => {
      actions.refreshAfterBulk();
      onToast(t("addFeed.subscribed", { title: feed.title }));
      onClose();
    },
  });

  const submit = () => {
    if (tab === "feed") {
      if (url.trim() && !add.isPending) add.mutate(url.trim());
    } else {
      if (nlHost.trim() && nlUser.trim() && nlPass && !addNewsletter.isPending)
        addNewsletter.mutate();
    }
  };

  // Enter submits the newsletter form from any of its fields — matching the
  // feed tab, where Enter in the URL input subscribes. Previously only the
  // password field carried a submit handler, so a keyboard user filling the
  // form top-to-bottom (host → username → …) found Enter dead everywhere
  // except the last field. The `isComposing` guard skips the Enter that only
  // confirms an IME candidate (CJK input in the title/host/folder fields), the
  // same guard the feed URL input uses.
  const nlKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.nativeEvent.isComposing) submit();
  };

  /** Subscribe directly from a discovery result row. */
  const subscribeResult = (r: DiscoveryResult) => {
    if (!add.isPending) add.mutate(r.feedUrl);
  };

  // Escape closes the dialog from anywhere inside it, not just the input.
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

  const newsletterReady =
    nlHost.trim() !== "" && nlUser.trim() !== "" && nlPass !== "";

  const showDiscovery = tab === "feed" && debounced.length >= 2;
  const hasResults =
    grouped.scraped.length > 0 || grouped.byCategory.size > 0;

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal"
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="addfeed-dialog-title"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 id="addfeed-dialog-title">{t("addFeed.title")}</h2>

        {/* Source-type tabs: a plain feed/site URL, or an IMAP mailbox. */}
        <div className="seg" role="tablist" style={{ marginBottom: 4 }}>
          <button
            role="tab"
            aria-selected={tab === "feed"}
            className={`seg-btn${tab === "feed" ? " active" : ""}`}
            onClick={() => setTab("feed")}
          >
            {t("addFeed.tabFeed")}
          </button>
          <button
            role="tab"
            aria-selected={tab === "newsletter"}
            className={`seg-btn${tab === "newsletter" ? " active" : ""}`}
            onClick={() => setTab("newsletter")}
          >
            {t("addFeed.tabNewsletter")}
          </button>
        </div>

        {tab === "feed" ? (
          <>
            <p className="modal-hint">{t("addFeed.hint")}</p>
            <input
              className="modal-input"
              type="text"
              autoFocus
              placeholder={t("addFeed.discoverPlaceholder")}
              aria-label={t("addFeed.urlLabel")}
              {...NO_AUTOCORRECT}
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              onKeyDown={(e) => {
                // Ignore the Enter that only confirms an IME candidate.
                if (e.key === "Enter" && !e.nativeEvent.isComposing) submit();
              }}
            />

            {/* Discovery results — curated directory + live page scrape. */}
            {showDiscovery && (
              <div className="discover-results" role="listbox">
                {discovery.isLoading && (
                  <div className="discover-empty">
                    {t("addFeed.discoverSearching")}
                  </div>
                )}
                {/* A failed search must not masquerade as "no feeds found". */}
                {!discovery.isLoading && discovery.isError && (
                  <div className="discover-empty">
                    {t("addFeed.discoverError")}
                  </div>
                )}
                {!discovery.isLoading && !discovery.isError && !hasResults && (
                  <div className="discover-empty">
                    {t("addFeed.discoverNoResults")}
                  </div>
                )}
                {grouped.scraped.length > 0 && (
                  <div className="discover-group">
                    <div className="discover-group-label">
                      {t("addFeed.discoverFromPage")}
                    </div>
                    {grouped.scraped.map((r) => (
                      <DiscoverRow
                        key={r.feedUrl}
                        result={r}
                        disabled={add.isPending}
                        onSubscribe={() => subscribeResult(r)}
                        addLabel={t("addFeed.discoverAdd")}
                      />
                    ))}
                  </div>
                )}
                {[...grouped.byCategory.entries()].map(([cat, rows]) => (
                  <div className="discover-group" key={cat}>
                    <div className="discover-group-label">{cat}</div>
                    {rows.map((r) => (
                      <DiscoverRow
                        key={r.feedUrl}
                        result={r}
                        disabled={add.isPending}
                        onSubscribe={() => subscribeResult(r)}
                        addLabel={t("addFeed.discoverAdd")}
                      />
                    ))}
                  </div>
                ))}
              </div>
            )}

            {(folders.data?.length ?? 0) > 0 && (
              <select
                className="s-select"
                style={{ width: "100%" }}
                aria-label={t("addFeed.folderLabel")}
                value={folderId ?? ""}
                onChange={(e) =>
                  setFolderId(e.target.value ? Number(e.target.value) : null)
                }
              >
                <option value="">{t("addFeed.noFolder")}</option>
                {folders.data!.map((f) => (
                  <option key={f.id} value={f.id}>
                    {f.name}
                  </option>
                ))}
              </select>
            )}
            {add.isError && (
              <div className="modal-error">{errorText(add.error)}</div>
            )}
          </>
        ) : (
          <>
            <p className="modal-hint">{t("addFeed.newsletterHint")}</p>
            <input
              className="modal-input"
              type="text"
              autoFocus
              placeholder={t("addFeed.nlTitlePlaceholder")}
              aria-label={t("addFeed.nlTitleLabel")}
              {...NO_AUTOCORRECT}
              value={nlTitle}
              onChange={(e) => setNlTitle(e.target.value)}
              onKeyDown={nlKeyDown}
            />
            <div style={{ display: "flex", gap: 8 }}>
              <input
                className="modal-input"
                type="text"
                style={{ flex: 2 }}
                placeholder={t("addFeed.nlHostPlaceholder")}
                aria-label={t("addFeed.nlHostLabel")}
                {...NO_AUTOCORRECT}
                value={nlHost}
                onChange={(e) => setNlHost(e.target.value)}
                onKeyDown={nlKeyDown}
              />
              <input
                className="modal-input"
                type="text"
                style={{ flex: 1 }}
                placeholder="993"
                aria-label={t("addFeed.nlPortLabel")}
                {...NO_AUTOCORRECT}
                value={nlPort}
                onChange={(e) => setNlPort(e.target.value)}
                onKeyDown={nlKeyDown}
              />
            </div>
            <input
              className="modal-input"
              type="text"
              placeholder={t("addFeed.nlUserPlaceholder")}
              aria-label={t("addFeed.nlUserLabel")}
              {...NO_AUTOCORRECT}
              value={nlUser}
              onChange={(e) => setNlUser(e.target.value)}
              onKeyDown={nlKeyDown}
            />
            <input
              className="modal-input"
              type="password"
              placeholder={t("addFeed.nlPassPlaceholder")}
              aria-label={t("addFeed.nlPassLabel")}
              {...NO_AUTOCORRECT}
              value={nlPass}
              onChange={(e) => setNlPass(e.target.value)}
              onKeyDown={nlKeyDown}
            />
            <input
              className="modal-input"
              type="text"
              placeholder="INBOX"
              aria-label={t("addFeed.nlFolderLabel")}
              {...NO_AUTOCORRECT}
              value={nlFolder}
              onChange={(e) => setNlFolder(e.target.value)}
              onKeyDown={nlKeyDown}
            />
            {addNewsletter.isError && (
              <div className="modal-error">
                {errorText(addNewsletter.error)}
              </div>
            )}
          </>
        )}

        <div className="modal-actions">
          <button className="s-btn" onClick={onClose}>
            {t("common.cancel")}
          </button>
          {tab === "feed" ? (
            <button
              className="s-btn primary"
              onClick={submit}
              disabled={!url.trim() || add.isPending}
            >
              <Icon name="plus" size={12} />
              {add.isPending ? t("addFeed.adding") : t("addFeed.subscribe")}
            </button>
          ) : (
            <button
              className="s-btn primary"
              onClick={submit}
              disabled={!newsletterReady || addNewsletter.isPending}
            >
              <Icon name="plus" size={12} />
              {addNewsletter.isPending
                ? t("addFeed.connecting")
                : t("addFeed.connect")}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

/** One discovery result row with a quick-subscribe button. */
function DiscoverRow({
  result,
  disabled,
  onSubscribe,
  addLabel,
}: {
  result: DiscoveryResult;
  disabled: boolean;
  onSubscribe: () => void;
  addLabel: string;
}) {
  return (
    <div className="discover-row" role="option" aria-selected={false}>
      <div className="discover-text">
        <span className="discover-title">{result.title}</span>
        {result.description && (
          <span className="discover-desc">{result.description}</span>
        )}
        {!result.description && (
          <span className="discover-desc discover-url">{result.feedUrl}</span>
        )}
      </div>
      <button
        className="s-btn discover-add"
        onClick={onSubscribe}
        disabled={disabled}
        aria-label={`${addLabel} — ${result.title}`}
      >
        <Icon name="plus" size={11} />
        {addLabel}
      </button>
    </div>
  );
}
