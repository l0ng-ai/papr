// Thin typed wrappers over the Tauri command surface (src-tauri/src/commands.rs).

import { invoke, Channel } from "@tauri-apps/api/core";
import { imageBytes, type ImageBytesResponse } from "./lib/imageBytes";
import type {
  AiEvent,
  ArticleDetail,
  ArticlePreviewTranslation,
  ArticleQuery,
  ArticleSummary,
  DiscoveryResult,
  Feed,
  Folder,
  Highlight,
  NewsletterInput,
  NewsletterSource,
  RefreshProgress,
  Rule,
  RuleAction,
  RuleField,
  RulePreview,
  SmartCounts,
  Tag,
  TranslateEvent,
} from "./types";

// ── folders ──
export const listFolders = () => invoke<Folder[]>("list_folders");
export const createFolder = (name: string, parentId: number | null = null) =>
  invoke<number>("create_folder", { name, parentId });
export const moveFolder = (id: number, parentId: number | null) =>
  invoke<void>("move_folder", { id, parentId });
export const renameFolder = (id: number, name: string) =>
  invoke<void>("rename_folder", { id, name });
export const deleteFolder = (id: number, deleteFeeds = false) =>
  invoke<void>("delete_folder", { id, deleteFeeds });

// ── images ──
/** Fetch an image's raw bytes via the backend, which walks Referer fallbacks
 *  (none → image origin → article URL) until the host serves it — hotlink
 *  protection demands different Referers on different hosts. Used by the
 *  reader's "Save image" action and to retry images the webview itself failed
 *  to load. `pageUrl` is the embedding article's link. */
export const fetchImage = (url: string, pageUrl?: string | null) =>
  invoke<ImageBytesResponse>("fetch_image", { url, pageUrl: pageUrl ?? null }).then(
    imageBytes,
  );

/** "Save image" via a native save dialog, entirely in Rust (fetch → dialog →
 *  write). Returns whether a file was actually written — false when the user
 *  cancels. Kept in Rust so a large image never round-trips the JSON IPC
 *  channel as a number array. */
export const saveImage = (url: string, pageUrl?: string | null) =>
  invoke<boolean>("save_image", { url, pageUrl: pageUrl ?? null });

// ── feeds ──
export const listFeeds = () => invoke<Feed[]>("list_feeds");
export const addFeed = (url: string, folderId: number | null) =>
  invoke<Feed>("add_feed", { url, folderId });
/**
 * Discover feeds matching a query — curated directory + live page scrape.
 * `lang` is the UI language; the curated directory is scoped to it so the
 * recommendations are in a language the user reads.
 */
export const searchFeedDirectory = (query: string, lang: string) =>
  invoke<DiscoveryResult[]>("search_feed_directory", { query, lang });
export const deleteFeed = (id: number) => invoke<void>("delete_feed", { id });
export const moveFeed = (id: number, folderId: number | null) =>
  invoke<void>("move_feed", { id, folderId });
export const renameFeed = (id: number, title: string) =>
  invoke<void>("rename_feed", { id, title });
/** Set a feed's refresh interval (minutes). `null` follows the global
 *  setting; `525600` opts the feed out of automatic refresh. */
export const setFeedRefreshInterval = (id: number, minutes: number | null) =>
  invoke<void>("set_feed_refresh_interval", { id, minutes });
/** Toggle a feed's auto-translate flag. When on, opening an article from the
 *  feed translates it into the configured target language straight away. */
export const setFeedAutoTranslate = (id: number, enabled: boolean) =>
  invoke<void>("set_feed_auto_translate", { id, enabled });
/** Set a feed's per-feed open mode. `null` reverts to the default behaviour
 *  (reader view, honouring the global auto-extract preference). */
export const setFeedOpenMode = (
  id: number,
  mode: "reader" | "extracted" | "web" | null,
) => invoke<void>("set_feed_open_mode", { id, mode });

/** Refresh feeds, reporting progress through the supplied callback. With no
 *  `scope` this refreshes every feed; pass `{ feedId }` for a single feed or
 *  `{ folderId }` for every feed in one folder. */
export function refreshFeeds(
  onProgress?: (p: RefreshProgress) => void,
  scope?: { feedId?: number; folderId?: number },
): Promise<number> {
  const channel = new Channel<RefreshProgress>();
  if (onProgress) channel.onmessage = onProgress;
  return invoke<number>("refresh_feeds", {
    onProgress: channel,
    feedId: scope?.feedId ?? null,
    folderId: scope?.folderId ?? null,
  });
}

// ── articles ──
export const listArticles = (
  query: ArticleQuery,
  unreadOnly: boolean,
  search: string | null,
  oldestFirst: boolean,
  limit: number,
  offset: number,
) =>
  invoke<ArticleSummary[]>("list_articles", {
    query,
    unreadOnly,
    search,
    oldestFirst,
    limit,
    offset,
  });

