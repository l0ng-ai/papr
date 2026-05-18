//! HTML sanitization and text extraction. Every piece of feed- or web-supplied
//! HTML passes through `sanitize` before it is ever stored or rendered.

use ammonia::{Builder, UrlRelative};
use ego_tree::iter::Edge;
use scraper::node::Node;
use scraper::Html;
use url::Url;

/// Sanitize untrusted HTML for safe rendering inside the reader webview.
/// Relative URLs are rewritten against `base` so feed images/links resolve.
pub fn sanitize(html: &str, base: Option<&str>) -> String {
    let mut builder = Builder::default();
    builder
        .link_rel(Some("noopener noreferrer nofollow"))
        .add_generic_attributes(["loading"]);

    let parsed_base = base.and_then(|b| Url::parse(b).ok());
    if let Some(b) = parsed_base {
        builder.url_relative(UrlRelative::RewriteWithBase(b));
    }
    builder.clean(html).to_string()
}

/// Tags whose text content is dropped wholesale (it isn't human-readable copy).
const SKIP_TAGS: &[&str] = &["script", "style", "template", "noscript"];

/// Block-level tags: their edges are word boundaries, so text on either side
/// must not be allowed to run together (`</h1><p>` → "TitleBody").
const BLOCK_TAGS: &[&str] = &[
    "address", "article", "aside", "blockquote", "br", "caption", "dd", "div",
    "dl", "dt", "figcaption", "figure", "footer", "h1", "h2", "h3", "h4", "h5",
    "h6", "header", "hr", "li", "main", "nav", "ol", "p", "pre", "section",
    "table", "td", "th", "tr", "ul",
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- html_to_text: the six behaviours iteration 115 fixed by hand but
    //     never pinned with a regression test. ---

    #[test]
    fn block_boundaries_keep_words_apart() {
        // `</h1><p>` must not collapse to "TitleBody".
        assert_eq!(
            html_to_text("<h1>Title</h1><p>Body</p>"),
            "Title Body"
        );
    }

    #[test]
    fn inline_tags_do_not_break_words() {
        // `un<b>der</b>line` is one word — inline edges add no space.
        assert_eq!(html_to_text("un<b>der</b>line"), "underline");
    }

    #[test]
    fn html_entities_are_decoded() {
        assert_eq!(
            html_to_text("<p>Tom &amp; Jerry</p>"),
            "Tom & Jerry"
        );
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

    #[test]
    fn sanitize_rewrites_relative_urls_against_base() {
        let out = sanitize(
            "<img src=\"/pic.png\">",
            Some("https://example.com/post/"),
        );
        assert!(
            out.contains("https://example.com/pic.png"),
            "relative URL not rewritten: {out}"
        );
    }
}
