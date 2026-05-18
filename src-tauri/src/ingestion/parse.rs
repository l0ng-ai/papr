//! Feed parsing (RSS / Atom / JSON Feed via `feed-rs`), feed auto-discovery,
//! and source-type detection for the multi-source aggregation feature.

use crate::db::NewArticle;
use crate::error::AppResult;
use crate::models::{Enclosure, SourceType};
use crate::sanitize;
use feed_rs::model::{Entry, Feed as RawFeed};
use scraper::{Html, Selector};
use url::Url;

/// Metadata + articles extracted from a single feed document.
pub struct ParsedFeed {
    pub title: Option<String>,
    pub site_url: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub articles: Vec<NewArticle>,
}

/// Parse raw feed bytes. `base_url` is the feed URL, used to resolve relatives.
pub fn parse_feed(bytes: &[u8], base_url: &str) -> AppResult<ParsedFeed> {
    let raw: RawFeed = feed_rs::parser::parse(bytes)?;

    let site_url = pick_site_url(&raw.links).or_else(|| Some(base_url.to_string()));
    let articles = raw
        .entries
        .iter()
        .filter_map(|e| map_entry(e, site_url.as_deref().unwrap_or(base_url)))
        .collect();

    Ok(ParsedFeed {
        title: raw.title.map(|t| t.content),
        site_url,
        description: raw.description.map(|t| t.content),
        icon: raw.icon.or(raw.logo).map(|i| i.uri),
        articles,
    })
}

fn pick_site_url(links: &[feed_rs::model::Link]) -> Option<String> {
    links
        .iter()
        .find(|l| l.rel.as_deref() == Some("alternate"))
        .or_else(|| links.iter().find(|l| l.rel.is_none()))
        .map(|l| l.href.clone())
}

/// Resolve a possibly-relative link href against the feed's base URL.
///
/// `feed_rs::parser::parse` does not carry a base URI, so an Atom feed that
/// uses relative entry links (`<link href="/posts/123">`) yields relative
/// hrefs. Stored unresolved, such a URL breaks "open in browser", full-text
/// extraction and sync URL-matching downstream. Joining against `base` turns
/// it into the absolute URL the rest of the app expects; an already-absolute
/// href is returned unchanged, and an unparseable pair falls back to the raw
/// value rather than dropping the link.
fn resolve_url(href: &str, base: &str) -> String {
    match Url::parse(href) {
        Ok(_) => href.to_string(),
        Err(_) => Url::parse(base)
            .ok()
            .and_then(|b| b.join(href).ok())
            .map(|u| u.to_string())
            .unwrap_or_else(|| href.to_string()),
    }
}

/// Infer an audio/video MIME type from a media URL's file extension, for
/// enclosures whose feed omitted the `type` attribute. Returns `None` for
/// unknown or non-media extensions.
fn mime_from_url(url: &str) -> Option<&'static str> {
    // Strip any query string / fragment before reading the extension.
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    match ext.as_str() {
        "mp3" => Some("audio/mpeg"),
        "m4a" | "aac" => Some("audio/aac"),
        "ogg" | "oga" | "opus" => Some("audio/ogg"),
        "wav" => Some("audio/wav"),
        "flac" => Some("audio/flac"),
        "mp4" | "m4v" => Some("video/mp4"),
        "webm" => Some("video/webm"),
        "mov" => Some("video/quicktime"),
        _ => None,
    }
}

/// Clamp a feed-supplied publication date so it never lands in the future.
///
/// Misconfigured or spammy feeds routinely ship entries dated weeks, months,
/// or years ahead. Because the article list sorts on
/// `COALESCE(published_at, fetched_at) DESC`, a single such entry pins itself
/// to the top of the newest-first list permanently, burying genuinely recent
/// articles. A 24-hour grace window absorbs harmless publisher/client clock
/// skew; anything beyond it is clamped down to "now".
pub fn clamp_publish_date(
    date: chrono::DateTime<chrono::Utc>,
) -> chrono::DateTime<chrono::Utc> {
    let now = chrono::Utc::now();
    let cutoff = now + chrono::Duration::hours(24);
    if date > cutoff {
        now
    } else {
        date
    }
}