/** 0-based position of `articleId` in the list these filters produce, or null
 *  when it isn't in that list (filtered out / different feed). Drives paging the
 *  middle pane down to an article opened from search. */
export const articleIndex = (
  query: ArticleQuery,
  unreadOnly: boolean,
  oldestFirst: boolean,
  articleId: number,
) =>
  invoke<number | null>("article_index", {
    query,
    unreadOnly,
    oldestFirst,
    articleId,
  });

export const getArticle = (id: number) =>
  invoke<ArticleDetail>("get_article", { id });
export const markRead = (id: number, read: boolean) =>
  invoke<void>("mark_read", { id, read });
export const markStarred = (id: number, starred: boolean) =>
  invoke<void>("mark_starred", { id, starred });
export const markReadLater = (id: number, value: boolean) =>
  invoke<void>("mark_read_later", { id, value });
export const markAllRead = (query: ArticleQuery) =>
  invoke<number>("mark_all_read", { query });
export const smartCounts = () => invoke<SmartCounts>("smart_counts");

// ── full-text extraction ──
export const extractFulltext = (articleId: number) =>
  invoke<string>("extract_fulltext", { articleId });

// ── OPML ──
export const importOpml = (content: string) =>
  invoke<number>("import_opml", { content });
export const exportOpml = () => invoke<string>("export_opml");

// ── AI (streaming over a Channel) ──
export interface SummaryPromptConfig { prompt: string; defaultPrompt: string }
export const getSummaryPrompt = () => invoke<SummaryPromptConfig>("get_summary_prompt");
export interface AiPreferences { provider: string; model: string; baseUrl: string; effort: string }
export const getAiPreferences = () => invoke<AiPreferences>("get_ai_preferences");
export const saveSummaryPrompt = (prompt: string) => invoke<void>("save_summary_prompt", { prompt });

export function aiSummarize(
  articleId: number,
  onToken: (e: AiEvent) => void,
): Promise<void> {
  const channel = new Channel<AiEvent>();
  channel.onmessage = onToken;
  return invoke<void>("ai_summarize", { articleId, onToken: channel });
}

export interface ArticleChatMessage { role: "user" | "assistant"; content: string }
export function aiArticleChat(articleId: number, question: string, history: ArticleChatMessage[], onToken: (e: AiEvent) => void): Promise<void> {
  const channel = new Channel<AiEvent>();
  channel.onmessage = onToken;
  return invoke<void>("ai_article_chat", { articleId, question, history, onToken: channel });
}
export interface CodexStatus { email: string | null; plan: string | null; loggedIn: boolean; pending: boolean; error: string | null }
export interface CodexModel { id: string; name: string; isDefault: boolean; reasoningEfforts: string[]; defaultEffort: string | null }
export const codexStatus = () => invoke<CodexStatus>("codex_status");
export const codexLogin = () => invoke<string>("codex_login");
export const codexCancelLogin = () => invoke<void>("codex_cancel_login");
export const codexLogout = () => invoke<void>("codex_logout");
export const codexModels = () => invoke<CodexModel[]>("codex_models");

export function aiAsk(
  question: string,
  onToken: (e: AiEvent) => void,
): Promise<void> {
  const channel = new Channel<AiEvent>();
  channel.onmessage = onToken;
  return invoke<void>("ai_ask", { question, onToken: channel });
}

export function aiDigest(onToken: (e: AiEvent) => void): Promise<void> {
  const channel = new Channel<AiEvent>();
  channel.onmessage = onToken;
  return invoke<void>("ai_digest", { onToken: channel });
}

/** Translate the article body into `lang` using `engine` (`llm` / `google` /
 *  `deepl` / `bing`). Progress is reported per batch over `onEvent` (start →
 *  batch* → done); the full result is also persisted and returned via the final
 *  `done` event. */
export function aiTranslate(
  articleId: number,
  lang: string,
  engine: string,
  onEvent: (e: TranslateEvent) => void,
): Promise<void> {
  const channel = new Channel<TranslateEvent>();
  channel.onmessage = onEvent;
  return invoke<void>("ai_translate", { articleId, lang, engine, onEvent: channel });
}

/** Translate only the list preview fields for an article and persist them in the
 *  preview cache. */
export const translateArticlePreview = (articleId: number, lang: string, engine: string) =>
  invoke<ArticlePreviewTranslation>("translate_article_preview", { articleId, lang, engine });

// ── settings ──
export const getSetting = (key: string) =>
  invoke<string | null>("get_setting", { key });
export const setSetting = (key: string, value: string) =>
  invoke<void>("set_setting", { key, value });

