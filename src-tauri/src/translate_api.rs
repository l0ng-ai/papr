//! Article translation via a dedicated machine-translation API. Unlike the LLM
//! path this replaced, these providers translate an HTML fragment natively
//! (preserving tags) and return it in one round-trip per batch — no per-token
//! streaming, no "preserve the markup" prompt to coax. Three providers are
//! supported, each with the user's own API key: DeepL, Google Cloud Translation,
//! and any LibreTranslate-compatible endpoint.
//!
//! The pure helpers here — config normalisation, language-code mapping, request
//! body building, response parsing — carry the logic worth testing; the batching
//! and persistence glue lives in `commands::translate_article`.

use crate::error::{AppError, AppResult};
use reqwest::{Client, Response};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Per-request cap for a translation batch. MT calls are quick, but a large
/// batch over a slow link (or a self-hosted endpoint) needs more than the
/// shared client's ~30s feed-fetch timeout.
const TRANSLATE_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Input character budget for one translation batch, used by
/// `translate::chunk_blocks`. Dedicated MT APIs accept far larger requests than
/// the per-token LLM path did (Google caps a single `q` at 30k code points;
/// DeepL/LibreTranslate accept large bodies), so batches are coarser than the
/// old LLM budget — fewer round-trips, still well under every provider's limit.
pub const TRANSLATE_CHUNK_BUDGET: usize = 8000;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TranslateProvider {
    DeepL,
    Google,
    /// A LibreTranslate-compatible endpoint (`POST /translate`).
    Compatible,
    /// Youdao (网易有道) web-page translation. Signature-authed and
    /// form-encoded rather than the JSON+key shape the others share.
    Youdao,
}

impl TranslateProvider {
    fn parse(s: Option<&str>) -> Self {
        match s {
            Some("google") => TranslateProvider::Google,
            Some("compatible") | Some("libretranslate") => TranslateProvider::Compatible,
            Some("youdao") => TranslateProvider::Youdao,
            _ => TranslateProvider::DeepL,
        }
    }
}

/// Resolved translation configuration read from the settings table.
pub struct TranslateConfig {
    provider: TranslateProvider,
    api_key: String,
    /// Second credential for signature-authed providers (Youdao's appSecret).
    /// Empty for the simple-key providers, which don't use it.
    api_secret: String,
    /// API root without a trailing slash; the per-provider path is appended.
    base_url: String,
}

impl TranslateConfig {
    /// Build a config from raw settings, applying per-provider defaults.
    ///
    /// The key is trimmed (a pasted credential routinely carries a trailing
    /// newline that breaks the auth header) and required for DeepL/Google; a
    /// LibreTranslate-compatible endpoint may be keyless, so an empty key is
    /// accepted there. Youdao additionally requires the `api_secret` (its
    /// appSecret), used only to sign the request. A compatible provider has no
    /// official endpoint, so its base URL is mandatory.
    pub fn new(
        provider: Option<String>,
        api_key: Option<String>,
        api_secret: Option<String>,
        base_url: Option<String>,
    ) -> AppResult<Self> {
        let provider = TranslateProvider::parse(provider.as_deref());
        let api_key = api_key
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty());
        let api_key = match provider {
            TranslateProvider::Compatible => api_key.unwrap_or_default(),
            _ => api_key.ok_or_else(|| AppError::code("noTranslateKey"))?,
        };
        let api_secret = api_secret
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_default();
        // Youdao signs every request, so its appSecret is as essential as the key.
        if provider == TranslateProvider::Youdao && api_secret.is_empty() {
            return Err(AppError::code("noTranslateSecret"));
        }
        let base_url = base_url
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty());
        let base_url = match base_url {
            Some(u) => u,
            None => default_base_url(provider, &api_key)
                .ok_or_else(|| AppError::code("noTranslateEndpoint"))?
                .to_string(),
        };
        Ok(TranslateConfig {
            provider,
            api_key,
            api_secret,
            base_url,
        })
    }
}

/// The official API root for a provider, used when no custom base URL is set.
/// DeepL splits free vs paid endpoints by the key suffix (`:fx` = free), so the
/// right host is chosen from the key. A compatible endpoint has no default — the
/// caller must supply one — so this returns `None`.
fn default_base_url(provider: TranslateProvider, api_key: &str) -> Option<&'static str> {
    match provider {
        TranslateProvider::DeepL => Some(if api_key.ends_with(":fx") {
            "https://api-free.deepl.com"
        } else {
            "https://api.deepl.com"
        }),
        TranslateProvider::Google => Some("https://translation.googleapis.com"),
        TranslateProvider::Youdao => Some("https://openapi.youdao.com"),
        TranslateProvider::Compatible => None,
    }
}

