//! AI features: cloud LLM streaming for article summaries and RAG Q&A.
//! Provider-agnostic (Anthropic / OpenAI); a local backend can later implement
//! the same `stream_chat` contract.

use crate::error::{AppError, AppResult};
use reqwest::Client;
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;
use tauri::ipc::Channel;

/// Per-request cap for AI streaming. The shared HTTP client carries the
/// feed-fetch timeout (~30s), which would truncate a long generation — so AI
/// requests override it with a generous bound.
const AI_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Output token cap, applied to every provider so a response stays bounded in
/// length and cost. Summaries / Q&A / digests all fit comfortably within it.
const MAX_TOKENS: u32 = 1024;

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
        let api_key = api_key
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| AppError::code("noAiKey"))?;
        let provider = match provider.as_deref() {
            Some("openai") => Provider::OpenAi,
            _ => Provider::Anthropic,
        };
        let model = model.filter(|m| !m.trim().is_empty()).unwrap_or_else(|| {
            match provider {
                Provider::Anthropic => "claude-sonnet-4-6".to_string(),
                Provider::OpenAi => "gpt-4.1-mini".to_string(),
            }
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
        }
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
) -> AppResult<ChatOutcome> {
    let result = match cfg.provider {
        Provider::Anthropic => stream_anthropic(client, cfg, system, user, channel).await,
        Provider::OpenAi => stream_openai(client, cfg, system, user, channel).await,
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

async fn stream_anthropic(
    client: &Client,
    cfg: &AiConfig,
    system: &str,
    user: &str,
    channel: &Channel<AiEvent>,
) -> AppResult<ChatOutcome> {
    let body = json!({
        "model": cfg.model,
        "max_tokens": MAX_TOKENS,
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
    channel: &Channel<AiEvent>,
) -> AppResult<ChatOutcome> {
    let body = json!({
        "model": cfg.model,
        "max_tokens": MAX_TOKENS,
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

/// Drive the Server-Sent-Events response, extracting text deltas per provider.
async fn consume_sse(
    mut resp: reqwest::Response,
    channel: &Channel<AiEvent>,
    provider: Provider,
) -> AppResult<ChatOutcome> {
    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        return Err(AppError::other(format!("AI API error {status}: {detail}")));
    }

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
            let Some(data) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            // Both providers can deliver an error mid-stream after a 200 OK
            // (rate limit, overload, content filter). Surface it instead of
            // ending the generation silently with a truncated summary.
            if let Some(msg) = extract_error(&value, provider) {
                return Err(AppError::other(format!("AI stream error: {msg}")));
            }
            if let Some(text) = extract_delta(&value, provider) {
                full.push_str(&text);
                // A send failure means the frontend dropped the channel (the
                // user closed the AI panel). Stop streaming instead of
                // downloading the rest of the response into a void — and flag
                // the result as interrupted so the caller does not persist a
                // truncated fragment as a finished summary.
                if channel.send(AiEvent::Delta(text)).is_err() {
                    log::debug!("AI stream channel closed; aborting early");
                    return Ok(ChatOutcome { text: full, completed: false });
                }
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
        Provider::OpenAi => v.get("error").filter(|e| e.is_object()),
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
        Provider::OpenAi => v["choices"][0]["delta"]["content"]
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
}