// ── AI ──
/** What a successful `testAi` round trip saw (mirrors `ai::TestOutcome`). */
export interface AiTestOutcome {
  model: string;
  baseUrl: string;
  latencyMs: number;
  reply: string;
}

/**
 * Send one minimal request to the configured provider and report what came
 * back. Takes the values currently in the settings form so the test works
 * before saving; empty model/URL fields use provider defaults.
 */
export const testAi = (args: {
  provider: string;
  apiKey: string;
  model: string;
  baseUrl: string;
}) => invoke<AiTestOutcome>("ai_test", args);

// ── storage ──
export interface StorageStats {
  dbBytes: number;
  articleCount: number;
  feedCount: number;
}
export const storageStats = () => invoke<StorageStats>("storage_stats");
export const cleanupArticles = (days: number) =>
  invoke<number>("cleanup_articles", { days });
export const vacuumDb = () => invoke<void>("vacuum_db");
export const resetSettings = () => invoke<void>("reset_settings");
export const clearAllData = () => invoke<void>("clear_all_data");

// ── network ──
export const applyNetworkSettings = () =>
  invoke<void>("apply_network_settings");

// ── GReader sync (FreshRSS / Miniflux) ──
export type GReaderProvider = "freshrss" | "miniflux";
export interface FreshRssStatus {
  connected: boolean;
  url: string | null;
  provider: GReaderProvider;
}
export const freshrssStatus = () => invoke<FreshRssStatus>("freshrss_status");
export const freshrssConnect = (
  url: string,
  username: string,
  password: string,
  provider: GReaderProvider = "freshrss",
) => invoke<void>("freshrss_connect", { url, username, password, provider });
export const freshrssDisconnect = () => invoke<void>("freshrss_disconnect");
export const freshrssSync = () => invoke<number>("freshrss_sync");

// ── tray ──
export const refreshTray = () => invoke<void>("refresh_tray");

// ── deep links ──
/** Drain a `papr://subscribe` URL delivered before the webview could receive
 *  the `deep-link-subscribe` event (a cold-start launch). */
export const takePendingDeepLink = () =>
  invoke<string | null>("take_pending_deep_link");

// ── tags ──
export const listTags = () => invoke<Tag[]>("list_tags");
export const createTags = (names: string, parentId: number | null = null) =>
  invoke<number[]>("create_tags", { names, parentId });
export const createTag = (name: string, parentId: number | null = null) =>
  invoke<number>("create_tag", { name, parentId });
export const moveTag = (id: number, parentId: number | null) => invoke<void>("move_tag", { id, parentId });
export const renameTag = (id: number, name: string) =>
  invoke<void>("rename_tag", { id, name });
export const setTagColor = (id: number, color: string) =>
  invoke<void>("set_tag_color", { id, color });
export const deleteTag = (id: number) => invoke<void>("delete_tag", { id });
export const reorderTags = (ids: number[]) =>
  invoke<void>("reorder_tags", { ids });
export const setArticleTag = (articleId: number, tagId: number, on: boolean) =>
  invoke<void>("set_article_tag", { articleId, tagId, on });

// ── filter rules ──
export const listRules = () => invoke<Rule[]>("list_rules");
export const createRule = (
  name: string,
  feedId: number | null,
  field: RuleField,
  query: string,
  action: RuleAction,
) => invoke<number>("create_rule", { name, feedId, field, query, action });
export const updateRule = (
  id: number,
  name: string,
  enabled: boolean,
  feedId: number | null,
  field: RuleField,
  query: string,
  action: RuleAction,
) =>
  invoke<void>("update_rule", {
    id,
    name,
    enabled,
    feedId,
    field,
    query,
    action,
  });
export const deleteRule = (id: number) => invoke<void>("delete_rule", { id });
export const previewRule = (
  feedId: number | null,
  field: RuleField,
  query: string,
) => invoke<RulePreview>("preview_rule", { feedId, field, query });
/** Apply a rule's action to the already-stored articles it matches; returns the
 *  number acted on. Run once after saving so the rule affects the existing
 *  backlog. A `skip` rule deletes its matches — confirm before calling. */
export const applyRuleToExisting = (
  feedId: number | null,
  field: RuleField,
  query: string,
  action: RuleAction,
) =>
  invoke<number>("apply_rule_to_existing", { feedId, field, query, action });

// ── highlights / annotations (F7) ──
export interface NewHighlight {
  articleId: number;
  quote: string;
  prefix: string;
  suffix: string;
  textOffset: number;
  color: string;
  note: string;
}
export const createHighlight = (h: NewHighlight) =>
  invoke<number>("create_highlight", { ...h });
