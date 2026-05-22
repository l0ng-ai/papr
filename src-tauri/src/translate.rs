//! Full-text article translation. Splits an article body into batches of whole
//! top-level blocks, each translated by the cloud LLM (`ai::stream_chat`) with
//! the HTML structure preserved, then reassembled. The pure helpers here —
//! block chunking, prompt building, code-fence stripping — carry the logic worth
//! testing; the streaming/persistence glue lives in `commands::ai_translate`.

use scraper::node::Node;
use scraper::{ElementRef, Html};

/// Generic wrapper tags that carry no readable text of their own. A body that is
/// a single such container (the common "everything inside one `<div>`" feed
/// shape) is unwrapped so its children can be batched rather than translated as
/// one oversized block. Unwrapping is repeated for nested wrappers.
const UNWRAP_TAGS: &[&str] = &["div", "article", "section", "main"];

/// The human-readable name for a UI/target language code, as used in the
/// translation instruction. Anything unrecognised falls back to English.
pub fn language_name(code: &str) -> &'static str {
    match code {
        "zh" => "Simplified Chinese",
        "ja" => "Japanese",
        _ => "English",
    }
}

/// Build the system prompt instructing the model to translate one batch of HTML
/// into `target` while leaving the markup intact.
pub fn translate_system_prompt(target: &str) -> String {
    format!(
        "You are a professional translator. Translate the text content of the \
         HTML fragment into {target}.\n\n\
         Rules:\n\
         - Preserve every HTML tag, attribute, and the overall structure exactly.\n\
         - Translate only human-readable text; do not translate code or URLs.\n\
         - Keep images, links, and all other markup intact.\n\
         - Output only the translated HTML fragment: no preamble, no code fences."
    )
}

/// Split source body HTML into batches of whole top-level blocks, each at most
/// `budget` bytes where possible. Whole elements are never split across batches;
/// a single block larger than `budget` becomes its own batch. A body that is a
/// single generic wrapper (`div`/`article`/`section`/`main`, possibly nested) is
/// unwrapped so its children are batched. Concatenating the returned batches
/// reproduces the source's block sequence (minus any unwrapped wrappers).
pub fn chunk_blocks(html: &str, budget: usize) -> Vec<String> {
    let frag = Html::parse_fragment(html);
    let mut nodes: Vec<_> = frag.root_element().children().collect();

    // Unwrap nested single generic containers so their children can be batched.
    loop {
        let elems: Vec<_> = nodes
            .iter()
            .filter(|n| n.value().as_element().is_some())
            .copied()
            .collect();
        match elems.as_slice() {
            [only] => match only.value().as_element() {
                Some(el) if UNWRAP_TAGS.contains(&el.name()) => {
                    nodes = only.children().collect();
                }
                _ => break,
            },
            _ => break,
        }
    }

    // Serialize each top-level node, dropping whitespace-only text between blocks.
    let mut pieces: Vec<String> = Vec::new();
    for node in nodes {
        match node.value() {
            Node::Element(_) => {
                if let Some(el) = ElementRef::wrap(node) {
                    pieces.push(el.html());
                }
            }
            Node::Text(t) => {
                let text: &str = t;
                if !text.trim().is_empty() {
                    pieces.push(text.to_string());
                }
            }
            _ => {}
        }
    }

    // Greedily group whole pieces under the byte budget. A piece larger than the
    // budget sits alone rather than being split.
    let mut batches: Vec<String> = Vec::new();
    let mut cur = String::new();
    for piece in pieces {
        if !cur.is_empty() && cur.len() + piece.len() > budget {
            batches.push(std::mem::take(&mut cur));
        }
        cur.push_str(&piece);
    }
    if !cur.is_empty() {
        batches.push(cur);
    }
    batches
}

