//! HTML sanitization and text extraction. Every piece of feed- or web-supplied
//! HTML passes through `sanitize` before it is ever stored or rendered.

use ammonia::{Builder, UrlRelative};
use ego_tree::iter::Edge;
use ego_tree::NodeRef;
use lol_html::{element, rewrite_str, RewriteStrSettings};
use scraper::node::Node;
use scraper::{ElementRef, Html, Selector};
use std::borrow::Cow;
use std::sync::LazyLock;
use url::Url;

/// MathML elements a formula travels in, for feeds that typeset their math
/// before publishing (KaTeX's `mathml`/`htmlAndMathml` output, and the
/// assistive MathML MathJax hides next to its own rendering). ammonia has no
/// MathML in its default whitelist, so without this the whole tree was
/// unwrapped and the reader showed a flattened run of glyph tokens followed by
/// the LaTeX source — `D f ( P Q ) = … D_f(P\|Q)=\…` — where the formula
/// belonged.
///
/// These are kept as a *carrier* for the TeX source, not as the final
/// rendering: the reader reads the LaTeX out of
/// `<annotation encoding="application/x-tex">` and re-typesets every formula
/// with the KaTeX bundle it ships (see `src/lib/math.ts`), replacing the
/// carrier — including the visual half of KaTeX's `htmlAndMathml` output,
/// whose inline styles and classes no HTML sanitizer can preserve. Keeping the
/// markup intact is therefore all that matters here.
const MATHML_TAGS: &[&str] = &[
    "math", "semantics", "annotation", "mrow", "mi", "mo", "mn", "ms", "mtext", "mspace",
    "msup", "msub", "msubsup", "mfrac", "msqrt", "mroot", "mstyle", "merror", "mpadded",
    "mphantom", "menclose", "mfenced", "mtable", "mtr", "mtd", "mlabeledtr", "mover",
    "munder", "munderover", "mmultiscripts", "mprescripts", "none",
];

/// Attributes worth keeping on the MathML above. `display` (whether the formula
/// is inline or block) and `encoding` (which child holds the TeX) are the two
/// the reader actually reads; the rest only matter if a formula falls back to
/// the browser's own MathML rendering.
///
/// None of these is a URL, a `style`, or an event handler, so the whitelist
/// stays free of script / network / CSS surfaces. `annotation-xml` is
/// deliberately not in `MATHML_TAGS`: it can carry a whole foreign subtree
/// (Content MathML, even HTML) and no formula the reader renders needs it.
const MATHML_ATTRS: &[&str] = &[
    "display", "encoding", "accent", "accentunder", "columnalign", "columnlines",
    "columnspacing", "columnspan", "denomalign", "depth", "displaystyle", "fence", "form",
    "frame", "height", "largeop", "linebreak", "linethickness", "lspace", "mathbackground",
    "mathcolor", "mathsize", "mathvariant", "maxsize", "minsize", "movablelimits", "notation",
    "numalign", "open", "close", "rowalign", "rowlines", "rowspacing", "rowspan", "rspace",
    "scriptlevel", "separator", "separators", "stretchy", "symmetric", "voffset", "width",
];

