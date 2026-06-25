//! Full-text article extraction. Pulls the main content out of a noisy web
//! page (Readability algorithm via `dom_smoothie`) for the "read full text" and
//! "read later" features.

use crate::error::{AppError, AppResult};
use crate::sanitize;
use dom_smoothie::Readability;
use scraper::{Html, Selector};
use std::sync::LazyLock;
use url::Url;

static LEAD_IMAGE_SELECTORS: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse(
        r#"meta[property="og:image"], meta[name="og:image"],
           meta[property="twitter:image"], meta[name="twitter:image"],
           meta[itemprop="image"], link[rel="image_src"]"#,
    )
    .expect("lead image selector is valid")
});

/// Extract the main article HTML from a full web page, then sanitize it.
/// `dom_smoothie`'s reader is not `Send`, so this stays fully synchronous —
/// call it inside `spawn_blocking`, never across an `.await`.
pub fn extract_article(html: &str, url: &str) -> AppResult<String> {
    let mut readability = Readability::new(html, Some(url), None)
        .map_err(|e| AppError::other(format!("readability init: {e}")))?;
    let article = readability
        .parse()
        .map_err(|e| AppError::other(format!("readability parse: {e}")))?;
    let content = article.content.to_string();
    if content.trim().is_empty() {
        return Err(AppError::code("noExtractableContent"));
    }
    Ok(sanitize::sanitize(&content, Some(url)))
}

/// Pull a page-level lead image from metadata, resolving relative URLs against
/// the final article URL. Summary-only feeds such as 少数派 often omit media
/// fields in RSS, but the article page still exposes an `og:image`/Twitter
/// image that can be used as the reader hero after full-text extraction.
pub fn lead_image(html: &str, base: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    doc.select(&LEAD_IMAGE_SELECTORS).find_map(|el| {
        let raw = el
            .value()
            .attr("content")
            .or_else(|| el.value().attr("href"))?
            .trim();
        resolve_http_url(raw, base)
    })
}

fn resolve_http_url(raw: &str, base: &str) -> Option<String> {
    if raw.is_empty() || raw.starts_with("data:") {
        return None;
    }
    let url = Url::parse(raw)
        .or_else(|_| Url::parse(base).and_then(|b| b.join(raw)))
        .ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::lead_image;

    #[test]
    fn lead_image_reads_og_image() {
        let html = r#"<meta property="og:image" content="https://ex.com/a.jpg">"#;
        assert_eq!(
            lead_image(html, "https://site.test/post").as_deref(),
            Some("https://ex.com/a.jpg")
        );
    }

    #[test]
    fn lead_image_resolves_relative_urls() {
        let html = r#"<meta name="twitter:image" content="/img/a.jpg">"#;
        assert_eq!(
            lead_image(html, "https://site.test/post/1").as_deref(),
            Some("https://site.test/img/a.jpg")
        );
    }
}
