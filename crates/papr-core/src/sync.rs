//! Synchronisation over the Google Reader compatible API.
//!
//! Supports any GReader-compatible backend; today FreshRSS and Miniflux.
//! Protocol is identical (`ClientLogin`, `reader/api/0/edit-tag`,
//! `stream/contents/...`, `com.google/*` tags) — only the API root path
//! differs per provider, so the `Provider` enum centralises that mapping.
//!
//! Flow: `ClientLogin` for an auth token, push any queued local read/starred
//! changes via `edit-tag`, pull the subscription list (to subscribe locally to
//! new server feeds) and push any local-only feeds back to the server (so the
//! two subscription lists converge rather than drifting), then pull the recent
//! reading-list (to reconcile read/starred state, matched to local articles by
//! URL).

use crate::db;
use crate::error::{AppError, AppResult};
use crate::ingestion::parse;
use crate::sanitize;
use chrono::{TimeZone, Utc};
use reqwest::{Client, RequestBuilder};
use rusqlite::Connection;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::Mutex;

/// The writer connection behind an async mutex — what every sync function reads
/// and writes through. The desktop app passes `state.db`; the CLI passes the
/// connection it opened. Paired with a shared [`Client`] for the HTTP calls.
type Db = Mutex<Connection>;

const READ_TAG: &str = "user/-/state/com.google/read";
const STARRED_TAG: &str = "user/-/state/com.google/starred";
const READING_LIST: &str = "user/-/state/com.google/reading-list";

fn has_state_tag(categories: &[String], tag: &str) -> bool {
    categories
        .iter()
        .any(|c| c == tag || c.ends_with(tag.trim_start_matches("user/-")))
}

/// Which GReader-compatible backend the user is connected to. The wire
/// protocol is identical; only where the API root sits under the server URL
/// differs (FreshRSS mounts it at `/api/greader.php`, Miniflux serves it at
/// the server root).
#[derive(Clone, Copy)]
enum Provider {
    FreshRss,
    Miniflux,
}

impl Provider {
    /// Path segment to append to the user-supplied server URL to reach the
    /// GReader API root. Miniflux serves `/accounts/ClientLogin` and
    /// `/reader/api/0/...` straight off the server root, so its suffix is
    /// empty.
    fn path_suffix(self) -> &'static str {
        match self {
            Provider::FreshRss => "/api/greader.php",
            Provider::Miniflux => "",
        }
    }

    /// Parse the persisted setting. Missing / unknown → FreshRss, so older
    /// installs (where this setting didn't exist) keep working unchanged.
    fn from_setting(s: Option<&str>) -> Self {
        match s.unwrap_or("").trim() {
            "miniflux" => Provider::Miniflux,
            _ => Provider::FreshRss,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Provider::FreshRss => "freshrss",
            Provider::Miniflux => "miniflux",
        }
    }
}

/// Normalise a user-supplied server URL to its GReader API root for the
/// chosen provider. Idempotent: if the user already typed the full path,
/// don't append it again.
fn greader_base(url: &str, provider: Provider) -> String {
    let t = url.trim().trim_end_matches('/');
    let suffix = provider.path_suffix();
    if t.ends_with(suffix) || t.contains(&format!("{suffix}/")) {
        t.to_string()
    } else {
        format!("{t}{suffix}")
    }
}

/// An authenticated FreshRSS session.
struct Session {
    base: String,
    auth: String,
    token: String,
}

impl Session {
    fn get(&self, http: &Client, path: &str) -> RequestBuilder {
        http.get(format!("{}/reader/api/0/{path}", self.base))
            .header("Authorization", format!("GoogleLogin auth={}", self.auth))
    }
    fn post(&self, http: &Client, path: &str) -> RequestBuilder {
        http.post(format!("{}/reader/api/0/{path}", self.base))
            .header("Authorization", format!("GoogleLogin auth={}", self.auth))
    }
}

async fn send_ok(req: RequestBuilder, label: &str) -> AppResult<reqwest::Response> {
    let resp = match req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            log::warn!("sync: {label} request failed: {e}");
            return Err(e.into());
        }
    };
    let status = resp.status();
    if !status.is_success() {
        log::warn!("sync: {label} failed: status={status}");
    }
    Ok(resp.error_for_status()?)
}

async fn json_ok<T: DeserializeOwned>(resp: reqwest::Response, label: &str) -> AppResult<T> {
    match resp.json().await {
        Ok(value) => Ok(value),
        Err(e) => {
            log::warn!("sync: {label} JSON decode failed: {e}");
            Err(e.into())
        }
    }
}

async fn list_subscriptions(session: &Session, http: &Client, label: &str) -> AppResult<SubList> {
    json_ok(
        send_ok(
            session.get(http, "subscription/list?output=json"),
            &format!("GET subscription/list {label}"),
        )
        .await?,
        &format!("subscription/list {label}"),
    )
    .await
}