/// Map the app's canonical language code (BCP-47-ish, as stored in
/// `translate_target_lang` and listed in the UI) to the code a given provider
/// expects. Providers disagree on casing and region tags: DeepL wants uppercase
/// with explicit English/Portuguese/Chinese variants, Google wants BCP-47, and
/// LibreTranslate wants a bare ISO-639-1 code. Unlisted codes fall back to the
/// language subtag in the provider's customary form, which is correct for the
/// many languages that need no special-casing (de, fr, ja, ko, …).
pub fn provider_lang_code(provider: TranslateProvider, app_code: &str) -> String {
    // The language subtag, e.g. "pt" from "pt-BR" or "zh" from "zh-Hans".
    let base = app_code.split('-').next().unwrap_or(app_code);
    match provider {
        TranslateProvider::DeepL => match app_code {
            "en" => "EN-US".into(),
            "pt" => "PT-PT".into(),
            "pt-BR" => "PT-BR".into(),
            "no" => "NB".into(),
            "zh-Hans" => "ZH".into(),
            "zh-Hant" => "ZH-HANT".into(),
            _ => base.to_uppercase(),
        },
        TranslateProvider::Google => match app_code {
            "zh-Hans" => "zh-CN".into(),
            "zh-Hant" => "zh-TW".into(),
            // Google has no Brazilian variant; both Portuguese forms map to "pt".
            "pt-BR" => "pt".into(),
            _ => base.to_string(),
        },
        TranslateProvider::Compatible => match app_code {
            // LibreTranslate uses bare ISO-639-1; it has no Traditional Chinese
            // or Brazilian Portuguese variant, so both collapse to the base.
            "zh-Hans" | "zh-Hant" => "zh".into(),
            "no" => "nb".into(),
            _ => base.to_string(),
        },
        TranslateProvider::Youdao => match app_code {
            // Youdao spells Chinese as zh-CHS / zh-CHT; everything else is the
            // bare ISO-639-1 subtag (en, ja, fr, …).
            "zh-Hans" => "zh-CHS".into(),
            "zh-Hant" => "zh-CHT".into(),
            _ => base.to_string(),
        },
    }
}

/// Build the JSON request body for one batch of HTML. The API key is NOT
/// included here (it travels in a header or the URL, except for the compatible
/// provider where the caller injects it) so this stays a pure, secret-free
/// function.
fn build_request_body(provider: TranslateProvider, html: &str, target: &str) -> Value {
    match provider {
        TranslateProvider::DeepL => json!({
            "text": [html],
            "target_lang": target,
            "tag_handling": "html",
        }),
        TranslateProvider::Google => json!({
            "q": html,
            "target": target,
            "format": "html",
        }),
        TranslateProvider::Compatible => json!({
            "q": html,
            "source": "auto",
            "target": target,
            "format": "html",
        }),
        // Youdao is form-encoded and signed, handled outside this JSON path.
        TranslateProvider::Youdao => {
            unreachable!("Youdao builds a form-encoded request, not a JSON body")
        }
    }
}

/// Extract the translated HTML from a provider's JSON response.
fn parse_response(provider: TranslateProvider, v: &Value) -> AppResult<String> {
    let missing = || AppError::other("Translation API returned no text");
    match provider {
        TranslateProvider::DeepL => v["translations"][0]["text"]
            .as_str()
            .map(String::from)
            .ok_or_else(missing),
        TranslateProvider::Google => v["data"]["translations"][0]["translatedText"]
            .as_str()
            .map(String::from)
            .ok_or_else(missing),
        TranslateProvider::Compatible => v["translatedText"]
            .as_str()
            .map(String::from)
            .ok_or_else(missing),
        TranslateProvider::Youdao => {
            // Youdao always returns an errorCode; "0" is success and the
            // translated HTML rides in the top-level `data` string.
            let code = v["errorCode"].as_str().unwrap_or("");
            if code != "0" {
                return Err(AppError::other(format!("Youdao error {code}")));
            }
            v["data"].as_str().map(String::from).ok_or_else(missing)
        }
    }
}