/// Strip a surrounding ```/```html markdown code fence a model may wrap its HTML
/// output in, returning the inner content trimmed. Unfenced input is returned
/// trimmed but otherwise unchanged.
pub fn strip_code_fence(s: &str) -> String {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let rest = rest.strip_suffix("```").unwrap_or(rest);
        // Drop the opening fence's optional language tag line (`html`, ``, …).
        let inner = match rest.find('\n') {
            Some(i) => &rest[i + 1..],
            None => rest,
        };
        return inner.trim().to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── chunk_blocks ────────────────────────────────────────────────────

    #[test]
    fn groups_multiple_blocks_into_one_batch_under_budget() {
        let out = chunk_blocks("<p>a</p><p>b</p><p>c</p>", 1000);
        assert_eq!(out, vec!["<p>a</p><p>b</p><p>c</p>"]);
    }

    #[test]
    fn splits_into_batches_when_over_budget() {
        // Each `<p>x</p>` is 8 bytes; a budget of 8 forces one block per batch.
        let out = chunk_blocks("<p>a</p><p>b</p><p>c</p>", 8);
        assert_eq!(out, vec!["<p>a</p>", "<p>b</p>", "<p>c</p>"]);
        // Reassembly reproduces the source block sequence.
        assert_eq!(out.concat(), "<p>a</p><p>b</p><p>c</p>");
    }

    #[test]
    fn never_splits_a_single_oversized_block() {
        let big = format!("<p>{}</p>", "x".repeat(100));
        let out = chunk_blocks(&big, 8);
        assert_eq!(out, vec![big]);
    }

    #[test]
    fn unwraps_a_single_generic_container() {
        // The wrapping <div> is dropped so its children can be batched.
        let out = chunk_blocks("<div><p>a</p><p>b</p></div>", 8);
        assert_eq!(out, vec!["<p>a</p>", "<p>b</p>"]);
    }

    #[test]
    fn unwraps_nested_generic_containers() {
        let out = chunk_blocks("<div><section><p>a</p><p>b</p></section></div>", 8);
        assert_eq!(out, vec!["<p>a</p>", "<p>b</p>"]);
    }

    #[test]
    fn does_not_unwrap_structural_containers() {
        // A <ul> carries list structure; its <li> children must stay wrapped.
        let out = chunk_blocks("<ul><li>a</li><li>b</li></ul>", 1000);
        assert_eq!(out, vec!["<ul><li>a</li><li>b</li></ul>"]);
    }

    #[test]
    fn keeps_images_inside_their_block() {
        let html = r#"<p>intro</p><figure><img src="https://e.com/a.png"></figure>"#;
        let out = chunk_blocks(html, 8);
        assert_eq!(out.len(), 2);
        assert!(out[1].contains("https://e.com/a.png"));
    }

    #[test]
    fn empty_or_whitespace_input_yields_no_batches() {
        assert!(chunk_blocks("", 1000).is_empty());
        assert!(chunk_blocks("   \n  ", 1000).is_empty());
    }

    #[test]
    fn bare_text_is_a_single_batch() {
        assert_eq!(chunk_blocks("just text", 1000), vec!["just text"]);
    }

    // ── translate_system_prompt ─────────────────────────────────────────

    #[test]
    fn prompt_names_the_target_language() {
        let p = translate_system_prompt("Simplified Chinese");
        assert!(p.contains("Simplified Chinese"), "missing language: {p}");
    }

    #[test]
    fn prompt_demands_html_be_preserved() {
        let p = translate_system_prompt("Japanese");
        let lower = p.to_lowercase();
        assert!(lower.contains("html"), "no HTML mention: {p}");
        assert!(lower.contains("preserve") || lower.contains("keep"), "no preserve directive: {p}");
    }

    // ── strip_code_fence ────────────────────────────────────────────────

    #[test]
    fn strips_language_tagged_fence() {
        assert_eq!(strip_code_fence("```html\n<p>x</p>\n```"), "<p>x</p>");
    }

    #[test]
    fn strips_bare_fence() {
        assert_eq!(strip_code_fence("```\n<p>x</p>\n```"), "<p>x</p>");
    }

    #[test]
    fn leaves_unfenced_content_untouched() {
        assert_eq!(strip_code_fence("<p>x</p>"), "<p>x</p>");
        assert_eq!(strip_code_fence("  <p>x</p>  "), "<p>x</p>");
    }

    // ── language_name ───────────────────────────────────────────────────

    #[test]
    fn language_name_maps_codes_with_english_fallback() {
        assert_eq!(language_name("zh"), "Simplified Chinese");
        assert_eq!(language_name("ja"), "Japanese");
        assert_eq!(language_name("en"), "English");
        assert_eq!(language_name("xx"), "English");
    }
}
