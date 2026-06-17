//! HTML sanitization and text extraction. Every piece of feed- or web-supplied
//! HTML passes through `sanitize` before it is ever stored or rendered.

use ammonia::{Builder, UrlRelative};
use ego_tree::iter::Edge;
use lol_html::{element, rewrite_str, RewriteStrSettings};
use scraper::node::Node;
use scraper::{Html, Selector};
use std::sync::LazyLock;
use url::Url;

/// Sanitize untrusted HTML for safe rendering inside the reader webview.
/// Relative URLs are rewritten against `base` so feed images/links resolve.
pub fn sanitize(html: &str, base: Option<&str>) -> String {
    // Recover lazy-loaded image URLs before ammonia runs: feeds that lazy-load
    // (少数派/sspai, WeChat, many CMSes) put the real URL in `data-src`/`srcset`
    // and leave `src` empty or a placeholder, and ammonia's default whitelist
    // drops those attributes — so without this the `<img>` reaches the reader
    // with no usable `src` and silently shows nothing. See `promote_lazy_images`.
    let html = promote_lazy_images(html);

    let mut builder = Builder::default();
    builder
        .link_rel(Some("noopener noreferrer nofollow"))
        .add_generic_attributes(["loading"])
        // Load every feed image without a `Referer`. Common image hosts —
        // notably Sina's `*.sinaimg.cn` CDN, which backs 喷嚏图卦 and many
        // Weibo-sourced feeds — hotlink-protect by Referer: a request carrying
        // the reader's own origin is 403'd, while one with no Referer is
        // served. Without this the image silently fails to load (and the
        // reader then hides the broken `<img>`), so the article looks
        // text-only. Forcing the attribute on every `<img>` also overrides any
        // weaker policy the feed shipped. Hosts that instead *require* a
        // Referer (e.g. `cdnfile.sspai.com`) are covered by the reader's
        // retry-through-backend path — see `commands::fetch_image`.
        .set_tag_attribute_value("img", "referrerpolicy", "no-referrer");

    let parsed_base = base.and_then(|b| Url::parse(b).ok());
    if let Some(b) = parsed_base {
        builder.url_relative(UrlRelative::RewriteWithBase(b));
    }
    builder.clean(&html).to_string()
}

/// Promote a lazy-loaded image's real URL into `src` so it survives `sanitize`.
/// Lazy-loading feeds ship `<img src="" data-src="https://…">` (or a tiny
/// `data:` placeholder in `src`, or only a `srcset`); ammonia's default img
/// whitelist keeps `src` but drops `data-*`/`srcset`, which would leave an
/// `<img>` with nothing to load. Filling `src` here lets the recovered URL flow
/// through the rest of the pipeline unchanged — relative-URL rewriting, the
/// forced `no-referrer` policy, the webview load, and the `fetch_image` Referer
/// fallback that handles hosts like `cdnfile.sspai.com`. Runs before `clean`,
/// so ammonia still has the final say on safety; a rewrite failure falls back
/// to the original HTML.
fn promote_lazy_images(html: &str) -> String {
    let handler = element!("img", |el| {
        let has_real_src = el.get_attribute("src").is_some_and(|s| {
            let s = s.trim();
            !s.is_empty() && !s.starts_with("data:")
        });
        if !has_real_src {
            let recovered = [
                "data-src",
                "data-original",
                "data-actualsrc",
                "data-lazy-src",
            ]
            .iter()
            .find_map(|a| el.get_attribute(a))
            .or_else(|| {
                // `srcset` is "url1 1x, url2 2x" / "url1 480w, …" — take the
                // first candidate's URL.
                el.get_attribute("srcset").and_then(|ss| {
                    ss.split(',')
                        .next()
                        .and_then(|c| c.split_whitespace().next())
                        .map(str::to_string)
                })
            });
            if let Some(url) = recovered {
                let url = url.trim();
                if !url.is_empty() {
                    let _ = el.set_attribute("src", url);
                }
            }
        }
        Ok(())
    });
    rewrite_str(
        html,
        RewriteStrSettings {
            element_content_handlers: vec![handler],
            ..Default::default()
        },
    )
    .unwrap_or_else(|_| html.to_string())
}

/// Tags whose text content is dropped wholesale (it isn't human-readable copy).
const SKIP_TAGS: &[&str] = &["script", "style", "template", "noscript"];

/// Block-level tags: their edges are word boundaries, so text on either side
/// must not be allowed to run together (`</h1><p>` → "TitleBody").
const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "caption",
    "dd",
    "div",
    "dl",
    "dt",
    "figcaption",
    "figure",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "td",
    "th",
    "tr",
    "ul",
];