fn subscription_stream(sub: &Sub, fallback_url: &str) -> String {
    if sub.id.is_empty() {
        format!("feed/{fallback_url}")
    } else {
        sub.id.clone()
    }
}

fn label_tag(name: &str) -> String {
    format!("user/-/label/{}", name.trim())
}

fn folder_tag(cat: &SubCat) -> Option<String> {
    cat.folder_name().map(|name| {
        if cat.id.contains("/label/") && !cat.id.is_empty() {
            cat.id.clone()
        } else {
            label_tag(&name)
        }
    })
}

async fn subscribe_url(session: &Session, http: &Client, url: &str, label: &str) -> AppResult<()> {
    let stream = format!("feed/{url}");
    send_ok(
        session.post(http, "subscription/edit").form(&[
            ("ac", "subscribe"),
            ("s", stream.as_str()),
            ("T", session.token.as_str()),
        ]),
        &format!("POST subscription/edit subscribe {label}"),
    )
    .await?;
    Ok(())
}

async fn set_subscription_folder(
    session: &Session,
    http: &Client,
    stream: &str,
    remove: &[String],
    folder: Option<&str>,
    label: &str,
) -> AppResult<()> {
    let mut form = vec![
        ("ac".to_string(), "edit".to_string()),
        ("s".to_string(), stream.to_string()),
        ("T".to_string(), session.token.clone()),
    ];
    if let Some(folder) = folder.map(str::trim).filter(|s| !s.is_empty()) {
        form.push(("a".to_string(), label_tag(folder)));
    }
    for tag in remove {
        form.push(("r".to_string(), tag.clone()));
    }
    send_ok(
        session.post(http, "subscription/edit").form(&form),
        &format!("POST subscription/edit folder {label}"),
    )
    .await?;
    Ok(())
}

async fn unsubscribe_stream(
    session: &Session,
    http: &Client,
    stream: &str,
    label: &str,
) -> AppResult<()> {
    send_ok(
        session.post(http, "subscription/edit").form(&[
            ("ac", "unsubscribe"),
            ("s", stream),
            ("T", session.token.as_str()),
        ]),
        &format!("POST subscription/edit unsubscribe {label}"),
    )
    .await?;
    Ok(())
}

async fn push_state(
    session: &Session,
    http: &Client,
    remote_id: &str,
    field: &str,
    value: bool,
) -> AppResult<()> {
    let tag = if field == "starred" {
        STARRED_TAG
    } else {
        READ_TAG
    };
    let action = if value { "a" } else { "r" };
    send_ok(
        session.post(http, "edit-tag").form(&[
            ("i", remote_id),
            (action, tag),
            ("T", session.token.as_str()),
        ]),
        "POST edit-tag",
    )
    .await?;
    Ok(())
}

/// Exchange username + password for a long-lived auth token.
async fn client_login(http: &Client, base: &str, user: &str, pass: &str) -> AppResult<String> {
    let resp = match http
        .post(format!("{base}/accounts/ClientLogin"))
        .form(&[("Email", user), ("Passwd", pass)])
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            log::warn!("sync: ClientLogin request failed for user {user}: {e}");
            return Err(e.into());
        }
    };
    if !resp.status().is_success() {
        log::warn!(
            "sync: ClientLogin failed for user {user}: status={}",
            resp.status()
        );
        return Err(AppError::code("freshrssLoginFailed"));
    }
    let body = resp.text().await?;
    body.lines()
        .find_map(|l| l.strip_prefix("Auth="))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::code("freshrssNoToken"))
}

/// Build a session from an existing auth token by fetching a fresh write
/// (edit-tag) token. Fails fast if the auth token is no longer valid.
async fn session_with_token(http: &Client, base: &str, auth: String) -> AppResult<Session> {
    let resp = http
        .get(format!("{base}/reader/api/0/token"))
        .header("Authorization", format!("GoogleLogin auth={auth}"));
    let token = send_ok(resp, "GET token")
        .await
        .map_err(|_| AppError::code("freshrssLoginFailed"))?
        .text()
        .await?
        .trim()
        .to_string();
    Ok(Session {
        base: base.to_string(),
        auth,
        token,
    })
}

/// Log in with username + password and obtain a full session.
async fn login(http: &Client, base: &str, user: &str, pass: &str) -> AppResult<Session> {
    let auth = client_login(http, base, user, pass).await?;
    session_with_token(http, base, auth).await
}

