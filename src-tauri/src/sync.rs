//! Synchronisation over the Google Reader compatible API.
//!
//! Supports any GReader-compatible backend; today FreshRSS and Miniflux.
//! Protocol is identical (`ClientLogin`, `reader/api/0/edit-tag`,
//! `stream/contents/...`, `com.google/*` tags) — only the API root path
//! differs per provider, so the `Provider` enum centralises that mapping.
//!
//! Flow: `ClientLogin` for an auth token, push any queued local read/starred
//! changes via `edit-tag`, then pull the subscription list (to subscribe to
//! new feeds) and the recent reading-list (to reconcile read/starred state,
//! matched to local articles by URL).

use crate::db;
use crate::error::{AppError, AppResult};
use crate::ingestion::parse;
use crate::state::AppState;
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;
use tauri::{AppHandle, Manager};

const READ_TAG: &str = "user/-/state/com.google/read";
const STARRED_TAG: &str = "user/-/state/com.google/starred";
const READING_LIST: &str = "user/-/state/com.google/reading-list";

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

/// Exchange username + password for a long-lived auth token.
async fn client_login(http: &Client, base: &str, user: &str, pass: &str) -> AppResult<String> {
    let resp = http
        .post(format!("{base}/accounts/ClientLogin"))
        .form(&[("Email", user), ("Passwd", pass)])
        .send()
        .await?;
    if !resp.status().is_success() {
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
    let token = http
        .get(format!("{base}/reader/api/0/token"))
        .header("Authorization", format!("GoogleLogin auth={auth}"))
        .send()
        .await?
        .error_for_status()
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
    }
}

#[derive(Deserialize)]
struct Contents {
    #[serde(default)]
    items: Vec<Item>,
}
#[derive(Deserialize)]
struct Item {
    id: String,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    canonical: Vec<Href>,
    #[serde(default)]
    alternate: Vec<Href>,
}
#[derive(Deserialize)]
struct Href {
    href: String,
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
    provider: Provider,
}

/// Stored GReader credentials, if a server is configured. The setting keys
/// are still named `freshrss_*` for backwards compatibility with installs
/// that predate multi-provider support — the values are provider-agnostic.
async fn creds(app: &AppHandle) -> AppResult<Option<Creds>> {
    let state = app.state::<AppState>();
    let conn = state.db.lock().await;
    let url = db::get_setting(&conn, "freshrss_url")?.unwrap_or_default();
    let user = db::get_setting(&conn, "freshrss_user")?.unwrap_or_default();
    let nonempty = |k| db::get_setting(&conn, k).map(|v| v.filter(|s| !s.is_empty()));
    let auth = nonempty("freshrss_auth")?;
    let legacy_pass = nonempty("freshrss_pass")?;
    let provider = Provider::from_setting(
        db::get_setting(&conn, "freshrss_provider")?.as_deref(),
    );
    if url.trim().is_empty() || user.is_empty() || (auth.is_none() && legacy_pass.is_none()) {
        return Ok(None);
    }
    Ok(Some(Creds { url, user, auth, legacy_pass, provider }))
}

/// The configured GReader server URL and provider, or `None` when not
/// connected.
pub async fn connected_url(app: &AppHandle) -> AppResult<Option<(String, String)>> {
    Ok(creds(app).await?.map(|c| (c.url, c.provider.as_str().to_string())))
}

/// Persist a verified connection, storing the auth token and never the
/// password (any legacy stored password is also cleared).
async fn persist_session(
    app: &AppHandle,
    url: &str,
    user: &str,
    auth: &str,
    provider: Provider,
) -> AppResult<()> {
    let state = app.state::<AppState>();
    let conn = state.db.lock().await;
    db::set_setting(&conn, "freshrss_url", url.trim())?;
    db::set_setting(&conn, "freshrss_user", user)?;
    db::set_setting(&conn, "freshrss_auth", auth)?;
    db::set_setting(&conn, "freshrss_pass", "")?;
    db::set_setting(&conn, "freshrss_provider", provider.as_str())?;
    Ok(())
}

/// Verify credentials against the server and, on success, persist them.
pub async fn connect(
    app: &AppHandle,
    url: &str,
    user: &str,
    pass: &str,
    provider: Option<&str>,
) -> AppResult<()> {
    let provider = Provider::from_setting(provider);
    let base = greader_base(url, provider);
    let http = app.state::<AppState>().http();
    let session = login(&http, &base, user, pass).await?; // verifies credentials
    persist_session(app, url, user, &session.auth, provider).await
}

/// Forget the stored GReader credentials.
pub async fn disconnect(app: &AppHandle) -> AppResult<()> {
    let state = app.state::<AppState>();
    let conn = state.db.lock().await;
    for key in [
        "freshrss_url",
        "freshrss_user",
        "freshrss_auth",
        "freshrss_pass",
        "freshrss_provider",
    ] {
        db::set_setting(&conn, key, "")?;
    }
    Ok(())
}

/// Run a full sync if a server is connected. Returns `true` when a sync
/// actually ran, so the caller can refresh the UI for the reconciled state.
pub async fn run_if_connected(app: &AppHandle) -> AppResult<bool> {
    if creds(app).await?.is_some() {
        sync_now(app).await.map(|_| true)
    } else {
        Ok(false)
    }
}

