import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { openUrl } from "@tauri-apps/plugin-opener";
import * as api from "../api";
import { useUi } from "../store";
import { usePlayer } from "../player";
import { useArticleActions } from "../hooks/articleActions";
import { renderMarkdown } from "../lib/markdown";
import { fullDate } from "../lib/feedMeta";
import { errorText } from "../lib/errors";
import { tagColor } from "../lib/tagColors";
import type { ArticleDetail } from "../types";
import Icon from "./Icon";
import TagPicker from "./TagPicker";
import HighlightLayer from "./HighlightLayer";
import SendToMenu from "./SendToMenu";

interface Props {
  onToast: (msg: string) => void;
}

function youtubeId(url: string | null): string | null {
  if (!url) return null;
  const m =
    url.match(/[?&]v=([\w-]{11})/) || url.match(/youtu\.be\/([\w-]{11})/);
  return m ? m[1] : null;
}

/** Decode a URL fragment, tolerating a malformed `%` escape. A real-world
 *  anchor can carry a literal percent (`#100%-growth`, `#section-50%`), which
 *  is not a valid escape sequence — `decodeURIComponent` throws `URIError` on
 *  it. The bare value still works as an `id` lookup, so fall back to it rather
 *  than letting the throw escape the click handler and kill the link. */
function decodeFragment(frag: string): string {
  try {
    return decodeURIComponent(frag);
  } catch {
    return frag;
  }
}

/** Pull the in-page fragment out of a link click, or null if it isn't one.
 *
 *  Two shapes count as in-page: a bare `#frag` href, and — because the body
 *  HTML is sanitized with the article's URL as the rewrite base — an absolute
 *  `https://site/article#frag` that resolves to the very article being read.
 *  `sourceUrl` is the article's own URL, used to recognise that second case.
 */
function inPageFragment(raw: string, sourceUrl: string | null): string | null {
  if (raw[0] === "#") return decodeFragment(raw.slice(1));
  if (!sourceUrl) return null;
  try {
    const u = new URL(raw);
    const b = new URL(sourceUrl);
    if (u.hash && u.origin === b.origin && u.pathname === b.pathname) {
      return decodeFragment(u.hash.slice(1));
    }
  } catch {
    /* not a parseable absolute URL — treat as external */
  }
  return null;
}

/** Build a click handler for links inside injected HTML (article body, AI
 *  summary). In-page anchor links (footnotes, tables of contents) scroll to
 *  their target within the reader; everything else opens in the external
 *  browser — a bare <a> click would otherwise navigate the Tauri webview away
 *  from the app entirely (or, for a fragment link, to a bogus `app://…#frag`). */
function makeLinkClickHandler(sourceUrl: string | null) {
  return (e: React.MouseEvent) => {
    const link = (e.target as HTMLElement).closest("a");
    if (!link) return;
    const raw = link.getAttribute("href");
    if (!raw) return;
    e.preventDefault();

    const hash = inPageFragment(raw, sourceUrl);
    if (hash != null) {
      if (hash === "") return; // bare `#` — no element to reach
      const root = link.closest(".article-body, .ai-prose");
      // getElementById can't be scoped to the body, so match by id or the
      // legacy `<a name>` form within the rendered content.
      const target = root?.querySelector(
        `[id="${CSS.escape(hash)}"], a[name="${CSS.escape(hash)}"]`,
      );
      target?.scrollIntoView({ behavior: "smooth", block: "start" });
      return;
    }

    openUrl(link.href).catch(() => {});
  };
}

