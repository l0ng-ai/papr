//! AI features: cloud LLM streaming for article summaries and RAG Q&A.
//! Provider-agnostic (Anthropic / OpenAI); a local backend can later implement
//! the same `stream_chat` contract.

use crate::error::{AppError, AppResult};
use reqwest::{Client, Response};
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;
use tauri::ipc::Channel;

/// Map a non-success HTTP response to an `AppError` naming the service and
/// carrying the response body; returns the response unchanged on success.
async fn ensure_success(resp: Response, service: &str) -> AppResult<Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let detail = resp.text().await.unwrap_or_default();
    Err(AppError::other(format!("{service} error {status}: {detail}")))
}

/// Per-request cap for AI streaming. The shared HTTP client carries the
/// feed-fetch timeout (~30s), which would truncate a long generation — so AI
/// requests override it with a generous bound.
const AI_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Output token cap for summaries / Q&A / digests, applied to every provider so
/// a response stays bounded in length and cost. These all fit comfortably within
/// it. Translation overrides it with [`TRANSLATE_MAX_TOKENS`].
pub const MAX_TOKENS: u32 = 1024;

/// Output token cap for one translation batch. A batch's translated HTML tracks
/// its input length (tags are echoed too), so it needs far more room than a
/// summary. Paired with [`TRANSLATE_CHUNK_BUDGET`] so a batch fits under it.
pub const TRANSLATE_MAX_TOKENS: u32 = 4096;

/// Input character budget for one translation batch, used by
/// `translate::chunk_blocks`. Chosen alongside [`TRANSLATE_MAX_TOKENS`] so the
/// translated output of a full batch stays under the output cap.
pub const TRANSLATE_CHUNK_BUDGET: usize = 3000;

/// Hard cap on the SSE line buffer. A well-behaved provider delimits every
/// event with a newline, so the buffer never holds more than a single frame.
/// A misbehaving or non-SSE endpoint (the base URL can point at any
/// OpenAI-compatible server, including a local one) could instead stream bytes
/// with no newline at all — without this cap that response would accumulate in
/// memory unbounded. 8 MiB is far larger than any genuine SSE frame while
/// still stopping a runaway stream, mirroring `fetch::MAX_BODY_BYTES`.
const MAX_SSE_BUFFER: usize = 8 * 1024 * 1024;

/// Token-level events streamed to the frontend over an `ipc::Channel`.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase", tag = "type", content = "data")]
pub enum AiEvent {
    Delta(String),
    Done,
    Error(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Provider {
    Anthropic,
    OpenAi,
    /// DeepSeek exposes an OpenAI-compatible API (same `/chat/completions`
    /// endpoint, request body and SSE shape), so it reuses the OpenAI request
    /// and parsing paths; only the default base URL and model differ.
    DeepSeek,
}

/// Resolved AI configuration read from the settings table.
pub struct AiConfig {
    provider: Provider,
    api_key: String,
    model: String,
    /// API root, without a trailing slash — the per-provider request path
    /// (`/messages`, `/chat/completions`) is appended to it. Defaults to the
    /// official endpoint; an override points at any compatible provider
    /// (OpenRouter, Groq, DeepSeek, a local server, …).
    base_url: String,
}

impl AiConfig {
    /// Build a config from raw settings, applying per-provider defaults.
    pub fn new(
        provider: Option<String>,
        api_key: Option<String>,
        model: Option<String>,
        base_url: Option<String>,
    ) -> AppResult<Self> {
        // Trim the key: it lands verbatim in an auth header (`x-api-key` /
        // `Authorization`). A pasted key routinely carries a trailing newline
        // or space from the clipboard — left in, that either trips reqwest's
        // header-value validation or earns a 401 from the provider. Trim like
        // `base_url` already does so the stored value is normalised at the one
        // chokepoint, independent of any frontend trimming.
        let api_key = api_key
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .ok_or_else(|| AppError::code("noAiKey"))?;
        let provider = match provider.as_deref() {
            Some("openai") => Provider::OpenAi,
            Some("deepseek") => Provider::DeepSeek,
            _ => Provider::Anthropic,
        };
        // Likewise trim the model name — it is JSON-serialised into the
        // request body, and a stray space/newline yields a "model not found".
        let model = model
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| match provider {
                Provider::Anthropic => "claude-sonnet-4-6".to_string(),
                Provider::OpenAi => "gpt-4.1-mini".to_string(),
                Provider::DeepSeek => "deepseek-chat".to_string(),
            });
        let base_url = base_url
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| provider.default_base_url().to_string());
        Ok(AiConfig {
            provider,
            api_key,
            model,
            base_url,
        })
    }
}

