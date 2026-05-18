//! "Send to…" share integrations (feature F8). Pushes a single article out to
//! a read-later / archive / device service: Pocket, Instapaper and Kindle.
//! (The fourth target, Notion full-page, lives in `export.rs` so it can reuse
//! that module's Notion HTTP client — see `export::build_notion_page` /
//! `export::post_article_to_notion`.)
//!
//! Every target-specific payload *builder* here is a pure function with unit
//! tests below; only the network / SMTP call itself is a thin wrapper. This
//! mirrors the structure of `export.rs`.

use crate::error::{AppError, AppResult};
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Message, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::SmtpTransport;
use lettre::Transport;
use reqwest::Client;
use serde_json::{json, Value};

/// The article fields a "Send to…" action needs. A small owned struct so the
/// builders stay pure — they never touch the database. Mirrors
/// `export::ExportArticle`, plus the body HTML the Kindle document needs.
#[derive(Debug, Clone)]
pub struct ShareArticle {
    pub title: String,
    pub url: Option<String>,
    pub author: Option<String>,
    pub feed_title: String,
    pub published_at: Option<String>,
    /// Sanitized article body HTML — extracted full text if present, else the
    /// feed-supplied content. May be empty when neither is available.
    pub body_html: String,
}

// ─────────────────────────── Pocket ───────────────────────────
//
// NOTE: Pocket's hosted service has an uncertain future — Mozilla announced in
// 2025 that it is winding the product down. We implement against its
// documented v3 `/v3/add` API contract regardless: the credentials and request
// shape below match that contract, so the integration works for as long as the
// endpoint stays up (and against any compatible replacement).

/// Build the JSON body for a Pocket `POST /v3/add` request. Pocket
/// authenticates every call with the app's `consumer_key` plus the user's
/// OAuth `access_token` carried *in the body* (not a header). `url` is the
/// only required item field; `title` is sent when known so Pocket does not
/// have to re-fetch the page to learn it. Pure — unit-tested below.
pub fn build_pocket_body(
    consumer_key: &str,
    access_token: &str,
    article: &ShareArticle,
    url: &str,
) -> Value {
    let mut body = json!({
        "consumer_key": consumer_key,
        "access_token": access_token,
        "url": url,
    });
    if !article.title.trim().is_empty() {
        body["title"] = json!(article.title);
    }
    body
}

/// Add an article to Pocket. Thin wrapper over the pure builder.
pub async fn post_to_pocket(
    client: &Client,
    consumer_key: &str,
    access_token: &str,
    article: &ShareArticle,
) -> AppResult<()> {
    let url = article
        .url
        .as_deref()
        .filter(|u| !u.trim().is_empty())
        .ok_or_else(|| AppError::code("noArticleUrl"))?;
    let body = build_pocket_body(consumer_key, access_token, article, url);
    let resp = client
        .post("https://getpocket.com/v3/add")
        .header("Content-Type", "application/json; charset=UTF-8")
        .header("X-Accept", "application/json")
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        // Pocket reports the real reason in an `X-Error` header.
        let detail = resp
            .headers()
            .get("x-error")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .unwrap_or_else(|| resp.status().to_string());
        return Err(AppError::other(format!("Pocket error {status}: {detail}")));
    }
    Ok(())
}

// ─────────────────────────── Instapaper ───────────────────────────

/// Percent-encode one value for an `application/x-www-form-urlencoded` body.
/// Conservative: only the RFC 3986 unreserved set is left as-is, everything
/// else (including space → `%20`) is escaped.
fn form_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Build the `application/x-www-form-urlencoded` body for an Instapaper
/// `POST /api/add` request. `url` is required; `title` and `selection` are
/// sent only when present. Instapaper authenticates with HTTP Basic auth (the
/// username/password), which is applied by the wrapper, not encoded here.
/// Pure — unit-tested below.
pub fn build_instapaper_body(article: &ShareArticle, url: &str, selection: Option<&str>) -> String {
    let mut parts: Vec<String> = vec![format!("url={}", form_encode(url))];
    if !article.title.trim().is_empty() {
        parts.push(format!("title={}", form_encode(&article.title)));
    }
    if let Some(sel) = selection.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(format!("selection={}", form_encode(sel)));
    }
    parts.join("&")
}