/// Strip all markup from HTML, yielding collapsed plain text. Used for the
/// FTS body index, list snippets, and AI prompt context.
///
/// Parsing into a DOM (rather than letting ammonia concatenate text nodes)
/// gets two things right that a plain tag-strip does not: HTML entities are
/// decoded (`&amp;` → `&`), and a space is emitted at every block boundary so
/// adjacent paragraphs/headings keep their words apart — while inline tags
/// (`un<b>der</b>line`) still join seamlessly. The traversal is iterative, so
/// pathologically deep markup can't overflow the stack.
pub fn html_to_text(html: &str) -> String {
    let frag = Html::parse_fragment(html);
    let mut out = String::new();
    // Depth of the current script/style/etc. subtree — text is dropped while
    // this is non-zero.
    let mut skip = 0u32;
    for edge in frag.tree.root().traverse() {
        match edge {
            Edge::Open(node) => match node.value() {
                Node::Element(el) => {
                    let name = el.name();
                    if SKIP_TAGS.contains(&name) {
                        skip += 1;
                    } else if skip == 0 && BLOCK_TAGS.contains(&name) {
                        out.push(' ');
                    }
                }
                Node::Text(t) if skip == 0 => out.push_str(t),
                _ => {}
            },
            Edge::Close(node) => {
                if let Node::Element(el) = node.value() {
                    let name = el.name();
                    if SKIP_TAGS.contains(&name) {
                        skip = skip.saturating_sub(1);
                    } else if skip == 0 && BLOCK_TAGS.contains(&name) {
                        out.push(' ');
                    }
                }
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

static IMG_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("img").expect("img is a valid selector"));

/// The first usable image URL embedded in a block of (already-sanitized) HTML.
/// Used as a card-thumbnail fallback when the feed ships no media thumbnail —
/// many feeds put the lead image only as an `<img>` in the entry body. Because
/// `sanitize` has already rewritten relative URLs against the feed base, a
/// non-absolute `src` left here is unresolvable, and a `data:` blob is an
/// inline pixel rather than a real thumbnail; both are skipped.
pub fn first_image(html: &str) -> Option<String> {
    let frag = Html::parse_fragment(html);
    frag.select(&IMG_SELECTOR).find_map(|el| {
        let src = el.value().attr("src")?.trim();
        (src.starts_with("http://") || src.starts_with("https://")).then(|| src.to_string())
    })
}

/// HTML-escape a string for safe interpolation into element text or an
/// attribute value. Escapes the five characters that can break out of either
/// context (`& < > " '`).
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- html_to_text behaviours, each pinned with a regression test. ---

    #[test]
    fn block_boundaries_keep_words_apart() {
        // `</h1><p>` must not collapse to "TitleBody".
        assert_eq!(html_to_text("<h1>Title</h1><p>Body</p>"), "Title Body");
    }

    #[test]
    fn inline_tags_do_not_break_words() {
        // `un<b>der</b>line` is one word — inline edges add no space.
        assert_eq!(html_to_text("un<b>der</b>line"), "underline");
    }

    #[test]
    fn html_entities_are_decoded() {
        assert_eq!(html_to_text("<p>Tom &amp; Jerry</p>"), "Tom & Jerry");
        assert_eq!(html_to_text("caf&#233;"), "café");
        assert_eq!(html_to_text("a &lt;tag&gt; b"), "a <tag> b");
    }

    #[test]
    fn script_and_style_content_is_dropped() {
        assert_eq!(
            html_to_text("<p>hi</p><script>alert(1)</script><style>x{}</style>"),
            "hi"
        );
    }

    #[test]
    fn whitespace_is_collapsed() {
        assert_eq!(
            html_to_text("<p>  lots   of\n\n  space </p>"),
            "lots of space"
        );
    }

    #[test]
    fn empty_and_plain_input() {
        assert_eq!(html_to_text(""), "");
        assert_eq!(html_to_text("just text"), "just text");
    }

    #[test]
    fn deeply_nested_markup_does_not_overflow() {
        // The traversal is iterative; 5000 nested divs must not blow the stack.
        let deep = format!("{}word{}", "<div>".repeat(5000), "</div>".repeat(5000));
        assert_eq!(html_to_text(&deep), "word");
    }

    // --- sanitize: the XSS boundary for all feed-supplied HTML. ---

    #[test]
    fn sanitize_strips_scripts_and_event_handlers() {
        let out = sanitize("<p onclick=\"steal()\">hi</p><script>evil()</script>", None);
        assert!(!out.contains("script"), "script tag survived: {out}");
        assert!(!out.contains("onclick"), "event handler survived: {out}");
        assert!(out.contains("hi"));
    }

    #[test]
    fn sanitize_adds_rel_to_links() {
        let out = sanitize("<a href=\"https://example.com\">x</a>", None);
        assert!(out.contains("noopener"), "link rel missing: {out}");
    }

    // --- first_image: card-thumbnail fallback from body HTML. ---

    #[test]
    fn first_image_returns_the_first_absolute_img() {
        let html =
            r#"<p>intro</p><img src="https://ex.com/a.png"><img src="https://ex.com/b.png">"#;
        assert_eq!(first_image(html).as_deref(), Some("https://ex.com/a.png"));
    }

    #[test]
    fn first_image_skips_unusable_sources() {
        // A leftover relative src can't resolve (sanitize would have made real
        // ones absolute); a data: blob is an inline pixel, not a thumbnail.
        assert_eq!(first_image(r#"<img src="/local.png">"#), None);
        assert_eq!(
            first_image(r#"<img src="data:image/png;base64,AAAA">"#),
            None
        );
        assert_eq!(first_image("<p>no images here</p>"), None);
        assert_eq!(first_image(""), None);
    }

    #[test]
    fn first_image_falls_through_relative_to_next_absolute() {
        let html = r#"<img src="/rel.png"><img src="https://ex.com/real.jpg">"#;
        assert_eq!(
            first_image(html).as_deref(),
            Some("https://ex.com/real.jpg")
        );
    }

    #[test]
    fn sanitize_marks_images_no_referrer() {
        // Hotlink-protected hosts (e.g. *.sinaimg.cn behind 喷嚏图卦) 403 a
        // request that carries the reader's origin as Referer; `no-referrer`
        // is what makes the image load.
        let out = sanitize(r#"<img src="https://wx1.sinaimg.cn/large/a.jpg">"#, None);
        assert!(
            out.contains(r#"referrerpolicy="no-referrer""#),
            "img missing no-referrer policy: {out}"
        );
    }

    #[test]
    fn sanitize_overrides_weaker_image_referrer_policy() {
        // A feed shipping its own (Referer-leaking) policy must not win.
        let out = sanitize(
            r#"<img src="https://wx1.sinaimg.cn/large/a.jpg" referrerpolicy="origin">"#,
            None,
        );
        assert!(out.contains(r#"referrerpolicy="no-referrer""#), "{out}");
        assert!(!out.contains(r#"referrerpolicy="origin""#), "{out}");
    }

    #[test]
    fn sanitize_rewrites_relative_urls_against_base() {
        let out = sanitize("<img src=\"/pic.png\">", Some("https://example.com/post/"));
        assert!(
            out.contains("https://example.com/pic.png"),
            "relative URL not rewritten: {out}"
        );
    }

    // --- promote_lazy_images: recover lazy-loaded <img> URLs before sanitize. ---

    #[test]
    fn promotes_data_src_to_src() {
        // sspai and many CMSes ship the real URL in data-src with src empty;
        // ammonia would otherwise drop data-src, leaving an unloadable <img>.
        let out = sanitize(
            r#"<img src="" data-src="https://cdnfile.sspai.com/a.jpg">"#,
            None,
        );
        assert!(
            out.contains(r#"src="https://cdnfile.sspai.com/a.jpg""#),
            "data-src not promoted: {out}"
        );
    }

    #[test]
    fn promotes_srcset_first_candidate_when_no_src() {
        let out = sanitize(
            r#"<img srcset="https://ex.com/a.jpg 1x, https://ex.com/b.jpg 2x">"#,
            None,
        );
        assert!(
            out.contains(r#"src="https://ex.com/a.jpg""#),
            "srcset not promoted: {out}"
        );
    }

    #[test]
    fn promotes_over_data_placeholder_src() {
        // A 1px data: placeholder in src must not block promotion.
        let out = sanitize(
            r#"<img src="data:image/gif;base64,AAAA" data-original="https://ex.com/real.jpg">"#,
            None,
        );
        assert!(out.contains(r#"src="https://ex.com/real.jpg""#), "{out}");
        assert!(
            !out.contains("data:image/gif"),
            "placeholder survived: {out}"
        );
    }

    #[test]
    fn real_src_is_not_overwritten_by_data_src() {
        let out = sanitize(
            r#"<img src="https://ex.com/real.jpg" data-src="https://ex.com/other.jpg">"#,
            None,
        );
        assert!(out.contains(r#"src="https://ex.com/real.jpg""#), "{out}");
        assert!(
            !out.contains("other.jpg"),
            "data-src wrongly overrode src: {out}"
        );
    }

    #[test]
    fn promoted_image_still_gets_no_referrer() {
        // The recovered src must still flow through the no-referrer policy that
        // hotlink-protected hosts depend on.
        let out = sanitize(r#"<img data-src="https://cdnfile.sspai.com/a.jpg">"#, None);
        assert!(out.contains(r#"referrerpolicy="no-referrer""#), "{out}");
        assert!(out.contains("cdnfile.sspai.com/a.jpg"), "{out}");
    }
}
