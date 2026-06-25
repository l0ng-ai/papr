//! Email newsletter ingestion (feature F5, Part B).
//!
//! Competitors offer a hosted "dedicated address" for newsletters; that needs
//! a mail server Papr — being local-first — does not run. The local-first
//! equivalent is to **poll an IMAP mailbox** the user already owns (a Gmail
//! label, a Fastmail folder, …) and turn each message into an article.
//!
//! ## Design
//!
//! A newsletter source is stored as a normal row in the `feeds` table
//! (`source_type = 'newsletter'`) so it shows up in the sidebar, carries
//! articles, and participates in retention/search exactly like an RSS feed —
//! no parallel UI is needed. Its IMAP connection details live in a dedicated
//! `newsletter_sources` table keyed by `feed_id` (see migration #10 in
//! `db.rs`). The `feed_url` of the backing feed row is a synthetic
//! `imap://user@host/folder` string purely so the `UNIQUE(feed_url)`
//! constraint stops the same mailbox being added twice — it is never fetched
//! over HTTP. The IMAP app-password is stored in that table in plaintext, the
//! same approach `freshrss_connect` takes for sync credentials (the whole DB
//! is local to the user's machine).
//!
//! ## Testability
//!
//! The email → article conversion ([`email_to_article`]) is a pure function
//! over raw RFC822 bytes, exhaustively unit-tested below. The live IMAP
//! connection ([`fetch_recent`]) is the only impure part and is isolated so
//! the parsing layer can be tested without a network or a mail account.

use crate::db::NewArticle;
use crate::error::{AppError, AppResult};
use crate::models::Enclosure;
use crate::sanitize;
use mail_parser::{MessageParser, MimeHeaders};

/// IMAP connection details for one polled newsletter mailbox.
#[derive(Debug, Clone)]
pub struct NewsletterConfig {
    /// IMAP server host, e.g. `imap.gmail.com`.
    pub host: String,
    /// IMAP port — almost always 993 (implicit TLS).
    pub port: u16,
    /// Login username (usually the full email address).
    pub username: String,
    /// App-specific password / token.
    pub password: String,
    /// Mailbox/folder to poll, e.g. `INBOX` or `Newsletters`.
    pub folder: String,
}

/// A converted email, ready to be persisted as an article.
pub struct ParsedEmail {
    /// The `From` display name, falling back to the address — used as author.
    /// Surfaced separately from `article.author` so callers can group / label
    /// by sender (the test suite asserts on it).
    #[allow(dead_code)]
    pub from_name: String,
    /// The bare `From` email address.
    #[allow(dead_code)]
    pub from_addr: String,
    /// The article body — sanitized HTML when the mail had an HTML part,
    /// otherwise plain text wrapped as a `<pre>` block.
    pub article: NewArticle,
}