export default function Reader({ onToast }: Props) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const actions = useArticleActions(onToast);
  const id = useUi((s) => s.selectedArticleId);
  const useSerif = useUi((s) => s.useSerif);
  const focusMode = useUi((s) => s.focusMode);
  const setFocusMode = useUi((s) => s.setFocusMode);
  const aiOpen = useUi((s) => s.aiOpen);
  const setAiOpen = useUi((s) => s.setAiOpen);
  const markReadOnOpen = useUi((s) => s.prefs.markReadOnOpen);
  const markReadOnScroll = useUi((s) => s.prefs.markReadOnScroll);
  const showReadingTime = useUi((s) => s.prefs.showReadingTime);
  const autoExtract = useUi((s) => s.prefs.autoExtract);

  const [scrolled, setScrolled] = useState(false);
  const [showExtracted, setShowExtracted] = useState(true);
  const [tagPick, setTagPick] = useState<{ x: number; y: number } | null>(null);
  const [sendTo, setSendTo] = useState<{ x: number; y: number } | null>(null);
  const [heroBroken, setHeroBroken] = useState(false);
  const [progress, setProgress] = useState(0);
  const scrollRef = useRef<HTMLDivElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  // Article id we already auto-marked read via scroll, so a flurry of scroll
  // events near the foot doesn't fire `setRead` repeatedly before the
  // optimistic cache patch lands.
  const scrollMarkedRef = useRef<number | null>(null);
  const playTrack = usePlayer((s) => s.play);
  const playingSrc = usePlayer((s) => (s.playing ? s.track?.src : null));

  const article = useQuery({
    queryKey: ["article", id],
    queryFn: () => api.getArticle(id as number),
    enabled: id != null,
  });
  const a: ArticleDetail | undefined = article.data;

  const readMinutes = useMemo(() => {
    const html = a?.extractedHtml || a?.contentHtml || "";
    const words = html
      .replace(/<[^>]+>/g, " ")
      .trim()
      .split(/\s+/)
      .filter(Boolean).length;
    const chars = html.replace(/<[^>]+>/g, "").length;
    // mixed CJK / latin estimate
    return Math.max(2, Math.round(Math.max(words / 220, chars / 480)));
    // Recompute when the body changes — including after full-text extraction
    // replaces the short feed snippet, which keeps the same article id.
  }, [a?.extractedHtml, a?.contentHtml]);

  // Reset scroll + extraction view on article change.
  useEffect(() => {
    setShowExtracted(true);
    setScrolled(false);
    setTagPick(null);
    setSendTo(null);
    setHeroBroken(false);
    setProgress(0);
    scrollMarkedRef.current = null;
    if (scrollRef.current) scrollRef.current.scrollTop = 0;
  }, [id]);

  // Hide article-body images that fail to load — a broken-image icon in the
  // middle of an article is just noise. Runs whenever the body changes
  // (article switch, extract toggle, extraction finishing).
  useEffect(() => {
    const el = bodyRef.current;
    if (!el) return;
    el.querySelectorAll("img").forEach((img) => {
      if (img.complete && img.naturalWidth === 0) {
        img.style.display = "none";
      } else {
        img.addEventListener("error", () => {
          img.style.display = "none";
        });
      }
    });
  }, [a?.id, showExtracted, a?.extractedHtml]);

  // Mark as read once when an unread article is opened (if the user opted in).
  useEffect(() => {
    if (a && !a.isRead && markReadOnOpen) actions.setRead(a.id, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [a?.id]);

  // The extracted article id travels as the mutation variable, not via the
  // `a` closure: extraction is async and the user can switch articles before
  // it resolves. Keying onSuccess off the live `a` would invalidate the wrong
  // article (the extracted text never shows on return) and toast "full text
  // extracted" while reading an unrelated, un-extracted article.
  const extract = useMutation({
    mutationFn: (articleId: number) => api.extractFulltext(articleId),
    onSuccess: (_data, articleId) => {
      qc.invalidateQueries({ queryKey: ["article", articleId] });
      // Only the article still on screen should flip into the extracted view
      // and surface the toast.
      if (useUi.getState().selectedArticleId === articleId) {
        setShowExtracted(true);
        onToast(t("reader.fullTextExtracted"));
      }
    },
    onError: (e) => onToast(errorText(e)),
  });

  // With "auto-extract full text" on, a summary-only feed item is upgraded to
  // the full page the moment it's opened, so the reader never shows a two-line
  // stub. Skipped when the feed already carries the whole article, when there
  // is no source URL to fetch, or once attempted for this article — so a
  // failed fetch isn't retried on every re-render.
  const autoExtractedRef = useRef<number | null>(null);
  useEffect(() => {
    if (!autoExtract || !a || !a.url || a.extractedHtml) return;
    if (autoExtractedRef.current === a.id || extract.isPending) return;
    const plain = (a.contentHtml || "").replace(/<[^>]+>/g, "").trim();
    if (plain.length >= 800) return; // feed already delivers the full text
    autoExtractedRef.current = a.id;
    extract.mutate(a.id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [a?.id, a?.extractedHtml, autoExtract]);

  // Mark the current article read once its foot is reached. Also fires for an
  // article short enough to need no scrolling at all (`scrollHeight` already
  // within `clientHeight`) — that case produces no `scroll` event, so without
  // a render-time check a fully-visible short article would never be marked
  // read despite "mark read on scroll" being on.
  const markReadIfAtFoot = useCallback(() => {
    const el = scrollRef.current;
    if (!el || !markReadOnScroll || !a || a.isRead) return;
    if (scrollMarkedRef.current === a.id) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 120) {
      scrollMarkedRef.current = a.id;
      actions.setRead(a.id, true);
    }
  }, [markReadOnScroll, a, actions]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    setScrolled(el.scrollTop > 8);
    const max = el.scrollHeight - el.clientHeight;
    setProgress(max > 0 ? Math.min(1, el.scrollTop / max) : 0);
    markReadIfAtFoot();
  };

  // A short article that fits the viewport never fires `scroll`, so check the
  // foot condition once the body has laid out (article switch, extract toggle,
  // extraction finishing). The check is deferred briefly so body images have a
  // chance to load — measuring `scrollHeight` before they do could read a
  // too-small height and mark a genuinely long article read prematurely. The
  // `scrollMarkedRef` guard keeps it idempotent.
  useEffect(() => {
    const timer = window.setTimeout(markReadIfAtFoot, 400);
    return () => window.clearTimeout(timer);
  }, [markReadIfAtFoot, showExtracted, a?.extractedHtml, a?.contentHtml]);


  const copyLink = () => {
    if (!a?.url) return;
    navigator.clipboard.writeText(a.url).then(() => onToast(t("reader.linkCopied")), () => {});
  };
  const share = () => {
    if (!a?.url) return;
    if (navigator.share) {
      navigator.share({ title: a.title, url: a.url }).catch((e) => {
        // A user-cancelled share rejects with AbortError — only fall back to
        // copying the link on a genuine failure (e.g. share unsupported).
        if ((e as Error)?.name !== "AbortError") copyLink();
      });
    } else {
      copyLink();
    }
  };

  if (id == null) {
    const kbd = {
      fontFamily: "var(--mono)",
      fontSize: 10,
      padding: "1px 5px",
      border: "1px solid var(--hair)",
      borderRadius: 3,
    };
    return (
      <div className="reader" role="main">
        <div className="reader-toolbar" data-tauri-drag-region />
        <div className="empty" style={{ flex: 1 }}>
          <div className="glyph">
            <Icon name="rss" size={22} />
          </div>
          <div>{t("reader.emptySelectArticle")}</div>
          <div style={{ fontSize: 11.5, color: "var(--muted-2)" }}>
            {t("reader.emptyHintPrefix")} <kbd style={kbd}>J</kbd> /{" "}
            <kbd style={kbd}>K</kbd> {t("reader.emptyHintSuffix")}
          </div>
        </div>
      </div>
    );
  }

  // An article is selected but its detail isn't loaded yet — still fetching
  // or the fetch failed. Surface that explicitly instead of falling through
  // to the "select an article" empty state, which would be misleading.
  if (!a) {
    return (
      <div className="reader" role="main">
        <div className="reader-toolbar" data-tauri-drag-region />
        {article.isError ? (
          <div className="empty" style={{ flex: 1 }}>
            <div className="glyph">
              <Icon name="alert" size={22} />
            </div>
            <div>{t("reader.loadError")}</div>
            <button
              className="empty-retry"
              onClick={() => article.refetch()}
              disabled={article.isFetching}
            >
              <Icon name="refresh" size={12} />
              {t("common.retry")}
            </button>
          </div>
        ) : (
          <div className="reader-scroll">
            <div className="article reader-content" aria-hidden="true">
              <div className="sk-line" style={{ width: "28%" }} />
              <div
                className="sk-line"
                style={{ width: "82%", height: 24, marginBottom: 18 }}
              />
              <div
                className="sk-line"
                style={{ width: "44%", marginBottom: 30 }}
              />
              {Array.from({ length: 9 }).map((_, i) => (
                <div
                  key={i}
                  className="sk-line"
                  style={{ width: i % 3 === 2 ? "58%" : "100%", height: 12 }}
                />
              ))}
            </div>
          </div>
        )}
      </div>
    );
  }

  const hasExtracted = !!a.extractedHtml;
  const body =
    (showExtracted && a.extractedHtml ? a.extractedHtml : a.contentHtml) || "";
  const ytId = a.sourceType === "youtube" ? youtubeId(a.url) : null;
  // Identifies the rendered body markup so HighlightLayer can re-apply its
  // <mark> overlay whenever the Reader swaps the body (extract toggle,
  // extraction finishing). The body string itself is used rather than a
  // numeric proxy like its length: two distinct bodies (e.g. the feed
  // content vs. the extracted full text) can share a length, and a length
  // collision would leave the new DOM with no highlights re-applied.
  const bodyVersion = body;

  return (
    <div className="reader" role="main">
      <div
        className={`reader-toolbar ${scrolled ? "scrolled" : ""}`}
        data-tauri-drag-region
      >
        <button
          className={`tb-btn ${a.isStarred ? "on" : ""}`}
          onClick={() => actions.setStarred(a.id, !a.isStarred)}
          title={t("reader.tbStar")}
          aria-label={t("reader.tbStar")}
          aria-pressed={a.isStarred}
        >
          <Icon name={a.isStarred ? "star-fill" : "star"} size={16} />
        </button>
        <button
          className={`tb-btn ${a.readLater ? "on" : ""}`}
          onClick={() => actions.setReadLater(a.id, !a.readLater)}
          title={t("reader.tbReadLater")}
          aria-label={t("reader.tbReadLater")}
          aria-pressed={a.readLater}
        >
          <Icon name={a.readLater ? "bookmark-fill" : "bookmark"} size={16} />
        </button>
        <button
          className={`tb-btn ${a.tags.length > 0 ? "on" : ""}`}
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            setTagPick((p) => (p ? null : { x: r.left, y: r.bottom + 6 }));
          }}
          title={t("reader.tbTags")}
          aria-label={t("reader.tbTags")}
          aria-haspopup="menu"
          aria-expanded={tagPick != null}
        >
          <Icon name="tag" size={16} />
        </button>
        <button
          className={`tb-btn ${aiOpen ? "on" : ""}`}
          onClick={() => setAiOpen(!aiOpen)}
          title={t("reader.tbAiSummary")}
          aria-label={t("reader.tbAiSummary")}
          aria-pressed={aiOpen}
        >
          <Icon name={aiOpen ? "sparkle-fill" : "sparkle"} size={16} />
        </button>
        <button
          className={`tb-btn ${hasExtracted && showExtracted ? "on" : ""} ${
            extract.isPending ? "spinning" : ""
          }`}
          onClick={() =>
            hasExtracted ? setShowExtracted((v) => !v) : extract.mutate(a.id)
          }
          // Extraction needs the source URL; without one (and nothing
          // extracted yet) the button can only error, so disable it.
          disabled={extract.isPending || (!hasExtracted && !a.url)}
          title={hasExtracted ? t("reader.tbToggleFullText") : t("reader.tbExtractFullText")}
          aria-label={hasExtracted ? t("reader.tbToggleFullText") : t("reader.tbExtractFullText")}
          aria-pressed={hasExtracted ? showExtracted : undefined}
          aria-busy={extract.isPending}
        >
          <Icon name="text" size={16} />
        </button>
        <button
          className="tb-btn"
          title={t("reader.tbCopyLink")}
          aria-label={t("reader.tbCopyLink")}
          onClick={copyLink}
          disabled={!a.url}
        >
          <Icon name="copy" size={16} />
        </button>
        <button
          className="tb-btn"
          title={t("reader.tbShare")}
          aria-label={t("reader.tbShare")}
          onClick={share}
          disabled={!a.url}
        >
          <Icon name="share" size={16} />
        </button>
        <button
          className={`tb-btn ${sendTo ? "on" : ""}`}
          title={t("sendTo.title")}
          aria-label={t("sendTo.title")}
          aria-haspopup="menu"
          aria-expanded={sendTo != null}
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            setSendTo((p) => (p ? null : { x: r.left, y: r.bottom + 6 }));
          }}
        >
          <Icon name="open" size={16} />
        </button>
        <HighlightLayer
          // Keyed by article id so the export menu / popovers reset cleanly
          // when the reader switches articles.
          key={a.id}
          articleId={a.id}
          bodyRef={bodyRef}
          bodyVersion={bodyVersion}
          onToast={onToast}
        />
        <button
          className={`tb-btn ${focusMode ? "on" : ""}`}
          onClick={() => setFocusMode(!focusMode)}
          title={t("reader.tbFocusMode")}
          aria-label={t("reader.tbFocusMode")}
          aria-pressed={focusMode}
        >
          <Icon name="focus" size={16} />
        </button>
        <div className="tb-btn spacer" />
        {a.url && (
          <button
            className="tb-btn"
            title={t("reader.tbOpenInBrowser")}
            aria-label={t("reader.tbOpenInBrowser")}
            onClick={() => openUrl(a.url!).catch(() => {})}
          >
            <Icon name="open" size={16} />
          </button>
        )}
      </div>

      <div className="read-progress-track" aria-hidden="true">
        <div
          className="read-progress"
          style={{ transform: `scaleX(${progress})` }}
        />
      </div>

      <div className="reader-scroll" ref={scrollRef} onScroll={onScroll}>
        <article className="article reader-content" key={a.id}>
          <span className="article-feed">
            <Icon name="rss" size={13} />
            {a.feedTitle}
          </span>
          <h1 className="article-title">{a.title}</h1>
          <div className="article-meta">
            {a.author && <span className="author">{a.author}</span>}
            {a.author && a.publishedAt && <span>·</span>}
            {a.publishedAt && <span>{fullDate(a.publishedAt)}</span>}
            {showReadingTime && (
              <>
                <span>·</span>
                <span>{t("reader.readMinutes", { count: readMinutes })}</span>
              </>
            )}
            {extract.isPending && (
              <>
                <span>·</span>
                <span>{t("reader.extractingFullText")}</span>
              </>
            )}
          </div>

          {a.tags.length > 0 && (
            <div className="article-tags">
              {a.tags.map((tag) => (
                <button
                  key={tag.id}
                  className="article-tag"
                  style={{ "--tag-c": tagColor(tag.color) } as React.CSSProperties}
                  onClick={() =>
                    useUi.getState().select({ kind: "tag", value: tag.id }, tag.name)
                  }
                >
                  <span className="tag-dot" />
                  {tag.name}
                </button>
              ))}
            </div>
          )}

          {ytId ? (
            <iframe
              style={{ width: "100%", aspectRatio: "16 / 9" }}
              // Privacy-enhanced host: YouTube sets no tracking cookies
              // until the viewer actually starts the video.
              src={`https://www.youtube-nocookie.com/embed/${ytId}`}
              title={a.title}
              referrerPolicy="strict-origin-when-cross-origin"
              allowFullScreen
            />
          ) : (
            a.imageUrl &&
            !heroBroken &&
            // Skip the hero when the body already embeds the same image, so
            // feeds that repeat their lead image don't show it twice.
            !body.includes(a.imageUrl) && (
              <img
                src={a.imageUrl}
                alt=""
                onError={() => setHeroBroken(true)}
              />
            )
          )}

          {a.enclosures
            .filter((e) => e.mimeType?.startsWith("audio"))
            .map((e, i) => {
              const isPlaying = playingSrc === e.url;
              return (
                <button
                  className={`episode ${isPlaying ? "playing" : ""}`}
                  key={`a${i}`}
                  onClick={() =>
                    playTrack({
                      articleId: a.id,
                      title: a.title,
                      feedTitle: a.feedTitle,
                      src: e.url,
                    })
                  }
                >
                  <span className="episode-play">
                    <Icon name={isPlaying ? "pause" : "play"} size={15} />
                  </span>
                  <span className="episode-text">
                    {isPlaying
                      ? t("reader.episodePlaying")
                      : t("reader.episodePlay")}
                  </span>
                </button>
              );
            })}
          {a.enclosures
            .filter((e) => e.mimeType?.startsWith("video"))
            .map((e, i) => (
              <div className="enclosure" key={`v${i}`}>
                <video controls src={e.url} />
              </div>
            ))}

          <div
            className="article-body"
            ref={bodyRef}
            data-serif={useSerif}
            onClick={makeLinkClickHandler(a.url)}
            dangerouslySetInnerHTML={{
              __html: body || `<p><em>${t("reader.noContent")}</em></p>`,
            }}
          />
        </article>
      </div>

      <AIDrawer
        // Keyed by article id so switching articles remounts the drawer:
        // its `text` state then re-initialises from the new article's
        // summary, rather than carrying the previous one's across.
        key={a.id}
        open={aiOpen}
        article={a}
        onClose={() => setAiOpen(false)}
        onToast={onToast}
      />

      {tagPick && (
        <TagPicker
          articleId={a.id}
          attached={a.tags.map((tg) => tg.id)}
          x={tagPick.x}
          y={tagPick.y}
          onClose={() => setTagPick(null)}
          onToast={onToast}
        />
      )}

      {sendTo && (
        <SendToMenu
          articleId={a.id}
          x={sendTo.x}
          y={sendTo.y}
          onClose={() => setSendTo(null)}
          onToast={onToast}
        />
      )}
    </div>
  );
}