/// Youdao's `input`, the middle term of its signature: the whole query when it
/// is at most 20 characters, otherwise the first 10 chars, the char length, and
/// the last 10 chars concatenated. Char boundaries (not bytes) so multibyte
/// content is never split.
fn youdao_input(q: &str) -> String {
    let chars: Vec<char> = q.chars().collect();
    let len = chars.len();
    if len <= 20 {
        return q.to_string();
    }
    let first: String = chars[..10].iter().collect();
    let last: String = chars[len - 10..].iter().collect();
    format!("{first}{len}{last}")
}

/// Youdao's v3 request signature: `sha256(appKey + input + salt + curtime +
/// appSecret)`, lowercase hex.
fn youdao_sign(app_key: &str, q: &str, salt: &str, curtime: &str, app_secret: &str) -> String {
    let raw = format!("{app_key}{}{salt}{curtime}{app_secret}", youdao_input(q));
    hex::encode(Sha256::digest(raw.as_bytes()))
}

/// Translate one HTML fragment with Youdao's web-page translation endpoint:
/// a signed, form-encoded POST to `/translate_html`.
async fn translate_html_youdao(
    client: &Client,
    cfg: &TranslateConfig,
    html: &str,
    target: &str,
) -> AppResult<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let curtime = now.as_secs().to_string();
    // Salt only needs to be unique per request and match the value in the sign;
    // the nanosecond clock gives that without pulling in a UUID dependency.
    let salt = now.as_nanos().to_string();
    let sign = youdao_sign(&cfg.api_key, html, &salt, &curtime, &cfg.api_secret);

    let params = [
        ("q", html),
        ("from", "auto"),
        ("to", target),
        ("appKey", cfg.api_key.as_str()),
        ("salt", salt.as_str()),
        ("sign", sign.as_str()),
        ("signType", "v3"),
        ("curtime", curtime.as_str()),
    ];
    let resp = client
        .post(format!("{}/translate_html", cfg.base_url))
        .timeout(TRANSLATE_REQUEST_TIMEOUT)
        .form(&params)
        .send()
        .await?;
    let resp = ensure_success(resp).await?;
    let json: Value = resp.json().await?;
    parse_response(TranslateProvider::Youdao, &json)
}

/// Map a non-success HTTP response to an `AppError` carrying the body.
async fn ensure_success(resp: Response) -> AppResult<Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let detail = resp.text().await.unwrap_or_default();
    Err(AppError::other(format!(
        "Translation API error {status}: {detail}"
    )))
}