/// Convert one raw RFC822 message into an article.
///
/// Pure and network-free. Mapping:
/// - `Subject`            → title
/// - `From` display name  → author (address used when there is no name)
/// - HTML body (or text)  → sanitized content
/// - `Date`               → `published_at` (RFC 3339); `None` when absent
/// - `Message-ID`         → guid (falls back to a content hash)
///
/// Returns `None` only when the bytes do not parse as an email at all.
pub fn email_to_article(raw: &[u8]) -> Option<ParsedEmail> {
    let msg = MessageParser::default().parse(raw)?;

    let subject = msg.subject().unwrap_or("(no subject)").trim();
    let title = if subject.is_empty() {
        "(no subject)".to_string()
    } else {
        subject.to_string()
    };

    // ── Sender: prefer the display name, fall back to the address. ──
    let (from_name, from_addr) = match msg.from().and_then(|addr| addr.first()) {
        Some(a) => {
            let addr = a.address().unwrap_or("").trim().to_string();
            let name = a
                .name()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| addr.clone());
            (name, addr)
        }
        None => ("Unknown sender".to_string(), String::new()),
    };

    // ── Body: a genuine HTML part wins; otherwise wrap the plain-text part.
    // `body_html` / `html_bodies` would synthesize HTML from a text-only
    // mail, so we look for a part whose decoded type is actually `Html`. ──
    let has_html_part = msg
        .html_bodies()
        .any(|p| matches!(p.body, mail_parser::PartType::Html(_)));
    let (content_html, body_text) = if has_html_part {
        match msg.body_html(0) {
            Some(html) => {
                let sanitized = sanitize::sanitize(&html, None);
                let text = sanitize::html_to_text(&html);
                (Some(sanitized), text)
            }
            None => (None, String::new()),
        }
    } else if let Some(text) = msg.body_text(0) {
        // Plain-text newsletter: preserve its line breaks by wrapping the
        // (HTML-escaped) text in a <pre> block.
        let text = text.trim().to_string();
        let escaped = sanitize::escape_html(&text);
        (Some(format!("<pre>{escaped}</pre>")), text)
    } else {
        (None, String::new())
    };

    // ── Date → RFC 3339. `mail-parser` already decodes RFC 2822 dates. A
    // future-dated `Date:` header (clock-wrong sender, spam) would otherwise
    // pin the message to the top of the newest-first list forever, so clamp
    // it the same way the RSS path does in `parse::clamp_publish_date`. ──
    let published_at = msg
        .date()
        .map(|d| d.to_rfc3339())
        .filter(|s| !s.is_empty())
        .map(|s| match chrono::DateTime::parse_from_rfc3339(&s) {
            Ok(dt) => crate::ingestion::parse::clamp_publish_date(dt.with_timezone(&chrono::Utc))
                .to_rfc3339(),
            Err(_) => s,
        });

    // ── GUID: Message-ID is globally unique; hash the body if missing. ──
    let guid = msg
        .message_id()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("papr-newsletter-{}", stable_hash(raw)));

    // ── Attachments → enclosures (so podcast-style audio mails still work). ──
    let enclosures: Vec<Enclosure> = msg
        .attachments()
        .filter_map(|att| {
            let name = att.attachment_name()?;
            let mime = att
                .content_type()
                .map(|c| {
                    let mut t = c.ctype().to_string();
                    if let Some(sub) = c.subtype() {
                        t.push('/');
                        t.push_str(sub);
                    }
                    t
                });
            // Emails embed attachment bytes inline; we only surface a
            // descriptive pseudo-URL (there is nothing to link to).
            Some(Enclosure {
                url: format!("cid:{name}"),
                mime_type: mime,
                length: Some(att.body.len() as i64),
            })
        })
        .collect();

    let summary = if body_text.is_empty() {
        None
    } else {
        Some(body_text.chars().take(280).collect())
    };

    let article = NewArticle {
        guid,
        url: None,
        title,
        author: Some(from_name.clone()),
        summary,
        content_html,
        body_text,
        image_url: None,
        published_at,
        enclosures,
    };

    Some(ParsedEmail {
        from_name,
        from_addr,
        article,
    })
}

/// A deterministic 64-bit hash, used to synthesize a guid for an email that
/// carries no `Message-ID` header (so re-polling the same mail still dedups).
fn stable_hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Synthetic feed URL for a newsletter source. Not fetched over HTTP — it only
/// gives the `feeds.feed_url UNIQUE` constraint a value so the same mailbox
/// cannot be added twice.
pub fn synthetic_feed_url(cfg: &NewsletterConfig) -> String {
    format!(
        "imap://{}@{}:{}/{}",
        cfg.username, cfg.host, cfg.port, cfg.folder
    )
}

// ─────────────────────────── live IMAP ───────────────────────────

