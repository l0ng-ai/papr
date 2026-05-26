//! Optional LLM translation engine. The dedicated machine-translation providers
//! in `translate_api` are the default, but a user can instead translate articles
//! with the same cloud LLM that powers summaries — slower and pricier, but
//! context-aware and often higher quality. This module is the LLM counterpart to
//! `translate_api`: same `translate_html` shape (one HTML batch in, translated
//! HTML out), so `commands::translate_article` can pick an engine and otherwise
//! treat them alike.
//!
//! Where a dedicated MT API preserves markup natively, the LLM must be *asked*
//! to — and coaxed out of its habit of wrapping output in a code fence — so the
//! prompt building and fence stripping live here, with the pure helpers tested.

use crate::ai::{self, AiConfig};
use crate::error::AppResult;
use reqwest::Client;

/// The human-readable English name for an app language code, used in the
/// translation instruction. Covers the codes offered in the UI
/// (`src/translateLanguages.ts`); anything unrecognised falls back to English.
pub fn language_name(code: &str) -> &'static str {
    match code {
        "ar" => "Arabic",
        "bg" => "Bulgarian",
        "cs" => "Czech",
        "da" => "Danish",
        "de" => "German",
        "el" => "Greek",
        "es" => "Spanish",
        "et" => "Estonian",
        "fi" => "Finnish",
        "fr" => "French",
        "he" => "Hebrew",
        "hi" => "Hindi",
        "hu" => "Hungarian",
        "id" => "Indonesian",
        "it" => "Italian",
        "ja" => "Japanese",
        "ko" => "Korean",
        "lt" => "Lithuanian",
        "lv" => "Latvian",
        "nl" => "Dutch",
        "no" => "Norwegian",
        "pl" => "Polish",
        "pt" => "Portuguese",
        "pt-BR" => "Brazilian Portuguese",
        "ro" => "Romanian",
        "ru" => "Russian",
        "sk" => "Slovak",
        "sl" => "Slovenian",
        "sv" => "Swedish",
        "th" => "Thai",
        "tr" => "Turkish",
        "uk" => "Ukrainian",
        "vi" => "Vietnamese",
        "zh-Hans" => "Simplified Chinese",
        "zh-Hant" => "Traditional Chinese",
        _ => "English",
    }
}

/// Build the system prompt instructing the model to translate one batch of HTML
/// into `target` (a language name) while leaving the markup intact.
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

/// Translate one HTML fragment into `app_code` using the LLM, returning the
/// translated HTML with any wrapping code fence removed. Mirrors
/// `translate_api::translate_html` so the two engines are interchangeable.
pub async fn translate_html(
    client: &Client,
    cfg: &AiConfig,
    html: &str,
    app_code: &str,
) -> AppResult<String> {
    let system = translate_system_prompt(language_name(app_code));
    let text = ai::complete_chat(client, cfg, &system, html, ai::TRANSLATE_MAX_TOKENS).await?;
    Ok(strip_code_fence(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_name_maps_codes_with_english_fallback() {
        assert_eq!(language_name("zh-Hans"), "Simplified Chinese");
        assert_eq!(language_name("zh-Hant"), "Traditional Chinese");
        assert_eq!(language_name("ja"), "Japanese");
        assert_eq!(language_name("pt-BR"), "Brazilian Portuguese");
        assert_eq!(language_name("en"), "English");
        assert_eq!(language_name("xx"), "English");
    }

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
        assert!(
            lower.contains("preserve") || lower.contains("keep"),
            "no preserve directive: {p}"
        );
    }

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
}