#[derive(Deserialize)]
struct SubList {
    #[serde(default)]
    subscriptions: Vec<Sub>,
}
#[derive(Deserialize)]
struct Sub {
    #[serde(default)]
    id: String,
    url: Option<String>,
    title: Option<String>,
    #[serde(default)]
    categories: Vec<SubCat>,
}
/// A GReader category ("label") a subscription belongs to. FreshRSS/Miniflux
/// folders surface here; we map the first named one onto a local folder.
#[derive(Deserialize)]
struct SubCat {
    #[serde(default)]
    id: String,
    #[serde(default)]
    label: Option<String>,
}
impl SubCat {
    /// Human folder name for this category. Prefer the explicit `label`,
    /// otherwise derive it from the `user/-/label/NAME` id. `None` for an
    /// unnamed category, so it is skipped rather than creating a blank folder.
    ///
    /// FreshRSS files every feed the user hasn't categorised under a built-in
    /// "Uncategorized" label. That isn't a real folder — mapping it onto a
    /// local one buries every top-level feed in a junk folder that doesn't
    /// match the server's own presentation — so it is treated as no folder.
    fn folder_name(&self) -> Option<String> {
        self.label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                self.id
                    .rsplit_once("/label/")
                    .map(|(_, n)| n.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .filter(|n| !n.eq_ignore_ascii_case("Uncategorized"))
    }
}

#[derive(Deserialize)]
struct Contents {
    #[serde(default)]
    items: Vec<Item>,
}
#[derive(Deserialize)]
struct IdList {
    #[serde(default, rename = "itemRefs")]
    item_refs: Vec<ItemRef>,
}
#[derive(Deserialize)]
struct ItemRef {
    id: String,
}
#[derive(Deserialize)]
struct Item {
    id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    published: Option<i64>,
    #[serde(default)]
    summary: Option<ItemContent>,
    #[serde(default)]
    content: Option<ItemContent>,
    #[serde(default)]
    origin: Option<ItemOrigin>,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    canonical: Vec<Href>,
    #[serde(default)]
    alternate: Vec<Href>,
}
#[derive(Deserialize)]
struct ItemContent {
    #[serde(default)]
    content: String,
}
#[derive(Deserialize)]
struct ItemOrigin {
    #[serde(default, rename = "streamId")]
    stream_id: String,
}
#[derive(Deserialize)]
struct Href {
    href: String,
}

fn item_url(item: &Item) -> Option<String> {
    item.canonical
        .first()
        .or_else(|| item.alternate.first())
        .map(|h| h.href.trim().to_string())
        .filter(|u| !u.is_empty())
}

fn item_article(item: &Item, url: Option<String>) -> db::NewArticle {
    let html = item
        .content
        .as_ref()
        .or(item.summary.as_ref())
        .map(|c| c.content.trim().to_string())
        .filter(|s| !s.is_empty());
    let body_text = html
        .as_deref()
        .map(sanitize::html_to_text)
        .unwrap_or_default();
    let summary = item
        .summary
        .as_ref()
        .map(|s| sanitize::html_to_text(&s.content))
        .filter(|s| !s.is_empty());
    let published_at = item.published.and_then(|ts| {
        Utc.timestamp_opt(ts, 0)
            .single()
            .map(|dt| parse::clamp_publish_date(dt).to_rfc3339())
    });
    db::NewArticle {
        guid: item.id.clone(),
        url,
        title: item
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Untitled")
            .to_string(),
        author: item.author.clone().filter(|s| !s.trim().is_empty()),
        summary,
        content_html: html,
        body_text,
        image_url: None,
        published_at,
        enclosures: Vec::new(),
    }
}

/// Stored GReader connection. We persist the long-lived auth token rather
/// than the password — a leaked token is revocable server-side and can't be
/// replayed against the user's other accounts. `legacy_pass` holds a
/// plaintext password from an older install, awaiting one-time migration.
struct Creds {
    url: String,
    user: String,
    auth: Option<String>,
    legacy_pass: Option<String>,
    miniflux_api_key: Option<String>,
    provider: Provider,
}

/// Stored GReader credentials, if a server is configured. The setting keys
/// are still named `freshrss_*` for backwards compatibility with installs
/// that predate multi-provider support — the values are provider-agnostic.
async fn creds(db: &Db) -> AppResult<Option<Creds>> {
    let conn = db.lock().await;
    let url = db::get_setting(&conn, "freshrss_url")?.unwrap_or_default();
    let user = db::get_setting(&conn, "freshrss_user")?.unwrap_or_default();
    let nonempty = |k| db::get_setting(&conn, k).map(|v| v.filter(|s| !s.is_empty()));
    let auth = nonempty("freshrss_auth")?;
    let legacy_pass = nonempty("freshrss_pass")?;
    let miniflux_api_key = nonempty("miniflux_api_key")?;
    let provider = Provider::from_setting(db::get_setting(&conn, "freshrss_provider")?.as_deref());
    if url.trim().is_empty() || user.is_empty() || (auth.is_none() && legacy_pass.is_none()) {
        return Ok(None);
    }
    Ok(Some(Creds {
        url,
        user,
        auth,
        legacy_pass,
        miniflux_api_key,
        provider,
    }))
}

/// The configured GReader server URL and provider, or `None` when not
/// connected.
pub async fn connected_url(db: &Db) -> AppResult<Option<(String, String)>> {
    Ok(creds(db)
        .await?
        .map(|c| (c.url, c.provider.as_str().to_string())))
}

/// Persist a verified connection, storing the auth token and never the
/// password (any legacy stored password is also cleared).
async fn persist_session(
    db: &Db,
    url: &str,
    user: &str,
    auth: &str,
    provider: Provider,
) -> AppResult<()> {
    let conn = db.lock().await;
    db::set_setting(&conn, "freshrss_url", url.trim())?;
    db::set_setting(&conn, "freshrss_user", user)?;
    db::set_setting(&conn, "freshrss_auth", auth)?;
    db::set_setting(&conn, "freshrss_pass", "")?;
    db::set_setting(&conn, "freshrss_provider", provider.as_str())?;
    Ok(())
}

/// Verify credentials against the server and, on success, persist them.
pub async fn connect(
    db: &Db,
    http: &Client,
    url: &str,
    user: &str,
    pass: &str,
    provider: Option<&str>,
    api_key: Option<&str>,
) -> AppResult<()> {
    let provider = Provider::from_setting(provider);
    let base = greader_base(url, provider);
    let session = login(http, &base, user, pass).await?; // verifies credentials
    persist_session(db, url, user, &session.auth, provider).await?;
    if matches!(provider, Provider::Miniflux) {
        let conn = db.lock().await;
        db::set_setting(&conn, "miniflux_api_key", api_key.unwrap_or("").trim())?;
    }
    Ok(())
}

/// Forget the stored GReader credentials.
pub async fn disconnect(db: &Db) -> AppResult<()> {
    let conn = db.lock().await;
    for key in [
        "freshrss_url",
        "freshrss_user",
        "freshrss_auth",
        "freshrss_pass",
        "freshrss_provider",
        "miniflux_api_key",
    ] {
        db::set_setting(&conn, key, "")?;
    }
    Ok(())
}

/// Run a full sync if a server is connected. Returns `true` when a sync
/// actually ran, so the caller can refresh the UI for the reconciled state.
pub async fn run_if_connected(db: &Db, http: &Client) -> AppResult<bool> {
    if creds(db).await?.is_some() {
        sync_now(db, http).await.map(|_| true)
    } else {
        Ok(false)
    }
}

async fn session_from_creds(
    db: &Db,
    http: &Client,
    creds: &Creds,
    base: &str,
) -> AppResult<Session> {
    match &creds.auth {
        Some(auth) => session_with_token(http, base, auth.clone()).await,
        None => {
            let pass = creds.legacy_pass.as_deref().unwrap_or_default();
            let session = login(http, base, &creds.user, pass).await?;
            persist_session(db, &creds.url, &creds.user, &session.auth, creds.provider).await?;
            Ok(session)
        }
    }
}

/// Best-effort deletion of one server-side subscription by feed URL.
pub async fn unsubscribe_subscription_url(db: &Db, http: &Client, url: &str) -> AppResult<bool> {
    let Some(creds) = creds(db).await? else {
        log::info!("sync: no server connected; local unsubscribe only");
        return Ok(false);
    };
    let base = greader_base(&creds.url, creds.provider);
    let session = session_from_creds(db, http, &creds, &base).await?;
    let subs = list_subscriptions(&session, http, "for unsubscribe").await?;
    let Some(old) = subs
        .subscriptions
        .iter()
        .find(|s| s.url.as_deref() == Some(url))
    else {
        log::info!("sync: subscription URL not found on server: {url}");
        return Ok(false);
    };
    let stream = subscription_stream(old, url);
    unsubscribe_stream(&session, http, &stream, "feed-url").await?;
    log::info!("sync: unsubscribed server from feed: {url}");
    Ok(true)
}

/// Best-effort propagation for a local feed URL edit. Without this, the next
/// sync can pull the old server subscription back into the local DB as a
/// duplicate feed with the same title.
pub async fn replace_subscription_url(
    db: &Db,
    http: &Client,
    old_url: &str,
    new_url: &str,
) -> AppResult<bool> {
    let Some(creds) = creds(db).await? else {
        log::info!("sync: no server connected; local feed URL update only");
        return Ok(false);
    };
    let base = greader_base(&creds.url, creds.provider);
    let session = session_from_creds(db, http, &creds, &base).await?;

    let subs = list_subscriptions(&session, http, "for replace-url").await?;
    let Some(old) = subs
        .subscriptions
        .iter()
        .find(|s| s.url.as_deref() == Some(old_url))
    else {
        log::info!("sync: old feed URL not found on server: {old_url}");
        return Ok(false);
    };

    let stream = subscription_stream(old, old_url);
    unsubscribe_stream(&session, http, &stream, "replace-url").await?;
    subscribe_url(&session, http, new_url, "replace-url").await?;

    log::info!("sync: replaced server subscription URL: {old_url} -> {new_url}");
    Ok(true)
}

#[derive(Serialize)]
struct MinifluxCategoryCreate<'a> {
    title: &'a str,
}