/// Classes the reader looks for to recognise — and swap out — the wrappers a
/// typeset formula arrives in. Only these four survive sanitization on a
/// `<span>`; every other class is stripped as before.
const MATH_CLASSES: &[&str] = &["katex", "katex-display", "katex-mathml", "katex-html"];

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
        // Allow inline HTML5 video. ammonia's default whitelist drops <video>
        // and <source> entirely, so a feed that embeds a clip with a <video>
        // tag reached the reader as nothing at all. Permit the element plus its
        // safe layout/playback attributes (and <source> for multiple formats);
        // `src`/`poster` are URL attributes, so they go through the same
        // relative-URL rewriting and scheme filtering as every other URL here,
        // which strips `javascript:` and friends. `autoplay` is deliberately
        // not whitelisted — feed-embedded video must be user-initiated.
        .add_tags(["video", "source"])
        .add_tag_attributes(
            "video",
            ["src", "poster", "width", "height", "preload", "loop", "muted", "playsinline"],
        )
        .add_tag_attributes("source", ["src", "type", "media"])
        // Always expose player controls: a feed clip with no `controls` (and no
        // whitelisted `autoplay`) would otherwise be a frozen, unplayable frame.
        .set_tag_attribute_value("video", "controls", "")
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
        .set_tag_attribute_value("img", "referrerpolicy", "no-referrer")
        // Inline (base64) images: a feed that embeds its pictures in the body
        // itself ships `<img src="data:image/png;base64,…">`, and ammonia drops
        // the whole `src` — `data` is not one of its default URL schemes — which
        // left the reader showing a broken image with nothing for the
        // click-to-zoom viewer to open. Re-admit the scheme and hand the narrow
        // policy to `keep_data_uris`: only the attributes that *decode* inline
        // bytes keep a media payload, so a `data:text/html` link (which would
        // navigate the webview to a document the feed authored) is still
        // stripped. See `keep_data_uris`.
        .add_url_schemes(["data"])
        .attribute_filter(keep_data_uris)
        // Formulas: let the markup carrying the TeX source through unchanged
        // (see MATHML_TAGS), plus the KaTeX / MathJax wrappers the reader needs
        // to recognise so it can replace the whole rendered block.
        .add_tags(MATHML_TAGS)
        .add_tags(["mjx-container", "mjx-assistive-mml"])
        .add_allowed_classes("span", MATH_CLASSES)
        .add_tag_attributes("mjx-container", ["jax", "display"])
        .add_tag_attributes("mjx-assistive-mml", ["display", "unselectable"]);

    for tag in MATHML_TAGS {
        builder.add_tag_attributes(tag, MATHML_ATTRS);
    }

    let parsed_base = base.and_then(|b| Url::parse(b).ok());
    if let Some(b) = parsed_base {
        builder.url_relative(UrlRelative::RewriteWithBase(b));
    }
    builder.clean(&html).to_string()
}

/// Per-attribute policy for `data:` URLs, which ammonia admits globally once
/// `sanitize` re-allows the scheme (its default URL schemes exclude `data`).
///
/// A `data:` URL is kept only where the webview decodes the bytes as *media*,
/// and only for a media type it can render:
///
/// * `img src` / `video poster` → `image/*`
/// * `video src` / `source src` → `image/*`, `video/*`, `audio/*`
///
/// Everything else is dropped — most importantly `<a href="data:…">`, which
/// would navigate the reader's own webview to a page the feed controls. This
/// filter is handed *every* attribute, so it must return non-`data:` values
/// untouched.
fn keep_data_uris<'a>(element: &str, attribute: &str, value: &'a str) -> Option<Cow<'a, str>> {
    if !value.starts_with("data:") {
        return Some(Cow::Borrowed(value));
    }
    let allowed: &[&str] = match (element, attribute) {
        ("img", "src") | ("video", "poster") => &["image/"],
        ("video", "src") | ("source", "src") => &["image/", "video/", "audio/"],
        _ => return None,
    };
    data_uri_media_type(value)
        .is_some_and(|mime| allowed.iter().any(|p| mime.starts_with(p)))
        .then_some(Cow::Borrowed(value))
}

