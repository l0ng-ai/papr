//! Feed discovery (feature F6).
//!
//! Two pure, network-free building blocks:
//!
//! * [`search_directory`] — case-insensitive matching against a bundled
//!   curated directory of well-known feeds (embedded with `include_str!`).
//! * [`parse_deep_link`] — parsing a `papr://subscribe?url=<encoded>` deep
//!   link handed over by the browser extension.
//!
//! Both are deliberately side-effect-free so they can be unit-tested without
//! the filesystem or the network. The live page-scrape half of discovery
//! (fetching a URL and running `parse::discover_feeds`) lives in the
//! `search_feed_directory` Tauri command, which orchestrates these pieces.

use serde::{Deserialize, Serialize};

/// The curated directory, embedded into the binary at compile time so no
/// Tauri resource configuration is required.
const DIRECTORY_JSON: &str = include_str!("../../resources/feed-directory.json");

/// One entry in the curated feed directory.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryEntry {
    /// Display name of the feed.
    pub title: String,
    /// The subscribable feed URL.
    pub feed_url: String,
    /// The human-facing website the feed belongs to.
    pub site_url: String,
    /// Broad category — used to group results in the UI. Authored in the
    /// entry's own language, so a directory slice reads natively.
    pub category: String,
    /// One-line description shown under the title.
    pub description: String,
    /// Primary language of the feed's content (`"en"`, `"zh"`, `"ja"`).
    /// Explore recommends the slice matching the user's chosen UI language.
    pub lang: String,
}

/// A single discovery result, returned to the frontend.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryResult {
    /// Display name of the feed.
    pub title: String,
    /// The subscribable feed URL — passed straight to `add_feed`.
    pub feed_url: String,
    /// The website the feed belongs to, when known.
    pub site_url: Option<String>,
    /// Category for directory entries; `None` for live page scrapes.
    pub category: Option<String>,
    /// Short description, when known.
    pub description: Option<String>,
    /// `true` when the result came from the curated directory, `false` when
    /// it was scraped live from a page the user pasted.
    pub from_directory: bool,
}

impl DiscoveryResult {
    /// Build a result from a curated [`DirectoryEntry`].
    fn from_entry(e: &DirectoryEntry) -> Self {
        DiscoveryResult {
            title: e.title.clone(),
            feed_url: e.feed_url.clone(),
            site_url: Some(e.site_url.clone()),
            category: Some(e.category.clone()),
            description: Some(e.description.clone()),
            from_directory: true,
        }
    }

    /// Build a result from a feed URL discovered live on a scraped page.
    pub fn from_scrape(feed_url: String, title: Option<String>) -> Self {
        DiscoveryResult {
            title: title.unwrap_or_else(|| feed_url.clone()),
            feed_url,
            site_url: None,
            category: None,
            description: None,
            from_directory: false,
        }
    }
}

/// Parse the embedded directory JSON. Panics only on a malformed bundled
/// asset, which is a build-time error rather than a runtime condition.
pub fn directory() -> Vec<DirectoryEntry> {
    serde_json::from_str(DIRECTORY_JSON).expect("bundled feed-directory.json is valid JSON")
}

/// The primary subtag of a BCP-47-ish language tag, lowercased: `"zh-CN"` and
/// `"ZH"` both reduce to `"zh"`. Used to match a UI locale against the
/// directory's coarser per-language tags.
fn primary_lang(tag: &str) -> String {
    tag.split(['-', '_'])
        .next()
        .unwrap_or(tag)
        .to_lowercase()
}

/// Case-insensitive search of the curated directory, scoped to one language.
///
/// `lang` is matched against each entry's `lang` tag by primary subtag, so a
/// `"zh-CN"` UI locale selects the `"zh"` slice. When the directory carries no
/// entry for the requested language the search falls back to English, so
/// Explore is never empty for a locale we have not curated feeds for yet.
///
/// An empty query returns the whole language slice (the UI shows it as a
/// browsable gallery); otherwise an entry matches when the query is a
/// substring of its title, category, or description. Results preserve the
/// directory's authored order.
pub fn search_directory(query: &str, lang: &str) -> Vec<DiscoveryResult> {
    let needle = query.trim().to_lowercase();
    let dir = directory();
    let want = primary_lang(lang);
    let want = if dir.iter().any(|e| primary_lang(&e.lang) == want) {
        want
    } else {
        "en".to_string()
    };
    dir.iter()
        .filter(|e| primary_lang(&e.lang) == want)
        .filter(|e| {
            needle.is_empty()
                || e.title.to_lowercase().contains(&needle)
                || e.category.to_lowercase().contains(&needle)
                || e.description.to_lowercase().contains(&needle)
        })
        .map(DiscoveryResult::from_entry)
        .collect()
}

