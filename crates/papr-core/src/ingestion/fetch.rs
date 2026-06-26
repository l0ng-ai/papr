//! HTTP fetching with conditional GET (ETag / If-Modified-Since) so unchanged
//! feeds cost a single 304 round-trip.

use crate::db;
use crate::error::{AppError, AppResult};
use reqwest::header::{
    CONTENT_TYPE, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED,
};
use reqwest::{Client, StatusCode};
use rusqlite::Connection;
use std::time::Duration;

pub const USER_AGENT: &str = "Papr/0.1 (+https://github.com/papr-reader)";

/// Hard cap on a fetched body. Feeds and article pages are text — a few
/// hundred KB at most — so 16 MiB is generous while still stopping a
/// hostile or misconfigured server from exhausting memory.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Read a response body, aborting if it exceeds `MAX_BODY_BYTES`. Streams in
/// chunks so an unbounded (or lying-`Content-Length`) response can't first be
/// buffered whole.
async fn read_capped(mut resp: reqwest::Response) -> AppResult<Vec<u8>> {
    if resp.content_length().is_some_and(|n| n > MAX_BODY_BYTES as u64) {
        return Err(AppError::code("responseTooLarge"));
    }
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if buf.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(AppError::code("responseTooLarge"));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Build the shared HTTP client (connection pooling, gzip/brotli, redirects).
///
/// `timeout_secs` bounds the whole request. `proxy` is one of `"system"`
/// (honour `HTTP(S)_PROXY` env vars), `"none"` (bypass all proxies), or an
/// explicit proxy URL.
pub fn build_client(timeout_secs: u64, proxy: &str) -> Client {
    let mut builder = Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(timeout_secs.clamp(5, 300)))
        .connect_timeout(Duration::from_secs(10));

    match proxy {
        "system" | "" => {}
        "none" => builder = builder.no_proxy(),
        custom => {
            if let Ok(p) = reqwest::Proxy::all(custom) {
                builder = builder.proxy(p);
            }
        }
    }
    builder.build().expect("failed to build reqwest client")
}

/// Build the HTTP client from the persisted network settings.
pub fn build_client_from_settings(conn: &Connection) -> Client {
    let timeout = db::setting_parsed::<u64>(conn, "net_timeout_sec", 30);
    let proxy = db::get_setting(conn, "net_proxy")
        .ok()
        .flatten()
        .unwrap_or_else(|| "system".to_string());
    build_client(timeout, &proxy)
}