#[derive(Deserialize)]
struct MinifluxCategory {
    id: i64,
    title: String,
}

#[derive(Serialize)]
struct MinifluxCategoryUpdate<'a> {
    title: &'a str,
}

fn miniflux_api(creds: &Creds) -> Option<(String, &str)> {
    if !matches!(creds.provider, Provider::Miniflux) {
        return None;
    }
    Some((
        creds.url.trim().trim_end_matches('/').to_string(),
        creds.miniflux_api_key.as_deref()?,
    ))
}

async fn miniflux_categories(creds: &Creds, http: &Client) -> AppResult<Vec<MinifluxCategory>> {
    let Some((base, api_key)) = miniflux_api(creds) else {
        return Ok(Vec::new());
    };
    json_ok(
        send_ok(
            http.get(format!("{base}/v1/categories"))
                .header("X-Auth-Token", api_key),
            "GET miniflux categories",
        )
        .await?,
        "miniflux categories",
    )
    .await
}

fn find_miniflux_category_id(categories: &[MinifluxCategory], name: &str) -> Option<i64> {
    let name = name.trim();
    categories
        .iter()
        .find(|c| c.title.trim().eq_ignore_ascii_case(name))
        .map(|c| c.id)
}

/// Create an empty Miniflux category. GReader labels only exist on
/// subscriptions, so empty local folders need Miniflux's native API.
pub async fn create_remote_folder(db: &Db, http: &Client, name: &str) -> AppResult<bool> {
    let Some(creds) = creds(db).await? else {
        log::info!("sync: no server connected; local folder create only");
        return Ok(false);
    };
    let Some((base, api_key)) = miniflux_api(&creds) else {
        if matches!(creds.provider, Provider::Miniflux) {
            log::warn!("sync: missing Miniflux API key; reconnect Miniflux to sync empty folders");
            return Ok(false);
        }
        log::info!("sync: provider has no native empty folder create");
        return Ok(false);
    };
    if find_miniflux_category_id(&miniflux_categories(&creds, http).await?, name).is_some() {
        return Ok(true);
    }
    send_ok(
        http.post(format!("{base}/v1/categories"))
            .header("X-Auth-Token", api_key)
            .json(&MinifluxCategoryCreate { title: name.trim() }),
        "POST miniflux categories",
    )
    .await?;
    log::info!("sync: created Miniflux category: {}", name.trim());
    Ok(true)
}