export const listHighlights = (articleId: number) =>
  invoke<Highlight[]>("list_highlights", { articleId });
export const listAllHighlights = () =>
  invoke<Highlight[]>("list_all_highlights");
export const updateHighlightNote = (id: number, note: string) =>
  invoke<void>("update_highlight_note", { id, note });
export const setHighlightColor = (id: number, color: string) =>
  invoke<void>("set_highlight_color", { id, color });
export const deleteHighlight = (id: number) =>
  invoke<void>("delete_highlight", { id });

// ── newsletter sources (IMAP-polled email newsletters) ──
export const addNewsletterSource = (input: NewsletterInput) =>
  invoke<Feed>("add_newsletter_source", { input });
export const listNewsletterSources = () =>
  invoke<NewsletterSource[]>("list_newsletter_sources");
export const removeNewsletterSource = (feedId: number) =>
  invoke<void>("remove_newsletter_source", { feedId });

// ── in-app original-page view (issue #49) ──
// A native child webview overlaid on the reading area. Bounds are logical
// (CSS) pixels relative to the window content top-left — i.e. what
// getBoundingClientRect returns inside the main webview.
export interface PageViewBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}
export const openPageView = (url: string, b: PageViewBounds, zoom = 1) =>
  invoke<void>("open_page_view", { url, ...b, zoom });
export const setPageViewZoom = (zoom: number) => invoke<void>("set_page_view_zoom", { zoom });
export const setPageViewBounds = (b: PageViewBounds) =>
  invoke<void>("set_page_view_bounds", { ...b });
export const setPageViewVisible = (visible: boolean) =>
  invoke<void>("set_page_view_visible", { visible });
export const closePageView = () => invoke<void>("close_page_view");

export const configureAiProvider = (provider: string, model?: string, baseUrl?: string, effort?: string) =>
  invoke<[string, string]>("configure_ai_provider", { provider, model, baseUrl, effort });

// ── WebDAV library sync ──
export interface WebDavStatus {
  conflicts: number;
  connected: boolean;
  url: string | null;
  username: string | null;
  lastSync: string | null;
  lastError: string | null;
}
export interface WebDavRecord { kind: string; key: string; clock: number; nonce: string; deleted: boolean; data: Record<string, unknown> }
export interface WebDavConflict { kind: string; key: string; local: WebDavRecord; remote: WebDavRecord; reason: string; title: string }
export const webdavConflicts = () => invoke<WebDavConflict[]>("webdav_conflicts");
export const webdavResolve = (kind: string, key: string, choice: "local" | "remote" | "newer" | "root") => invoke<void>("webdav_resolve", { kind, key, choice });
export const webdavStatus = () => invoke<WebDavStatus>("webdav_status");
export const webdavConnect = (url: string, username: string, password: string, directory = "Papr") =>
  invoke<void>("webdav_connect", { url, username, password, directory });
export const webdavDisconnect = () => invoke<void>("webdav_disconnect");
export const webdavSync = () => invoke<number>("webdav_sync");
export const setReadingProgress = (id: number, progress: number) =>
  invoke<void>("set_reading_progress", { id, progress });

export const openYoutubePlayer = (videoId: string) => invoke<void>("open_youtube_player", { videoId });

export interface FeedCleanupPlan { total: number; remove: number; protected: number; token: string }
export const previewFeedCleanup = (id: number) => invoke<FeedCleanupPlan>("preview_feed_cleanup", { id });
export const cleanupFeedArticles = (id: number, token: string, automatic: boolean) => invoke<number>("cleanup_feed_articles", { id, token, automatic });
export const disableFeedLimit = (id: number) => invoke<void>("disable_feed_limit", { id });

export interface WebDavNetworkOptions { concurrency: number; timeoutSecs: number; proxy: string }
export interface WebDavProgress { running: boolean; phase: string; completed: number; total: number; downloaded: number; uploaded: number; bytesPerSecond: number; retries: number; elapsedMs: number }
export const webdavNetworkOptions = () => invoke<WebDavNetworkOptions>("webdav_network_options");
export const webdavSaveNetwork = (options: WebDavNetworkOptions) => invoke<void>("webdav_save_network", { options });
export const webdavProgress = () => invoke<WebDavProgress>("webdav_progress");
export const webdavCancel = () => invoke<void>("webdav_cancel");
export const webdavRelocate = (directory: string) => invoke<string>("webdav_relocate", { directory });

export interface WebDavSchedule { intervalMinutes: number }
export const webdavSchedule = () => invoke<WebDavSchedule>("webdav_schedule");
export const webdavSaveSchedule = (options: WebDavSchedule) => invoke<void>("webdav_save_schedule", { options });
export const webdavAutoSync = () => invoke<boolean>("webdav_auto_sync");