fn map_entry(e: &Entry, base: &str) -> Option<NewArticle> {
    // Resolve a relative entry link against the feed's base URL — see
    // `resolve_url`. Done before `guid` falls back to the URL so the dedup
    // key is the absolute form too.
    let url = pick_site_url(&e.links).map(|u| resolve_url(&u, base));
    let guid = if e.id.trim().is_empty() {
        url.clone()?
    } else {
        e.id.clone()
    };

    let raw_html = e
        .content
        .as_ref()
        .and_then(|c| c.body.clone())
        .or_else(|| e.summary.as_ref().map(|s| s.content.clone()))
        .unwrap_or_default();
    let content_html = if raw_html.is_empty() {
        None
    } else {
        Some(sanitize::sanitize(&raw_html, Some(base)))
    };
    let body_text = sanitize::html_to_text(&raw_html);

    let summary = e
        .summary
        .as_ref()
        .map(|s| sanitize::html_to_text(&s.content))
        .filter(|s| !s.is_empty());

    // Image: prefer a media thumbnail, then any image-typed media content.
    let image_url = e.media.iter().find_map(|m| {
        m.thumbnails
            .first()
            .map(|t| t.image.uri.clone())
            .or_else(|| {
                m.content.iter().find_map(|c| {
                    let is_img = c
                        .content_type
                        .as_ref()
                        .map(|t| t.ty().as_str() == "image")
                        .unwrap_or(false);
                    if is_img {
                        c.url.as_ref().map(|u| u.to_string())
                    } else {
                        None
                    }
                })
            })
    });

    // Enclosures: audio/video media content (podcasts, video). Many real-world
    // podcast feeds ship an `<enclosure>` without a `type` attribute, leaving
    // `content_type` empty — in that case fall back to inferring the kind from
    // the URL's file extension so the episode stays playable.
    let enclosures: Vec<Enclosure> = e
        .media
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|c| {
            let url = c.url.as_ref()?.to_string();
            let declared = c.content_type.as_ref().map(|t| t.to_string());
            let mime_type = declared.or_else(|| mime_from_url(&url).map(String::from));
            let is_av = mime_type
                .as_deref()
                .map(|m| m.starts_with("audio") || m.starts_with("video"))
                .unwrap_or(false);
            if is_av {
                Some(Enclosure {
                    url,
                    mime_type,
                    length: c.size.map(|s| s as i64),
                })
            } else {
                None
            }
        })
        .collect();

    Some(NewArticle {
        guid,
        url,
        title: e
            .title
            .as_ref()
            .map(|t| t.content.clone())
            .unwrap_or_else(|| "(untitled)".into()),
        author: e.authors.first().map(|p| p.name.clone()),
        summary,
        content_html,
        body_text,
        image_url,
        published_at: e
            .published
            .or(e.updated)
            .map(clamp_publish_date)
            .map(|d| d.to_rfc3339()),
        enclosures,
    })
}

/// Detect the source type from a feed/site URL — drives differentiated UI.
pub fn detect_source_type(url: &str) -> SourceType {
    let host = Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_lowercase()))
        .unwrap_or_default();
    if host.contains("youtube.com") || host.contains("youtu.be") {
        SourceType::Youtube
    } else if host.contains("bsky.app") || host.contains("bsky.social") {
        SourceType::Bluesky
    } else {
        // Mastodon and podcast detection happen after parsing (see refine_source_type).
        SourceType::Rss
    }
}

/// Refine the source type once a feed has been parsed (e.g. audio enclosures
/// → podcast). `host` is the feed host for Mastodon's `/@user.rss` pattern.
pub fn refine_source_type(initial: SourceType, parsed: &ParsedFeed, feed_url: &str) -> SourceType {
    if initial != SourceType::Rss {
        return initial;
    }
    let has_audio = parsed.articles.iter().any(|a| {
        a.enclosures
            .iter()
            .any(|e| e.mime_type.as_deref().map(|m| m.starts_with("audio")).unwrap_or(false))
    });
    if has_audio {
        return SourceType::Podcast;
    }
    if feed_url.contains("/@") && feed_url.ends_with(".rss") {
        return SourceType::Mastodon;
    }
    SourceType::Rss
}