/// The media type a `data:` URL declares — `data:image/png;base64,…` →
/// `image/png` — lowercased, or `None` when it declares none (`data:,text`).
/// Parameters (`;base64`, `;charset=…`) are ignored.
fn data_uri_media_type(url: &str) -> Option<String> {
    let meta = url.strip_prefix("data:")?.split(',').next()?;
    let mime = meta.split(';').next()?.trim();
    (!mime.is_empty()).then(|| mime.to_ascii_lowercase())
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
///
/// A formula is the exception to "concatenate the text": left alone its MathML
/// would be read glyph by glyph (`D`, `f`, `(`, `P`, …), so the whole subtree
/// is replaced by the formula's TeX source instead — see `math_source` — which
/// is what makes a math-heavy article searchable and summarisable at all.
pub fn html_to_text(html: &str) -> String {
    let frag = Html::parse_fragment(html);
    let mut out = String::new();
    // Depth of the current script/style/etc. subtree — text is dropped while
    // this is non-zero.
    let mut skip = 0u32;
    // Depth of the current <math> subtree, whose own text is replaced by the
    // formula's TeX source on entry (and therefore skipped below).
    let mut math = 0u32;
    for edge in frag.tree.root().traverse() {
        match edge {
            Edge::Open(node) => match node.value() {
                Node::Element(el) => {
                    let name = el.name();
                    if SKIP_TAGS.contains(&name) {
                        skip += 1;
                    } else if skip == 0 && math == 0 && name == "math" {
                        // Space-padded so the formula can't run into the words
                        // around it once whitespace is collapsed.
                        out.push(' ');
                        out.push_str(&math_source(node));
                        out.push(' ');
                        math += 1;
                    } else if skip == 0 && math == 0 && BLOCK_TAGS.contains(&name) {
                        out.push(' ');
                    }
                }
                Node::Text(t) if skip == 0 && math == 0 => out.push_str(t),
                _ => {}
            },
            Edge::Close(node) => {
                if let Node::Element(el) = node.value() {
                    let name = el.name();
                    if SKIP_TAGS.contains(&name) {
                        skip = skip.saturating_sub(1);
                    } else if math > 0 && name == "math" {
                        math -= 1;
                    } else if skip == 0 && math == 0 && BLOCK_TAGS.contains(&name) {
                        out.push(' ');
                    }
                }
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

static MATH_ANNOTATIONS: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse("annotation, annotation-xml").expect("annotation is a valid selector")
});

/// Whether a MathML `<annotation encoding="…">` holds the formula's TeX source.
/// KaTeX and MathJax both publish it as `application/x-tex` (some tools use
/// `application/x-latex`). A naive `contains("tex")` would also accept
/// `text/plain` — "tex" sits inside "text" — whose content is a plain-text
/// *rendering* of the formula, not TeX, so require a non-text media type.
fn is_tex_encoding(encoding: &str) -> bool {
    let encoding = encoding.trim().to_ascii_lowercase();
    !encoding.starts_with("text/") && encoding.contains("tex")
}

/// The plain text a formula contributes to `html_to_text`: the LaTeX source
/// from the MathML `<annotation encoding="application/x-tex">` that KaTeX and
/// MathJax both keep inside a formula, falling back to the flattened
/// presentation tokens for hand-written MathML (Word/Docs exports) that has no
/// annotation. `math` is the `<math>` node itself.
fn math_source(math: NodeRef<'_, Node>) -> String {
    let Some(el) = ElementRef::wrap(math) else {
        return String::new();
    };
    let tex = el
        .select(&MATH_ANNOTATIONS)
        .filter(|a| a.value().attr("encoding").is_some_and(is_tex_encoding))
        .map(|a| a.text().collect::<String>())
        .find(|t| !t.trim().is_empty());
    match tex {
        Some(t) => t.trim().to_string(),
        None => flattened_math_text(math),
    }
}

/// The flattened presentation tokens of a formula — the glyph run a hand-written
/// / Word-exported MathML offers when it carries no TeX annotation. Text inside
/// `<annotation>`/`<annotation-xml>` is skipped: that content is a *source* form
/// (TeX, or a plain-text rendering), never the glyphs the reader would see, so
/// including it would splice a duplicate of the formula into the index.
fn flattened_math_text(math: NodeRef<'_, Node>) -> String {
    let mut out = String::new();
    let mut skip = 0u32;
    for edge in math.traverse() {
        match edge {
            Edge::Open(node) => match node.value() {
                Node::Element(el) if el.name() == "annotation" || el.name() == "annotation-xml" => {
                    skip += 1;
                }
                Node::Text(t) if skip == 0 => out.push_str(t),
                _ => {}
            },
            Edge::Close(node) => {
                if let Node::Element(el) = node.value() {
                    if el.name() == "annotation" || el.name() == "annotation-xml" {
                        skip = skip.saturating_sub(1);
                    }
                }
            }
        }
    }
    out.trim().to_string()
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

    // --- video: inline HTML5 video survives sanitize (issue #39). ---

    #[test]
    fn sanitize_keeps_video_and_source() {
        let out = sanitize(
            r#"<video poster="https://ex.com/p.jpg"><source src="https://ex.com/v.mp4" type="video/mp4"></video>"#,
            None,
        );
        assert!(out.contains("<video"), "video tag dropped: {out}");
        assert!(out.contains("<source"), "source tag dropped: {out}");
        assert!(out.contains(r#"src="https://ex.com/v.mp4""#), "source src dropped: {out}");
        assert!(out.contains(r#"type="video/mp4""#), "source type dropped: {out}");
        assert!(out.contains(r#"poster="https://ex.com/p.jpg""#), "poster dropped: {out}");
    }

    #[test]
    fn sanitize_forces_video_controls() {
        // A clip with no controls (and no autoplay) is an unplayable frozen
        // frame — controls are forced on so the reader can always play it.
        let out = sanitize(r#"<video src="https://ex.com/v.mp4"></video>"#, None);
        assert!(out.contains("controls"), "controls not forced: {out}");
    }

    #[test]
    fn sanitize_strips_video_autoplay_and_handlers() {
        // autoplay isn't whitelisted (feed video must be user-initiated), and
        // event handlers are stripped like everywhere else.
        let out = sanitize(
            r#"<video src="https://ex.com/v.mp4" autoplay onerror="evil()"></video>"#,
            None,
        );
        assert!(!out.contains("autoplay"), "autoplay survived: {out}");
        assert!(!out.contains("onerror"), "event handler survived: {out}");
    }

    #[test]
    fn sanitize_rewrites_relative_video_src_against_base() {
        let out = sanitize(
            r#"<video src="/clip.mp4"></video>"#,
            Some("https://example.com/post/"),
        );
        assert!(
            out.contains("https://example.com/clip.mp4"),
            "relative video src not rewritten: {out}"
        );
    }

    #[test]
    fn sanitize_drops_javascript_video_src() {
        let out = sanitize(r#"<video src="javascript:evil()"></video>"#, None);
        assert!(!out.contains("javascript:"), "javascript: src survived: {out}");
    }

    // --- inline data: images: a body that embeds its pictures as base64. ---

    #[test]
    fn sanitize_keeps_inline_data_image() {
        // `<img src="data:image/png;base64,…">` is a picture the feed inlined in
        // the body. ammonia's default URL schemes exclude `data`, so the `src`
        // was dropped and the reader showed a broken image.
        let src = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==";
        let out = sanitize(&format!(r#"<p>x</p><img src="{src}" alt="dot"><p>y</p>"#), None);
        assert!(out.contains(src), "inline image src dropped: {out}");
        assert!(out.contains(r#"alt="dot""#), "inline image alt dropped: {out}");
    }

    #[test]
    fn sanitize_keeps_inline_svg_and_clip_data_uris() {
        // An SVG payload is safe inside <img> (an image context never runs the
        // document's scripts), and a data: clip/poster is what a feed that
        // inlines a short video ships.
        let svg = "data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciLz4=";
        let clip = "data:video/mp4;base64,AAAAIGZ0eXBpc29tAAACAGlzb21pc28yYXZjMW1wNDE=";
        let out = sanitize(
            &format!(r#"<img src="{svg}"><video poster="{svg}" src="{clip}"></video>"#),
            None,
        );
        assert!(out.contains(svg), "inline svg dropped: {out}");
        assert!(out.contains(clip), "inline clip dropped: {out}");
    }

    #[test]
    fn sanitize_keeps_an_unquoted_inline_data_image() {
        // Feed markup often leaves attributes unquoted (dreams.plus does). A
        // base64 payload *without* `=` padding is legal unquoted, and has to come
        // out of the sanitizer quoted and intact.
        let out = sanitize(
            r#"<img alt=pic src=data:image/png;base64,iVBORw0KGgoAAAANSUhEUg>"#,
            None,
        );
        assert!(
            out.contains(r#"src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUg""#),
            "unquoted inline src dropped: {out}"
        );
    }

    #[test]
    fn sanitize_drops_non_media_data_uris() {
        // The `data` scheme is admitted globally, so the narrow policy has to be
        // enforced per attribute: a link to a feed-authored HTML document would
        // navigate the webview, and a non-media payload smuggled into a media
        // attribute is not something the reader can render.
        let out = sanitize(
            r#"<a href="data:text/html,<b>x</b>">link</a><img src="data:text/html;base64,PGI+eDwvYj4="><video src="data:application/octet-stream;base64,AAAA"></video>"#,
            None,
        );
        assert!(!out.contains("data:text/html"), "html data: URL survived: {out}");
        assert!(!out.contains("data:application"), "binary data: URL survived: {out}");
    }

    #[test]
    fn sanitize_still_drops_other_schemes_after_allowing_data() {
        // Re-allowing `data` must not widen anything else: script-bearing and
        // unlisted schemes stay stripped on every element.
        let out = sanitize(
            r#"<img src="javascript:evil()"><video src="javascript:evil()"></video><a href="my-scheme:home">x</a>"#,
            None,
        );
        assert!(!out.contains("javascript:"), "javascript: survived: {out}");
        assert!(!out.contains("my-scheme:"), "unlisted scheme survived: {out}");
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

    // --- math: formulas reach the reader as a TeX-source carrier, and index as
    //     their TeX source. Chooses `\frac{dP}{dQ}` because its flattened MathML
    //     tokens (`dPdQ`) differ from the TeX, so a test can tell which was used.

    #[test]
    fn formula_text_uses_the_annotation_tex() {
        // KaTeX's `mathml` output: the TeX lives in `<annotation>`, the
        // presentation tokens are what the reader must *not* index.
        let html = concat!(
            r#"<p>entropy</p>"#,
            r#"<span class="katex"><math display="block">"#,
            r#"<semantics><mrow><mi>d</mi><mi>P</mi><mi>d</mi><mi>Q</mi></mrow>"#,
            r#"<annotation encoding="application/x-tex">\frac{dP}{dQ}</annotation>"#,
            r#"</semantics></math></span>"#,
        );
        let text = html_to_text(html);
        assert_eq!(text, r"entropy \frac{dP}{dQ}");
    }

    #[test]
    fn formula_text_falls_back_to_tokens_without_annotation() {
        // Hand-written / Word-exported MathML has no annotation; the flattened
        // presentation tokens are the only text it offers.
        let html = "<math><mrow><mi>x</mi><mo>=</mo><mn>1</mn></mrow></math>";
        assert_eq!(html_to_text(html), "x=1");
    }

    #[test]
    fn formula_text_ignores_a_non_tex_annotation() {
        // An annotation that isn't the TeX source (e.g. a `text/plain` copy)
        // must not be mistaken for the formula body.
        let html = concat!(
            "<math><semantics><mrow><mi>a</mi></mrow>",
            r#"<annotation encoding="text/plain">not the formula</annotation>"#,
            "</semantics></math>",
        );
        assert_eq!(html_to_text(html), "a");
    }

    #[test]
    fn sanitize_keeps_the_mathml_carrier() {
        let out = sanitize(
            concat!(
                r#"<span class="katex"><math display="block">"#,
                r#"<semantics><mrow><mo stretchy="false">(</mo><mi>x</mi></mrow>"#,
                r#"<annotation encoding="application/x-tex">(x</annotation>"#,
                r#"</semantics></math></span>"#,
            ),
            None,
        );
        assert!(out.contains("<math"), "math dropped: {out}");
        assert!(out.contains(r#"display="block""#), "display dropped: {out}");
        assert!(out.contains("<annotation"), "annotation dropped: {out}");
        assert!(
            out.contains(r#"encoding="application/x-tex""#),
            "encoding dropped: {out}"
        );
        assert!(
            out.contains(r#"stretchy="false""#),
            "presentation attr dropped: {out}"
        );
        assert!(out.contains(r#"class="katex""#), "katex class dropped: {out}");
    }

    #[test]
    fn sanitize_keeps_katex_classes_but_strips_style_and_other_classes() {
        // The reader recognises the wrappers by class; `style` (which no
        // sanitizer can preserve meaningfully) and unrelated KaTeX classes are
        // stripped as before.
        let out = sanitize(
            concat!(
                r#"<span class="katex-display" style="text-align:center">"#,
                r#"<span class="katex"><span class="katex-mathml mord">"#,
                r#"<math><semantics><annotation encoding="application/x-tex">x</annotation>"#,
                "</semantics></math></span></span></span>",
            ),
            None,
        );
        assert!(out.contains("katex-display"), "wrapper class dropped: {out}");
        assert!(out.contains("katex-mathml"), "wrapper class dropped: {out}");
        assert!(!out.contains("mord"), "stray class survived: {out}");
        assert!(!out.contains("style="), "style survived: {out}");
    }

    #[test]
    fn sanitize_does_not_smuggle_markup_through_mathml() {
        // Attribute-smuggling attempt: markup hidden in an `mtext` child's
        // attribute value. Ammonia parses the input as HTML, so the payload is
        // attribute text, never real elements.
        let out = sanitize(
            concat!(
                "<math><mtext>",
                r#"<a title="</mtext><img src=x onerror=alert(1)>">x</a>"#,
                "</mtext></math>",
            ),
            None,
        );
        let frag = Html::parse_fragment(&out);
        let sel = Selector::parse("img, script").expect("valid selector");
        assert_eq!(
            frag.select(&sel).count(),
            0,
            "markup smuggled through MathML: {out}"
        );
    }
}