impl Provider {
    /// The official API root for this provider, used when the user has not
    /// set a custom base URL.
    fn default_base_url(self) -> &'static str {
        match self {
            Provider::Anthropic => "https://api.anthropic.com/v1",
            Provider::OpenAi => "https://api.openai.com/v1",
            // DeepSeek serves the OpenAI-compatible API at this root; our code
            // appends `/chat/completions`.
            Provider::DeepSeek => "https://api.deepseek.com",
        }
    }

    /// Whether this provider speaks the Anthropic or the OpenAI wire format
    /// (request body, endpoint path, and SSE shape). DeepSeek is
    /// OpenAI-compatible, so it shares OpenAI's request and parsing paths.
    fn is_openai_compatible(self) -> bool {
        matches!(self, Provider::OpenAi | Provider::DeepSeek)
    }
}

/// The result of a streamed chat completion.
pub struct ChatOutcome {
    /// The accumulated response text.
    pub text: String,
    /// Whether the stream ran to completion. `false` when the frontend dropped
    /// the channel mid-stream (the user closed the AI panel) — the text is then
    /// a truncated fragment that callers must not persist as a finished result.
    pub completed: bool,
}

/// Stream a single-turn chat completion, forwarding each token to `channel`.
/// Returns the accumulated response text and whether the stream completed.
pub async fn stream_chat(
    client: &Client,
    cfg: &AiConfig,
    system: &str,
    user: &str,
    channel: &Channel<AiEvent>,
    max_tokens: u32,
) -> AppResult<ChatOutcome> {
    let result = if cfg.provider.is_openai_compatible() {
        stream_openai(client, cfg, system, user, Some(channel), max_tokens).await
    } else {
        stream_anthropic(client, cfg, system, user, Some(channel), max_tokens).await
    };
    match &result {
        Ok(_) => {
            let _ = channel.send(AiEvent::Done);
        }
        Err(e) => {
            let _ = channel.send(AiEvent::Error(e.to_string()));
        }
    }
    result
}

