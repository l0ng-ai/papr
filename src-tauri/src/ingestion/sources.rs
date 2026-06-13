//! Multi-source URL normalization (feature F5).
//!
//! Users paste many kinds of links — YouTube channels, subreddits, Mastodon
//! profiles — that are not themselves subscribable feed documents. This module
//! recognizes those patterns and rewrites them to the real feed URL, reporting
//! the resulting [`SourceType`].
//!
//! Everything here is a *pure* function so it can be unit-tested without the
//! network. The one case that genuinely needs a page fetch — resolving a
//! YouTube `@handle` / `/c/` / `/user/` vanity URL to a channel id — is split
//! into a pure HTML-extraction function ([`extract_channel_id`]); the network
//! fetch itself stays in `add_feed`.

use crate::models::SourceType;
use url::Url;

/// Outcome of running a pasted string through [`normalize_source`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Normalized {
    /// The URL was recognized and rewritten to a directly-subscribable feed.
    Feed {
        /// The real feed URL to fetch and store.
        url: String,
        /// The classified source type.
        source_type: SourceType,
    },
    /// The URL is a YouTube vanity link (`@handle`, `/c/Name`, `/user/Name`,
    /// `youtu.be/...`) whose channel id can only be learned by fetching the
    /// page HTML. `add_feed` fetches `page_url`, then calls
    /// [`extract_channel_id`] + [`youtube_feed_url`].
    NeedsYoutubeResolution {
        /// The page to fetch in order to extract the channel id.
        page_url: String,
    },
    /// Not a recognized special source — hand back to the normal feed /
    /// auto-discovery flow untouched.
    Untouched,
}

/// Inspect a user-pasted URL/string and, if it matches a known source pattern,
/// rewrite it to the subscribable feed URL. Non-matching input is returned as
/// [`Normalized::Untouched`] so the caller's existing discovery flow handles it.
pub fn normalize_source(input: &str) -> Normalized {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Normalized::Untouched;
    }

    // Tolerate a missing scheme (`youtube.com/@x`) — prepend https:// so the
    // URL parser can do host/path matching.
    let with_scheme = super::discovery::normalize_query_url(trimmed);
    let Ok(url) = Url::parse(&with_scheme) else {
        return Normalized::Untouched;
    };
    let host = url.host_str().unwrap_or("").to_lowercase();
    let path = url.path().to_string();

    if host.ends_with("youtube.com") || host == "youtu.be" || host.ends_with(".youtu.be") {
        return normalize_youtube(&host, &path, &url);
    }
    if host == "reddit.com" || host.ends_with(".reddit.com") {
        if let Some(feed) = normalize_reddit(&path) {
            return Normalized::Feed {
                url: feed,
                source_type: SourceType::Reddit,
            };
        }
    }
    if let Some(feed) = normalize_mastodon(&with_scheme, &path) {
        return Normalized::Feed {
            url: feed,
            source_type: SourceType::Mastodon,
        };
    }

    Normalized::Untouched
}

/// Build the canonical YouTube channel feed URL for a `UC…` channel id.
pub fn youtube_feed_url(channel_id: &str) -> String {
    format!("https://www.youtube.com/feeds/videos.xml?channel_id={channel_id}")
}

/// The public RSSHub instance used when the user hasn't configured their own.
pub const DEFAULT_RSSHUB_INSTANCE: &str = "https://rsshub.app";

/// Expand an `rsshub://route` short link into a full feed URL on `instance`.
///
/// RSSHub documents routes in a scheme-style shorthand —
/// `rsshub://github/issue/DIYgod/RSSHub` — that maps onto a normal HTTP feed
/// at `<instance>/github/issue/DIYgod/RSSHub`. Once expanded the result is an
/// ordinary RSS URL, so the caller can run it through the regular fetch/parse
/// flow with no further special-casing.
///
/// Returns `None` for anything that isn't an `rsshub://` link (so callers fall
/// through to the normal flow) or for an empty route. Pure, so it is unit
/// tested without the network; the `instance` setting lookup lives in the
/// caller, mirroring how the YouTube network fetch stays out of this module.
pub fn expand_rsshub(input: &str, instance: &str) -> Option<String> {
    let trimmed = input.trim();
    // Schemes are case-insensitive; users paste `rsshub://` but tolerate any
    // casing. "rsshub://" is pure ASCII, so byte-slicing past it is safe.
    if !trimmed.get(..9)?.eq_ignore_ascii_case("rsshub://") {
        return None;
    }
    let route = trimmed[9..].trim_start_matches('/');
    if route.is_empty() {
        return None;
    }
    let base = instance.trim().trim_end_matches('/');
    Some(format!("{base}/{route}"))
}