pub async fn rename_remote_folder(
    db: &Db,
    http: &Client,
    old_name: &str,
    new_name: &str,
) -> AppResult<bool> {
    let Some(creds) = creds(db).await? else {
        return Ok(false);
    };
    let Some((base, api_key)) = miniflux_api(&creds) else {
        return Ok(false);
    };
    let categories = miniflux_categories(&creds, http).await?;
    let Some(id) = find_miniflux_category_id(&categories, old_name) else {
        log::info!("sync: remote folder not found for rename: {old_name}");
        return Ok(false);
    };
    send_ok(
        http.put(format!("{base}/v1/categories/{id}"))
            .header("X-Auth-Token", api_key)
            .json(&MinifluxCategoryUpdate {
                title: new_name.trim(),
            }),
        "PUT miniflux category",
    )
    .await?;
    log::info!(
        "sync: renamed Miniflux category: {old_name} -> {}",
        new_name.trim()
    );
    Ok(true)
}

pub async fn delete_remote_folder(db: &Db, http: &Client, name: &str) -> AppResult<bool> {
    let Some(creds) = creds(db).await? else {
        return Ok(false);
    };
    let Some((base, api_key)) = miniflux_api(&creds) else {
        return Ok(false);
    };
    let categories = miniflux_categories(&creds, http).await?;
    let Some(id) = find_miniflux_category_id(&categories, name) else {
        log::info!("sync: remote folder not found for delete: {name}");
        return Ok(false);
    };
    send_ok(
        http.delete(format!("{base}/v1/categories/{id}"))
            .header("X-Auth-Token", api_key),
        "DELETE miniflux category",
    )
    .await?;
    log::info!("sync: deleted Miniflux category: {}", name.trim());
    Ok(true)
}