/// Push queued changes, then pull subscriptions and read/starred state.
/// Returns the number of local articles whose state was reconciled.
pub async fn sync_now(app: &AppHandle) -> AppResult<usize> {
    let creds = creds(app)
        .await?
        .ok_or_else(|| AppError::code("freshrssNotConnected"))?;
    let base = greader_base(&creds.url, creds.provider);
    let http = app.state::<AppState>().http();
    let session = match &creds.auth {
        Some(auth) => session_with_token(&http, &base, auth.clone()).await?,
        None => {
            // Legacy install: exchange the plaintext password for a token,
            // then migrate so the password is no longer kept on disk.
            let pass = creds.legacy_pass.as_deref().unwrap_or_default();
            let session = login(&http, &base, &creds.user, pass).await?;
            persist_session(app, &creds.url, &creds.user, &session.auth, creds.provider).await?;
            session
        }
    };

    // 1 ── push: flush queued local read/starred changes. `take_sync_queue`
    // removes the rows up front, so any push that fails must be re-queued —
    // otherwise a network blip silently drops the user's change forever.
    let queue = {
        let state = app.state::<AppState>();
        let conn = state.db.lock().await;
        db::take_sync_queue(&conn)?
    };
    let mut failed: Vec<db::SyncEntry> = Vec::new();
    for entry in queue {
        let tag = if entry.field == "starred" {
            STARRED_TAG
        } else {
            READ_TAG
        };
        let action = if entry.value { "a" } else { "r" };
        let pushed = session
            .post(&http, "edit-tag")
            .form(&[
                ("i", entry.remote_id.as_str()),
                (action, tag),
                ("T", session.token.as_str()),
            ])
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .is_ok();
        if !pushed {
            failed.push(entry);
        }
    }
    if !failed.is_empty() {
        log::warn!("sync: {} change(s) failed to push, re-queued", failed.len());
        let state = app.state::<AppState>();
        let conn = state.db.lock().await;
        for entry in &failed {
            let _ = db::requeue_sync(&conn, entry.article_id, &entry.field, entry.value);
        }
    }

    // 2 ── pull subscriptions: subscribe locally to any feed we don't have.
    let subs: SubList = session
        .get(&http, "subscription/list?output=json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    {
        let state = app.state::<AppState>();
        let conn = state.db.lock().await;
        for sub in subs.subscriptions {
            // Resolve the server-side folder (GReader "label") before moving
            // `url` out of `sub`, mapping it onto a local folder by name.
            let folder_id = sub
                .categories
                .iter()
                .find_map(SubCat::folder_name)
                .map(|name| db::folder_id_by_name(&conn, &name))
                .transpose()?;
            let Some(feed_url) = sub.url.filter(|u| !u.is_empty()) else {
                continue;
            };
            match db::find_feed_by_url(&conn, &feed_url)? {
                None => {
                    let title = sub.title.unwrap_or_else(|| feed_url.clone());
                    let st = parse::detect_source_type(&feed_url);
                    let _ = db::insert_feed(&conn, &feed_url, None, &title, None, st, folder_id);
                }
                // Reconcile the folder for a feed we already track, but only
                // when it isn't filed locally yet — don't yank a feed the user
                // has organised by hand back into the server's folder.
                Some(id) => {
                    if let Some(folder_id) = folder_id {
                        if db::feed_folder_id(&conn, id)?.is_none() {
                            db::move_feed(&conn, id, Some(folder_id))?;
                        }
                    }
                }
            }
        }
    }

    // 3 ── pull read/starred state for recent items, matched by URL.
    let contents: Contents = session
        .get(
            &http,
            &format!("stream/contents/{READING_LIST}?output=json&n=1000"),
        )
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let mut reconciled = 0usize;
    {
        let state = app.state::<AppState>();
        let conn = state.db.lock().await;
        // Articles with still-unsent local edits keep their local state; we
        // only assign their remote id so the next sync can push them.
        let pending: std::collections::HashSet<i64> =
            db::pending_sync_article_ids(&conn)?.into_iter().collect();
        for item in contents.items {
            let url = item
                .canonical
                .first()
                .or_else(|| item.alternate.first())
                .map(|h| h.href.clone());
            let Some(url) = url else { continue };
            if let Some(aid) = db::article_id_by_url(&conn, &url)? {
                db::set_remote_id(&conn, aid, &item.id)?;
                if !pending.contains(&aid) {
                    let read = item.categories.iter().any(|c| c == READ_TAG);
                    let starred = item.categories.iter().any(|c| c == STARRED_TAG);
                    db::set_sync_state(&conn, aid, read, starred)?;
                }
                reconciled += 1;
            }
        }
    }
    Ok(reconciled)
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
            cat("user/-/label/Tech", Some("Tech")).folder_name().as_deref(),
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
        assert_eq!(cat("user/-/state/com.google/read", None).folder_name(), None);
        assert_eq!(cat("", Some("   ")).folder_name(), None);
    }
}