/// True for the URL-safe id characters YouTube uses in channel/playlist ids:
/// ASCII alphanumerics plus `_` and `-`.
fn is_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Split a URL path into its non-empty segments.
fn path_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// True if `id` looks like a YouTube channel id (`UC` + 22 chars).
fn is_channel_id(id: &str) -> bool {
    id.len() == 24
        && id.starts_with("UC")
        && id.chars().all(is_id_char)
}

/// True if `id` looks like a YouTube playlist id (`PL`, `UU`, `LL`, `FL`, …).
fn is_playlist_id(id: &str) -> bool {
    (id.len() >= 13)
        && id.chars().all(is_id_char)
        && (id.starts_with("PL")
            || id.starts_with("UU")
            || id.starts_with("LL")
            || id.starts_with("FL")
            || id.starts_with("OL"))
}

fn normalize_youtube(host: &str, path: &str, url: &Url) -> Normalized {
    // Already a feed document — leave it for the normal flow.
    if path.contains("/feeds/videos.xml") {
        return Normalized::Untouched;
    }

    // A `playlist?list=PL…` URL has a playlist feed endpoint.
    if path.starts_with("/playlist") {
        if let Some((_, list)) = url.query_pairs().find(|(k, _)| k == "list") {
            if is_playlist_id(&list) {
                return Normalized::Feed {
                    url: format!(
                        "https://www.youtube.com/feeds/videos.xml?playlist_id={list}"
                    ),
                    source_type: SourceType::Youtube,
                };
            }
        }
    }

    let segments = path_segments(path);

    // `youtube.com/channel/UC…` — the channel id is right there in the path.
    if segments.len() >= 2 && segments[0] == "channel" && is_channel_id(segments[1]) {
        return Normalized::Feed {
            url: youtube_feed_url(segments[1]),
            source_type: SourceType::Youtube,
        };
    }

    // Vanity URLs we can recognize but not resolve without fetching the page:
    //   /@handle      /c/Name      /user/Name
    //   youtu.be/<videoId or handle>
    let is_vanity = segments
        .first()
        .map(|s| s.starts_with('@') || matches!(*s, "c" | "user"))
        .unwrap_or(false);
    let is_short_host = host == "youtu.be" || host.ends_with(".youtu.be");
    if is_vanity || (is_short_host && !segments.is_empty()) {
        return Normalized::NeedsYoutubeResolution {
            page_url: url.as_str().to_string(),
        };
    }

    Normalized::Untouched
}

/// Extract `r/SUBREDDIT` from a Reddit path and build its `.rss` feed URL.
/// Handles `/r/SUB`, `/r/SUB/`, `/r/SUB/top`, `/r/SUB/.rss`, etc.
fn normalize_reddit(path: &str) -> Option<String> {
    let segments = path_segments(path);
    if segments.len() < 2 || segments[0] != "r" {
        return None;
    }
    let sub = segments[1];
    // A subreddit name is alphanumeric + underscore; reject anything else so a
    // post permalink (`/r/SUB/comments/...`) does not become a bogus feed.
    if sub.is_empty()
        || !sub
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    match segments.get(2).copied() {
        // Bare subreddit (`/r/SUB`, `/r/SUB/`) → its main feed.
        None => Some(format!("https://www.reddit.com/r/{sub}/.rss")),
        // A sub-listing (`/r/SUB/top`) → keep that listing's feed.
        Some(listing @ ("hot" | "new" | "top" | "rising")) => Some(format!(
            "https://www.reddit.com/r/{sub}/{listing}/.rss"
        )),
        // Already a `.rss` URL, or a `.rss` listing — leave for the caller.
        Some(s) if s.ends_with(".rss") => None,
        // Anything else (`/comments/...`, `/wiki/...`) is not a subreddit
        // feed — a post permalink must not become a bogus feed.
        Some(_) => None,
    }
}