/// Pull Miniflux categories, including empty ones that never appear in
/// GReader subscription labels.
pub async fn sync_remote_folders(db: &Db, http: &Client) -> AppResult<bool> {
    let Some(creds) = creds(db).await? else {
        return Ok(false);
    };
    if matches!(creds.provider, Provider::Miniflux) && creds.miniflux_api_key.is_none() {
        log::warn!("sync: missing Miniflux API key; reconnect Miniflux to sync empty folders");
        return Ok(false);
    }
    let categories = miniflux_categories(&creds, http).await?;
    if categories.is_empty() {
        return Ok(false);
    }
    let mut imported = 0usize;
    {
        let conn = db.lock().await;
        for category in categories {
            if !category.title.trim().is_empty() {
                db::folder_id_by_name(&conn, &category.title)?;
                imported += 1;
            }
        }
    }
    log::info!("sync: synced Miniflux categories={imported}");
    Ok(true)
}

/// Best-effort propagation for moving a local feed into/out of a folder.
pub async fn set_subscription_folder_url(
    db: &Db,
    http: &Client,
    url: &str,
    folder: Option<&str>,
) -> AppResult<bool> {
    let Some(creds) = creds(db).await? else {
        log::info!("sync: no server connected; local feed folder update only");
        return Ok(false);
    };
    let base = greader_base(&creds.url, creds.provider);
    let session = session_from_creds(db, http, &creds, &base).await?;
    let subs = list_subscriptions(&session, http, "for folder").await?;
    let Some(sub) = subs
        .subscriptions
        .iter()
        .find(|s| s.url.as_deref() == Some(url))
    else {
        subscribe_url(&session, http, url, "folder-missing-feed").await?;
        set_subscription_folder(
            &session,
            http,
            &format!("feed/{url}"),
            &[],
            folder,
            "folder-new-feed",
        )
        .await?;
        return Ok(true);
    };
    let stream = subscription_stream(sub, url);
    let keep = folder.map(label_tag);
    let remove: Vec<String> = sub
        .categories
        .iter()
        .filter_map(folder_tag)
        .filter(|tag| keep.as_deref() != Some(tag.as_str()))
        .collect();
    set_subscription_folder(&session, http, &stream, &remove, folder, "folder").await?;
    log::info!("sync: updated server folder for feed: {url} -> {folder:?}");
    Ok(true)
}

async fn pull_contents(
    session: &Session,
    http: &Client,
    provider: Provider,
) -> AppResult<Contents> {
    if matches!(provider, Provider::Miniflux) {
        let ids: IdList = json_ok(
            send_ok(
                session.get(
                    http,
                    &format!("stream/items/ids?output=json&s={READING_LIST}&n=1000"),
                ),
                "GET stream/items/ids reading-list",
            )
            .await?,
            "stream/items/ids reading-list",
        )
        .await?;
        log::info!("sync: pulled {} remote item ids", ids.item_refs.len());
        if ids.item_refs.is_empty() {
            return Ok(Contents { items: Vec::new() });
        }
        let mut form = vec![
            ("T".to_string(), session.token.clone()),
            ("output".to_string(), "json".to_string()),
        ];
        for item in ids.item_refs {
            form.push(("i".to_string(), item.id));
        }
        return json_ok(
            send_ok(
                session.post(http, "stream/items/contents").form(&form),
                "POST stream/items/contents",
            )
            .await?,
            "stream/items/contents",
        )
        .await;
    }

    json_ok(
        send_ok(
            session.get(
                http,
                &format!("stream/contents/{READING_LIST}?output=json&n=1000"),
            ),
            "GET stream/contents reading-list",
        )
        .await?,
        "stream/contents reading-list",
    )
    .await
}

async fn push_queue(db: &Db, http: &Client, session: &Session, label: &str) -> AppResult<usize> {
    let queue = {
        let conn = db.lock().await;
        db::take_sync_queue(&conn)?
    };
    log::info!("sync: {label}: pushable queued changes={}", queue.len());
    let mut pushed = 0usize;
    let mut failed: Vec<db::SyncEntry> = Vec::new();
    for entry in queue {
        let ok = match push_state(session, http, &entry.remote_id, &entry.field, entry.value).await
        {
            Ok(_) => true,
            Err(e) => {
                log::warn!(
                    "sync: failed to push article state article_id={} remote_id={} field={} value={}: {e}",
                    entry.article_id,
                    entry.remote_id,
                    entry.field,
                    entry.value
                );
                false
            }
        };
        if ok {
            pushed += 1;
        } else {
            failed.push(entry);
        }
    }
    if !failed.is_empty() {
        log::warn!("sync: {} change(s) failed to push, re-queued", failed.len());
        let conn = db.lock().await;
        for entry in &failed {
            let _ = db::requeue_sync(&conn, entry.article_id, &entry.field, entry.value);
        }
    }
    Ok(pushed)
}