/// Add an article to Instapaper. Thin wrapper over the pure builder.
pub async fn post_to_instapaper(
    client: &Client,
    username: &str,
    password: &str,
    article: &ShareArticle,
    selection: Option<&str>,
) -> AppResult<()> {
    let url = article
        .url
        .as_deref()
        .filter(|u| !u.trim().is_empty())
        .ok_or_else(|| AppError::code("noArticleUrl"))?;
    let body = build_instapaper_body(article, url, selection);
    let resp = client
        .post("https://www.instapaper.com/api/add")
        .basic_auth(username, Some(password))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        return Err(AppError::other(format!(
            "Instapaper error {status}: {detail}"
        )));
    }
    Ok(())
}

// ─────────────────────────── Kindle ───────────────────────────

/// HTML-escape a string for safe interpolation into element text / attributes.
fn escape_html(s: &str) -> String {
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

/// Build a clean, self-contained HTML document for an article — title, byline
/// and body — suitable for emailing to a `@kindle.com` address. The body HTML
/// is embedded verbatim (it is already sanitized upstream by `sanitize`).
/// Pure — unit-tested below.
pub fn build_kindle_document(article: &ShareArticle) -> String {
    let title = escape_html(&article.title);

    // A single byline line assembled from whichever metadata is present.
    let mut byline: Vec<String> = Vec::new();
    if let Some(author) = article.author.as_deref().filter(|a| !a.trim().is_empty()) {
        byline.push(escape_html(author));
    }
    byline.push(escape_html(&article.feed_title));
    if let Some(date) = article.published_at.as_deref().filter(|d| !d.trim().is_empty()) {
        byline.push(escape_html(date));
    }
    let byline = byline.join(" · ");

    let body = if article.body_html.trim().is_empty() {
        "<p><em>No content.</em></p>".to_string()
    } else {
        article.body_html.clone()
    };

    let source = match article.url.as_deref().filter(|u| !u.trim().is_empty()) {
        Some(url) => format!(
            "<p class=\"source\">Source: <a href=\"{0}\">{0}</a></p>",
            escape_html(url)
        ),
        None => String::new(),
    };

    format!(
        "<!DOCTYPE html>\n\
<html lang=\"en\">\n\
<head>\n\
<meta charset=\"utf-8\">\n\
<title>{title}</title>\n\
<style>\n\
body {{ font-family: Georgia, serif; line-height: 1.5; margin: 1em; }}\n\
h1 {{ font-size: 1.5em; margin-bottom: 0.2em; }}\n\
.byline {{ color: #666; font-size: 0.9em; margin-bottom: 1.5em; }}\n\
.source {{ color: #666; font-size: 0.85em; margin-top: 2em; }}\n\
img {{ max-width: 100%; height: auto; }}\n\
</style>\n\
</head>\n\
<body>\n\
<h1>{title}</h1>\n\
<p class=\"byline\">{byline}</p>\n\
{body}\n\
{source}\n\
</body>\n\
</html>\n"
    )
}

/// Turn an article title into a filesystem/attachment-safe `.html` filename.
/// Pure — unit-tested below.
pub fn kindle_attachment_name(title: &str) -> String {
    let safe: String = title
        .chars()
        .map(|c| if "/\\:*?\"<>|\r\n\t".contains(c) { '-' } else { c })
        .collect();
    let safe = safe.trim();
    if safe.is_empty() {
        "article.html".to_string()
    } else {
        format!("{safe}.html")
    }
}

/// Resolved SMTP configuration for "Send to Kindle".
#[derive(Debug, Clone)]
pub struct KindleConfig {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: String,
    /// The `From:` address — Amazon only accepts mail from an address the user
    /// has approved. Defaults to `smtp_username` when left blank.
    pub from_address: String,
    /// The destination `@kindle.com` address.
    pub kindle_address: String,
}

impl KindleConfig {
    /// Build a config from raw settings, rejecting an incomplete one with a
    /// localisable error code.
    pub fn new(
        smtp_host: Option<String>,
        smtp_port: Option<String>,
        smtp_username: Option<String>,
        smtp_password: Option<String>,
        from_address: Option<String>,
        kindle_address: Option<String>,
    ) -> AppResult<Self> {
        let nonempty = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let smtp_host = nonempty(smtp_host).ok_or_else(|| AppError::code("noKindleConfig"))?;
        let smtp_username =
            nonempty(smtp_username).ok_or_else(|| AppError::code("noKindleConfig"))?;
        let smtp_password =
            nonempty(smtp_password).ok_or_else(|| AppError::code("noKindleConfig"))?;
        let kindle_address =
            nonempty(kindle_address).ok_or_else(|| AppError::code("noKindleConfig"))?;
        // Default port 587 (SMTP submission with STARTTLS) when unset.
        let smtp_port = nonempty(smtp_port)
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(587);
        let from_address = nonempty(from_address).unwrap_or_else(|| smtp_username.clone());
        Ok(KindleConfig {
            smtp_host,
            smtp_port,
            smtp_username,
            smtp_password,
            from_address,
            kindle_address,
        })
    }
}

/// Build the `lettre` MIME message that carries an article to a Kindle. The
/// document is attached as an `.html` file; Amazon's "Send to Kindle" service
/// converts it on receipt. The body text is a short human-readable note.
/// Pure (no network) — unit-tested below.
pub fn build_kindle_message(cfg: &KindleConfig, article: &ShareArticle) -> AppResult<Message> {
    let document = build_kindle_document(article);
    let filename = kindle_attachment_name(&article.title);

    let from = cfg
        .from_address
        .parse()
        .map_err(|e| AppError::other(format!("invalid From address: {e}")))?;
    let to = cfg
        .kindle_address
        .parse()
        .map_err(|e| AppError::other(format!("invalid Kindle address: {e}")))?;

    let attachment = Attachment::new(filename).body(
        document.into_bytes(),
        ContentType::parse("text/html; charset=utf-8").unwrap(),
    );
    let note = SinglePart::plain(format!(
        "Sent from Papr: \"{}\"\n\nThe article is attached as an HTML file.",
        article.title
    ));

    Message::builder()
        .from(from)
        .to(to)
        // "Send to Kindle" uses the subject only as a document title hint.
        .subject(article.title.clone())
        .multipart(
            lettre::message::MultiPart::mixed()
                .singlepart(note)
                .singlepart(attachment),
        )
        .map_err(|e| AppError::other(format!("build email: {e}")))
}

/// Email an article to the user's Kindle. Builds the MIME message (pure) then
/// performs the one impure step — the blocking SMTP send. The `imap`/`lettre`
/// transports are blocking, so callers run this on the Tokio blocking pool.
pub fn send_to_kindle(cfg: &KindleConfig, article: &ShareArticle) -> AppResult<()> {
    let message = build_kindle_message(cfg, article)?;
    let creds = Credentials::new(cfg.smtp_username.clone(), cfg.smtp_password.clone());
    // Pick the TLS mode by port. Port 465 is implicit TLS (SMTPS) — the
    // connection is wrapped in TLS from the first byte; using STARTTLS against
    // it fails because the server never speaks plaintext. Every other port
    // (587 submission, 25, custom) uses STARTTLS: connect in the clear, then
    // upgrade. Without this split a user who entered 465 — a common consumer
    // SMTP setting (Gmail, Fastmail, …) — could never send to Kindle, the send
    // failing with an opaque handshake error.
    let builder = if cfg.smtp_port == 465 {
        SmtpTransport::relay(&cfg.smtp_host)
    } else {
        SmtpTransport::starttls_relay(&cfg.smtp_host)
    };
    let mailer = builder
        .map_err(|e| AppError::other(format!("SMTP connect: {e}")))?
        .port(cfg.smtp_port)
        .credentials(creds)
        .build();
    mailer
        .send(&message)
        .map_err(|e| AppError::other(format!("SMTP send: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ShareArticle {
        ShareArticle {
            title: "Rust in 2024".to_string(),
            url: Some("https://example.com/rust".to_string()),
            author: Some("Jane Doe".to_string()),
            feed_title: "Example Blog".to_string(),
            published_at: Some("2024-01-15".to_string()),
            body_html: "<p>The borrow checker is great.</p>".to_string(),
        }
    }

    fn bare() -> ShareArticle {
        ShareArticle {
            title: "Untitled".to_string(),
            url: None,
            author: None,
            feed_title: "Feed".to_string(),
            published_at: None,
            body_html: String::new(),
        }
    }

    // ── Pocket ──

    #[test]
    fn pocket_body_carries_credentials_and_url() {
        let body = build_pocket_body("CK", "AT", &sample(), "https://example.com/rust");
        assert_eq!(body["consumer_key"], "CK");
        assert_eq!(body["access_token"], "AT");
        assert_eq!(body["url"], "https://example.com/rust");
        assert_eq!(body["title"], "Rust in 2024");
    }

    #[test]
    fn pocket_body_omits_empty_title() {
        let mut a = sample();
        a.title = "   ".to_string();
        let body = build_pocket_body("CK", "AT", &a, "https://example.com/rust");
        assert!(body.get("title").is_none());
    }

    #[test]
    fn pocket_body_special_characters_preserved() {
        let mut a = sample();
        a.title = "C# & \"Rust\" <tags> 🎉".to_string();
        let body = build_pocket_body("CK", "AT", &a, "https://example.com/x?a=1&b=2");
        assert_eq!(body["title"], "C# & \"Rust\" <tags> 🎉");
        assert_eq!(body["url"], "https://example.com/x?a=1&b=2");
    }

    // ── Instapaper ──

    #[test]
    fn instapaper_body_minimal_is_just_url() {
        let body = build_instapaper_body(&bare(), "https://example.com/a", None);
        // bare()'s title is non-empty ("Untitled"), so title is included.
        assert!(body.starts_with("url=https%3A%2F%2Fexample.com%2Fa"));
        assert!(body.contains("title=Untitled"));
        assert!(!body.contains("selection="));
    }

    #[test]
    fn instapaper_body_includes_selection_when_present() {
        let body = build_instapaper_body(&sample(), "https://example.com/rust", Some("a quote"));
        assert!(body.contains("selection=a%20quote"));
    }

    #[test]
    fn instapaper_body_omits_blank_selection() {
        let body = build_instapaper_body(&sample(), "https://example.com/rust", Some("   "));
        assert!(!body.contains("selection="));
    }

    #[test]
    fn instapaper_body_omits_empty_title() {
        let mut a = sample();
        a.title = "".to_string();
        let body = build_instapaper_body(&a, "https://example.com/rust", None);
        assert_eq!(body, "url=https%3A%2F%2Fexample.com%2Frust");
    }

    #[test]
    fn instapaper_body_form_encodes_special_characters() {
        let mut a = sample();
        a.title = "C# & Rust=fast".to_string();
        let body = build_instapaper_body(&a, "https://example.com/p?x=1&y=2", Some("100% sure"));
        // '&', '=', '#', ' ', '%' must all be escaped.
        assert!(body.contains("title=C%23%20%26%20Rust%3Dfast"));
        assert!(body.contains("url=https%3A%2F%2Fexample.com%2Fp%3Fx%3D1%26y%3D2"));
        assert!(body.contains("selection=100%25%20sure"));
        // Each field is a clean key=value pair joined by a literal '&'.
        assert_eq!(body.matches('&').count(), 2);
    }

    #[test]
    fn form_encode_leaves_unreserved_set_intact() {
        assert_eq!(form_encode("aZ09-_.~"), "aZ09-_.~");
        assert_eq!(form_encode(" "), "%20");
        assert_eq!(form_encode("ä"), "%C3%A4"); // multi-byte UTF-8
    }

    // ── Kindle document ──

    #[test]
    fn kindle_document_has_title_byline_and_body() {
        let doc = build_kindle_document(&sample());
        assert!(doc.starts_with("<!DOCTYPE html>"));
        assert!(doc.contains("<title>Rust in 2024</title>"));
        assert!(doc.contains("<h1>Rust in 2024</h1>"));
        assert!(doc.contains("Jane Doe · Example Blog · 2024-01-15"));
        assert!(doc.contains("<p>The borrow checker is great.</p>"));
        assert!(doc.contains("Source: <a href=\"https://example.com/rust\">"));
    }

    #[test]
    fn kindle_document_escapes_title_special_characters() {
        let mut a = sample();
        a.title = "C# & <script>\"x\"".to_string();
        let doc = build_kindle_document(&a);
        assert!(doc.contains("<title>C# &amp; &lt;script&gt;&quot;x&quot;</title>"));
        assert!(doc.contains("<h1>C# &amp; &lt;script&gt;&quot;x&quot;</h1>"));
        // The raw "<script>" must never appear unescaped in the title/byline.
        assert!(!doc.contains("<title>C# & <script>"));
    }

    #[test]
    fn kindle_document_byline_omits_absent_author_and_date() {
        let doc = build_kindle_document(&bare());
        // Only the feed title remains.
        assert!(doc.contains("<p class=\"byline\">Feed</p>"));
        assert!(!doc.contains(" · "));
    }

    #[test]
    fn kindle_document_empty_body_has_placeholder() {
        let doc = build_kindle_document(&bare());
        assert!(doc.contains("<p><em>No content.</em></p>"));
    }

    #[test]
    fn kindle_document_no_url_omits_source_line() {
        let doc = build_kindle_document(&bare());
        assert!(!doc.contains("class=\"source\""));
    }

    #[test]
    fn kindle_document_handles_long_body() {
        let mut a = sample();
        a.body_html = format!("<p>{}</p>", "word ".repeat(20_000));
        let doc = build_kindle_document(&a);
        assert!(doc.len() > 80_000);
        assert!(doc.ends_with("</html>\n"));
    }

    // ── Kindle attachment name ──

    #[test]
    fn attachment_name_sanitizes_and_appends_extension() {
        assert_eq!(kindle_attachment_name("Hello World"), "Hello World.html");
        assert_eq!(
            kindle_attachment_name("a/b:c*d?\"e<f>g|h"),
            "a-b-c-d--e-f-g-h.html"
        );
        assert_eq!(kindle_attachment_name("line\nbreak"), "line-break.html");
    }

    #[test]
    fn attachment_name_falls_back_when_title_blank() {
        assert_eq!(kindle_attachment_name("   "), "article.html");
        assert_eq!(kindle_attachment_name(""), "article.html");
    }

    // ── Kindle MIME message ──

    fn kindle_cfg() -> KindleConfig {
        KindleConfig {
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 587,
            smtp_username: "me@example.com".to_string(),
            smtp_password: "secret".to_string(),
            from_address: "me@example.com".to_string(),
            kindle_address: "me@kindle.com".to_string(),
        }
    }

    #[test]
    fn kindle_message_has_expected_headers_and_attachment() {
        let msg = build_kindle_message(&kindle_cfg(), &sample()).unwrap();
        let raw = String::from_utf8(msg.formatted()).unwrap();
        assert!(raw.contains("From: me@example.com"));
        assert!(raw.contains("To: me@kindle.com"));
        assert!(raw.contains("Subject: Rust in 2024"));
        // The HTML document rides along as an attachment.
        assert!(raw.contains("Content-Disposition: attachment"));
        assert!(raw.contains("Rust in 2024.html"));
        assert!(raw.contains("multipart/mixed"));
    }

    #[test]
    fn kindle_message_rejects_invalid_addresses() {
        let mut cfg = kindle_cfg();
        cfg.kindle_address = "not an email".to_string();
        assert!(build_kindle_message(&cfg, &sample()).is_err());
    }

    #[test]
    fn kindle_config_requires_core_fields() {
        // Missing host.
        assert!(KindleConfig::new(
            None,
            Some("587".into()),
            Some("u".into()),
            Some("p".into()),
            None,
            Some("k@kindle.com".into()),
        )
        .is_err());
        // Missing kindle address.
        assert!(KindleConfig::new(
            Some("h".into()),
            None,
            Some("u".into()),
            Some("p".into()),
            None,
            None,
        )
        .is_err());
    }

    #[test]
    fn kindle_config_defaults_port_and_from() {
        let cfg = KindleConfig::new(
            Some("smtp.example.com".into()),
            None,
            Some("me@example.com".into()),
            Some("pw".into()),
            None,
            Some("me@kindle.com".into()),
        )
        .unwrap();
        assert_eq!(cfg.smtp_port, 587);
        // from defaults to the username.
        assert_eq!(cfg.from_address, "me@example.com");
    }

    #[test]
    fn kindle_config_blank_strings_treated_as_unset() {
        let cfg = KindleConfig::new(
            Some(" smtp.example.com ".into()),
            Some("  ".into()),
            Some("me@example.com".into()),
            Some("pw".into()),
            Some("  ".into()),
            Some("me@kindle.com".into()),
        )
        .unwrap();
        assert_eq!(cfg.smtp_host, "smtp.example.com"); // trimmed
        assert_eq!(cfg.smtp_port, 587); // blank port → default
        assert_eq!(cfg.from_address, "me@example.com"); // blank from → username
    }
}