function AIDrawer({
  open,
  article,
  onClose,
  onToast,
}: {
  open: boolean;
  article: ArticleDetail;
  onClose: () => void;
  onToast: (m: string) => void;
}) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  // Initialised from the article's stored summary (if any). The parent keys
  // this component by article id, so a switch remounts it and re-runs this
  // initialiser — no separate "reset on article change" effect is needed.
  const [text, setText] = useState<string | null>(article.aiSummary);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const [retry, setRetry] = useState(0);
  // Identifies the latest summarize run. Closing the drawer mid-stream cancels
  // an effect run but the component stays mounted (it is only moved off-screen),
  // so the underlying request keeps streaming and its promise settles later.
  // Only the run whose generation still matches may touch `busy` on settle —
  // otherwise a stale run's `finally` would either wedge the drawer on the
  // loading state or clobber a newer run's `busy` flag.
  const runRef = useRef(0);

  // Generate a summary the first time the drawer opens for an article, and
  // again whenever the user hits Retry. `failed` is in the guard so a failed
  // run isn't silently re-attempted just because the drawer was reopened.
  useEffect(() => {
    if (!open || busy || text || failed) return;
    const run = ++runRef.current;
    let cancelled = false;
    // Whether the stream settled (resolved or rejected) on its own. If the
    // cleanup runs while this is still false, the drawer was closed mid-stream
    // — the accumulated `text` is then a truncated fragment.
    let settled = false;
    // An error raised inside the stream surfaces twice: once as an `error`
    // channel event (carrying the precise provider message) and again as the
    // command's rejected promise. Toast only the first so the user does not
    // see the same failure reported twice; the `.catch` still toasts for
    // failures that abort before streaming starts (no key, bad config) and so
    // never emit an `error` event.
    let sawErrorEvent = false;
    setBusy(true);
    setText("");
    api
      .aiSummarize(article.id, (ev) => {
        if (cancelled) return;
        if (ev.type === "delta") setText((s) => (s ?? "") + ev.data);
        else if (ev.type === "error") {
          sawErrorEvent = true;
          setFailed(true);
          onToast(ev.data);
        }
      })
      .then(() => {
        if (!cancelled) qc.invalidateQueries({ queryKey: ["article", article.id] });
      })
      .catch((e) => {
        if (!cancelled && !sawErrorEvent) {
          setFailed(true);
          onToast(errorText(e));
        }
      })
      .finally(() => {
        settled = true;
        // Clear `busy` for the current run even if it was cancelled — the
        // component is still mounted, and leaving `busy` true would wedge the
        // drawer on the loading state. Skip if a newer run has superseded us.
        if (runRef.current === run) setBusy(false);
      });
    return () => {
      cancelled = true;
      // Closed mid-stream: the backend discards an interrupted generation
      // (it is never persisted), so drop the partial fragment held here too.
      // Reopening then re-generates from scratch instead of showing — and
      // permanently freezing on — a truncated half-summary.
      if (!settled) setText(article.aiSummary);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, article.id, retry]);

  const loading = busy && !text;
  const onRetry = () => {
    setText("");
    setFailed(false);
    setRetry((n) => n + 1);
  };
  // Parse + sanitize the summary only when the text changes, not on every
  // AIDrawer re-render (e.g. each open/close toggle).
  const html = useMemo(() => (text ? renderMarkdown(text) : ""), [text]);

  return (
    <div
      className={`ai-drawer ${open ? "open" : ""}`}
      // A labelled complementary landmark so screen-reader users can jump
      // straight to the summary.
      role="complementary"
      aria-label={t("reader.aiSummaryTitle")}
      // When closed the drawer is only moved off-screen — `inert` keeps its
      // close button and content out of the tab order and the a11y tree.
      inert={!open}
    >
      <div className="ai-head">
        <span className="accent-ico">
          <Icon name="sparkle-fill" size={15} />
        </span>
        <h3>{t("reader.aiSummaryTitle")}</h3>
        <button
          className="tb-btn close"
          onClick={onClose}
          title={t("common.close")}
          aria-label={t("common.close")}
        >
          <Icon name="x" size={14} />
        </button>
      </div>
      <div className="ai-body" aria-live="polite" aria-busy={busy}>
        {loading && (
          <div className="ai-loading">
            <span className="ai-dot" />
            <span className="ai-dot" />
            <span className="ai-dot" />
            <span style={{ marginLeft: 4 }}>{t("reader.aiReadingFullText")}</span>
          </div>
        )}
        {failed && !busy && (
          <div className="ai-error">
            <Icon name="alert" size={18} />
            <span>{t("reader.aiError")}</span>
            <button className="empty-retry" onClick={onRetry}>
              <Icon name="refresh" size={12} />
              {t("common.retry")}
            </button>
          </div>
        )}
        {text && !failed && (
          <>
            <div
              className="ai-prose"
              onClick={makeLinkClickHandler(article.url)}
              dangerouslySetInnerHTML={{ __html: html }}
            />
            <div
              style={{
                fontSize: 11,
                color: "var(--muted-2)",
                marginTop: 24,
                lineHeight: 1.5,
              }}
            >
              {t("reader.aiDisclaimer")}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