/// Connect to the IMAP server over implicit TLS and return the raw RFC822
/// bytes of the most recent `limit` messages in `cfg.folder`.
///
/// This is the **only** impure function in this module. It is a synchronous,
/// blocking call (the `imap` crate is blocking) — callers must run it on a
/// blocking thread (`spawn_blocking`). Parsing the returned bytes is delegated
/// to the pure [`email_to_article`].
pub fn fetch_recent(cfg: &NewsletterConfig, limit: usize) -> AppResult<Vec<Vec<u8>>> {
    // Implicit-TLS IMAP (port 993). `imap::ClientBuilder` wraps the TLS
    // handshake; `rustls` is used so there is no OpenSSL system dependency.
    let client = imap::ClientBuilder::new(&cfg.host, cfg.port)
        .connect()
        .map_err(|e| AppError::other(format!("IMAP connect failed: {e}")))?;

    let mut session = client
        .login(&cfg.username, &cfg.password)
        .map_err(|(e, _)| AppError::other(format!("IMAP login failed: {e}")))?;

    let mailbox = session
        .select(&cfg.folder)
        .map_err(|e| AppError::other(format!("IMAP select '{}' failed: {e}", cfg.folder)))?;

    let total = mailbox.exists;
    let mut bodies: Vec<Vec<u8>> = Vec::new();
    if total > 0 {
        // Fetch the last `limit` messages by sequence number (newest tail).
        let start = total.saturating_sub(limit as u32) + 1;
        let range = format!("{start}:{total}");
        let messages = session
            .fetch(range, "RFC822")
            .map_err(|e| AppError::other(format!("IMAP fetch failed: {e}")))?;
        for msg in messages.iter() {
            if let Some(body) = msg.body() {
                bodies.push(body.to_vec());
            }
        }
    }

    let _ = session.logout();
    Ok(bodies)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A complete plain-text email.
    const PLAIN: &[u8] = b"From: Jane Doe <jane@example.com>\r\n\
Subject: Weekly digest\r\n\
Date: Tue, 15 Nov 2022 10:30:00 +0000\r\n\
Message-ID: <abc123@example.com>\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hello there, this is the body of the newsletter.\r\n";

    #[test]
    fn parses_plain_text_email() {
        let p = email_to_article(PLAIN).expect("should parse");
        assert_eq!(p.article.title, "Weekly digest");
        assert_eq!(p.from_name, "Jane Doe");
        assert_eq!(p.from_addr, "jane@example.com");
        assert_eq!(p.article.author.as_deref(), Some("Jane Doe"));
        assert!(p.article.body_text.contains("body of the newsletter"));
        assert_eq!(p.article.guid, "abc123@example.com");
        assert!(p.article.published_at.is_some());
        // Plain text is wrapped in <pre>.
        assert!(p.article.content_html.as_deref().unwrap().contains("<pre>"));
    }

    #[test]
    fn parses_multipart_html_email() {
        let raw = b"From: News <news@site.com>\r\n\
Subject: HTML newsletter\r\n\
Date: Wed, 01 Jan 2025 12:00:00 +0000\r\n\
Message-ID: <html1@site.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=\"BOUND\"\r\n\
\r\n\
--BOUND\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
plain fallback\r\n\
--BOUND\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body><h1>Big News</h1><p>An <b>HTML</b> body.</p><script>alert(1)</script></body></html>\r\n\
--BOUND--\r\n";
        let p = email_to_article(raw).expect("should parse");
        assert_eq!(p.article.title, "HTML newsletter");
        let html = p.article.content_html.as_deref().unwrap();
        // HTML part is preferred and sanitized (no <script>).
        assert!(html.contains("Big News"));
        assert!(!html.contains("<script>"));
        // body_text is the text rendering of the HTML part.
        assert!(p.article.body_text.contains("Big News"));
        assert!(p.article.body_text.contains("HTML body"));
    }

    #[test]
    fn far_future_date_is_clamped_to_now() {
        // A `Date:` header decades in the future must not pin the message to
        // the top of the list forever — it is clamped down to ~now.
        let raw = b"From: a@b.com\r\n\
Subject: From the future\r\n\
Date: Mon, 01 Jan 2099 12:00:00 +0000\r\n\
Message-ID: <future@b.com>\r\n\
Content-Type: text/plain\r\n\
\r\n\
body\r\n";
        let p = email_to_article(raw).expect("parse");
        let published = p.article.published_at.expect("has a date");
        let parsed = chrono::DateTime::parse_from_rfc3339(&published).expect("rfc3339");
        // Clamped to roughly "now", well short of the year-2099 header.
        assert!(parsed.with_timezone(&chrono::Utc) < chrono::Utc::now() + chrono::Duration::days(2));
    }

    #[test]
    fn missing_date_yields_none_published_at() {
        let raw = b"From: x@y.com\r\n\
Subject: No date here\r\n\
Message-ID: <nodate@y.com>\r\n\
Content-Type: text/plain\r\n\
\r\n\
body\r\n";
        let p = email_to_article(raw).expect("should parse");
        assert_eq!(p.article.published_at, None);
        assert_eq!(p.article.title, "No date here");
    }

    #[test]
    fn missing_message_id_synthesizes_stable_guid() {
        let raw = b"From: a@b.com\r\n\
Subject: No id\r\n\
Content-Type: text/plain\r\n\
\r\n\
some content\r\n";
        let p1 = email_to_article(raw).expect("parse");
        let p2 = email_to_article(raw).expect("parse");
        assert!(p1.article.guid.starts_with("papr-newsletter-"));
        // Deterministic: the same bytes always hash to the same guid.
        assert_eq!(p1.article.guid, p2.article.guid);
    }

    #[test]
    fn sender_without_display_name_uses_address() {
        let raw = b"From: bare@example.org\r\n\
Subject: Bare sender\r\n\
Content-Type: text/plain\r\n\
\r\n\
hi\r\n";
        let p = email_to_article(raw).expect("parse");
        assert_eq!(p.from_name, "bare@example.org");
        assert_eq!(p.from_addr, "bare@example.org");
    }

    #[test]
    fn missing_subject_falls_back() {
        let raw = b"From: a@b.com\r\n\
Message-ID: <s@b.com>\r\n\
Content-Type: text/plain\r\n\
\r\n\
body\r\n";
        let p = email_to_article(raw).expect("parse");
        assert_eq!(p.article.title, "(no subject)");
    }

    #[test]
    fn encoded_word_subject_is_decoded() {
        // RFC 2047 encoded-word: "=?utf-8?B?...?=" → "Héllo"
        let raw = b"From: a@b.com\r\n\
Subject: =?utf-8?B?SMOpbGxv?=\r\n\
Message-ID: <enc@b.com>\r\n\
Content-Type: text/plain\r\n\
\r\n\
body\r\n";
        let p = email_to_article(raw).expect("parse");
        assert_eq!(p.article.title, "Héllo");
    }

    #[test]
    fn quoted_printable_body_is_decoded() {
        let raw = b"From: a@b.com\r\n\
Subject: QP body\r\n\
Message-ID: <qp@b.com>\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
Caf=C3=A9 time=21\r\n";
        let p = email_to_article(raw).expect("parse");
        assert!(
            p.article.body_text.contains("Café time!"),
            "body was: {:?}",
            p.article.body_text
        );
    }

    #[test]
    fn latin1_charset_body_is_decoded() {
        // ISO-8859-1 byte 0xE9 == 'é'.
        let mut raw: Vec<u8> = b"From: a@b.com\r\n\
Subject: Latin1\r\n\
Message-ID: <l1@b.com>\r\n\
Content-Type: text/plain; charset=iso-8859-1\r\n\
\r\n\
caf"
        .to_vec();
        raw.push(0xE9);
        raw.extend_from_slice(b"\r\n");
        let p = email_to_article(&raw).expect("parse");
        assert!(
            p.article.body_text.contains("café"),
            "body was: {:?}",
            p.article.body_text
        );
    }

    #[test]
    fn html_special_chars_escaped_in_plain_wrap() {
        let raw = b"From: a@b.com\r\n\
Subject: Angle brackets\r\n\
Message-ID: <ab@b.com>\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
use <div> & such\r\n";
        let p = email_to_article(raw).expect("parse");
        let html = p.article.content_html.as_deref().unwrap();
        assert!(html.contains("&lt;div&gt;"));
        assert!(html.contains("&amp;"));
    }

    #[test]
    fn garbage_bytes_still_parse_as_degenerate_email() {
        // mail-parser is lenient; arbitrary bytes parse as a body-only email.
        // We only require it not to panic and to produce some article.
        let p = email_to_article(b"not really an email");
        assert!(p.is_some());
    }

    #[test]
    fn synthetic_url_is_unique_per_mailbox() {
        let cfg = NewsletterConfig {
            host: "imap.gmail.com".into(),
            port: 993,
            username: "me@gmail.com".into(),
            password: "secret".into(),
            folder: "Newsletters".into(),
        };
        assert_eq!(
            synthetic_feed_url(&cfg),
            "imap://me@gmail.com@imap.gmail.com:993/Newsletters"
        );
    }
}