/// True when the query looks like a URL or bare domain — in which case the
/// discovery command should ALSO fetch the page and scrape it for feeds.
///
/// Heuristics: an explicit scheme, or a dotted token with no whitespace whose
/// last label looks like a TLD (`example.com`, `blog.example.co.uk/path`).
pub fn looks_like_url(query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() || q.contains(char::is_whitespace) {
        return false;
    }
    if q.contains("://") {
        return true;
    }
    // Bare domain: strip any path/query, require at least one dot, and a
    // final label of 2+ ascii-alpha chars (a plausible TLD).
    let host = q.split(['/', '?', '#']).next().unwrap_or(q);
    let labels: Vec<&str> = host.split('.').filter(|s| !s.is_empty()).collect();
    if labels.len() < 2 {
        return false;
    }
    let tld = labels.last().copied().unwrap_or("");
    tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
}

/// Normalize a URL-ish query into a fetchable absolute URL by adding a scheme
/// when the user typed a bare domain.
pub fn normalize_query_url(query: &str) -> String {
    let q = query.trim();
    if q.contains("://") {
        q.to_string()
    } else {
        format!("https://{q}")
    }
}

/// The outcome of parsing a `papr://` deep link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepLink {
    /// `papr://subscribe?url=<encoded feed url>` — subscribe to a feed.
    Subscribe { url: String },
}