/// Recognize a Mastodon profile URL (`https://instance/@user`) and append the
/// `.rss` suffix Mastodon exposes for every account's public timeline.
fn normalize_mastodon(full_url: &str, path: &str) -> Option<String> {
    let segments = path_segments(path);
    // A Mastodon profile is exactly one path segment, starting with `@`.
    if segments.len() != 1 {
        return None;
    }
    let handle = segments[0];
    if !handle.starts_with('@') || handle.len() < 2 {
        return None;
    }
    // Already a `.rss` URL — nothing to do.
    if handle.ends_with(".rss") {
        return None;
    }
    // Reuse the input's scheme + host; just swap the path for `/@user.rss`.
    let base = Url::parse(full_url).ok()?;
    let host = base.host_str()?;
    let scheme = base.scheme();
    Some(format!("{scheme}://{host}/{handle}.rss"))
}

/// Pull a YouTube channel id (`UC…`) out of a channel page's HTML.
///
/// Pure and network-free: `add_feed` fetches the page, this parses it.
///
/// Source order matters for *correctness*, not just coverage. A channel page's
/// HTML embeds many `channelId` values — recommended channels, the uploader of
/// every thumbnail in the sidebar, comment authors — and they often appear
/// *before* the page's own id. Picking the first `"channelId"` blindly would
/// subscribe the user to the wrong channel. So the authoritative,
/// page-owner-specific signals are tried first:
///
/// 1. `"externalId"` / `"externalChannelId"` — YouTube's own metadata keys for
///    *this* channel; never used for sidebar/recommended entries.
/// 2. `<link rel="canonical" href=".../channel/UC…">` — the `<head>` canonical
///    URL always points at the page's own channel.
/// 3. The `og:url` (or any) bare `/channel/UC…` substring.
/// 4. Only as a last resort, the ambiguous plain `"channelId"` JSON key.
///
/// Returns `None` if no id can be found.
pub fn extract_channel_id(html: &str) -> Option<String> {
    // 1. Owner-specific metadata keys — unambiguous, so tried first.
    for key in ["\"externalId\":\"", "\"externalChannelId\":\""] {
        if let Some(id) = find_after(html, key, '"') {
            if is_channel_id(&id) {
                return Some(id);
            }
        }
    }

    // 2. <link rel="canonical" href="https://www.youtube.com/channel/UC...">
    if let Some(id) = canonical_channel_id(html) {
        return Some(id);
    }

    // 3. A bare `/channel/UC...` substring anywhere (e.g. og:url meta tag).
    if let Some(id) = channel_id_after_marker(html) {
        return Some(id);
    }

    // 4. Last resort: the plain `"channelId"` JSON key. Ambiguous (also used
    //    for recommended channels), so only consulted when nothing above hit.
    if let Some(id) = find_after(html, "\"channelId\":\"", '"') {
        if is_channel_id(&id) {
            return Some(id);
        }
    }

    None
}

/// Pull a valid channel id out of the text following the first `/channel/`
/// marker in `text` — used for both bare `/channel/UC…` substrings and the
/// `href` of a `<link rel="canonical">` tag.
fn channel_id_after_marker(text: &str) -> Option<String> {
    let rest = text.split("/channel/").nth(1)?;
    let id: String = rest.chars().take_while(|c| is_id_char(*c)).collect();
    is_channel_id(&id).then_some(id)
}

/// Return the substring of `html` after the first occurrence of `marker`, up to
/// (but not including) the next `end` byte.
fn find_after(html: &str, marker: &str, end: char) -> Option<String> {
    let start = html.find(marker)? + marker.len();
    let tail = &html[start..];
    let stop = tail.find(end)?;
    Some(tail[..stop].to_string())
}