/// Run a completion to the end and return its full text WITHOUT forwarding
/// per-token deltas to the frontend. Translation uses this and reports progress
/// once per batch instead of once per token — token-level IPC over a full
/// article would flood the webview's main thread and freeze the UI.
pub async fn complete_chat(
    client: &Client,
    cfg: &AiConfig,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> AppResult<String> {
    let outcome = if cfg.provider.is_openai_compatible() {
        stream_openai(client, cfg, system, user, None, max_tokens).await
    } else {
        stream_anthropic(client, cfg, system, user, None, max_tokens).await
    }?;
    Ok(outcome.text)
}

async fn stream_anthropic(
    client: &Client,
    cfg: &AiConfig,
    system: &str,
    user: &str,
    channel: Option<&Channel<AiEvent>>,
    max_tokens: u32,
) -> AppResult<ChatOutcome> {
    let body = json!({
        "model": cfg.model,
        "max_tokens": max_tokens,
        "system": system,
        "stream": true,
        "messages": [{ "role": "user", "content": user }],
    });
    let resp = client
        .post(format!("{}/messages", cfg.base_url))
        .header("x-api-key", &cfg.api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .timeout(AI_REQUEST_TIMEOUT)
        .json(&body)
        .send()
        .await?;
    consume_sse(resp, channel, Provider::Anthropic).await
}

async fn stream_openai(
    client: &Client,
    cfg: &AiConfig,
    system: &str,
    user: &str,
    channel: Option<&Channel<AiEvent>>,
    max_tokens: u32,
) -> AppResult<ChatOutcome> {
    let body = json!({
        "model": cfg.model,
        "max_tokens": max_tokens,
        "stream": true,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
    });
    let resp = client
        .post(format!("{}/chat/completions", cfg.base_url))
        .bearer_auth(&cfg.api_key)
        .timeout(AI_REQUEST_TIMEOUT)
        .json(&body)
        .send()
        .await?;
    consume_sse(resp, channel, Provider::OpenAi).await
}

/// What to do after handling one SSE line.
#[derive(Debug)]
enum LineOutcome {
    /// Line handled; keep consuming the stream.
    Continue,
    /// The frontend dropped the channel — stop and report the result as
    /// interrupted so the caller does not persist a truncated fragment.
    ChannelClosed,
}

/// Process a single SSE line: pull the `data:` payload, surface any provider
/// error, and forward a text delta to `channel` (appending it to `full`).
fn handle_sse_line(
    line: &str,
    provider: Provider,
    full: &mut String,
    channel: Option<&Channel<AiEvent>>,
) -> AppResult<LineOutcome> {
    let Some(data) = line.trim().strip_prefix("data:") else {
        return Ok(LineOutcome::Continue);
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(LineOutcome::Continue);
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return Ok(LineOutcome::Continue);
    };
    // Both providers can deliver an error mid-stream after a 200 OK
    // (rate limit, overload, content filter). Surface it instead of
    // ending the generation silently with a truncated summary.
    if let Some(msg) = extract_error(&value, provider) {
        return Err(AppError::other(format!("AI stream error: {msg}")));
    }
    if let Some(text) = extract_delta(&value, provider) {
        full.push_str(&text);
        // When a token channel is present, a send failure means the frontend
        // dropped it (the user closed the AI panel). Stop streaming instead of
        // downloading the rest of the response into a void. The silent path
        // (translation) passes `None` and simply accumulates into `full`.
        if let Some(ch) = channel {
            if ch.send(AiEvent::Delta(text)).is_err() {
                log::debug!("AI stream channel closed; aborting early");
                return Ok(LineOutcome::ChannelClosed);
            }
        }
    }
    Ok(LineOutcome::Continue)
}

/// Drive the Server-Sent-Events response, extracting text deltas per provider.
async fn consume_sse(
    resp: reqwest::Response,
    channel: Option<&Channel<AiEvent>>,
    provider: Provider,
) -> AppResult<ChatOutcome> {
    let mut resp = ensure_success(resp, "AI API").await?;

    let mut buf: Vec<u8> = Vec::new();
    let mut full = String::new();

    while let Some(chunk) = resp.chunk().await? {
        buf.extend_from_slice(&chunk);
        // Guard against a provider that never delimits its frames: a buffer
        // this large is not a genuine SSE event, so fail instead of growing
        // memory without bound.
        if buf.len() > MAX_SSE_BUFFER {
            return Err(AppError::other(
                "AI stream error: response is not server-sent events",
            ));
        }
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw);
            match handle_sse_line(&line, provider, &mut full, channel)? {
                LineOutcome::Continue => {}
                LineOutcome::ChannelClosed => {
                    return Ok(ChatOutcome { text: full, completed: false });
                }
            }
        }
    }
    // The stream ended. A standards-compliant provider newline-terminates
    // every frame, but a custom OpenAI-compatible endpoint (a local LLM
    // server, which the base-URL override explicitly allows) may close the
    // connection right after the final `data:` line with no trailing newline.
    // Without this, that last frame — carrying the closing token(s) of the
    // response — would be left unprocessed in `buf` and silently dropped.
    if !buf.is_empty() {
        let line = String::from_utf8_lossy(&buf);
        match handle_sse_line(&line, provider, &mut full, channel)? {
            LineOutcome::Continue => {}
            LineOutcome::ChannelClosed => {
                return Ok(ChatOutcome { text: full, completed: false });
            }
        }
    }
    Ok(ChatOutcome { text: full, completed: true })
}

/// Detect a provider error object carried inside an SSE data frame.
///
/// For the OpenAI-compatible path the error must be a non-null object: many
/// compatible servers (OpenRouter and others) include a literal `"error": null`
/// alongside `choices` in their *successful* chunks. Treating that as a fault
/// would abort an otherwise-fine generation with a bogus "stream error".
fn extract_error(v: &Value, provider: Provider) -> Option<String> {
    let err = match provider {
        Provider::Anthropic => (v["type"] == "error").then(|| &v["error"]),
        // DeepSeek is OpenAI-compatible; in practice it always reaches this
        // function tagged as `OpenAi` (see `stream_openai`), but handle it
        // identically so the match stays exhaustive.
        Provider::OpenAi | Provider::DeepSeek => v.get("error").filter(|e| e.is_object()),
    }?;
    Some(
        err["message"]
            .as_str()
            .filter(|m| !m.is_empty())
            .unwrap_or("stream error")
            .to_string(),
    )
}

