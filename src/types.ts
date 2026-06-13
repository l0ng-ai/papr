// Type mirrors of the Rust domain model (see src-tauri/src/models.rs).

export type SourceType =
  | "rss"
  | "youtube"
  | "podcast"
  | "mastodon"
  | "bluesky"
  | "reddit"
  | "newsletter";

/** A configured email-newsletter source (mirrors commands::NewsletterSource). */
export interface NewsletterSource {
  feedId: number;
  title: string;
  host: string;
  port: number;
  username: string;
  folder: string;
}

/** Payload for add_newsletter_source (mirrors commands::NewsletterInput). */
export interface NewsletterInput {
  title: string | null;
  host: string;
  port: number;
  username: string;
  password: string;
  folder: string;
}

/** A feed-discovery result (mirrors discovery::DiscoveryResult). */
export interface DiscoveryResult {
  title: string;
  feedUrl: string;
  siteUrl: string | null;
  category: string | null;
  description: string | null;
  /** true → curated directory entry, false → live page scrape. */
  fromDirectory: boolean;
}

export interface Folder {
  id: number;
  name: string;
  position: number;
}

export interface Feed {
  id: number;
  feedUrl: string;
  siteUrl: string | null;
  title: string;
  description: string | null;
  faviconUrl: string | null;
  folderId: number | null;
  sourceType: SourceType;
  lastFetchedAt: string | null;
  fetchError: string | null;
  unreadCount: number;
  /** Per-feed refresh interval in minutes. `null` follows the global
   *  setting; the `525600` sentinel means "never". */
  refreshIntervalMin: number | null;
}

export interface Enclosure {
  url: string;
  mimeType: string | null;
  length: number | null;
}

export interface Tag {
  id: number;
  name: string;
  color: string;
  position: number;
  articleCount: number;
}

export type RuleField = "title" | "author" | "content" | "any";
export type RuleAction = "skip" | "read" | "star";

/** Dry-run result for a draft filter rule (see preview_rule command). */
export interface RulePreview {
  count: number;
  samples: string[];
}

export interface Rule {
  id: number;
  name: string;
  enabled: boolean;
  feedId: number | null;
  field: RuleField;
  query: string;
  action: RuleAction;
  position: number;
}

export interface ArticleSummary {
  id: number;
  feedId: number;
  feedTitle: string;
  sourceType: SourceType;
  title: string;
  author: string | null;
  snippet: string | null;
  imageUrl: string | null;
  url: string | null;
  publishedAt: string | null;
  isRead: boolean;
  isStarred: boolean;
  readLater: boolean;
}

export interface ArticleDetail {
  id: number;
  feedId: number;
  feedTitle: string;
  sourceType: SourceType;
  title: string;
  author: string | null;
  url: string | null;
  contentHtml: string | null;
  extractedHtml: string | null;
  imageUrl: string | null;
  publishedAt: string | null;
  isRead: boolean;
  isStarred: boolean;
  readLater: boolean;
  aiSummary: string | null;
  /** Cached translated body HTML, if a translation has been generated. */
  translatedHtml: string | null;
  /** The target language code the cached translation was produced for. */
  translatedLang: string | null;
  enclosures: Enclosure[];
  tags: Tag[];
}

export interface SmartCounts {
  unread: number;
  starred: number;
  readLater: number;
}

/** A user highlight / annotation (mirrors models::Highlight). */
export interface Highlight {
  id: number;
  articleId: number;
  quote: string;
  prefix: string;
  suffix: string;
  textOffset: number;
  color: string;
  note: string;
  createdAt: string;
}

// Mirrors the adjacently-tagged Rust `ArticleQuery` enum.
export type ArticleQuery =
  | { kind: "all" }
  | { kind: "unread" }
  | { kind: "starred" }
  | { kind: "readLater" }
  | { kind: "feed"; value: number }
  | { kind: "folder"; value: number }
  | { kind: "tag"; value: number };

export type AiEvent =
  | { type: "delta"; data: string }
  | { type: "done" }
  | { type: "error"; data: string };

/** Batch-level translation progress (mirrors commands::TranslateEvent). */
export type TranslateEvent =
  | { type: "start"; data: { total: number } }
  | { type: "batch"; data: { html: string; done: number } }
  | { type: "done"; data: { html: string } };

export type RefreshProgress =
  | { event: "started"; data: { total: number } }
  | {
      event: "feedDone";
      data: { feedId: number; newArticles: number; error: string | null };
    }
  | { event: "finished"; data: { newArticles: number } };
