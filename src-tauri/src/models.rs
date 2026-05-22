//! Domain types shared between the database layer, ingestion, and the frontend.
//! All structs use camelCase when serialized so the React side stays idiomatic.

use serde::{Deserialize, Serialize};

/// The kind of source a feed represents. Drives differentiated rendering in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceType {
    Rss,
    Youtube,
    Podcast,
    Mastodon,
    Bluesky,
    /// A subreddit consumed via Reddit's `.rss` endpoint.
    Reddit,
    /// An email newsletter polled from an IMAP mailbox (see ingestion::newsletter).
    Newsletter,
}

impl SourceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceType::Rss => "rss",
            SourceType::Youtube => "youtube",
            SourceType::Podcast => "podcast",
            SourceType::Mastodon => "mastodon",
            SourceType::Bluesky => "bluesky",
            SourceType::Reddit => "reddit",
            SourceType::Newsletter => "newsletter",
        }
    }

}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub id: i64,
    pub name: String,
    pub position: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Feed {
    pub id: i64,
    pub feed_url: String,
    pub site_url: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub favicon_url: Option<String>,
    pub folder_id: Option<i64>,
    pub source_type: String,
    pub last_fetched_at: Option<String>,
    pub fetch_error: Option<String>,
    pub unread_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Enclosure {
    pub url: String,
    pub mime_type: Option<String>,
    pub length: Option<i64>,
}

/// A user-defined label that can be attached to any number of articles.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    pub id: i64,
    pub name: String,
    /// A palette key (resolved to a colour by the frontend).
    pub color: String,
    pub position: i64,
    /// How many articles currently carry this tag.
    pub article_count: i64,
}

/// A keyword filter applied to incoming articles at ingestion time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    /// `None` applies the rule to every feed; otherwise scoped to one feed.
    pub feed_id: Option<i64>,
    /// Which text to match: `title` | `author` | `content` | `any`.
    pub field: String,
    /// Comma-separated keywords; the rule fires if any one is a substring.
    pub query: String,
    /// What to do on a match: `skip` | `read` | `star`.
    pub action: String,
    pub position: i64,
}

/// A row in the article list pane. Keeps the payload small (no full HTML body).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleSummary {
    pub id: i64,
    pub feed_id: i64,
    pub feed_title: String,
    pub source_type: String,
    pub title: String,
    pub author: Option<String>,
    pub snippet: Option<String>,
    pub image_url: Option<String>,
    pub url: Option<String>,
    pub published_at: Option<String>,
    pub is_read: bool,
    pub is_starred: bool,
    pub read_later: bool,
}

/// The full article shown in the reading pane.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticleDetail {
    pub id: i64,
    pub feed_id: i64,
    pub feed_title: String,
    pub source_type: String,
    pub title: String,
    pub author: Option<String>,
    pub url: Option<String>,
    /// Sanitized HTML from the feed itself.
    pub content_html: Option<String>,
    /// Sanitized HTML from full-text extraction (dom_smoothie), if performed.
    pub extracted_html: Option<String>,
    pub image_url: Option<String>,
    pub published_at: Option<String>,
    pub is_read: bool,
    pub is_starred: bool,
    pub read_later: bool,
    pub ai_summary: Option<String>,
    /// Cached translated body HTML, if a translation has been generated.
    pub translated_html: Option<String>,
    /// The target language code the cached translation was produced for.
    pub translated_lang: Option<String>,
    pub enclosures: Vec<Enclosure>,
    /// Tags currently attached to this article.
    pub tags: Vec<Tag>,
}

/// A user highlight / annotation pinned to a span of an article's rendered
/// plain text (feature F7). `text_offset` plus `prefix` / `suffix` form a
/// resilient anchor: the offset is tried first, the context window second.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Highlight {
    pub id: i64,
    pub article_id: i64,
    /// The highlighted text itself.
    pub quote: String,
    /// A short window of text immediately before the quote (for re-anchoring).
    pub prefix: String,
    /// A short window of text immediately after the quote (for re-anchoring).
    pub suffix: String,
    /// Character offset of the quote within the article's plain-text render.
    pub text_offset: i64,
    /// Palette key resolved to a colour by the frontend.
    pub color: String,
    /// Optional user note; an empty string means no note.
    pub note: String,
    pub created_at: String,
}

/// Filters for the article list. Mirrors the sidebar selection in the UI.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum ArticleQuery {
    All,
    Unread,
    Starred,
    ReadLater,
    Feed(i64),
    Folder(i64),
    Tag(i64),
}

/// Live progress for a refresh run, streamed to the frontend over an ipc::Channel.
//
// `rename_all_fields` is required in addition to `rename_all`: the latter only
// camelCases the variant names, not the fields inside struct variants — so
// without it `feed_id` / `new_articles` would reach the frontend snake-cased,
// mismatching the camelCase RefreshProgress type in src/types.ts.
#[derive(Debug, Clone, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "event",
    content = "data"
)]
pub enum RefreshProgress {
    Started { total: usize },
    FeedDone { feed_id: i64, new_articles: usize, error: Option<String> },
    Finished { new_articles: usize },
}