fn extract_delta(v: &Value, provider: Provider) -> Option<String> {
    match provider {
        Provider::Anthropic => {
            if v["type"] == "content_block_delta" {
                v["delta"]["text"].as_str().map(String::from)
            } else {
                None
            }
        }
        // DeepSeek shares the OpenAI SSE shape; see the note in `extract_error`.
        Provider::OpenAi | Provider::DeepSeek => v["choices"][0]["delta"]["content"]
            .as_str()
            .map(String::from),
    }
}

#[cfg(test)]
mod tests {
    use super::{extract_delta, extract_error, Provider};
    use serde_json::json;

    #[test]
    fn openai_null_error_field_is_not_an_error() {
        // OpenRouter and other OpenAI-compatible servers ship `"error": null`
        // inside ordinary successful chunks — it must not abort the stream.
        let chunk = json!({
            "choices": [{ "delta": { "content": "hello" } }],
            "error": null,
        });
        assert_eq!(extract_error(&chunk, Provider::OpenAi), None);
        assert_eq!(
            extract_delta(&chunk, Provider::OpenAi).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn openai_real_error_object_is_surfaced() {
        let chunk = json!({ "error": { "message": "rate limit exceeded" } });
        assert_eq!(
            extract_error(&chunk, Provider::OpenAi).as_deref(),
            Some("rate limit exceeded")
        );
    }

    #[test]
    fn openai_error_object_without_message_falls_back() {
        let chunk = json!({ "error": { "code": 500 } });
        assert_eq!(
            extract_error(&chunk, Provider::OpenAi).as_deref(),
            Some("stream error")
        );
    }

    #[test]
    fn openai_plain_delta_chunk_has_no_error() {
        let chunk = json!({ "choices": [{ "delta": { "content": "x" } }] });
        assert_eq!(extract_error(&chunk, Provider::OpenAi), None);
    }

    #[test]
    fn anthropic_error_event_is_surfaced() {
        let chunk = json!({ "type": "error", "error": { "message": "overloaded" } });
        assert_eq!(
            extract_error(&chunk, Provider::Anthropic).as_deref(),
            Some("overloaded")
        );
    }

    #[test]
    fn anthropic_content_delta_is_not_an_error() {
        let chunk = json!({
            "type": "content_block_delta",
            "delta": { "type": "text_delta", "text": "world" },
        });
        assert_eq!(extract_error(&chunk, Provider::Anthropic), None);
        assert_eq!(
            extract_delta(&chunk, Provider::Anthropic).as_deref(),
            Some("world")
        );
    }

    // --- handle_sse_line: per-line parsing, including the final unterminated
    //     frame a non-compliant endpoint may close the stream on. ---

    use super::{handle_sse_line, AiEvent, LineOutcome};
    use std::sync::{Arc, Mutex};
    use tauri::ipc::{Channel, InvokeResponseBody};

    /// A `Channel<AiEvent>` whose every sent delta is recorded into the
    /// returned buffer — lets the line parser be exercised without a webview.
    fn recording_channel() -> (Channel<AiEvent>, Arc<Mutex<Vec<String>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = received.clone();
        let channel = Channel::new(move |body: InvokeResponseBody| {
            let json = match body {
                InvokeResponseBody::Json(s) => s,
                InvokeResponseBody::Raw(b) => String::from_utf8_lossy(&b).into_owned(),
            };
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            if v["type"] == "delta" {
                sink.lock().unwrap().push(v["data"].as_str().unwrap().to_string());
            }
            Ok(())
        });
        (channel, received)
    }

    #[test]
    fn sse_line_forwards_a_delta() {
        let (channel, got) = recording_channel();
        let mut full = String::new();
        let line = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n";
        let out = handle_sse_line(line, Provider::OpenAi, &mut full, Some(&channel)).unwrap();
        assert!(matches!(out, LineOutcome::Continue));
        assert_eq!(full, "hi");
        assert_eq!(*got.lock().unwrap(), vec!["hi"]);
    }

    #[test]
    fn sse_line_ignores_non_data_and_done_lines() {
        let (channel, got) = recording_channel();
        let mut full = String::new();
        for line in [": keep-alive comment\n", "data: [DONE]\n", "\n"] {
            handle_sse_line(line, Provider::OpenAi, &mut full, Some(&channel)).unwrap();
        }
        assert!(full.is_empty());
        assert!(got.lock().unwrap().is_empty());
    }

    #[test]
    fn sse_line_surfaces_a_mid_stream_error() {
        let (channel, _got) = recording_channel();
        let mut full = String::new();
        let line = "data: {\"error\":{\"message\":\"rate limited\"}}\n";
        let err = handle_sse_line(line, Provider::OpenAi, &mut full, Some(&channel)).unwrap_err();
        assert!(err.to_string().contains("rate limited"));
    }

    #[test]
    fn sse_final_frame_without_trailing_newline_is_not_dropped() {
        // A custom OpenAI-compatible endpoint may close the connection right
        // after the last `data:` line with no trailing `\n`. The closing
        // token must still be parsed — `handle_sse_line` is fed the leftover
        // buffer verbatim, exactly as `consume_sse` does after the read loop.
        let (channel, got) = recording_channel();
        let mut full = String::new();
        let last = "data: {\"choices\":[{\"delta\":{\"content\":\"!\"}}]}";
        handle_sse_line(last, Provider::OpenAi, &mut full, Some(&channel)).unwrap();
        assert_eq!(full, "!");
        assert_eq!(*got.lock().unwrap(), vec!["!"]);
    }

    // --- AiConfig::new: normalising pasted credentials. ---

    use super::AiConfig;

    #[test]
    fn config_trims_whitespace_off_a_pasted_api_key() {
        // A key copied from a webpage commonly carries a trailing newline /
        // spaces; left in, it breaks the auth header.
        let cfg = AiConfig::new(
            Some("openai".into()),
            Some("  sk-abc123\n".into()),
            None,
            None,
        )
        .expect("a key with surrounding whitespace is still a usable key");
        assert_eq!(cfg.api_key, "sk-abc123");
    }

    #[test]
    fn config_trims_whitespace_off_a_pasted_model_name() {
        let cfg = AiConfig::new(
            Some("anthropic".into()),
            Some("sk-key".into()),
            Some(" claude-sonnet-4-6 ".into()),
            None,
        )
        .unwrap();
        assert_eq!(cfg.model, "claude-sonnet-4-6");
    }

    #[test]
    fn config_rejects_a_whitespace_only_api_key() {
        // Trimmed to empty — treated as "no key set", not a usable credential.
        // `AiConfig` deliberately holds no `Debug` impl (it carries a secret),
        // so match the result rather than `unwrap_err`.
        match AiConfig::new(Some("openai".into()), Some("   \n".into()), None, None) {
            Ok(_) => panic!("a whitespace-only key must not be accepted"),
            Err(e) => assert!(e.to_string().contains("noAiKey")),
        }
    }

    #[test]
    fn deepseek_provider_uses_its_own_defaults_and_openai_wire_format() {
        // DeepSeek has no model or base URL set, so it must fall back to its
        // own defaults — not OpenAI's — while still speaking the OpenAI wire
        // format (it is OpenAI-compatible).
        let cfg = AiConfig::new(Some("deepseek".into()), Some("sk-key".into()), None, None)
            .unwrap();
        assert_eq!(cfg.model, "deepseek-chat");
        assert_eq!(cfg.base_url, "https://api.deepseek.com");
        assert!(cfg.provider.is_openai_compatible());
    }

    #[test]
    fn deepseek_honours_a_custom_base_url() {
        let cfg = AiConfig::new(
            Some("deepseek".into()),
            Some("sk-key".into()),
            Some("deepseek-reasoner".into()),
            Some("https://proxy.example.com/v1/".into()),
        )
        .unwrap();
        assert_eq!(cfg.model, "deepseek-reasoner");
        // Trailing slash trimmed, as for every provider.
        assert_eq!(cfg.base_url, "https://proxy.example.com/v1");
    }

    #[test]
    fn config_falls_back_to_the_default_model_for_a_blank_one() {
        // A whitespace-only model name trims to empty and must yield the
        // provider default, not an empty string in the request body.
        let cfg = AiConfig::new(
            Some("openai".into()),
            Some("sk-key".into()),
            Some("  ".into()),
            None,
        )
        .unwrap();
        assert_eq!(cfg.model, "gpt-4.1-mini");
    }
}
