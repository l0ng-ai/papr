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
}

impl AiConfig {
    /// Build a config from raw settings, applying per-provider model defaults.
    pub fn new(provider: Option<String>, api_key: Option<String>, model: Option<String>) -> AppResult<Self> {
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
        Ok(AiConfig {
            provider,
            api_key,
            model,
        })
    }
}

/// Stream a single-turn chat completion, forwarding each token to `channel`.
/// Returns the fully accumulated response text.
pub async fn stream_chat(
    client: &Client,
    cfg: &AiConfig,
    system: &str,
    user: &str,
    channel: &Channel<AiEvent>,
) -> AppResult<String> {
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
) -> AppResult<String> {
    let body = json!({
        "model": cfg.model,
        "max_tokens": 1024,
        "system": system,
        "stream": true,
        "messages": [{ "role": "user", "content": user }],
    });
    let resp = client
        .post("https://api.anthropic.com/v1/messages")
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
) -> AppResult<String> {
    let body = json!({
        "model": cfg.model,
        "stream": true,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
    });
    let resp = client
        .post("https://api.openai.com/v1/chat/completions")
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
) -> AppResult<String> {
    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        return Err(AppError::other(format!("AI API error {status}: {detail}")));
    }

    let mut buf: Vec<u8> = Vec::new();
    let mut full = String::new();

    while let Some(chunk) = resp.chunk().await? {
        buf.extend_from_slice(&chunk);
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
                // downloading the rest of the response into a void.
                if channel.send(AiEvent::Delta(text)).is_err() {
                    log::debug!("AI stream channel closed; aborting early");
                    return Ok(full);
                }
            }
        }
    }
    Ok(full)
}

/// Detect a provider error object carried inside an SSE data frame.
fn extract_error(v: &Value, provider: Provider) -> Option<String> {
    let err = match provider {
        Provider::Anthropic => (v["type"] == "error").then(|| &v["error"]),
        Provider::OpenAi => v.get("error"),
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