/// Given the HTML of a web page, find `<link rel="alternate">` feed URLs.
/// Runs synchronously (uses `scraper`); never hold the result across `.await`.
pub fn discover_feeds(html: &str, page_url: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let selector = Selector::parse("link[rel~=alternate]").unwrap();
    let base = Url::parse(page_url).ok();
    let mut found = Vec::new();
    for el in doc.select(&selector) {
        let ty = el.value().attr("type").unwrap_or("").to_lowercase();
        let is_feed = ty.contains("rss") || ty.contains("atom") || ty.contains("json");
        if !is_feed {
            continue;
        }
        if let Some(href) = el.value().attr("href") {
            let resolved = base
                .as_ref()
                .and_then(|b| b.join(href).ok())
                .map(|u| u.to_string())
                .unwrap_or_else(|| href.to_string());
            if !found.contains(&resolved) {
                found.push(resolved);
            }
        }
    }
    found
}

/// Decide whether bytes look like a feed (vs an HTML page) by attempting a parse.
pub fn looks_like_feed(bytes: &[u8]) -> bool {
    feed_rs::parser::parse(bytes).is_ok()
}

#[cfg(test)]
mod tests {
    use super::{clamp_publish_date, mime_from_url, resolve_url};
    use chrono::{Duration, Utc};

    #[test]
    fn resolve_url_keeps_absolute_links_unchanged() {
        assert_eq!(
            resolve_url("https://other.example.com/post/1", "https://feed.example.com/rss"),
            "https://other.example.com/post/1"
        );
    }

    #[test]
    fn resolve_url_joins_relative_links_against_base() {
        // Root-relative and document-relative hrefs both resolve to absolutes.
        assert_eq!(
            resolve_url("/posts/123", "https://blog.example.com/feed.atom"),
            "https://blog.example.com/posts/123"
        );
        assert_eq!(
            resolve_url("123", "https://blog.example.com/posts/"),
            "https://blog.example.com/posts/123"
        );
    }

    #[test]
    fn resolve_url_falls_back_to_raw_when_base_is_unparseable() {
        assert_eq!(resolve_url("/posts/1", "not a url"), "/posts/1");
    }

    #[test]
    fn clamp_leaves_past_dates_untouched() {
        let past = Utc::now() - Duration::days(30);
        assert_eq!(clamp_publish_date(past), past);
    }

    #[test]
    fn clamp_allows_small_clock_skew() {
        // A few hours ahead — harmless publisher/client clock skew — is kept.
        let slightly_ahead = Utc::now() + Duration::hours(6);
        assert_eq!(clamp_publish_date(slightly_ahead), slightly_ahead);
    }

    #[test]
    fn clamp_pulls_far_future_dates_back_to_now() {
        let far_future = Utc::now() + Duration::days(365);
        let clamped = clamp_publish_date(far_future);
        assert!(clamped < far_future);
        // Clamped to roughly "now" — within a generous tolerance of the call.
        assert!((Utc::now() - clamped).num_seconds().abs() < 5);
    }

    #[test]
    fn infers_audio_and_video_from_extension() {
        assert_eq!(mime_from_url("https://cdn.example.com/ep1.mp3"), Some("audio/mpeg"));
        assert_eq!(mime_from_url("http://x/y/show.m4a"), Some("audio/aac"));
        assert_eq!(mime_from_url("https://x/clip.MP4"), Some("video/mp4"));
        assert_eq!(mime_from_url("https://x/clip.webm"), Some("video/webm"));
    }

    #[test]
    fn ignores_query_and_fragment_when_reading_extension() {
        assert_eq!(
            mime_from_url("https://traffic.example.com/ep.mp3?token=abc&t=1"),
            Some("audio/mpeg")
        );
        assert_eq!(mime_from_url("https://x/ep.m4a#chapter2"), Some("audio/aac"));
    }

    #[test]
    fn returns_none_for_non_media_or_extensionless_urls() {
        assert_eq!(mime_from_url("https://example.com/article.html"), None);
        assert_eq!(mime_from_url("https://example.com/feed"), None);
        assert_eq!(mime_from_url("https://example.com/image.jpg"), None);
    }
}