/// Local feed URLs the server doesn't already carry, so each can be subscribed
/// remotely. Pure set difference, factored out of `sync_now` so the selection
/// is unit-testable without a live server.
fn feeds_to_push<'a>(
    local: &'a [String],
    server: &std::collections::HashSet<String>,
) -> Vec<&'a str> {
    local
        .iter()
        .filter(|u| !server.contains(*u))
        .map(String::as_str)
        .collect()
}

/// Push queued changes, then pull subscriptions, remote articles, and
/// read/starred state. Returns the number of newly inserted unread articles.
pub async fn sync_now(db: &Db, http: &Client) -> AppResult<usize> {
    let creds = creds(db)
        .await?
        .ok_or_else(|| AppError::code("freshrssNotConnected"))?;
    let base = greader_base(&creds.url, creds.provider);
    log::info!(
        "sync: starting provider={} base={base}",
        creds.provider.as_str()
    );
    let session = match &creds.auth {
        Some(auth) => session_with_token(http, &base, auth.clone()).await?,
        None => {
            // Legacy install: exchange the plaintext password for a token,
            // then migrate so the password is no longer kept on disk.
            let pass = creds.legacy_pass.as_deref().unwrap_or_default();
            let session = login(http, &base, &creds.user, pass).await?;
            persist_session(db, &creds.url, &creds.user, &session.auth, creds.provider).await?;
            session
        }
    };

    // 1 ── push: flush queued local read/starred changes whose remote ids are
    // already known. Entries without remote ids stay queued until the pull
    // below maps them by URL.
    let pushed_before_pull = push_queue(db, http, &session, "before pull").await?;
    if let Err(e) = sync_remote_folders(db, http).await {
        log::warn!("sync: failed to sync remote folders: {e}");
    }

    // 2 ── pull subscriptions: subscribe locally to any feed we don't have and
    // keep a remote stream -> local feed map for the item pull below.
    let subs = list_subscriptions(&session, http, "").await?;
    let server_urls: std::collections::HashSet<String> = subs
        .subscriptions
        .iter()
        .filter_map(|s| s.url.clone())
        .filter(|u| !u.is_empty())
        .collect();
    log::info!("sync: server subscriptions={}", server_urls.len());
    let mut remote_feed_ids: HashMap<String, i64> = HashMap::new();
    {
        let conn = db.lock().await;
        for sub in subs.subscriptions {
            // Resolve the server-side folder (GReader "label") before moving
            // `url` out of `sub`, mapping it onto a local folder by name.
            let folder_id = sub
                .categories
                .iter()
                .find_map(SubCat::folder_name)
                .map(|name| db::folder_id_by_name(&conn, &name))
                .transpose()?;
            let Some(feed_url) = sub
                .url
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_string)
            else {
                continue;
            };
            let feed_id = match db::find_feed_by_url(&conn, &feed_url)? {
                None => {
                    let title = sub.title.clone().unwrap_or_else(|| feed_url.clone());
                    let st = parse::detect_source_type(&feed_url);
                    log::info!("sync: importing server subscription: {title} <{feed_url}>");
                    db::insert_feed(&conn, &feed_url, None, &title, None, st, folder_id)?
                }
                Some(id) => {
                    if db::feed_folder_id(&conn, id)? != folder_id {
                        db::move_feed(&conn, id, folder_id)?;
                    }
                    id
                }
            };
            let stream = subscription_stream(&sub, &feed_url);
            remote_feed_ids.insert(stream, feed_id);
            remote_feed_ids.insert(format!("feed/{feed_url}"), feed_id);
        }
    }

    // 2b ── push subscriptions: subscribe the server to any local feed it
    // doesn't have yet, so adding a feed in the app propagates to the server
    // instead of leaving the two sides to drift. Best-effort and idempotent —
    // re-subscribing a feed the server already has is a no-op there.
    let local_feed_targets = {
        let conn = db.lock().await;
        db::feed_sync_targets(&conn)?
    };
    let local_feed_urls: Vec<String> = local_feed_targets
        .iter()
        .map(|(url, _)| url.clone())
        .collect();
    for url in feeds_to_push(&local_feed_urls, &server_urls) {
        let pushed = match subscribe_url(&session, http, url, "local-only").await {
            Ok(_) => true,
            Err(e) => {
                log::warn!("sync: failed to subscribe server to {url}: {e}");
                false
            }
        };
        if pushed {
            log::info!("sync: subscribed server to local feed: {url}");
            let folder = local_feed_targets
                .iter()
                .find(|(u, _)| u == url)
                .and_then(|(_, folder)| folder.as_deref());
            if folder.is_some() {
                if let Err(e) = set_subscription_folder(
                    &session,
                    http,
                    &format!("feed/{url}"),
                    &[],
                    folder,
                    "local-only",
                )
                .await
                {
                    log::warn!("sync: failed to set server folder for {url}: {e}");
                }
            }
        }
    }

    // 3 ── pull recent remote items. The sync server is the source of truth in
    // connected mode, so new remote articles are inserted locally and remote
    // read/starred state overwrites local state.
    let contents = pull_contents(&session, http, creds.provider).await?;

    let mut reconciled = 0usize;
    let mut inserted = 0usize;
    {
        let conn = db.lock().await;
        for item in contents.items {
            let url = item_url(&item);
            let read = has_state_tag(&item.categories, READ_TAG);
            let starred = has_state_tag(&item.categories, STARRED_TAG);
            let feed_id = item
                .origin
                .as_ref()
                .and_then(|o| remote_feed_ids.get(&o.stream_id))
                .copied();
            let mut aid = if let Some(url) = url.as_deref() {
                db::article_id_by_url(&conn, url)?
            } else {
                None
            };
            if aid.is_none() {
                if let Some(feed_id) = feed_id {
                    aid = db::article_id_by_feed_guid(&conn, feed_id, &item.id)?;
                }
            }

            let aid = match (aid, feed_id) {
                (Some(aid), _) => aid,
                (None, Some(feed_id)) => {
                    let article = item_article(&item, url);
                    if db::upsert_article(&conn, feed_id, &article, false, &[])? && !read {
                        inserted += 1;
                    }
                    match db::article_id_by_feed_guid(&conn, feed_id, &item.id)? {
                        Some(aid) => aid,
                        None => continue,
                    }
                }
                (None, None) => {
                    log::debug!("sync: skipped remote item without known feed: {}", item.id);
                    continue;
                }
            };

            db::set_remote_id(&conn, aid, &item.id)?;
            db::set_sync_state(&conn, aid, read, starred)?;
            db::clear_sync_queue_for_article(&conn, aid)?;
            reconciled += 1;
        }
    }

    log::info!(
        "sync: finished; reconciled_articles={reconciled}; inserted_articles={inserted}; pushed_before_pull={pushed_before_pull}"
    );
    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(id: &str, label: Option<&str>) -> SubCat {
        SubCat {
            id: id.to_string(),
            label: label.map(str::to_string),
        }
    }

    #[test]
    fn folder_name_prefers_label() {
        assert_eq!(
            cat("user/-/label/Tech", Some("Tech"))
                .folder_name()
                .as_deref(),
            Some("Tech")
        );
    }

    #[test]
    fn folder_name_falls_back_to_label_id() {
        // Some servers omit the human label; derive it from the id instead.
        assert_eq!(
            cat("user/-/label/科技", None).folder_name().as_deref(),
            Some("科技")
        );
    }

    #[test]
    fn folder_name_skips_unnamed_categories() {
        // A state tag (not a label) or a blank label is not a folder.
        assert_eq!(
            cat("user/-/state/com.google/read", None).folder_name(),
            None
        );
        assert_eq!(cat("", Some("   ")).folder_name(), None);
    }

    #[test]
    fn folder_name_skips_freshrss_uncategorized() {
        // FreshRSS's built-in "Uncategorized" label is not a real folder, by
        // either label or id, and regardless of case.
        assert_eq!(
            cat("user/-/label/Uncategorized", Some("Uncategorized")).folder_name(),
            None
        );
        assert_eq!(cat("user/-/label/uncategorized", None).folder_name(), None);
    }

    #[test]
    fn feeds_to_push_selects_only_local_only_feeds() {
        let local = vec![
            "https://a.example/feed".to_string(),
            "https://b.example/feed".to_string(),
            "https://c.example/feed".to_string(),
        ];
        let server: std::collections::HashSet<String> =
            ["https://b.example/feed".to_string()].into_iter().collect();
        assert_eq!(
            feeds_to_push(&local, &server),
            vec!["https://a.example/feed", "https://c.example/feed"]
        );
    }

    #[test]
    fn feeds_to_push_empty_when_server_has_everything() {
        let local = vec!["https://a.example/feed".to_string()];
        let server: std::collections::HashSet<String> =
            ["https://a.example/feed".to_string()].into_iter().collect();
        assert!(feeds_to_push(&local, &server).is_empty());
    }

    #[test]
    fn state_tag_accepts_numeric_user_prefix() {
        let categories = vec![
            "user/123/state/com.google/read".to_string(),
            "user/-/state/com.google/starred".to_string(),
        ];
        assert!(has_state_tag(&categories, READ_TAG));
        assert!(has_state_tag(&categories, STARRED_TAG));
    }
}