/// Result of a conditional GET against a feed URL.
pub enum Fetched {
    /// Server returned 304 — the stored copy is still current.
    NotModified,
    /// Fresh content, along with revalidation headers to store.
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

/// Conditional GET. Sends `If-None-Match`/`If-Modified-Since` when we have them.
pub async fn conditional_get(
    client: &Client,
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> AppResult<Fetched> {
    let mut req = client.get(url);
    if let Some(e) = etag {
        req = req.header(IF_NONE_MATCH, e);
    }
    if let Some(lm) = last_modified {
        req = req.header(IF_MODIFIED_SINCE, lm);
    }

    let resp = req.send().await?;
    if resp.status() == StatusCode::NOT_MODIFIED {
        return Ok(Fetched::NotModified);
    }
    let resp = resp.error_for_status()?;
    let header = |name: reqwest::header::HeaderName| {
        resp.headers()
            .get(&name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    };
    let etag = header(ETAG);
    let last_modified = header(LAST_MODIFIED);
    let bytes = read_capped(resp).await?;
    Ok(Fetched::Body {
        bytes,
        etag,
        last_modified,
    })
}

/// Pull the `charset` parameter out of a `Content-Type` header value, e.g.
/// `text/html; charset=Shift_JIS` → `Some("shift_jis")`. Case-insensitive,
/// tolerant of surrounding quotes and whitespace.
fn charset_from_content_type(content_type: &str) -> Option<String> {
    content_type
        .split(';')
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("charset"))
        .map(|(_, v)| v.trim().trim_matches('"').trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
}

/// Read a charset label that begins at `tail` — the text right after a
/// `charset` keyword. Skips an optional `=` and surrounding quotes, then takes
/// the run of label characters.
fn parse_charset_value(tail: &str) -> Option<String> {
    let tail = tail.trim_start().strip_prefix('=').unwrap_or(tail);
    let value: String = tail
        .trim_start()
        .trim_start_matches(['"', '\''])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    (!value.is_empty()).then_some(value)
}

/// Sniff a charset from an HTML document's own `<meta>` declaration —
/// `<meta charset=...>` or `<meta http-equiv="Content-Type" content="...">`.
/// Only the first 2 KiB are scanned (the `<head>` is required to declare the
/// charset early); the bytes are read as ASCII, which is safe because every
/// supported encoding is ASCII-compatible for the markup itself.
///
/// The keyword is only honoured when it sits *inside* a `<meta …>` tag. A bare
/// substring search would be derailed by an earlier, unrelated `charset`: a
/// CSS `@charset "utf-8";` rule in an inline `<style>`, the word in an early
/// `<title>` or comment, or `charset` in a `<script>` blob — any of which can
/// precede the real `<meta charset>` and would otherwise be picked instead,
/// mis-decoding the page.
fn charset_from_html(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(2048)];
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    let mut search = 0;
    // Walk each `<meta …>` tag and look for `charset` within its bounds only.
    while let Some(rel) = text[search..].find("<meta") {
        let tag_start = search + rel;
        // The tag runs to the next '>' (or, in truncated HTML, the buffer end).
        let tag_end = text[tag_start..]
            .find('>')
            .map(|i| tag_start + i)
            .unwrap_or(text.len());
        let tag = &text[tag_start..tag_end];
        if let Some(i) = tag.find("charset") {
            if let Some(value) = parse_charset_value(&tag[i + "charset".len()..]) {
                return Some(value);
            }
        }
        search = tag_end;
    }
    None
}

/// Decode fetched HTML/text bytes into a `String` using the page's declared
/// character encoding rather than blindly assuming UTF-8. Many non-English
/// sites still serve Shift-JIS, GBK, EUC-KR or ISO-8859-1; decoding those as
/// UTF-8 produces mojibake that breaks full-text extraction, feed discovery
/// and YouTube channel-id resolution.
///
/// Encoding is resolved in priority order: the HTTP `Content-Type` charset,
/// then the document's own `<meta charset>`, then UTF-8 as the default.
/// Unknown labels fall back to UTF-8. Mirrors the WHATWG resource-decoding
/// order browsers use.
pub fn decode_html(bytes: &[u8], content_type: Option<&str>) -> String {
    let label = content_type
        .and_then(charset_from_content_type)
        .or_else(|| charset_from_html(bytes));
    let encoding = label
        .as_deref()
        .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

/// Plain GET returning `(body, content_type, final_url)` — used for feed
/// auto-discovery and full-text article extraction.
pub async fn get(client: &Client, url: &str) -> AppResult<(Vec<u8>, Option<String>, String)> {
    let resp = client.get(url).send().await?.error_for_status()?;
    let final_url = resp.url().to_string();
    let content_type = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let bytes = read_capped(resp).await?;
    Ok((bytes, content_type, final_url))
}

#[cfg(test)]
mod tests {
    use super::{charset_from_content_type, charset_from_html, decode_html};

    #[test]
    fn charset_parsed_from_content_type_header() {
        assert_eq!(
            charset_from_content_type("text/html; charset=UTF-8").as_deref(),
            Some("utf-8")
        );
        assert_eq!(
            charset_from_content_type("text/html;charset=\"Shift_JIS\"").as_deref(),
            Some("shift_jis")
        );
        assert_eq!(
            charset_from_content_type("text/html ; CHARSET = gbk ").as_deref(),
            Some("gbk")
        );
        assert_eq!(charset_from_content_type("text/html"), None);
        assert_eq!(charset_from_content_type("text/html; charset="), None);
    }

    #[test]
    fn charset_sniffed_from_html_meta() {
        assert_eq!(
            charset_from_html(b"<html><head><meta charset=\"euc-kr\">").as_deref(),
            Some("euc-kr")
        );
        assert_eq!(
            charset_from_html(
                b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=Shift_JIS\">"
            )
            .as_deref(),
            Some("shift_jis")
        );
        assert_eq!(charset_from_html(b"<html><head><title>x</title>"), None);
    }

    #[test]
    fn charset_ignores_keyword_outside_meta_tags() {
        // A CSS `@charset` rule in an inline <style> precedes the real
        // <meta charset>. The bare-substring scan would pick the CSS rule's
        // "utf-8" and never reach the genuine Shift_JIS declaration.
        let html = b"<head><style>@charset \"UTF-8\";</style>\
            <meta charset=\"Shift_JIS\"></head>";
        assert_eq!(charset_from_html(html).as_deref(), Some("shift_jis"));

        // The literal word "charset" in an early <title> must not be mistaken
        // for a declaration.
        let html = b"<head><title>How to set the charset</title>\
            <meta charset=\"euc-kr\"></head>";
        assert_eq!(charset_from_html(html).as_deref(), Some("euc-kr"));

        // "charset" only ever appearing outside a <meta> tag yields nothing.
        assert_eq!(
            charset_from_html(b"<head><title>charset basics</title></head>"),
            None
        );
    }

    #[test]
    fn decode_html_honours_header_charset() {
        // ISO-8859-1 byte 0xE9 is 'é'. Decoded as UTF-8 it would be replaced
        // with U+FFFD; with the declared charset it round-trips correctly.
        let bytes = b"caf\xe9";
        let decoded = decode_html(bytes, Some("text/html; charset=iso-8859-1"));
        assert_eq!(decoded, "café");
    }

    #[test]
    fn decode_html_falls_back_to_meta_then_utf8() {
        // No header charset — the <meta> declaration is used instead.
        let mut html: Vec<u8> = b"<meta charset=windows-1252><body>".to_vec();
        html.push(0x80); // windows-1252 0x80 == '€'
        let decoded = decode_html(&html, None);
        assert!(decoded.contains('€'), "decoded: {decoded:?}");

        // No charset anywhere — valid UTF-8 round-trips untouched.
        assert_eq!(decode_html("héllo".as_bytes(), Some("text/html")), "héllo");
    }

    #[test]
    fn decode_html_ignores_unknown_charset_label() {
        // A bogus charset label must not panic — it falls back to UTF-8.
        assert_eq!(
            decode_html("plain".as_bytes(), Some("text/html; charset=not-a-charset")),
            "plain"
        );
    }
}
