# Article Translation — Design

## Summary

Add a full-body article translation feature to the Papr reader. The user toggles
between the original article and a translated version, rendered inline in the
same single-column reader. Translation reuses the existing bring-your-own-key LLM
pipeline (`ai::stream_chat`) and caches the result on the article row. The target
language is a dedicated setting, independent of the UI language.

## Decisions (locked)

| Question | Decision |
| --- | --- |
| What is translated | The full article body |
| How it is shown | A toggle: render original **or** translation inline (single column) |
| Target language | A dedicated setting picker, independent of UI language |
| Engine | Reuse the existing LLM (`ai::stream_chat`, BYO key) |
| Body strategy | Block-wise chunked translation that preserves HTML structure and images |

## Architecture

The feature mirrors two patterns already in the codebase, so it adds no new
concepts:

1. **Backend mirrors `ai_summarize`** (`commands.rs`): build a system + user
   prompt, stream tokens over an `ipc::Channel<AiEvent>`, and persist the result
   on the article row only when the stream completes.
2. **Frontend mirrors the existing `showExtracted` toggle** (`Reader.tsx:389`):
   the reader already swaps which HTML it renders (`extracted_html` vs
   `content_html`). The translation toggle is a sibling that adds a third source.

## Data model

New migration (append-only `M` entry in `db.rs`, never edit a shipped one):

```sql
ALTER TABLE articles ADD COLUMN translated_html TEXT;
ALTER TABLE articles ADD COLUMN translated_lang TEXT;
```

- `translated_html` — the cached, sanitized translated body HTML.
- `translated_lang` — the target language the cache was produced for (e.g. `en`,
  `zh`, `ja`).

Cache validity is decided at read time by comparing `translated_lang` against the
current `translate_target_lang` setting. A mismatch (the user changed the target
language) is treated as a cache miss and triggers re-translation. No bulk
invalidation is performed.

Persistence rule is copied from `set_ai_summary`: write the cache **only** when
the stream completed and the text is non-empty, so a translation interrupted
mid-stream is never cached as a truncated fragment.

## Backend

### New module `translate.rs`

Pure, testable HTML chunking — no I/O:

- Source HTML is `extracted_html` if present and non-empty, else `content_html`
  (the richest available body).
- Parse with `scraper` (already a dependency) and split into top-level
  block-level nodes, reusing the existing `BLOCK_TAGS` list from `sanitize.rs`.
- Greedily group consecutive blocks into batches under a character budget
  (~3000 chars) so each batch is one LLM request and long articles are handled
  by issuing multiple batches.
- Image nodes (and other non-text markup) are carried through untouched.

### New command `ai_translate(article_id, on_token)`

- Loads the source HTML, the resolved `AiConfig` (`load_ai_config`), and the
  target language from the `translate_target_lang` setting (default: follow UI
  language when unset).
- For each batch, calls `ai::stream_chat` with a system prompt:
  > Translate to `<target language>`. Preserve every HTML tag, attribute, and the
  > overall structure exactly. Translate only human-readable text. Do not
  > translate code, or URLs.
- Streams `AiEvent` deltas to the frontend so the user sees progress.
- On completion, concatenates the batches, runs the result through
  `sanitize::sanitize`, and persists via a new `db::set_translation(conn, id,
  html, lang)`.
- Reuses existing error codes: `noArticleBody` (empty body) and `noAiKey`
  (no API key configured).
- Registered in `lib.rs`'s `invoke_handler`.

### Token cap

Translation output length tracks input length, so the shared `MAX_TOKENS = 1024`
cap in `ai.rs` would truncate each batch. Translation must use a larger
per-request output cap (a dedicated constant, or a parameter threaded into
`stream_chat`). The chunk budget and the output cap are chosen together so a
batch's translation fits comfortably under the cap.

### `db.rs` changes

- `set_translation(conn, id, html, lang)` — `UPDATE articles SET
  translated_html = ?, translated_lang = ? WHERE id = ?`.
- Include `translated_html` and `translated_lang` in the `get_article` query so
  they reach `ArticleDetail`.

## Settings

- New setting key `translate_target_lang` (values mirror the UI language codes:
  `en` / `zh` / `ja` / …).
- `SettingsDialog` gains a dropdown to choose it.
- When unset, it defaults to the current UI language (reusing the mapping that
  `response_language` already encodes).

## Frontend

- `api.ts`: add `aiTranslate(articleId, onToken)`, mirroring `aiSummarize`'s
  `Channel<AiEvent>` wiring.
- `types.ts`: `ArticleDetail` gains `translatedHtml` and `translatedLang`.
- `Reader.tsx`:
  - Add a translation toggle button next to the existing full-text toggle.
  - State `showTranslation`. When toggled on:
    - If `translatedHtml` exists and `translatedLang` matches the target
      setting, render it directly.
    - Otherwise call `aiTranslate`, show a "translating…" state, and render the
      result on completion.
  - Body selection becomes:
    `showTranslation ? translatedHtml : (showExtracted && extractedHtml ?
    extractedHtml : contentHtml)`.
- Locales (`en`, `ja`, `zh`): strings for the toggle button, the settings label,
  and the translating/error states.

## Streaming UX

Stream deltas to drive a "translating…" indicator and progressive feedback.
Render the final sanitized HTML on completion. The cache is written only on
completion (matching the summary's "don't cache a truncated generation" rule).

## Error handling & edge cases

- Empty / title-only body → `noArticleBody` (existing code).
- No AI key configured → `noAiKey` (existing code).
- Source already in the target language → still sent through translation; the
  model largely passes it through. Acceptable; no detection (YAGNI).
- Very long article → handled by batching; more batches cost more, but the
  action is user-initiated.

## Testing

Rust unit tests, aligned with the existing `db.rs` test style:

- The block chunker: groups blocks under the character budget, never splits a
  block across batches, and carries non-text nodes through.
- Prompt construction for a batch.
- `set_translation` + `get_article` round-trip.
- Cache-validity logic: `translated_lang` mismatch is treated as a miss.

## Out of scope (YAGNI)

- Side-by-side / interleaved bilingual layouts.
- A dedicated machine-translation API (DeepL / Google).
- Bulk pre-translation of feeds.
- Skipping translation when the source is detected to already be in the target
  language.
