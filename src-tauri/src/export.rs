//! Highlight export (feature F7). Turns an article and its highlights into a
//! Markdown document, or into request bodies for the Readwise and Notion APIs.
//!
//! The document/body *builders* are pure functions with unit tests below; the
//! actual network calls are the thin `post_to_readwise` / `post_to_notion`
//! functions, kept separate so the logic stays testable without a network.

use crate::error::{AppError, AppResult};
use crate::models::Highlight;
use reqwest::Client;
use serde_json::{json, Value};

/// The article fields an export needs. A small owned struct so the builders
/// stay pure — they never touch the database.
#[derive(Debug, Clone)]
pub struct ExportArticle {
    pub title: String,
    pub url: Option<String>,
    pub author: Option<String>,
    pub feed_title: String,
    pub published_at: Option<String>,
}

// ─────────────────────────── Markdown ───────────────────────────

/// Escape the characters that would otherwise be interpreted as Markdown
/// syntax inside a line of body text. Conservative — only the markers that
/// actually start inline constructs.
fn escape_md(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Render one highlight as a Markdown blockquote, with its note (if any) as a
/// nested italic line. The quote text is split on newlines so every line of a
/// multi-line quote keeps the `>` blockquote marker.
fn highlight_block(h: &Highlight) -> String {
    let mut block = String::new();
    for line in h.quote.split('\n') {
        block.push_str("> ");
        block.push_str(&escape_md(line));
        block.push('\n');
    }
    let note = h.note.trim();
    if !note.is_empty() {
        block.push_str(">\n");
        for line in note.split('\n') {
            block.push_str("> *");
            block.push_str(&escape_md(line));
            block.push_str("*\n");
        }
    }
    block
}

/// Build a complete Markdown document for an article and its highlights.
/// Suitable for an Obsidian vault note: a YAML-free header block, a source
/// link, then every highlight as a blockquote. Pure — fully unit-tested.
pub fn build_markdown(article: &ExportArticle, highlights: &[Highlight]) -> String {
    let mut doc = String::new();
    doc.push_str("# ");
    doc.push_str(&escape_md(&article.title));
    doc.push_str("\n\n");

    // Metadata lines — only the fields that are present.
    doc.push_str("- **Source:** ");
    doc.push_str(&escape_md(&article.feed_title));
    doc.push('\n');
    if let Some(author) = article.author.as_deref().filter(|a| !a.is_empty()) {
        doc.push_str("- **Author:** ");
        doc.push_str(&escape_md(author));
        doc.push('\n');
    }
    if let Some(date) = article.published_at.as_deref().filter(|d| !d.is_empty()) {
        doc.push_str("- **Published:** ");
        doc.push_str(&escape_md(date));
        doc.push('\n');
    }
    if let Some(url) = article.url.as_deref().filter(|u| !u.is_empty()) {
        // A bare link — URLs are not Markdown-escaped so they stay clickable.
        doc.push_str("- **Link:** ");
        doc.push_str(url);
        doc.push('\n');
    }
    doc.push('\n');

    doc.push_str("## Highlights\n\n");
    if highlights.is_empty() {
        doc.push_str("_No highlights yet._\n");
        return doc;
    }
    for (i, h) in highlights.iter().enumerate() {
        if i > 0 {
            doc.push('\n');
        }
        doc.push_str(&highlight_block(h));
    }
    doc
}

// ─────────────────────────── Readwise ───────────────────────────

/// Build the JSON body for a Readwise `POST /api/v2/highlights/` request.
/// Readwise accepts a batch under a `highlights` array; each entry carries the
/// quote plus the shared article metadata. Pure — unit-tested below.
pub fn build_readwise_body(article: &ExportArticle, highlights: &[Highlight]) -> Value {
    let items: Vec<Value> = highlights
        .iter()
        .map(|h| {
            let mut item = json!({
                "text": h.quote,
                "title": article.title,
                "source_type": "papr",
                "category": "articles",
            });
            if let Some(author) = article.author.as_deref().filter(|a| !a.is_empty()) {
                item["author"] = json!(author);
            }
            if let Some(url) = article.url.as_deref().filter(|u| !u.is_empty()) {
                item["source_url"] = json!(url);
            }
            let note = h.note.trim();
            if !note.is_empty() {
                item["note"] = json!(note);
            }
            item
        })
        .collect();
    json!({ "highlights": items })
}

/// POST a batch of highlights to Readwise. Thin wrapper over the pure builder.
pub async fn post_to_readwise(
    client: &Client,
    token: &str,
    article: &ExportArticle,
    highlights: &[Highlight],
) -> AppResult<()> {
    if highlights.is_empty() {
        return Err(AppError::code("noHighlights"));
    }
    let body = build_readwise_body(article, highlights);
    let resp = client
        .post("https://readwise.io/api/v2/highlights/")
        .header("Authorization", format!("Token {token}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        return Err(AppError::other(format!("Readwise error {status}: {detail}")));
    }
    Ok(())
}

// ─────────────────────────── Notion ───────────────────────────

/// Notion's hard limit on the number of child blocks accepted in a single
/// `POST /v1/pages` or `PATCH /v1/blocks/{id}/children` request. A request
/// carrying more than this is rejected outright with a 400, so callers must
/// split a long block list into batches of at most this size.
const NOTION_MAX_CHILDREN: usize = 100;

/// Notion's per-run character limit. A single `rich_text` run carrying more
/// than this is rejected with a 400.
const NOTION_RUN_CHARS: usize = 2000;

/// Notion's hard limit on the number of runs in a single `rich_text` array.
/// A block whose `rich_text` exceeds this is rejected with a 400, so a long
/// paragraph line must be split across several blocks rather than one.
const NOTION_MAX_RUNS: usize = 100;

/// A Notion rich-text run capped at Notion's 2000-character-per-run limit.
/// Longer text is split into multiple runs so the request is never rejected.
fn notion_rich_text(text: &str) -> Vec<Value> {
    if text.is_empty() {
        return vec![];
    }
    text.chars()
        .collect::<Vec<_>>()
        .chunks(NOTION_RUN_CHARS)
        .map(|chunk| {
            let s: String = chunk.iter().collect();
            json!({ "type": "text", "text": { "content": s } })
        })
        .collect()
}

/// Like [`notion_rich_text`], but tags every run as italic. Used for highlight
/// notes — still split at the 2000-char-per-run limit so a long note never
/// trips Notion's per-request rejection.
fn notion_italic_rich_text(text: &str) -> Vec<Value> {
    notion_rich_text(text)
        .into_iter()
        .map(|mut run| {
            run["annotations"] = json!({ "italic": true });
            run
        })
        .collect()
}

/// Split `runs` into one or more blocks of a single `block_type`, each block's
/// `rich_text` array kept within Notion's `NOTION_MAX_RUNS` cap.
///
/// `notion_rich_text` already splits text at the 2000-char-per-run limit, but a
/// quote or note long enough to need more than `NOTION_MAX_RUNS` runs would
/// still produce one over-limit block — Notion rejects a block whose `rich_text`
/// array exceeds that run count with a 400, failing the *whole* export request.
/// A reader highlighting several long paragraphs (or attaching a lengthy note)
/// can realistically cross that threshold, so the runs are spread across as
/// many blocks of the same type as needed. Always yields at least one block, so
/// an empty `runs` still produces a block with an empty `rich_text` array.
fn notion_blocks_from_runs(block_type: &str, runs: Vec<Value>) -> Vec<Value> {
    let block = |rich_text: &[Value]| {
        json!({
            "object": "block",
            "type": block_type,
            block_type: { "rich_text": rich_text },
        })
    };
    if runs.is_empty() {
        return vec![block(&[])];
    }
    runs.chunks(NOTION_MAX_RUNS).map(block).collect()
}

/// Build the JSON body for a Notion `PATCH /v1/blocks/{id}/children` request
/// that appends an article's highlights to a page. Each highlight becomes a
/// `quote` block; a note becomes a following italic paragraph. A quote or note
/// too long to fit Notion's per-block run cap is split across several blocks.
/// Pure.
pub fn build_notion_body(article: &ExportArticle, highlights: &[Highlight]) -> Value {
    let mut children: Vec<Value> = Vec::new();

    // A heading2 block introducing the article.
    children.push(json!({
        "object": "block",
        "type": "heading_2",
        "heading_2": { "rich_text": notion_rich_text(&article.title) },
    }));

    for h in highlights {
        children.extend(notion_blocks_from_runs("quote", notion_rich_text(&h.quote)));
        let note = h.note.trim();
        if !note.is_empty() {
            children.extend(notion_blocks_from_runs(
                "paragraph",
                notion_italic_rich_text(note),
            ));
        }
    }
    json!({ "children": children })
}

/// PATCH a batch of child blocks onto a Notion block. The batch must already
/// be within `NOTION_MAX_CHILDREN`; chunking is the caller's job.
async fn append_notion_children(
    client: &Client,
    token: &str,
    block_id: &str,
    children: &[Value],
) -> AppResult<()> {
    let resp = client
        .patch(format!(
            "https://api.notion.com/v1/blocks/{block_id}/children"
        ))
        .header("Authorization", format!("Bearer {token}"))
        .header("Notion-Version", "2022-06-28")
        .header("Content-Type", "application/json")
        .json(&json!({ "children": children }))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        return Err(AppError::other(format!("Notion error {status}: {detail}")));
    }
    Ok(())
}

/// Append an article's highlights to a Notion page as child blocks. Thin
/// wrapper over the pure builder. The block list is sent in batches of at most
/// `NOTION_MAX_CHILDREN` so an article with many highlights/notes is not
/// rejected by Notion's per-request child limit.
pub async fn post_to_notion(
    client: &Client,
    token: &str,
    page_id: &str,
    article: &ExportArticle,
    highlights: &[Highlight],
) -> AppResult<()> {
    if highlights.is_empty() {
        return Err(AppError::code("noHighlights"));
    }
    let body = build_notion_body(article, highlights);
    let children = body["children"].as_array().cloned().unwrap_or_default();
    for batch in children.chunks(NOTION_MAX_CHILDREN) {
        append_notion_children(client, token, page_id, batch).await?;
    }
    Ok(())
}

// ─────────── Notion — full-article page (feature F8) ───────────
//
// Distinct from `build_notion_body` above (F7), which *appends highlight
// blocks* to an existing page. The F8 "Send to…" action instead creates a
// whole new Notion page for the article, with its metadata as page properties
// and its text as child paragraph blocks.

/// Split a block of plain text into Notion `paragraph` blocks, one per
/// source line. Each run is capped at Notion's 2000-char-per-run limit, and a
/// line long enough to need more than `NOTION_MAX_RUNS` runs is itself split
/// across several paragraph blocks — Notion rejects a block whose `rich_text`
/// array exceeds that run count, and `html_to_text` collapses an entire
/// article into a single newline-free line, so a longread would otherwise
/// produce one over-limit paragraph and fail the whole export. Blank lines
/// are dropped. Pure.
fn notion_paragraphs(text: &str) -> Vec<Value> {
    let mut blocks: Vec<Value> = Vec::new();
    for line in text.split('\n').map(str::trim).filter(|l| !l.is_empty()) {
        let runs = notion_rich_text(line);
        for run_chunk in runs.chunks(NOTION_MAX_RUNS) {
            blocks.push(json!({
                "object": "block",
                "type": "paragraph",
                "paragraph": { "rich_text": run_chunk },
            }));
        }
    }
    blocks
}

/// Assemble the full ordered list of child blocks for an article's Notion
/// page: an optional source bookmark, a metadata callout, then one paragraph
/// per non-empty body line. Kept separate from `build_notion_page` so the
/// `POST /v1/pages` caller can split the (possibly >100) blocks across the
/// create request and follow-up `append` requests.
fn notion_page_children(article: &ExportArticle, body_text: &str) -> Vec<Value> {
    let mut children: Vec<Value> = Vec::new();

    // A source bookmark, when the article has a URL.
    if let Some(url) = article.url.as_deref().filter(|u| !u.is_empty()) {
        children.push(json!({
            "object": "block",
            "type": "bookmark",
            "bookmark": { "url": url },
        }));
    }
    // A metadata callout (feed · author · date) so the page is self-describing.
    let mut meta: Vec<String> = vec![format!("Source: {}", article.feed_title)];
    if let Some(author) = article.author.as_deref().filter(|a| !a.is_empty()) {
        meta.push(format!("By {author}"));
    }
    if let Some(date) = article.published_at.as_deref().filter(|d| !d.is_empty()) {
        meta.push(date.to_string());
    }
    children.push(json!({
        "object": "block",
        "type": "callout",
        "callout": {
            "rich_text": notion_rich_text(&meta.join("  ·  ")),
            "icon": { "type": "emoji", "emoji": "📰" },
        },
    }));

    // The article body — one paragraph block per non-empty line.
    children.extend(notion_paragraphs(body_text));

    children
}

/// Build the JSON body for a Notion `POST /v1/pages` request that creates a
/// new page for a whole article under `parent_page_id`. The article's plain
/// text is supplied already stripped of markup (`body_text`); each line
/// becomes a paragraph block. A leading bookmark block links back to the
/// source.
///
/// Notion rejects a create request carrying more than `NOTION_MAX_CHILDREN`
/// blocks, so only the first batch is embedded here; `post_article_to_notion`
/// appends any overflow. Pure — unit-tested below.
pub fn build_notion_page(
    parent_page_id: &str,
    article: &ExportArticle,
    body_text: &str,
) -> Value {
    let children = notion_page_children(article, body_text);
    let first: Vec<Value> = children
        .into_iter()
        .take(NOTION_MAX_CHILDREN)
        .collect();
    json!({
        "parent": { "page_id": parent_page_id },
        "properties": {
            "title": {
                "title": notion_rich_text(&article.title),
            },
        },
        "children": first,
    })
}

/// Create a new Notion page for a whole article. Thin wrapper over the pure
/// `build_notion_page` builder; reuses the same Notion HTTP contract as
/// `post_to_notion`.
///
/// A long article produces more than Notion's `NOTION_MAX_CHILDREN`-block
/// per-request limit: the create request carries the first batch, then the
/// remaining blocks are appended to the freshly-created page in further
/// batches. Without this a long article would fail with a Notion 400.
pub async fn post_article_to_notion(
    client: &Client,
    token: &str,
    parent_page_id: &str,
    article: &ExportArticle,
    body_text: &str,
) -> AppResult<()> {
    let body = build_notion_page(parent_page_id, article, body_text);
    let resp = client
        .post("https://api.notion.com/v1/pages")
        .header("Authorization", format!("Bearer {token}"))
        .header("Notion-Version", "2022-06-28")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        return Err(AppError::other(format!("Notion error {status}: {detail}")));
    }

    // Append any blocks that did not fit in the create request. The overflow
    // is everything past the first NOTION_MAX_CHILDREN children.
    let all_children = notion_page_children(article, body_text);
    if all_children.len() > NOTION_MAX_CHILDREN {
        let page_id = resp
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v["id"].as_str().map(str::to_string))
            .ok_or_else(|| {
                AppError::other("Notion: missing page id in create response".to_string())
            })?;
        for batch in all_children[NOTION_MAX_CHILDREN..].chunks(NOTION_MAX_CHILDREN) {
            append_notion_children(client, token, &page_id, batch).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_article() -> ExportArticle {
        ExportArticle {
            title: "Rust in 2024".to_string(),
            url: Some("https://example.com/rust".to_string()),
            author: Some("Jane Doe".to_string()),
            feed_title: "Example Blog".to_string(),
            published_at: Some("2024-01-15".to_string()),
        }
    }

    fn hl(id: i64, quote: &str, note: &str) -> Highlight {
        Highlight {
            id,
            article_id: 1,
            quote: quote.to_string(),
            prefix: String::new(),
            suffix: String::new(),
            text_offset: 0,
            color: "yellow".to_string(),
            note: note.to_string(),
            created_at: "2024-01-15 10:00:00".to_string(),
        }
    }

    // ── Markdown ──

    #[test]
    fn markdown_includes_header_and_metadata() {
        let md = build_markdown(&sample_article(), &[hl(1, "borrow checker", "")]);
        assert!(md.starts_with("# Rust in 2024\n"));
        assert!(md.contains("- **Source:** Example Blog"));
        assert!(md.contains("- **Author:** Jane Doe"));
        assert!(md.contains("- **Published:** 2024-01-15"));
        assert!(md.contains("- **Link:** https://example.com/rust"));
        assert!(md.contains("> borrow checker"));
    }

    #[test]
    fn markdown_escapes_special_characters() {
        let mut a = sample_article();
        a.title = "C# *vs* _Rust_".to_string();
        let md = build_markdown(&a, &[hl(1, "a [link] and #hash", "")]);
        assert!(md.contains("# C\\# \\*vs\\* \\_Rust\\_"));
        assert!(md.contains("> a \\[link\\] and \\#hash"));
    }

    #[test]
    fn markdown_renders_note_as_nested_italic() {
        let md = build_markdown(&sample_article(), &[hl(1, "the quote", "my thought")]);
        assert!(md.contains("> the quote\n>\n> *my thought*\n"));
    }

    #[test]
    fn markdown_multiple_highlights_separated() {
        let md = build_markdown(
            &sample_article(),
            &[hl(1, "first", ""), hl(2, "second", "noted")],
        );
        assert!(md.contains("> first\n"));
        assert!(md.contains("> second\n"));
        assert!(md.contains("> *noted*"));
    }

    #[test]
    fn markdown_empty_highlight_list() {
        let md = build_markdown(&sample_article(), &[]);
        assert!(md.contains("## Highlights"));
        assert!(md.contains("_No highlights yet._"));
    }

    #[test]
    fn markdown_omits_absent_metadata() {
        let a = ExportArticle {
            title: "Untitled".to_string(),
            url: None,
            author: None,
            feed_title: "Feed".to_string(),
            published_at: None,
        };
        let md = build_markdown(&a, &[hl(1, "q", "")]);
        assert!(!md.contains("**Author:**"));
        assert!(!md.contains("**Link:**"));
        assert!(!md.contains("**Published:**"));
    }

    #[test]
    fn markdown_multiline_quote_keeps_blockquote_marker() {
        let md = build_markdown(&sample_article(), &[hl(1, "line one\nline two", "")]);
        assert!(md.contains("> line one\n> line two\n"));
    }

    // ── Readwise ──

    #[test]
    fn readwise_body_shape() {
        let body = build_readwise_body(&sample_article(), &[hl(1, "quote text", "a note")]);
        let items = body["highlights"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["text"], "quote text");
        assert_eq!(items[0]["title"], "Rust in 2024");
        assert_eq!(items[0]["author"], "Jane Doe");
        assert_eq!(items[0]["source_url"], "https://example.com/rust");
        assert_eq!(items[0]["note"], "a note");
        assert_eq!(items[0]["category"], "articles");
    }

    #[test]
    fn readwise_body_omits_empty_note() {
        let body = build_readwise_body(&sample_article(), &[hl(1, "q", "")]);
        assert!(body["highlights"][0].get("note").is_none());
    }

    #[test]
    fn readwise_body_special_characters_preserved() {
        let body = build_readwise_body(
            &sample_article(),
            &[hl(1, "quote with \"quotes\" & <tags>", "emoji 🎉")],
        );
        assert_eq!(
            body["highlights"][0]["text"],
            "quote with \"quotes\" & <tags>"
        );
        assert_eq!(body["highlights"][0]["note"], "emoji 🎉");
    }

    #[test]
    fn readwise_body_multiple_highlights() {
        let body = build_readwise_body(
            &sample_article(),
            &[hl(1, "a", ""), hl(2, "b", ""), hl(3, "c", "")],
        );
        assert_eq!(body["highlights"].as_array().unwrap().len(), 3);
    }

    // ── Notion ──

    #[test]
    fn notion_body_shape() {
        let body = build_notion_body(&sample_article(), &[hl(1, "the quote", "")]);
        let children = body["children"].as_array().unwrap();
        // heading + quote block.
        assert_eq!(children.len(), 2);
        assert_eq!(children[0]["type"], "heading_2");
        assert_eq!(children[1]["type"], "quote");
        assert_eq!(
            children[1]["quote"]["rich_text"][0]["text"]["content"],
            "the quote"
        );
    }

    #[test]
    fn notion_body_note_adds_paragraph() {
        let body = build_notion_body(&sample_article(), &[hl(1, "q", "my note")]);
        let children = body["children"].as_array().unwrap();
        // heading + quote + note paragraph.
        assert_eq!(children.len(), 3);
        assert_eq!(children[2]["type"], "paragraph");
        assert_eq!(
            children[2]["paragraph"]["rich_text"][0]["annotations"]["italic"],
            true
        );
    }

    #[test]
    fn notion_body_special_characters_preserved() {
        let body = build_notion_body(
            &sample_article(),
            &[hl(1, "quotes \" & <tags> 🎉", "")],
        );
        assert_eq!(
            body["children"][1]["quote"]["rich_text"][0]["text"]["content"],
            "quotes \" & <tags> 🎉"
        );
    }

    #[test]
    fn notion_body_long_note_splits_into_capped_runs() {
        // A highlight note longer than Notion's 2000-char-per-run limit must
        // be split into multiple runs — a single oversized run is rejected
        // with a 400. Each run also stays italic.
        let long_note = "n".repeat(5000);
        let body = build_notion_body(&sample_article(), &[hl(1, "q", &long_note)]);
        let children = body["children"].as_array().unwrap();
        // heading + quote + note paragraph.
        assert_eq!(children.len(), 3);
        let runs = children[2]["paragraph"]["rich_text"].as_array().unwrap();
        // 5000 / 2000 → 3 runs (2000 + 2000 + 1000).
        assert_eq!(runs.len(), 3);
        for run in runs {
            let len = run["text"]["content"].as_str().unwrap().chars().count();
            assert!(len <= 2000, "run exceeds Notion's per-run limit: {len}");
            assert_eq!(run["annotations"]["italic"], true);
        }
        let total: usize = runs
            .iter()
            .map(|r| r["text"]["content"].as_str().unwrap().chars().count())
            .sum();
        assert_eq!(total, 5000);
    }

    #[test]
    fn notion_body_overlong_quote_splits_across_blocks() {
        // A highlight whose quote needs more than NOTION_MAX_RUNS (100) runs of
        // 2000 chars each must be split across several `quote` blocks — one
        // over-limit `rich_text` array makes Notion reject the whole PATCH with
        // a 400. (`notion_rich_text` only caps per-run length, not run count.)
        let huge = "q".repeat(NOTION_RUN_CHARS * NOTION_MAX_RUNS * 2 + 500);
        let body = build_notion_body(&sample_article(), &[hl(1, &huge, "")]);
        let children = body["children"].as_array().unwrap();
        let quotes: Vec<&Value> = children
            .iter()
            .filter(|c| c["type"] == "quote")
            .collect();
        assert!(quotes.len() >= 3, "the long quote should span several blocks");
        let mut total_runs = 0usize;
        for q in &quotes {
            let runs = q["quote"]["rich_text"].as_array().unwrap();
            assert!(runs.len() <= NOTION_MAX_RUNS, "quote block exceeds run cap");
            total_runs += runs.len();
        }
        // Every block stays within Notion's per-array run cap, and no quote
        // text was dropped in the split.
        assert_eq!(total_runs, huge.chars().count().div_ceil(NOTION_RUN_CHARS));
    }

    #[test]
    fn notion_body_overlong_note_splits_across_blocks() {
        // Same hazard for a lengthy highlight note: it must span several
        // italic `paragraph` blocks rather than one over-limit block.
        let huge = "n".repeat(NOTION_RUN_CHARS * NOTION_MAX_RUNS + 1234);
        let body = build_notion_body(&sample_article(), &[hl(1, "q", &huge)]);
        let children = body["children"].as_array().unwrap();
        let notes: Vec<&Value> = children
            .iter()
            .filter(|c| c["type"] == "paragraph")
            .collect();
        assert!(notes.len() >= 2, "the long note should span several blocks");
        for p in &notes {
            let runs = p["paragraph"]["rich_text"].as_array().unwrap();
            assert!(runs.len() <= NOTION_MAX_RUNS, "note block exceeds run cap");
            // Every run stays italic across the split.
            for run in runs {
                assert_eq!(run["annotations"]["italic"], true);
            }
        }
    }

    #[test]
    fn notion_rich_text_splits_long_runs() {
        let long = "x".repeat(5000);
        let runs = notion_rich_text(&long);
        // 5000 / 2000 → 3 runs (2000 + 2000 + 1000).
        assert_eq!(runs.len(), 3);
        let total: usize = runs
            .iter()
            .map(|r| r["text"]["content"].as_str().unwrap().chars().count())
            .sum();
        assert_eq!(total, 5000);
    }

    #[test]
    fn notion_body_empty_highlights_just_heading() {
        let body = build_notion_body(&sample_article(), &[]);
        assert_eq!(body["children"].as_array().unwrap().len(), 1);
    }

    // ── Notion — full-article page (F8) ──

    #[test]
    fn notion_page_shape() {
        let body = build_notion_page("PARENT", &sample_article(), "First line.\nSecond line.");
        assert_eq!(body["parent"]["page_id"], "PARENT");
        assert_eq!(
            body["properties"]["title"]["title"][0]["text"]["content"],
            "Rust in 2024"
        );
        let children = body["children"].as_array().unwrap();
        // bookmark + callout + 2 paragraphs.
        assert_eq!(children.len(), 4);
        assert_eq!(children[0]["type"], "bookmark");
        assert_eq!(children[0]["bookmark"]["url"], "https://example.com/rust");
        assert_eq!(children[1]["type"], "callout");
        assert_eq!(children[2]["type"], "paragraph");
        assert_eq!(
            children[2]["paragraph"]["rich_text"][0]["text"]["content"],
            "First line."
        );
        assert_eq!(
            children[3]["paragraph"]["rich_text"][0]["text"]["content"],
            "Second line."
        );
    }

    #[test]
    fn notion_page_callout_carries_metadata() {
        let body = build_notion_page("P", &sample_article(), "body");
        let callout = &body["children"][1]["callout"]["rich_text"][0]["text"]["content"];
        let text = callout.as_str().unwrap();
        assert!(text.contains("Source: Example Blog"));
        assert!(text.contains("By Jane Doe"));
        assert!(text.contains("2024-01-15"));
    }

    #[test]
    fn notion_page_omits_bookmark_when_no_url() {
        let a = ExportArticle {
            title: "Untitled".to_string(),
            url: None,
            author: None,
            feed_title: "Feed".to_string(),
            published_at: None,
        };
        let body = build_notion_page("P", &a, "only line");
        let children = body["children"].as_array().unwrap();
        // callout + 1 paragraph, no bookmark.
        assert_eq!(children.len(), 2);
        assert_eq!(children[0]["type"], "callout");
        // Callout has just the feed title — no "By"/date fragments.
        let meta = children[0]["callout"]["rich_text"][0]["text"]["content"]
            .as_str()
            .unwrap();
        assert_eq!(meta, "Source: Feed");
    }

    #[test]
    fn notion_page_empty_body_has_no_paragraphs() {
        let body = build_notion_page("P", &sample_article(), "   \n\n  ");
        let children = body["children"].as_array().unwrap();
        // Only bookmark + callout survive — blank lines are dropped.
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn notion_page_special_characters_preserved() {
        let mut a = sample_article();
        a.title = "C# \" & <tags> 🎉".to_string();
        let body = build_notion_page("P", &a, "quotes \" & <tags> 🎉");
        assert_eq!(
            body["properties"]["title"]["title"][0]["text"]["content"],
            "C# \" & <tags> 🎉"
        );
        // body_text drops the bookmark+callout, paragraph is index 2.
        assert_eq!(
            body["children"][2]["paragraph"]["rich_text"][0]["text"]["content"],
            "quotes \" & <tags> 🎉"
        );
    }

    #[test]
    fn notion_page_splits_long_lines() {
        let long = "x".repeat(5000);
        let body = build_notion_page("P", &sample_article(), &long);
        // bookmark + callout + 1 paragraph; that paragraph's rich_text has 3 runs.
        let runs = body["children"][2]["paragraph"]["rich_text"]
            .as_array()
            .unwrap();
        assert_eq!(runs.len(), 3);
    }

    #[test]
    fn notion_page_splits_overlong_line_across_blocks() {
        // `html_to_text` collapses a whole article into one newline-free line,
        // so a longread arrives here as a single line. A line long enough to
        // need more than NOTION_MAX_RUNS (100) runs of 2000 chars each must be
        // split across several paragraph blocks — one over-limit `rich_text`
        // array would make Notion reject the whole create request with a 400.
        let huge = "z".repeat(NOTION_RUN_CHARS * NOTION_MAX_RUNS * 2 + 1234);
        let children = notion_page_children(&sample_article(), &huge);
        // bookmark + callout + the body, which must span multiple paragraphs.
        let paragraphs: Vec<&Value> = children
            .iter()
            .filter(|c| c["type"] == "paragraph")
            .collect();
        assert!(paragraphs.len() >= 3, "expected the long line to be split");
        // Every paragraph stays within Notion's per-array run limit, and the
        // total run count covers the whole input.
        let mut total_runs = 0usize;
        for p in &paragraphs {
            let runs = p["paragraph"]["rich_text"].as_array().unwrap();
            assert!(runs.len() <= NOTION_MAX_RUNS, "paragraph exceeds run cap");
            total_runs += runs.len();
        }
        let expected = huge.chars().count().div_ceil(NOTION_RUN_CHARS);
        assert_eq!(total_runs, expected, "no body content was dropped");
    }

    #[test]
    fn notion_page_caps_children_at_notion_limit() {
        // 300 body lines → 300 paragraph blocks + bookmark + callout = 302
        // assembled blocks, but the create request must carry at most 100.
        let body_text: String = (0..300)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let body = build_notion_page("P", &sample_article(), &body_text);
        let children = body["children"].as_array().unwrap();
        assert_eq!(children.len(), NOTION_MAX_CHILDREN);
        // The full list (used for the follow-up appends) keeps every block.
        let all = notion_page_children(&sample_article(), &body_text);
        assert_eq!(all.len(), 302);
        // The embedded batch is exactly the prefix of the full list.
        assert_eq!(&all[..NOTION_MAX_CHILDREN], children.as_slice());
    }

    #[test]
    fn notion_body_caps_children_at_notion_limit_only_when_chunked() {
        // build_notion_body itself is unbounded (it is the pure builder); the
        // chunking happens in post_to_notion. Verify a >100-highlight list
        // produces >100 blocks here, and that 100-sized chunks cover them all.
        let highlights: Vec<Highlight> =
            (0..150).map(|i| hl(i, "q", "")).collect();
        let body = build_notion_body(&sample_article(), &highlights);
        let children = body["children"].as_array().unwrap();
        // heading + 150 quote blocks.
        assert_eq!(children.len(), 151);
        let batches: Vec<_> = children.chunks(NOTION_MAX_CHILDREN).collect();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), 100);
        assert_eq!(batches[1].len(), 51);
    }
}