/// Parse a `papr://subscribe?url=<encoded>` deep link into a [`DeepLink`].
///
/// Pure and total: returns `None` for anything that is not a well-formed
/// subscribe link — wrong scheme, wrong host/action, a missing or empty `url`
/// parameter, or input that does not parse as a URL at all. URL-encoding in
/// the `url` query value is decoded (`url` crate's `query_pairs`).
pub fn parse_deep_link(input: &str) -> Option<DeepLink> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parsed = url::Url::parse(trimmed).ok()?;
    if parsed.scheme() != "papr" {
        return None;
    }
    // Tauri delivers custom-scheme links as `papr://subscribe?...`, so the
    // action lands in the host component. Accept it in the path too
    // (`papr:///subscribe`) for robustness across platforms.
    let action = match parsed.host_str() {
        Some(h) if !h.is_empty() => h.to_string(),
        _ => parsed.path().trim_matches('/').to_string(),
    };
    if action != "subscribe" {
        return None;
    }
    let target = parsed
        .query_pairs()
        .find(|(k, _)| k == "url")
        .map(|(_, v)| v.into_owned())?;
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    Some(DeepLink::Subscribe {
        url: target.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── bundled directory integrity ───────────────────────────────────

    #[test]
    fn directory_parses_and_is_substantial() {
        let dir = directory();
        assert!(
            dir.len() >= 40,
            "expected a sizeable directory, got {}",
            dir.len()
        );
        // Every entry must have the required, non-empty fields.
        for e in &dir {
            assert!(!e.title.is_empty(), "entry missing title");
            assert!(e.feed_url.starts_with("http"), "bad feed url: {}", e.feed_url);
            assert!(!e.category.is_empty(), "entry missing category: {}", e.title);
            assert!(!e.lang.is_empty(), "entry missing lang: {}", e.title);
        }
    }

    #[test]
    fn directory_curates_each_supported_language() {
        // Every UI locale the app ships must have its own curated slice, or
        // Explore would silently fall back to English for that user.
        let dir = directory();
        for lang in ["en", "zh", "ja"] {
            let n = dir.iter().filter(|e| primary_lang(&e.lang) == lang).count();
            assert!(n >= 10, "expected a curated {lang} slice, got {n}");
        }
    }

    #[test]
    fn directory_has_no_duplicate_feed_urls() {
        let dir = directory();
        let mut seen = std::collections::HashSet::new();
        for e in &dir {
            assert!(seen.insert(&e.feed_url), "duplicate feed url: {}", e.feed_url);
        }
    }

    // ── search_directory ──────────────────────────────────────────────

    /// Count of directory entries in one primary-language slice.
    fn lang_count(lang: &str) -> usize {
        directory()
            .iter()
            .filter(|e| primary_lang(&e.lang) == lang)
            .count()
    }

    #[test]
    fn empty_query_returns_whole_language_slice() {
        assert_eq!(search_directory("", "en").len(), lang_count("en"));
        assert_eq!(search_directory("   ", "en").len(), lang_count("en"));
        assert_eq!(search_directory("", "zh").len(), lang_count("zh"));
    }

    #[test]
    fn search_is_scoped_to_the_requested_language() {
        // A zh query never leaks English feeds and vice versa.
        let zh = search_directory("", "zh");
        assert!(!zh.is_empty());
        assert!(zh.iter().all(|r| r.from_directory));
        let en_titles: std::collections::HashSet<_> =
            search_directory("", "en").into_iter().map(|r| r.title).collect();
        assert!(
            zh.iter().all(|r| !en_titles.contains(&r.title)),
            "zh slice must not contain English feeds"
        );
    }

    #[test]
    fn unknown_language_falls_back_to_english() {
        // A locale we have not curated (Korean here) still gets a usable list.
        assert_eq!(
            search_directory("", "ko").len(),
            search_directory("", "en").len()
        );
    }

    #[test]
    fn primary_subtag_is_matched() {
        // "zh-CN" / "ZH" must resolve to the same slice as "zh".
        let base = search_directory("", "zh").len();
        assert_eq!(search_directory("", "zh-CN").len(), base);
        assert_eq!(search_directory("", "ZH").len(), base);
    }

    #[test]
    fn search_matches_title_case_insensitively() {
        let hits = search_directory("hacker news", "en");
        assert!(hits.iter().any(|r| r.title == "Hacker News"));
        let upper = search_directory("HACKER NEWS", "en");
        assert_eq!(hits.len(), upper.len());
    }

    #[test]
    fn search_matches_category() {
        let hits = search_directory("science", "en");
        assert!(hits.len() >= 3, "expected several Science feeds");
        assert!(hits.iter().all(|r| r.from_directory));
        // The category match must surface every entry filed under Science.
        let science_count = hits
            .iter()
            .filter(|r| r.category.as_deref() == Some("Science"))
            .count();
        assert_eq!(
            science_count,
            directory()
                .iter()
                .filter(|e| e.category == "Science")
                .count(),
            "every Science-category feed should be returned"
        );
    }

    #[test]
    fn search_matches_description() {
        // "astronomy" appears only in a description, not a title/category.
        let hits = search_directory("astronomy", "en");
        assert!(!hits.is_empty());
    }

    #[test]
    fn search_no_match_returns_empty() {
        assert!(search_directory("zzz-no-such-feed-xyz", "en").is_empty());
    }

    #[test]
    fn search_results_carry_directory_metadata() {
        let hits = search_directory("Rust Blog", "en");
        let r = hits.first().expect("expected a Rust Blog hit");
        assert!(r.from_directory);
        assert!(r.site_url.is_some());
        assert!(r.category.is_some());
        assert!(r.feed_url.starts_with("http"));
    }

    // ── looks_like_url ────────────────────────────────────────────────

    #[test]
    fn looks_like_url_detects_schemed_and_bare() {
        assert!(looks_like_url("https://example.com"));
        assert!(looks_like_url("http://example.com/feed"));
        assert!(looks_like_url("example.com"));
        assert!(looks_like_url("blog.example.co.uk/path"));
        assert!(looks_like_url("news.ycombinator.com"));
    }

    #[test]
    fn looks_like_url_rejects_plain_search_terms() {
        assert!(!looks_like_url("science news"));
        assert!(!looks_like_url("rust"));
        assert!(!looks_like_url(""));
        assert!(!looks_like_url("   "));
        // A dotted token whose last label is not a plausible TLD.
        assert!(!looks_like_url("version.1"));
    }

    #[test]
    fn normalize_query_url_adds_scheme_when_missing() {
        assert_eq!(normalize_query_url("example.com"), "https://example.com");
        assert_eq!(
            normalize_query_url("http://example.com"),
            "http://example.com"
        );
        assert_eq!(
            normalize_query_url("  example.com  "),
            "https://example.com"
        );
    }

    // ── parse_deep_link ───────────────────────────────────────────────

    #[test]
    fn deep_link_basic_subscribe() {
        let link = parse_deep_link("papr://subscribe?url=https://example.com/feed.xml");
        assert_eq!(
            link,
            Some(DeepLink::Subscribe {
                url: "https://example.com/feed.xml".to_string()
            })
        );
    }

    #[test]
    fn deep_link_decodes_percent_encoding() {
        let link = parse_deep_link(
            "papr://subscribe?url=https%3A%2F%2Fexample.com%2Ffeed%3Fa%3D1%26b%3D2",
        );
        assert_eq!(
            link,
            Some(DeepLink::Subscribe {
                url: "https://example.com/feed?a=1&b=2".to_string()
            })
        );
    }

    #[test]
    fn deep_link_accepts_action_in_path() {
        // `papr:///subscribe?...` — action in the path rather than the host.
        let link = parse_deep_link("papr:///subscribe?url=https://example.com/feed.xml");
        assert_eq!(
            link,
            Some(DeepLink::Subscribe {
                url: "https://example.com/feed.xml".to_string()
            })
        );
    }

    #[test]
    fn deep_link_rejects_wrong_scheme() {
        assert_eq!(
            parse_deep_link("https://subscribe?url=https://example.com/feed.xml"),
            None
        );
        assert_eq!(
            parse_deep_link("feedly://subscribe?url=https://example.com"),
            None
        );
    }

    #[test]
    fn deep_link_rejects_unknown_action() {
        assert_eq!(
            parse_deep_link("papr://unsubscribe?url=https://example.com"),
            None
        );
    }

    #[test]
    fn deep_link_rejects_missing_url_param() {
        assert_eq!(parse_deep_link("papr://subscribe"), None);
        assert_eq!(parse_deep_link("papr://subscribe?foo=bar"), None);
    }

    #[test]
    fn deep_link_rejects_empty_url_param() {
        assert_eq!(parse_deep_link("papr://subscribe?url="), None);
    }

    #[test]
    fn deep_link_rejects_garbage_and_empty() {
        assert_eq!(parse_deep_link(""), None);
        assert_eq!(parse_deep_link("   "), None);
        assert_eq!(parse_deep_link("not a url at all"), None);
    }
}