/// Find a `<link rel="canonical">` tag and pull a `/channel/UC…` id from its
/// `href`. Order-insensitive about the `rel` / `href` attribute positions.
///
/// A `rel="canonical"` substring that is not wrapped in a real `<…>` tag — it
/// can appear as text inside an embedded `<script>` JSON blob, or lead a
/// malformed document — must be skipped rather than aborting the scan: a
/// genuine `<link rel="canonical">` may still follow later in the document.
fn canonical_channel_id(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let mut search = 0;
    while let Some(rel) = lower[search..].find("rel=\"canonical\"") {
        let abs = search + rel;
        // The enclosing <link …> tag: scan forward to '>' and back to '<'.
        let Some(rel_tag_end) = lower[abs..].find('>') else {
            // No '>' anywhere after this point — nothing more to scan.
            break;
        };
        let tag_end = rel_tag_end + abs;
        let Some(tag_start) = lower[..abs].rfind('<') else {
            // No opening '<' before this occurrence — not a real tag; skip it
            // and keep scanning for a later, well-formed canonical link.
            search = tag_end;
            continue;
        };
        let tag = &html[tag_start..tag_end];
        if let Some(id) = channel_id_after_marker(tag) {
            return Some(id);
        }
        search = tag_end;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(n: Normalized) -> (String, SourceType) {
        match n {
            Normalized::Feed { url, source_type } => (url, source_type),
            other => panic!("expected Feed, got {other:?}"),
        }
    }

    // ── YouTube: channel id directly in the path ──────────────────────

    #[test]
    fn youtube_channel_url_rewrites_to_feed() {
        let (url, st) = feed(normalize_source(
            "https://www.youtube.com/channel/UCXuqSBlHAE6Xw-yeJA0Tunw",
        ));
        assert_eq!(
            url,
            "https://www.youtube.com/feeds/videos.xml?channel_id=UCXuqSBlHAE6Xw-yeJA0Tunw"
        );
        assert_eq!(st, SourceType::Youtube);
    }

    #[test]
    fn youtube_channel_url_without_scheme() {
        let (url, _) = feed(normalize_source(
            "youtube.com/channel/UCXuqSBlHAE6Xw-yeJA0Tunw",
        ));
        assert!(url.ends_with("channel_id=UCXuqSBlHAE6Xw-yeJA0Tunw"));
    }

    #[test]
    fn youtube_playlist_url_rewrites_to_playlist_feed() {
        let (url, st) = feed(normalize_source(
            "https://www.youtube.com/playlist?list=PLFgquLnL59alW3xmYiWRaoz0oM3H17Lth",
        ));
        assert_eq!(
            url,
            "https://www.youtube.com/feeds/videos.xml?playlist_id=PLFgquLnL59alW3xmYiWRaoz0oM3H17Lth"
        );
        assert_eq!(st, SourceType::Youtube);
    }

    // ── YouTube: vanity URLs that need a page fetch ───────────────────

    #[test]
    fn youtube_handle_needs_resolution() {
        match normalize_source("https://www.youtube.com/@veritasium") {
            Normalized::NeedsYoutubeResolution { page_url } => {
                assert!(page_url.contains("@veritasium"));
            }
            other => panic!("expected NeedsYoutubeResolution, got {other:?}"),
        }
    }

    #[test]
    fn youtube_c_and_user_need_resolution() {
        for url in [
            "https://www.youtube.com/c/Vsauce",
            "https://www.youtube.com/user/Vsauce",
        ] {
            assert!(matches!(
                normalize_source(url),
                Normalized::NeedsYoutubeResolution { .. }
            ));
        }
    }

    #[test]
    fn youtu_be_short_link_needs_resolution() {
        assert!(matches!(
            normalize_source("https://youtu.be/dQw4w9WgXcQ"),
            Normalized::NeedsYoutubeResolution { .. }
        ));
    }

    #[test]
    fn youtube_existing_feed_url_untouched() {
        assert_eq!(
            normalize_source(
                "https://www.youtube.com/feeds/videos.xml?channel_id=UCXuqSBlHAE6Xw-yeJA0Tunw"
            ),
            Normalized::Untouched
        );
    }

    // ── Reddit ────────────────────────────────────────────────────────

    #[test]
    fn reddit_subreddit_rewrites_to_rss() {
        let (url, st) = feed(normalize_source("https://www.reddit.com/r/rust"));
        assert_eq!(url, "https://www.reddit.com/r/rust/.rss");
        assert_eq!(st, SourceType::Reddit);
    }

    #[test]
    fn reddit_trailing_slash_and_no_www() {
        for input in [
            "https://www.reddit.com/r/rust/",
            "https://reddit.com/r/rust",
            "reddit.com/r/rust/",
            "https://old.reddit.com/r/rust/",
        ] {
            let (url, _) = feed(normalize_source(input));
            assert_eq!(url, "https://www.reddit.com/r/rust/.rss", "input: {input}");
        }
    }

    #[test]
    fn reddit_listing_variant_preserved() {
        let (url, _) = feed(normalize_source("https://www.reddit.com/r/rust/top"));
        assert_eq!(url, "https://www.reddit.com/r/rust/top/.rss");
    }

    #[test]
    fn reddit_post_permalink_not_treated_as_feed() {
        // A comment permalink must not become a bogus subreddit feed.
        assert_eq!(
            normalize_source("https://www.reddit.com/r/rust/comments/abc123/some_title/"),
            Normalized::Untouched
        );
    }

    #[test]
    fn reddit_home_page_untouched() {
        assert_eq!(
            normalize_source("https://www.reddit.com/"),
            Normalized::Untouched
        );
    }

    // ── Mastodon ──────────────────────────────────────────────────────

    #[test]
    fn mastodon_profile_rewrites_to_rss() {
        let (url, st) = feed(normalize_source("https://mastodon.social/@Gargron"));
        assert_eq!(url, "https://mastodon.social/@Gargron.rss");
        assert_eq!(st, SourceType::Mastodon);
    }

    #[test]
    fn mastodon_preserves_custom_instance() {
        let (url, _) = feed(normalize_source("https://hachyderm.io/@nova"));
        assert_eq!(url, "https://hachyderm.io/@nova.rss");
    }

    #[test]
    fn mastodon_already_rss_untouched() {
        assert_eq!(
            normalize_source("https://mastodon.social/@Gargron.rss"),
            Normalized::Untouched
        );
    }

    #[test]
    fn mastodon_deep_path_untouched() {
        // `/@user/123` is a single post, not a profile.
        assert_eq!(
            normalize_source("https://mastodon.social/@Gargron/109"),
            Normalized::Untouched
        );
    }

    // ── Non-matching input ────────────────────────────────────────────

    #[test]
    fn plain_feed_url_untouched() {
        assert_eq!(
            normalize_source("https://blog.rust-lang.org/feed.xml"),
            Normalized::Untouched
        );
    }

    #[test]
    fn empty_input_untouched() {
        assert_eq!(normalize_source("   "), Normalized::Untouched);
    }

    #[test]
    fn garbage_input_untouched() {
        assert_eq!(normalize_source("not a url at all"), Normalized::Untouched);
    }

    // ── rsshub:// expansion ───────────────────────────────────────────

    #[test]
    fn expand_rsshub_maps_route_onto_instance() {
        assert_eq!(
            expand_rsshub("rsshub://github/issue/DIYgod/RSSHub", DEFAULT_RSSHUB_INSTANCE)
                .as_deref(),
            Some("https://rsshub.app/github/issue/DIYgod/RSSHub")
        );
    }

    #[test]
    fn expand_rsshub_honours_custom_self_hosted_instance() {
        assert_eq!(
            expand_rsshub("rsshub://bilibili/user/video/2267573", "https://rss.example.com/")
                .as_deref(),
            Some("https://rss.example.com/bilibili/user/video/2267573")
        );
    }

    #[test]
    fn expand_rsshub_is_case_insensitive_and_trims_slashes() {
        // Scheme casing varies; leading route slashes and instance trailing
        // slashes must not produce a doubled `//`.
        assert_eq!(
            expand_rsshub("  RSSHub:///telegram/channel/awesomeDIYgod  ", "https://rsshub.app")
                .as_deref(),
            Some("https://rsshub.app/telegram/channel/awesomeDIYgod")
        );
    }

    #[test]
    fn expand_rsshub_rejects_non_rsshub_and_empty_route() {
        assert_eq!(expand_rsshub("https://rsshub.app/foo", DEFAULT_RSSHUB_INSTANCE), None);
        assert_eq!(expand_rsshub("rsshub://", DEFAULT_RSSHUB_INSTANCE), None);
        assert_eq!(expand_rsshub("rss", DEFAULT_RSSHUB_INSTANCE), None);
    }

    // ── channelId extraction ──────────────────────────────────────────

    #[test]
    fn extract_channel_id_from_json_key() {
        let html = r#"<html><script>var x = {"channelId":"UCXuqSBlHAE6Xw-yeJA0Tunw","foo":1};</script></html>"#;
        assert_eq!(
            extract_channel_id(html).as_deref(),
            Some("UCXuqSBlHAE6Xw-yeJA0Tunw")
        );
    }

    #[test]
    fn extract_channel_id_from_external_id_key() {
        let html = r#"{"metadata":{"externalId":"UCXuqSBlHAE6Xw-yeJA0Tunw"}}"#;
        assert_eq!(
            extract_channel_id(html).as_deref(),
            Some("UCXuqSBlHAE6Xw-yeJA0Tunw")
        );
    }

    #[test]
    fn extract_channel_id_prefers_owner_over_recommended() {
        // A real channel page embeds `channelId` for recommended/sidebar
        // channels *before* the page's own id. The owner-specific `externalId`
        // (and the <head> canonical link) must win so the user does not get
        // subscribed to a recommended channel by mistake.
        let html = r#"<head>
            <link rel="canonical" href="https://www.youtube.com/channel/UCXuqSBlHAE6Xw-yeJA0Tunw">
            </head><body><script>var data = {
              "relatedChannels":[{"channelId":"UCwrongRecommendedAAAAA1"}],
              "metadata":{"externalId":"UCXuqSBlHAE6Xw-yeJA0Tunw"}
            };</script></body>"#;
        assert_eq!(
            extract_channel_id(html).as_deref(),
            Some("UCXuqSBlHAE6Xw-yeJA0Tunw"),
            "owner id must win over a recommended channel's id"
        );
    }

    #[test]
    fn extract_channel_id_falls_back_to_plain_channel_id_key() {
        // When no owner-specific signal exists, the plain `channelId` key is
        // still consulted as a last resort.
        let html = r#"{"channelId":"UCXuqSBlHAE6Xw-yeJA0Tunw"}"#;
        assert_eq!(
            extract_channel_id(html).as_deref(),
            Some("UCXuqSBlHAE6Xw-yeJA0Tunw")
        );
    }

    #[test]
    fn extract_channel_id_from_canonical_link() {
        let html = r#"<head><link rel="canonical" href="https://www.youtube.com/channel/UCXuqSBlHAE6Xw-yeJA0Tunw"></head>"#;
        assert_eq!(
            extract_channel_id(html).as_deref(),
            Some("UCXuqSBlHAE6Xw-yeJA0Tunw")
        );
    }

    #[test]
    fn extract_channel_id_canonical_attrs_reordered() {
        let html = r#"<link href="https://www.youtube.com/channel/UCXuqSBlHAE6Xw-yeJA0Tunw" rel="canonical">"#;
        assert_eq!(
            extract_channel_id(html).as_deref(),
            Some("UCXuqSBlHAE6Xw-yeJA0Tunw")
        );
    }

    #[test]
    fn extract_channel_id_canonical_scan_survives_leading_non_tag_match() {
        // A `rel="canonical"` substring with no opening `<` before it (it leads
        // the document — a stray token or escaped script text) is not a real
        // tag. The previous scan used `?` on the `rfind('<')`, aborting the
        // whole `canonical_channel_id` scan on this non-tag occurrence so the
        // genuine `<link rel="canonical">` that follows was never reached.
        let html = "rel=\"canonical\" stray leading text >\n            \
            <link rel=\"canonical\" href=\"https://www.youtube.com/channel/UCXuqSBlHAE6Xw-yeJA0Tunw\">";
        assert_eq!(
            canonical_channel_id(html).as_deref(),
            Some("UCXuqSBlHAE6Xw-yeJA0Tunw"),
            "a non-tag leading rel=\"canonical\" must not abort the scan"
        );
    }

    #[test]
    fn extract_channel_id_canonical_handles_unterminated_tag() {
        // A `rel="canonical"` with no closing `>` anywhere after it
        // (truncated / garbled HTML) must not panic — the scan simply stops.
        // The bare `/channel/UC…` substring rule still recovers the id.
        let html = "<link rel=\"canonical\" href=\"https://www.youtube.com/channel/UCXuqSBlHAE6Xw-yeJA0Tunw";
        assert_eq!(
            extract_channel_id(html).as_deref(),
            Some("UCXuqSBlHAE6Xw-yeJA0Tunw")
        );
    }

    #[test]
    fn extract_channel_id_from_og_url() {
        let html = r#"<meta property="og:url" content="http://www.youtube.com/channel/UCXuqSBlHAE6Xw-yeJA0Tunw">"#;
        assert_eq!(
            extract_channel_id(html).as_deref(),
            Some("UCXuqSBlHAE6Xw-yeJA0Tunw")
        );
    }

    #[test]
    fn extract_channel_id_none_when_absent() {
        assert_eq!(extract_channel_id("<html>no channel here</html>"), None);
    }

    #[test]
    fn extract_channel_id_rejects_malformed_id() {
        // Too short — not a real 24-char channel id.
        let html = r#"{"channelId":"UCshort"}"#;
        assert_eq!(extract_channel_id(html), None);
    }

    #[test]
    fn youtube_feed_url_builder() {
        assert_eq!(
            youtube_feed_url("UCabc"),
            "https://www.youtube.com/feeds/videos.xml?channel_id=UCabc"
        );
    }
}