/// Translate one HTML fragment into `app_code`, returning the translated HTML.
/// Tags are preserved by the provider's native HTML handling.
pub async fn translate_html(
    client: &Client,
    cfg: &TranslateConfig,
    html: &str,
    app_code: &str,
) -> AppResult<String> {
    let target = provider_lang_code(cfg.provider, app_code);

    // Youdao diverges from the JSON+key shape: signed and form-encoded.
    if cfg.provider == TranslateProvider::Youdao {
        return translate_html_youdao(client, cfg, html, &target).await;
    }

    let mut body = build_request_body(cfg.provider, html, &target);

    let url = match cfg.provider {
        TranslateProvider::DeepL => format!("{}/v2/translate", cfg.base_url),
        TranslateProvider::Google => {
            format!("{}/language/translate/v2?key={}", cfg.base_url, cfg.api_key)
        }
        TranslateProvider::Compatible => format!("{}/translate", cfg.base_url),
        TranslateProvider::Youdao => unreachable!("Youdao is handled above"),
    };

    let mut req = client.post(url).timeout(TRANSLATE_REQUEST_TIMEOUT);
    match cfg.provider {
        // DeepL authenticates with a custom scheme in the Authorization header.
        TranslateProvider::DeepL => {
            req = req.header(
                "Authorization",
                format!("DeepL-Auth-Key {}", cfg.api_key),
            );
        }
        // Google takes the key as a query parameter (added to the URL above).
        TranslateProvider::Google => {}
        // LibreTranslate-compatible servers take the key in the body (when set).
        TranslateProvider::Compatible => {
            if !cfg.api_key.is_empty() {
                if let Value::Object(map) = &mut body {
                    map.insert("api_key".into(), json!(cfg.api_key));
                }
            }
        }
        TranslateProvider::Youdao => unreachable!("Youdao is handled above"),
    }

    let resp = req.json(&body).send().await?;
    let resp = ensure_success(resp).await?;
    let json: Value = resp.json().await?;
    parse_response(cfg.provider, &json)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── TranslateConfig::new ────────────────────────────────────────────

    #[test]
    fn deepl_free_key_picks_the_free_endpoint() {
        let cfg =
            TranslateConfig::new(Some("deepl".into()), Some("abc:fx".into()), None, None).unwrap();
        assert_eq!(cfg.base_url, "https://api-free.deepl.com");
    }

    #[test]
    fn deepl_paid_key_picks_the_paid_endpoint() {
        let cfg =
            TranslateConfig::new(Some("deepl".into()), Some("abc123".into()), None, None).unwrap();
        assert_eq!(cfg.base_url, "https://api.deepl.com");
    }

    #[test]
    fn config_trims_key_and_base_url() {
        let cfg = TranslateConfig::new(
            Some("google".into()),
            Some("  key123\n".into()),
            None,
            Some(" https://example.com/ ".into()),
        )
        .unwrap();
        assert_eq!(cfg.api_key, "key123");
        assert_eq!(cfg.base_url, "https://example.com");
    }

    #[test]
    fn deepl_requires_a_key() {
        match TranslateConfig::new(Some("deepl".into()), Some("  \n".into()), None, None) {
            Ok(_) => panic!("a blank key must be rejected for DeepL"),
            Err(e) => assert!(e.to_string().contains("noTranslateKey")),
        }
    }

    #[test]
    fn youdao_defaults_endpoint_and_requires_a_secret() {
        // appKey present, appSecret present → defaults to the official host.
        let cfg = TranslateConfig::new(
            Some("youdao".into()),
            Some("app-key".into()),
            Some("app-secret".into()),
            None,
        )
        .unwrap();
        assert_eq!(cfg.provider, TranslateProvider::Youdao);
        assert_eq!(cfg.base_url, "https://openapi.youdao.com");

        // appKey but no appSecret → rejected, Youdao must sign every request.
        match TranslateConfig::new(Some("youdao".into()), Some("app-key".into()), None, None) {
            Ok(_) => panic!("Youdao requires an appSecret"),
            Err(e) => assert!(e.to_string().contains("noTranslateSecret")),
        }
    }

    #[test]
    fn compatible_allows_a_keyless_endpoint() {
        let cfg = TranslateConfig::new(
            Some("compatible".into()),
            None,
            None,
            Some("https://lt.example".into()),
        )
        .unwrap();
        assert_eq!(cfg.provider, TranslateProvider::Compatible);
        assert!(cfg.api_key.is_empty());
    }

    #[test]
    fn compatible_requires_a_base_url() {
        match TranslateConfig::new(Some("compatible".into()), Some("k".into()), None, None) {
            Ok(_) => panic!("a compatible endpoint has no default base URL"),
            Err(e) => assert!(e.to_string().contains("noTranslateEndpoint")),
        }
    }

    // ── provider_lang_code ──────────────────────────────────────────────

    #[test]
    fn deepl_codes_are_uppercased_with_variants() {
        assert_eq!(provider_lang_code(TranslateProvider::DeepL, "en"), "EN-US");
        assert_eq!(provider_lang_code(TranslateProvider::DeepL, "pt-BR"), "PT-BR");
        assert_eq!(provider_lang_code(TranslateProvider::DeepL, "zh-Hans"), "ZH");
        assert_eq!(provider_lang_code(TranslateProvider::DeepL, "de"), "DE");
        assert_eq!(provider_lang_code(TranslateProvider::DeepL, "no"), "NB");
    }

    #[test]
    fn google_codes_are_bcp47() {
        assert_eq!(provider_lang_code(TranslateProvider::Google, "zh-Hans"), "zh-CN");
        assert_eq!(provider_lang_code(TranslateProvider::Google, "zh-Hant"), "zh-TW");
        assert_eq!(provider_lang_code(TranslateProvider::Google, "pt-BR"), "pt");
        assert_eq!(provider_lang_code(TranslateProvider::Google, "ja"), "ja");
    }

    #[test]
    fn compatible_codes_are_bare_iso() {
        assert_eq!(provider_lang_code(TranslateProvider::Compatible, "zh-Hans"), "zh");
        assert_eq!(provider_lang_code(TranslateProvider::Compatible, "zh-Hant"), "zh");
        assert_eq!(provider_lang_code(TranslateProvider::Compatible, "fr"), "fr");
        assert_eq!(provider_lang_code(TranslateProvider::Compatible, "no"), "nb");
    }

    #[test]
    fn youdao_codes_spell_chinese_specially() {
        assert_eq!(provider_lang_code(TranslateProvider::Youdao, "zh-Hans"), "zh-CHS");
        assert_eq!(provider_lang_code(TranslateProvider::Youdao, "zh-Hant"), "zh-CHT");
        assert_eq!(provider_lang_code(TranslateProvider::Youdao, "ja"), "ja");
        assert_eq!(provider_lang_code(TranslateProvider::Youdao, "pt-BR"), "pt");
    }

    // ── build_request_body ──────────────────────────────────────────────

    #[test]
    fn deepl_body_uses_html_tag_handling() {
        let b = build_request_body(TranslateProvider::DeepL, "<p>hi</p>", "ZH");
        assert_eq!(b["text"][0], "<p>hi</p>");
        assert_eq!(b["target_lang"], "ZH");
        assert_eq!(b["tag_handling"], "html");
    }

    #[test]
    fn google_body_uses_html_format() {
        let b = build_request_body(TranslateProvider::Google, "<p>hi</p>", "zh-CN");
        assert_eq!(b["q"], "<p>hi</p>");
        assert_eq!(b["target"], "zh-CN");
        assert_eq!(b["format"], "html");
    }

    #[test]
    fn compatible_body_carries_no_key() {
        let b = build_request_body(TranslateProvider::Compatible, "<p>hi</p>", "zh");
        assert_eq!(b["q"], "<p>hi</p>");
        assert_eq!(b["target"], "zh");
        assert_eq!(b["format"], "html");
        assert!(b.get("api_key").is_none());
    }

    // ── parse_response ──────────────────────────────────────────────────

    #[test]
    fn parses_deepl_response() {
        let v = json!({ "translations": [{ "text": "<p>你好</p>" }] });
        assert_eq!(parse_response(TranslateProvider::DeepL, &v).unwrap(), "<p>你好</p>");
    }

    #[test]
    fn parses_google_response() {
        let v = json!({ "data": { "translations": [{ "translatedText": "<p>你好</p>" }] } });
        assert_eq!(parse_response(TranslateProvider::Google, &v).unwrap(), "<p>你好</p>");
    }

    #[test]
    fn parses_compatible_response() {
        let v = json!({ "translatedText": "<p>你好</p>" });
        assert_eq!(parse_response(TranslateProvider::Compatible, &v).unwrap(), "<p>你好</p>");
    }

    #[test]
    fn missing_text_is_an_error() {
        let v = json!({ "unexpected": true });
        assert!(parse_response(TranslateProvider::DeepL, &v).is_err());
    }

    #[test]
    fn parses_youdao_success_and_surfaces_errors() {
        let ok = json!({ "errorCode": "0", "data": "<p>你好</p>" });
        assert_eq!(parse_response(TranslateProvider::Youdao, &ok).unwrap(), "<p>你好</p>");
        // A non-zero errorCode is a failure even though `data` may be absent.
        let bad = json!({ "errorCode": "401", "data": "" });
        let err = parse_response(TranslateProvider::Youdao, &bad).unwrap_err();
        assert!(err.to_string().contains("401"), "code not surfaced: {err}");
    }

    // ── Youdao signature ────────────────────────────────────────────────

    #[test]
    fn youdao_input_passes_short_text_through() {
        // 20 chars or fewer: the whole string is the input.
        assert_eq!(youdao_input("hello"), "hello");
        assert_eq!(youdao_input(&"x".repeat(20)), "x".repeat(20));
    }

    #[test]
    fn youdao_input_truncates_long_text_by_char() {
        // 25 chars → first 10 + "25" + last 10.
        let q: String = ('a'..='y').collect(); // a..y = 25 chars
        let got = youdao_input(&q);
        assert_eq!(got, "abcdefghij25pqrstuvwxy");
        // Multibyte content must split on char boundaries, not bytes.
        let zh = "一二三四五六七八九十壹贰叁肆伍陆柒捌玖拾"; // 20 chars
        assert_eq!(youdao_input(zh), zh);
        let zh21 = format!("{zh}甲"); // 21 chars
        assert!(youdao_input(&zh21).starts_with("一二三四五六七八九十"));
        assert!(youdao_input(&zh21).contains("21"));
    }

    #[test]
    fn youdao_sign_is_a_lowercase_hex_sha256() {
        let sign = youdao_sign("appkey", "hello", "salt", "1700000000", "secret");
        assert_eq!(sign.len(), 64, "sha256 hex is 64 chars: {sign}");
        assert!(sign.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
        // Deterministic for the same inputs, different when any input changes.
        assert_eq!(sign, youdao_sign("appkey", "hello", "salt", "1700000000", "secret"));
        assert_ne!(sign, youdao_sign("appkey", "hello", "salt", "1700000000", "other"));
    }
}
