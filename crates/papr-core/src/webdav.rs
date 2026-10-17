//! WebDAV transport: compressed incremental chunks and a conditional root.
//! A strong ETag is required for existing files: unsupported concurrency
//! protection fails closed instead of overwriting another device's upload.
use crate::db::{
    self,
    webdav::{self as replication, Record},
};
use crate::error::{AppError, AppResult};
use reqwest::{header, Client, StatusCode};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::Mutex;
use url::Url;

mod chunks;
mod directory;
pub use directory::{directory_url, relocate};
mod network;
mod schedule;
pub use schedule::Options as ScheduleOptions;
mod progress;
mod transport;
pub(crate) use chunks::schema as chunk_schema;
use futures_util::{stream, StreamExt};
pub use network::client as http_client;
pub use network::Options as NetworkOptions;
pub use progress::{cancel, get as progress, Progress};

type Db = Mutex<Connection>;
const FILE: &str = "papr-library-v1.json";
const LEGACY_MAX_BYTES: usize = 128 * 1024 * 1024;
// Serialize connect/disconnect/run in this process, including the scheduler.
static SYNC_LOCK: Mutex<()> = Mutex::const_new(());

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    records: Vec<Record>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub connected: bool,
    pub url: Option<String>,
    pub username: Option<String>,
    pub last_sync: Option<String>,
    pub last_error: Option<&'static str>,
    pub conflicts: usize,
}

#[derive(Clone)]
struct Credentials {
    url: Url,
    user: String,
    password: String,
    options: NetworkOptions,
}

fn base_url(input: &str) -> AppResult<Url> {
    let mut url = Url::parse(input.trim()).map_err(|_| AppError::code("webdavInvalidUrl"))?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::code("webdavInvalidUrl"));
    }
    if url
        .host_str()
        .is_some_and(|h| h.ends_with(".teracloud.jp") || h == "infini-cloud.net")
        && url.path().starts_with("/browser")
    {
        return Err(AppError::code("webdavBrowserUrl"));
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

fn credentials(conn: &Connection) -> AppResult<Option<Credentials>> {
    let Some(url) = db::get_setting(conn, "webdav_url")?.filter(|u| !u.is_empty()) else {
        return Ok(None);
    };
    Ok(Some(Credentials {
        url: base_url(&url)?,
        user: db::get_setting(conn, "webdav_user")?.unwrap_or_default(),
        password: db::get_setting(conn, "webdav_password")?.unwrap_or_default(),
        options: NetworkOptions::read(conn)?,
    }))
}

pub async fn status(db: &Db) -> AppResult<Status> {
    let conn = db.lock().await;
    let c = credentials(&conn)?;
    Ok(Status {
        connected: c.is_some(),
        url: c.as_ref().map(|c| c.url.to_string()),
        username: c.map(|c| c.user),
        last_sync: db::get_setting(&conn, "webdav_last_sync")?,
        conflicts: replication::conflicts(&conn)?.len(),
        last_error: db::get_setting(&conn, "webdav_last_error")?
            .filter(|s| !s.is_empty())
            .map(|s| {
                if s == "webdavNeedsResolution" {
                    "webdavNeedsResolution"
                } else if s == "webdavRateLimited" {
                    "webdavRateLimited"
                } else if s == "webdavPreconditionFailed" {
                    "webdavPreconditionFailed"
                } else if s == "webdavRemoteConflict" {
                    "webdavRemoteConflict"
                } else if s == "webdavConflict" {
                    "webdavConflict"
                } else {
                    "webdavSyncFailed"
                }
            }),
    })
}

fn request(
    http: &Client,
    c: &Credentials,
    method: reqwest::Method,
    url: Url,
) -> reqwest::RequestBuilder {
    http.request(method, url)
        .basic_auth(&c.user, Some(&c.password))
        // Compression can change the representation ETag; PUT needs the stored
        // resource validator. Never strip or rewrite an opaque server ETag.
        .header(header::ACCEPT_ENCODING, "identity")
        .timeout(Duration::from_secs(c.options.timeout_secs))
}

fn http_error(status: StatusCode) -> AppError {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => AppError::code("webdavAuthFailed"),
        StatusCode::TOO_MANY_REQUESTS => AppError::code("webdavRateLimited"),
        _ => AppError::other(format!("WebDAV: HTTP {status}")),
    }
}

// Strip the request URL from transport errors: some deployments put private
// tokens in their WebDAV path. Server response bodies are never logged.
fn network_error(e: reqwest::Error) -> AppError {
    AppError::Http(e.without_url())
}

/// Create only the final directory; the configured parent must already exist.
async fn ensure_collection(http: &Client, c: &Credentials) -> AppResult<()> {
    transport::retry(&c.url, || async {
        let resp = request(
            http,
            c,
            reqwest::Method::from_bytes(b"MKCOL").unwrap(),
            c.url.clone(),
        )
        .send()
        .await?;
        if resp.status() == StatusCode::METHOD_NOT_ALLOWED {
            transport::drain(resp).await;
            return Ok(());
        }
        transport::drain(transport::checked(resp).await?).await;
        Ok(())
    })
    .await
}

async fn download(http: &Client, c: &Credentials) -> AppResult<(Root, Option<String>)> {
    transport::retry(&c.url, || download_once(http, c)).await
}
async fn download_once(
    http: &Client,
    c: &Credentials,
) -> transport::Result<(Root, Option<String>)> {
    let mut resp = request(http, c, reqwest::Method::GET, c.url.join(FILE)?)
        .header(header::CACHE_CONTROL, "no-cache")
        .send()
        .await
        .map_err(network_error)?;
    if resp.status() == StatusCode::NOT_FOUND {
        return Ok((Root::Manifest(chunks::Manifest::empty()), None));
    }
    resp = transport::checked(resp).await?;
    let etag = resp
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.starts_with('"') && v.ends_with('"'))
        .ok_or_else(|| AppError::code("webdavEtagRequired"))?
        .to_string();
    if resp
        .content_length()
        .is_some_and(|n| n > LEGACY_MAX_BYTES as u64)
    {
        return Err(AppError::code("webdavTooLarge").into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(network_error)? {
        if body.len() + chunk.len() > LEGACY_MAX_BYTES {
            return Err(AppError::code("webdavTooLarge").into());
        }
        progress::bytes(chunk.len(), false);
        body.extend_from_slice(&chunk);
    }
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    let version: Version =
        serde_json::from_slice(&body).map_err(|_| AppError::code("webdavInvalidData"))?;
    let root = if matches!(version.version, 6 | 7) {
        if body.len() > 16 * 1024 * 1024 {
            return Err(AppError::code("webdavTooLarge").into());
        }
        let manifest: chunks::Manifest =
            serde_json::from_slice(&body).map_err(|_| AppError::code("webdavInvalidData"))?;
        manifest.validate()?;
        Root::Manifest(manifest)
    } else if (1..=5).contains(&version.version) {
        let mut snapshot: Snapshot =
            serde_json::from_slice(&body).map_err(|_| AppError::code("webdavInvalidData"))?;
        if snapshot.version < 4 {
            replication::upgrade_legacy_tags(&mut snapshot.records)?;
        }
        replication::upgrade_legacy_folders(&mut snapshot.records)?;
        Root::Legacy(snapshot)
    } else {
        return Err(AppError::code("webdavVersion").into());
    };
    Ok((root, Some(etag)))
}

#[derive(Serialize)]
enum Root {
    Legacy(Snapshot),
    Manifest(chunks::Manifest),
}

async fn fetch_chunk(http: &Client, c: &Credentials, p: &chunks::Part) -> AppResult<Vec<u8>> {
    Ok(fetch_chunk_meta(http, c, p).await?.0)
}
async fn fetch_chunk_meta(
    http: &Client,
    c: &Credentials,
    p: &chunks::Part,
) -> AppResult<(Vec<u8>, Option<String>)> {
    transport::retry(&c.url, || async {
        let mut resp = transport::checked(
            request(http, c, reqwest::Method::GET, c.url.join(&chunks::path(p))?)
                .header(header::ACCEPT_ENCODING, "identity")
                .send()
                .await?,
        )
        .await?;
        let etag = transport::etag(&resp);
        if resp.content_length().is_some_and(|n| n != p.bytes as u64) {
            return Err(AppError::code("webdavInvalidData").into());
        }
        let mut body = Vec::with_capacity(p.bytes);
        while let Some(b) = resp.chunk().await? {
            if body.len() + b.len() > p.bytes {
                return Err(AppError::code("webdavInvalidData").into());
            }
            progress::bytes(b.len(), false);
            body.extend_from_slice(&b);
        }
        if body.len() != p.bytes {
            return Err(AppError::code("webdavInvalidData").into());
        }
        Ok((body, etag))
    })
    .await
}

/// Verify read and write access using a disposable file. Never upload or merge
/// library data merely because the user saves connection settings.
pub async fn connect(
    db: &Db,
    http: &Client,
    url: &str,
    user: &str,
    password: &str,
) -> AppResult<()> {
    let _guard = SYNC_LOCK.lock().await;
    {
        let conn = db.lock().await;
        if db::is_freshrss_connected(&conn) {
            return Err(AppError::code("syncProviderConflict"));
        }
    }
    let c = Credentials {
        url: base_url(url)?,
        user: user.trim().into(),
        password: password.into(),
        options: NetworkOptions::read(&*db.lock().await)?,
    };
    ensure_collection(http, &c).await?;
    download(http, &c).await?;
    let nonce: String = {
        let conn = db.lock().await;
        replication::probe_id(&conn)?
    };
    let probe = c.url.join(&format!(".papr-test-{nonce}"))?;
    transport::retry(&c.url, || async {
        let resp = request(http, &c, reqwest::Method::PUT, probe.clone())
            .header(header::IF_NONE_MATCH, "*")
            .body("papr")
            .send()
            .await?;
        if resp.status() == StatusCode::PRECONDITION_FAILED {
            transport::drain(resp).await;
            return Ok(());
        }
        transport::drain(transport::checked(resp).await?).await;
        Ok(())
    })
    .await?;
    transport::retry(&c.url, || async {
        let resp = request(http, &c, reqwest::Method::DELETE, probe.clone())
            .send()
            .await?;
        if resp.status() == StatusCode::NOT_FOUND {
            transport::drain(resp).await;
            return Ok(());
        }
        transport::drain(transport::checked(resp).await?).await;
        Ok(())
    })
    .await?;
    let mut conn = db.lock().await;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if db::is_freshrss_connected(&tx) {
        return Err(AppError::code("syncProviderConflict"));
    }
    if db::get_setting(&tx, "webdav_url")?.as_deref() != Some(c.url.as_str())
        || db::get_setting(&tx, "webdav_user")?.as_deref() != Some(c.user.as_str())
    {
        tx.execute("DELETE FROM webdav_baseline", [])?;
        tx.execute("DELETE FROM webdav_conflicts", [])?;
        tx.execute("DELETE FROM webdav_observed", [])?;
        tx.execute("DELETE FROM webdav_incoming", [])?;
        tx.execute("DELETE FROM webdav_incoming_chunks", [])?;
        tx.execute("DELETE FROM webdav_objects", [])?;
        tx.execute("DELETE FROM webdav_buckets", [])?;
        tx.execute("DELETE FROM webdav_upload_receipts", [])?;
    }
    for (key, value) in [
        ("webdav_url", c.url.as_str()),
        ("webdav_user", c.user.as_str()),
        ("webdav_password", password),
        ("webdav_last_sync", ""),
        ("webdav_last_attempt", ""),
        ("webdav_last_error", ""),
    ] {
        db::set_setting(&tx, key, value)?;
    }
    tx.commit()?;
    progress::clear();
    Ok(())
}

pub async fn disconnect(db: &Db) -> AppResult<()> {
    let _guard = SYNC_LOCK.lock().await;
    let mut conn = db.lock().await;
    let tx = conn.transaction()?;
    for key in [
        "webdav_url",
        "webdav_user",
        "webdav_password",
        "webdav_last_sync",
        "webdav_last_error",
    ] {
        db::set_setting(&tx, key, "")?;
    }
    tx.commit()?;
    progress::clear();
    Ok(())
}

pub async fn conflicts(db: &Db) -> AppResult<Vec<replication::Conflict>> {
    replication::conflicts(&*db.lock().await)
}
pub async fn resolve(db: &Db, kind: &str, key: &str, choice: &str) -> AppResult<()> {
    let _guard = SYNC_LOCK.lock().await;
    replication::resolve_conflict(&mut *db.lock().await, kind, key, choice)
}

pub async fn run_if_connected(db: &Db, http: &Client) -> AppResult<bool> {
    // Timer, foreground and online events share this gate. Do not queue work
    // behind an upload, or retry failed attempts faster than the chosen interval.
    let Ok(_guard) = SYNC_LOCK.try_lock() else { return Ok(false); };
    {
        let conn = db.lock().await;
        if credentials(&conn)?.is_none() || !ScheduleOptions::read(&conn)?.due(&conn, chrono::Utc::now())? {
            return Ok(false);
        }
    }
    let configured = { http_client(&*db.lock().await)? };
    let _ = http;
    sync_locked(db, &configured).await.map(|_| true)
}

pub async fn try_sync_now(db: &Db, http: &Client) -> AppResult<usize> {
    let _guard = SYNC_LOCK.try_lock().map_err(|_| AppError::code("webdavBusy"))?;
    sync_locked(db, http).await
}

pub async fn sync_now(db: &Db, http: &Client) -> AppResult<usize> {
    let _guard = SYNC_LOCK.lock().await;
    sync_locked(db, http).await
}

async fn sync_locked(db: &Db, http: &Client) -> AppResult<usize> {
    let c = {
        let conn = db.lock().await;
        credentials(&conn)?.ok_or_else(|| AppError::code("webdavNotConnected"))?
    };
    db::set_setting(&*db.lock().await, "webdav_last_attempt", &chrono::Utc::now().to_rfc3339())?;
    let result = progress::run(sync_inner(db, http, &c)).await;
    let conn = db.lock().await;
    db::set_setting(&conn, "webdav_last_attempt", &chrono::Utc::now().to_rfc3339())?;
    match &result {
        Ok(_) => {
            db::set_setting(&conn, "webdav_last_sync", &chrono::Utc::now().to_rfc3339())?;
            db::set_setting(&conn, "webdav_last_error", "")?;
        }
        Err(error) => {
            db::set_setting(
                &conn,
                "webdav_last_error",
                if matches!(error, AppError::Coded("webdavNeedsResolution")) {
                    "webdavNeedsResolution"
                } else if matches!(error, AppError::Coded("webdavRateLimited")) {
                    "webdavRateLimited"
                } else if matches!(error, AppError::Coded("webdavCancelled")) {
                    ""
                } else if let AppError::Coded(code @ ("webdavPreconditionFailed" | "webdavRemoteConflict" | "webdavConflict")) = error {
                    code
                } else {
                    "webdavSyncFailed"
                },
            )?;
        }
    }
    result
}

// Both the number of requests and their resident payloads are bounded. A
// single oversized record runs alone; ordinary chunks can fill five slots.
const MEMORY_KIB: usize = 96 * 1024;
fn memory_cost(p: &chunks::Part) -> u32 {
    ((p.bytes.saturating_mul(2) + p.raw_bytes.saturating_mul(2)).div_ceil(1024))
        .clamp(1, MEMORY_KIB) as u32
}
fn upload_target(c: &Credentials) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::json!([c.url.as_str(), c.user])
                .to_string()
                .as_bytes()
        )
    )
}
async fn save_receipt(
    db: &Db,
    target: &str,
    p: &chunks::Part,
    etag: Option<String>,
) -> AppResult<()> {
    if let Some(etag) = etag {
        db.lock().await.execute("INSERT INTO webdav_upload_receipts(target,hash,bytes,etag) VALUES(?1,?2,?3,?4) ON CONFLICT(target,hash) DO UPDATE SET bytes=excluded.bytes,etag=excluded.etag",rusqlite::params![target,p.hash,p.bytes,etag])?;
    }
    Ok(())
}
async fn receipt_valid(
    db: &Db,
    http: &Client,
    c: &Credentials,
    p: &chunks::Part,
    target: &str,
) -> AppResult<bool> {
    use rusqlite::OptionalExtension;
    let receipt: Option<String> = db
        .lock()
        .await
        .query_row(
            "SELECT etag FROM webdav_upload_receipts WHERE target=?1 AND hash=?2 AND bytes=?3",
            rusqlite::params![target, p.hash, p.bytes],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(receipt) = receipt {
        let resp = transport::retry(&c.url, || async {
            let r = request(
                http,
                c,
                reqwest::Method::HEAD,
                c.url.join(&chunks::path(p))?,
            )
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .await?;
            if matches!(r.status().as_u16(), 404 | 405 | 501) {
                return Ok(r);
            }
            transport::checked(r).await
        })
        .await?;
        let unchanged = resp.status().is_success()
            && transport::etag(&resp).as_deref() == Some(&receipt)
            && resp
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<usize>().ok())
                == Some(p.bytes);
        transport::drain(resp).await;
        if unchanged {
            return Ok(true);
        }
        db.lock().await.execute(
            "DELETE FROM webdav_upload_receipts WHERE target=?1 AND hash=?2",
            rusqlite::params![target, p.hash],
        )?;
    }
    Ok(false)
}
async fn upload_chunk(
    db: &Db,
    http: &Client,
    c: &Credentials,
    p: &chunks::Part,
    target: &str,
) -> AppResult<()> {
    if receipt_valid(db, http, c, p, target).await? {
        return Ok(());
    }
    let body: bytes::Bytes = chunks::cached(&*db.lock().await, p)?
        .ok_or_else(|| AppError::code("webdavInvalidData"))?
        .into();
    let resp = transport::retry(&c.url, || async {
        let resp = request(http, c, reqwest::Method::PUT, c.url.join(&chunks::path(p))?)
            .header(header::IF_NONE_MATCH, "*")
            .header(header::CONTENT_TYPE, "application/gzip")
            .header(header::CONTENT_LENGTH, p.bytes)
            .body(transport::upload_body(body.clone()))
            .send()
            .await?;
        if resp.status() == StatusCode::PRECONDITION_FAILED {
            return Ok(resp);
        }
        transport::checked(resp).await
    })
    .await?;
    if resp.status() == StatusCode::PRECONDITION_FAILED {
        transport::drain(resp).await;
        // No trustworthy local upload receipt: verify the actual content.
        let (bytes, etag) = fetch_chunk_meta(http, c, p).await?;
        chunks::cache(&*db.lock().await, p, &bytes)?;
        save_receipt(db, target, p, etag).await?;
    } else {
        let etag = transport::etag(&resp);
        transport::drain(resp).await;
        save_receipt(db, target, p, etag).await?;
    }
    Ok(())
}

async fn sync_inner(db: &Db, http: &Client, c: &Credentials) -> AppResult<usize> {
    let mut total = 0;
    let mut refused = None;
    for _ in 0..3 {
        let (remote, etag) = download(http, c).await?;
        use sha2::{Digest, Sha256};
        let signature: [u8; 32] = Sha256::digest(serde_json::to_vec(&remote).map_err(|_| AppError::code("webdavInvalidData"))?).into();
        if refused.as_ref() == Some(&(etag.clone(), signature)) {
            // 412 with the same resource is not evidence of a second device.
            // Keep the previous root and local edits; never fall back to a blind PUT.
            return Err(AppError::code("webdavPreconditionFailed"));
        }
        match &remote {
            Root::Legacy(snapshot) => chunks::stage_legacy(&mut *db.lock().await, snapshot)?,
            Root::Manifest(manifest) => {
                let pending = {
                    let conn = db.lock().await;
                    manifest
                        .chunks
                        .iter()
                        .filter_map(|p| match chunks::available(&conn, p) {
                            Ok(false) => Some(Ok(p.clone())),
                            Ok(true) => None,
                            Err(e) => Some(Err(e)),
                        })
                        .collect::<AppResult<Vec<_>>>()?
                };
                progress::phase("download", pending.len());
                let memory = tokio::sync::Semaphore::new(MEMORY_KIB);
                let mut jobs = stream::iter(pending)
                    .map(|p| {
                        let memory = &memory;
                        async move {
                            let _permit = memory
                                .acquire_many(memory_cost(&p))
                                .await
                                .expect("sync memory semaphore");
                            let body = fetch_chunk(http, c, &p).await?;
                            chunks::cache(&*db.lock().await, &p, &body)?;
                            progress::completed();
                            Ok::<_, AppError>(())
                        }
                    })
                    .buffer_unordered(c.options.concurrency);
                while let Some(result) = jobs.next().await {
                    result?;
                }
                progress::phase("staging", 0);
                chunks::stage(&mut *db.lock().await, manifest)?;
            }
        }
        let outgoing = {
            let mut conn = db.lock().await;
            progress::phase("merging", 0);
            total += replication::merge_staged(&mut conn)?;
            progress::phase("exporting", 0);
            chunks::export(&mut conn)?
        };
        let remote_parts = match &remote {
            Root::Manifest(m) => m
                .chunks
                .iter()
                .map(|p| p.hash.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            _ => Default::default(),
        };
        if etag.is_some() && matches!(&remote,Root::Manifest(m) if *m==outgoing) {
            chunks::acknowledge(&*db.lock().await)?;
            chunks::prune(&*db.lock().await)?;
            db.lock().await.execute(
                "DELETE FROM webdav_upload_receipts WHERE target=?1",
                [upload_target(c)],
            )?;
            return Ok(total);
        }
        // Publish immutable objects first. A partial upload cannot expose an
        // incomplete snapshot; the old conditional root remains authoritative.
        let pending: Vec<_> = outgoing
            .chunks
            .iter()
            .filter(|p| !remote_parts.contains(p.hash.as_str()))
            .cloned()
            .collect();
        progress::phase("upload", pending.len());
        let memory = tokio::sync::Semaphore::new(MEMORY_KIB);
        let target = upload_target(c);
        let mut jobs = stream::iter(pending)
            .map(|p| {
                let memory = &memory;
                let target = &target;
                async move {
                    let _permit = memory
                        .acquire_many(memory_cost(&p))
                        .await
                        .expect("sync memory semaphore");
                    upload_chunk(db, http, c, &p, target).await?;
                    progress::completed();
                    Ok::<_, AppError>(())
                }
            })
            .buffer_unordered(c.options.concurrency);
        while let Some(result) = jobs.next().await {
            result?;
        }
        drop(jobs);
        progress::phase("publishing", 0);
        let body =
            serde_json::to_vec(&outgoing).map_err(|_| AppError::code("webdavInvalidData"))?;
        if body.len() > 16 * 1024 * 1024 {
            return Err(AppError::code("webdavTooLarge"));
        }
        let mut req = request(http, c, reqwest::Method::PUT, c.url.join(FILE)?)
            .header(header::CONTENT_TYPE, "application/json");
        req = if let Some(etag) = &etag {
            req.header(header::IF_MATCH, etag)
        } else {
            req.header(header::IF_NONE_MATCH, "*")
        };
        let resp = transport::retry(&c.url, || async {
            progress::bytes(body.len(), true);
            let resp = req
                .try_clone()
                .expect("clone manifest request")
                .body(body.clone())
                .send()
                .await?;
            if matches!(
                resp.status(),
                StatusCode::PRECONDITION_FAILED | StatusCode::CONFLICT
            ) {
                return Ok(resp);
            }
            transport::checked(resp).await
        })
        .await?;
        if matches!(
            resp.status(),
            StatusCode::PRECONDITION_FAILED | StatusCode::CONFLICT
        ) {
            let conflict = resp.status() == StatusCode::CONFLICT;
            transport::drain(resp).await;
            if conflict {
                return Err(AppError::code("webdavRemoteConflict"));
            }
            refused = Some((etag, signature));
            continue;
        }
        transport::drain(resp).await;
        chunks::acknowledge(&*db.lock().await)?;
        chunks::prune(&*db.lock().await)?;
        db.lock().await.execute(
            "DELETE FROM webdav_upload_receipts WHERE target=?1",
            [&target],
        )?;
        return Ok(total);
    }
    Err(AppError::code("webdavConflict"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Default)]
    struct Server {
        body: Option<Vec<u8>>,
        objects: std::collections::BTreeMap<String, Vec<u8>>,
        chunk_gets: usize,
        chunk_puts: usize,
        chunk_heads: usize,
        chunk_copies: usize,
        revision: u64,
        conflict: bool,
        missing_etag: bool,
        representation_etag: bool,
        reject_root: Option<u16>,
        commit_then_fail: bool,
        deny: bool,
        fail_upload: bool,
        fail_root: bool,
        conditional_writes: usize,
        delay_ms: u64,
        active: usize,
        max_active: usize,
        transient: usize,
        throttles: usize,
        copy_unsupported: bool,
        copy_fail_after: Option<usize>,
        change_source_during_copy: bool,
    }
    fn object_etag(body: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        format!("\"{:x}\"", Sha256::digest(body))
    }
    fn handle(
        s: &mut Server,
        method: &str,
        path: &str,
        headers: &str,
        bytes: &[u8],
    ) -> (u16, Option<String>, Vec<u8>) {
        let object = path.contains("papr-chunk-v6-");
        let primary = path == "/papr/papr-library-v1.json";
        let hdr = |key: &str| {
            headers.lines().find_map(|line| {
                line.split_once(':')
                    .filter(|(k, _)| k.eq_ignore_ascii_case(key))
                    .map(|(_, v)| v.trim().to_string())
            })
        };
        if s.deny {
            return (401, None, Vec::new());
        }
        if method == "MKCOL" {
            return (405, None, Vec::new());
        }
        if path.contains(".papr-test-") {
            return (if method == "PUT" { 201 } else { 204 }, None, Vec::new());
        }
        if object && s.transient > 0 {
            s.transient -= 1;
            s.throttles += 1;
            return (429, None, Vec::new());
        }
        if method == "COPY" {
            if s.copy_unsupported {
                return (405, None, Vec::new());
            }
            if object
                && s.copy_fail_after
                    .is_some_and(|limit| s.chunk_copies >= limit)
            {
                return (500, None, Vec::new());
            }
            assert_eq!(hdr("Overwrite").as_deref(), Some("F"));
            let destination = Url::parse(&hdr("Destination").unwrap())
                .unwrap()
                .path()
                .to_owned();
            if s.objects.contains_key(&destination) {
                return (412, None, Vec::new());
            }
            let source = if primary {
                s.body.clone()
            } else {
                s.objects.get(path).cloned()
            };
            let Some(source) = source else {
                return (404, None, Vec::new());
            };
            if primary && hdr("If-Match").as_deref() != Some(format!("\"{}\"", s.revision).as_str())
            {
                return (412, None, Vec::new());
            }
            if primary && s.fail_root {
                return (500, None, Vec::new());
            }
            if object {
                s.chunk_copies += 1;
                if s.change_source_during_copy {
                    s.revision += 1;
                    s.change_source_during_copy = false;
                }
            } else {
                let m: chunks::Manifest = serde_json::from_slice(&source).unwrap();
                let directory = destination.strip_suffix(FILE).unwrap();
                assert!(
                    m.chunks.iter().all(|p| s
                        .objects
                        .contains_key(&format!("{directory}{}", chunks::path(p)))),
                    "Root copied before all chunks"
                );
            }
            let etag = object_etag(&source);
            s.objects.insert(destination, source);
            return (201, Some(etag), Vec::new());
        }
        if object {
            if method == "GET" || method == "HEAD" {
                if method == "GET" {
                    s.chunk_gets += 1;
                } else {
                    s.chunk_heads += 1;
                }
                return match s.objects.get(path) {
                    Some(b) => (200, Some(object_etag(b)), b.clone()),
                    None => (404, None, Vec::new()),
                };
            }
            if method == "PUT" {
                assert_eq!(hdr("If-None-Match").as_deref(), Some("*"));
                s.chunk_puts += 1;
                if s.fail_upload {
                    return (500, None, Vec::new());
                }
                if s.objects.contains_key(path) {
                    return (412, None, Vec::new());
                }
                let etag = object_etag(bytes);
                s.objects.insert(path.into(), bytes.to_vec());
                return (201, Some(etag), Vec::new());
            }
        } else if method == "GET" {
            let body = if primary {
                s.body.clone()
            } else {
                s.objects.get(path).cloned()
            };
            return match body {
                Some(body) => {
                    let etag = if s.missing_etag {
                        None
                    } else if primary {
                        Some(format!("\"{}{}\"", s.revision, if s.representation_etag && hdr("Accept-Encoding").as_deref() != Some("identity") { "-gzip" } else { "" }))
                    } else {
                        Some(object_etag(&body))
                    };
                    (200, etag, body)
                }
                None => (404, None, Vec::new()),
            };
        } else if method == "PUT" {
            let current = if primary {
                s.body.as_ref()
            } else {
                s.objects.get(path)
            };
            let expected = if current.is_some() {
                format!(
                    "if-match: {}",
                    if primary {
                        format!("\"{}\"", s.revision)
                    } else {
                        object_etag(current.unwrap())
                    }
                )
            } else {
                "if-none-match: *".into()
            };
            assert!(hdr("If-Match").is_some() || hdr("If-None-Match").is_some(), "Missing conditional write");
            if !headers.to_ascii_lowercase().contains(&expected) { return (412,None,Vec::new()); }
            s.conditional_writes += 1;
            if let Some(code) = s.reject_root { return (code,None,Vec::new()); }
            if s.fail_upload || s.fail_root {
                return (500, None, Vec::new());
            }
            if s.conflict {
                s.conflict = false;
                s.revision += 1;
                return (412, None, Vec::new());
            }
            if primary {
                s.body = Some(bytes.to_vec());
                s.revision += 1;
            } else {
                s.objects.insert(path.into(), bytes.to_vec());
            }
            if s.commit_then_fail { s.commit_then_fail = false; return (503,None,Vec::new()); }
            return (201, None, Vec::new());
        }
        (400, None, Vec::new())
    }
    async fn server() -> (String, Arc<StdMutex<Server>>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/papr/", listener.local_addr().unwrap());
        let state = Arc::new(StdMutex::new(Server::default()));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let shared = shared.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let end = loop {
                        let mut buf = [0; 4096];
                        let Ok(n) = socket.read(&mut buf).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&buf[..n]);
                        if let Some(p) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                            break p + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_string();
                    let method = headers.split_whitespace().next().unwrap();
                    let path = headers.split_whitespace().nth(1).unwrap();
                    let length = headers
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    while bytes.len() < end + length {
                        let mut buf = [0; 4096];
                        let Ok(n) = socket.read(&mut buf).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&buf[..n]);
                    }
                    let object = path.contains("papr-chunk-v6-");
                    let delay = {
                        let mut s = shared.lock().unwrap();
                        if object {
                            s.active += 1;
                            s.max_active = s.max_active.max(s.active);
                        }
                        s.delay_ms
                    };
                    if object && delay > 0 {
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                    }
                    let (code, etag, body) = {
                        let mut s = shared.lock().unwrap();
                        let result =
                            handle(&mut s, method, path, &headers, &bytes[end..end + length]);
                        if object {
                            s.active -= 1;
                        }
                        result
                    };
                    let etag = etag.map(|s| format!("ETag: {s}\r\n")).unwrap_or_default();
                    let retry = if code == 429 {
                        "Retry-After: 0\r\n"
                    } else {
                        ""
                    };
                    let response=format!("HTTP/1.1 {code} Test\r\n{etag}{retry}Content-Length: {}\r\nConnection: close\r\n\r\n",body.len());
                    let _ = socket.write_all(response.as_bytes()).await;
                    if method != "HEAD" {
                        let _ = socket.write_all(&body).await;
                    }
                });
            }
        });
        (url, state, task)
    }
    fn database() -> Db {
        Mutex::new(db::open(std::path::Path::new(":memory:")).unwrap())
    }
    fn client() -> Client {
        Client::builder().no_proxy().build().unwrap()
    }
    async fn seed(db: &Db) {
        let conn = db.lock().await;
        conn.execute(
            "INSERT INTO feeds(feed_url,title) VALUES('https://example.com/rss','Feed')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO articles(feed_id,guid,title) VALUES(1,'one','Article')",
            [],
        )
        .unwrap();
        db::set_read(&conn, 1, true).unwrap();
    }
    async fn many(db: &Db) {
        seed(db).await;
        let c = db.lock().await;
        for i in 0..36 {
            c.execute("INSERT INTO articles(feed_id,guid,title,body_text) VALUES(1,?1,'Test article','A sample body for the latency test')",[format!("parallel-{i}")]).unwrap();
        }
    }
    #[tokio::test]
    async fn bounded_parallelism_retries_and_receipts_avoid_repeated_body_transfers() {
        let (url, server, task) = server().await;
        let http = client();
        let a = database();
        many(&a).await;
        db::set_setting(&*a.lock().await, "webdav_concurrency", "1").unwrap();
        connect(&a, &http, &url, "u", "p").await.unwrap();
        server.lock().unwrap().delay_ms = 15;
        let start = std::time::Instant::now();
        sync_now(&a, &http).await.unwrap();
        let serial = start.elapsed();
        assert_eq!(server.lock().unwrap().max_active, 1);
        {
            let mut s = server.lock().unwrap();
            s.body = None;
            s.objects.clear();
            s.max_active = 0;
            s.transient = 1;
        }
        db::set_setting(&*a.lock().await, "webdav_concurrency", "5").unwrap();
        let start = std::time::Instant::now();
        sync_now(&a, &http).await.unwrap();
        let parallel = start.elapsed();
        {
            let s = server.lock().unwrap();
            assert_eq!(s.max_active, 5);
            assert_eq!(s.throttles, 1);
        }
        println!(
            "15 ms/request fixture: serial {:?}, five parallel {:?}",
            serial, parallel
        );
        replication::set_progress(&*a.lock().await, 1, 0.7).unwrap();
        server.lock().unwrap().fail_root = true;
        assert!(sync_now(&a, &http).await.is_err());
        let (gets, puts) = {
            let s = server.lock().unwrap();
            (s.chunk_gets, s.chunk_puts)
        };
        server.lock().unwrap().fail_root = false;
        sync_now(&a, &http).await.unwrap();
        {
            let s = server.lock().unwrap();
            assert!(s.chunk_heads > 0);
            assert_eq!(
                s.chunk_gets, gets,
                "resume must not redownload verified upload bodies"
            );
            assert_eq!(s.chunk_puts, puts);
        }
        let b = database();
        db::set_setting(&*b.lock().await, "webdav_concurrency", "3").unwrap();
        connect(&b, &http, &url, "u", "p").await.unwrap();
        server.lock().unwrap().max_active = 0;
        sync_now(&b, &http).await.unwrap();
        assert_eq!(server.lock().unwrap().max_active, 3);
        assert_eq!(
            db::get_article(&*b.lock().await, 1)
                .unwrap()
                .reading_progress,
            0.7
        );
        task.abort();
    }
    #[tokio::test]
    async fn changed_etag_never_reuses_a_corrupt_previously_uploaded_object() {
        let (url, server, task) = server().await;
        let http = client();
        let a = database();
        seed(&a).await;
        connect(&a, &http, &url, "u", "p").await.unwrap();
        sync_now(&a, &http).await.unwrap();
        replication::set_progress(&*a.lock().await, 1, 0.75).unwrap();
        server.lock().unwrap().fail_root = true;
        assert!(sync_now(&a, &http).await.is_err());
        let (hash, bytes): (String, usize) = a
            .lock()
            .await
            .query_row(
                "SELECT hash,bytes FROM webdav_upload_receipts LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let path = format!("/papr/papr-chunk-v6-{hash}.jsonl.gz");
        let original = server
            .lock()
            .unwrap()
            .objects
            .insert(path.clone(), vec![b'x'; bytes])
            .unwrap();
        server.lock().unwrap().fail_root = false;
        let root = server.lock().unwrap().body.clone();
        assert!(matches!(
            sync_now(&a, &http).await,
            Err(AppError::Coded("webdavInvalidData"))
        ));
        assert_eq!(server.lock().unwrap().body, root);
        server.lock().unwrap().objects.insert(path, original);
        sync_now(&a, &http).await.unwrap();
        task.abort();
    }
    #[tokio::test]
    async fn server_side_directory_copy_is_conditional_keeps_source_and_pending_local_edits() {
        let (url, server, task) = server().await;
        let http = client();
        let a = database();
        seed(&a).await;
        connect(&a, &http, &url, "u", "p").await.unwrap();
        sync_now(&a, &http).await.unwrap();
        let before = {
            let mut s = server.lock().unwrap();
            s.objects
                .insert("/papr/unrelated.txt".into(), b"keep me".to_vec());
            s.change_source_during_copy = true;
            (s.body.clone(), s.chunk_gets, s.chunk_puts)
        };
        replication::set_progress(&*a.lock().await, 1, 0.4).unwrap();
        let target = relocate(&a, &http, "Papr").await.unwrap();
        assert_eq!(target, format!("{url}Papr/"));
        {
            let s = server.lock().unwrap();
            assert_eq!(s.body, before.0);
            assert!(s.chunk_copies > 0);
            assert!(s.chunk_heads > 0);
            assert_eq!(s.chunk_gets, before.1);
            assert_eq!(s.chunk_puts, before.2);
            assert_eq!(s.objects["/papr/unrelated.txt"], b"keep me");
        }
        sync_now(&a, &http).await.unwrap();
        let b = database();
        connect(&b, &http, &target, "u", "p").await.unwrap();
        sync_now(&b, &http).await.unwrap();
        assert_eq!(
            db::get_article(&*b.lock().await, 1)
                .unwrap()
                .reading_progress,
            0.4
        );
        assert_eq!(server.lock().unwrap().body, before.0);
        task.abort();
    }
    #[tokio::test]
    async fn directory_copy_can_resume_and_refuses_another_library() {
        let (url, server, task) = server().await;
        let http = client();
        let a = database();
        seed(&a).await;
        connect(&a, &http, &url, "u", "p").await.unwrap();
        sync_now(&a, &http).await.unwrap();
        server.lock().unwrap().copy_fail_after = Some(2);
        assert!(relocate(&a, &http, "New").await.is_err());
        assert_eq!(status(&a).await.unwrap().url.as_deref(), Some(url.as_str()));
        assert!(!server
            .lock()
            .unwrap()
            .objects
            .contains_key("/papr/New/papr-library-v1.json"));
        server.lock().unwrap().copy_fail_after = None;
        let target = relocate(&a, &http, "New").await.unwrap();
        assert_eq!(
            status(&a).await.unwrap().url.as_deref(),
            Some(target.as_str())
        );
        server.lock().unwrap().objects.insert(
            "/papr/New/Occupied/papr-library-v1.json".into(),
            serde_json::to_vec(&chunks::Manifest::empty()).unwrap(),
        );
        assert!(matches!(
            relocate(&a, &http, "Occupied").await,
            Err(AppError::Coded("webdavDirectoryOccupied"))
        ));
        assert_eq!(
            status(&a).await.unwrap().url.as_deref(),
            Some(target.as_str())
        );
        task.abort();
    }
    #[tokio::test]
    async fn copy_unsupported_falls_back_to_bounded_chunk_uploads() {
        let (url, server, task) = server().await;
        let http = client();
        let a = database();
        seed(&a).await;
        connect(&a, &http, &url, "u", "p").await.unwrap();
        sync_now(&a, &http).await.unwrap();
        server.lock().unwrap().copy_unsupported = true;
        let original = server.lock().unwrap().body.clone();
        let target = relocate(&a, &http, "Fallback").await.unwrap();
        let b = database();
        connect(&b, &http, &target, "u", "p").await.unwrap();
        sync_now(&b, &http).await.unwrap();
        assert_eq!(
            db::get_article(&*b.lock().await, 1).unwrap().title,
            "Article"
        );
        assert_eq!(server.lock().unwrap().body, original);
        task.abort();
    }
    #[tokio::test]
    async fn cancellation_keeps_the_published_root_intact_and_can_resume() {
        let (url, server, task) = server().await;
        let http = client();
        let a = Arc::new(database());
        many(&a).await;
        connect(&a, &http, &url, "u", "p").await.unwrap();
        server.lock().unwrap().delay_ms = 300;
        let run = {
            let a = a.clone();
            let http = http.clone();
            tokio::spawn(async move { sync_now(&a, &http).await })
        };
        tokio::time::timeout(Duration::from_secs(90), async {
            loop {
                if server.lock().unwrap().active > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(progress().running);
        cancel();
        assert!(matches!(
            run.await.unwrap(),
            Err(AppError::Coded("webdavCancelled"))
        ));
        assert!(server.lock().unwrap().body.is_none());
        server.lock().unwrap().delay_ms = 0;
        sync_now(&a, &http).await.unwrap();
        assert!(server.lock().unwrap().body.is_some());
        task.abort();
    }

    #[tokio::test]
    async fn identity_representation_avoids_spurious_single_device_preconditions() {
        let (url, server, task) = server().await;
        let http = client(); let a = database();
        seed(&a).await;
        connect(&a,&http,&url,"u","p").await.unwrap();
        sync_now(&a,&http).await.unwrap();
        server.lock().unwrap().representation_etag = true;
        // A negotiated representation has a validator inappropriate for PUT.
        let raw = http.get(format!("{url}{FILE}")).header(header::ACCEPT_ENCODING,"gzip").send().await.unwrap();
        let variant = raw.headers()[header::ETAG].to_str().unwrap().to_owned();
        assert!(variant.contains("-gzip"));
        let body = raw.bytes().await.unwrap();
        assert_eq!(http.put(format!("{url}{FILE}")).header(header::IF_MATCH,variant).body(body).send().await.unwrap().status(),StatusCode::PRECONDITION_FAILED);
        replication::set_progress(&*a.lock().await,1,0.5).unwrap();
        sync_now(&a,&http).await.unwrap();
        assert!(status(&a).await.unwrap().last_error.is_none());
        task.abort();
    }

    #[tokio::test]
    async fn unchanged_root_rejection_is_not_reported_as_another_device_and_keeps_edits() {
        let (url, server, task) = server().await;
        let http = client(); let a = database();
        seed(&a).await; connect(&a,&http,&url,"u","p").await.unwrap();
        sync_now(&a,&http).await.unwrap();
        let before = server.lock().unwrap().body.clone();
        replication::set_progress(&*a.lock().await,1,0.75).unwrap();
        for (status_code, code) in [(412,"webdavPreconditionFailed"),(409,"webdavRemoteConflict")] {
            server.lock().unwrap().reject_root = Some(status_code);
            assert!(matches!(sync_now(&a,&http).await,Err(AppError::Coded(c)) if c==code));
            assert_eq!(status(&a).await.unwrap().last_error,Some(code));
            assert_eq!(server.lock().unwrap().body,before);
            assert_eq!(db::get_article(&*a.lock().await,1).unwrap().reading_progress,0.75);
        }
        server.lock().unwrap().reject_root = None;
        server.lock().unwrap().commit_then_fail = true;
        // A lost success response may cause our own retry to get 412. Re-read
        // the new root and acknowledge it instead of claiming a foreign upload.
        sync_now(&a,&http).await.unwrap();
        assert_ne!(server.lock().unwrap().body,before);
        assert!(!run_if_connected(&a,&http).await.unwrap(),"Manual sync also resets automatic cadence");
        task.abort();
    }

    #[tokio::test]
    async fn background_and_manual_triggers_do_not_queue_behind_active_operations() {
        let a = database();
        let _guard = SYNC_LOCK.lock().await;
        assert!(!run_if_connected(&a,&client()).await.unwrap());
        assert!(matches!(try_sync_now(&a,&client()).await,Err(AppError::Coded("webdavBusy"))));
    }

    #[tokio::test]
    async fn two_devices_merge_over_http_and_retry_conditional_conflicts() {
        let (url, server, task) = server().await;
        let http = client();
        let a = database();
        let b = database();
        seed(&a).await;
        connect(&a, &http, &url, "alice", "SECRET").await.unwrap();
        assert!(
            server.lock().unwrap().body.is_none(),
            "Connecting must not upload the library"
        );
        sync_now(&a, &http).await.unwrap();
        connect(&b, &http, &url, "alice", "SECRET").await.unwrap();
        db::set_setting(&*b.lock().await, "ai_summary_prompt", "retry prompt").unwrap();
        server.lock().unwrap().conflict = true;
        sync_now(&b, &http).await.unwrap();
        {
            let conn = b.lock().await;
            assert!(db::get_article(&conn, 1).unwrap().is_read);
            db::set_starred(&conn, 1, true).unwrap();
        }
        sync_now(&b, &http).await.unwrap();
        sync_now(&a, &http).await.unwrap();
        assert!(db::get_article(&*a.lock().await, 1).unwrap().is_starred);
        assert!(status(&a).await.unwrap().last_sync.is_some());
        assert!(
            !String::from_utf8(server.lock().unwrap().body.clone().unwrap())
                .unwrap()
                .contains("SECRET")
        );
        assert_eq!(server.lock().unwrap().conditional_writes, 4);
        sync_now(&a, &http).await.unwrap();
        assert_eq!(
            server.lock().unwrap().conditional_writes,
            4,
            "Idle sync must not upload again"
        );
        disconnect(&a).await.unwrap();
        assert!(!status(&a).await.unwrap().connected);
        assert!(server.lock().unwrap().body.is_some());
        task.abort();
    }
    #[tokio::test]
    async fn ai_preferences_sync_over_http_with_conflict_review_and_no_secrets() {
        let (url, server, task) = server().await;
        let http = client();
        let a = database();
        let b = database();
        {
            let c = a.lock().await;
            db::configure_ai_selection(&c, "codex", Some("shared-model"), None, Some("high"))
                .unwrap();
            db::set_setting(&c, "ai_summary_prompt", "Shared prompt").unwrap();
            db::set_setting(&c, "ai_api_key", "SECRET_AI_KEY").unwrap();
            db::set_setting(&c, "ai_base_url", "https://SECRET@example.com/v1").unwrap();
        }
        connect(&a, &http, &url, "a", "SECRET_DAV").await.unwrap();
        sync_now(&a, &http).await.unwrap();
        connect(&b, &http, &url, "a", "SECRET_DAV").await.unwrap();
        sync_now(&b, &http).await.unwrap();
        {
            let c = b.lock().await;
            assert_eq!(
                db::get_setting(&c, "ai_provider").unwrap().as_deref(),
                Some("codex")
            );
            assert_eq!(
                db::get_setting(&c, "ai_model").unwrap().as_deref(),
                Some("shared-model")
            );
            assert_eq!(
                db::get_setting(&c, "ai_reasoning_effort_codex")
                    .unwrap()
                    .as_deref(),
                Some("high")
            );
            assert_eq!(
                db::get_setting(&c, "ai_summary_prompt").unwrap().as_deref(),
                Some("Shared prompt")
            );
            assert!(db::get_setting(&c, "ai_api_key").unwrap().is_none());
            db::set_setting(&c, "ai_summary_prompt", "Device B prompt").unwrap();
        }
        db::set_setting(&*a.lock().await, "ai_summary_prompt", "Device A prompt").unwrap();
        sync_now(&a, &http).await.unwrap();
        let before = server.lock().unwrap().body.clone();
        assert!(matches!(
            sync_now(&b, &http).await,
            Err(AppError::Coded("webdavNeedsResolution"))
        ));
        assert_eq!(server.lock().unwrap().body, before);
        let c = conflicts(&b).await.unwrap().remove(0);
        assert_eq!(c.kind, "ai_prompt");
        resolve(&b, "ai_prompt", "summary", "local").await.unwrap();
        sync_now(&b, &http).await.unwrap();
        sync_now(&a, &http).await.unwrap();
        assert_eq!(
            db::get_setting(&*a.lock().await, "ai_summary_prompt")
                .unwrap()
                .as_deref(),
            Some("Device B prompt")
        );
        let body = String::from_utf8(server.lock().unwrap().body.clone().unwrap()).unwrap();
        assert!(!body.contains("SECRET"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["version"],
            7
        );
        task.abort();
    }
    #[tokio::test]
    async fn conflicting_progress_pauses_before_upload_and_resolution_converges() {
        let (url, server, task) = server().await;
        let http = client();
        let a = database();
        let b = database();
        seed(&a).await;
        connect(&a, &http, &url, "a", "p").await.unwrap();
        sync_now(&a, &http).await.unwrap();
        connect(&b, &http, &url, "a", "p").await.unwrap();
        sync_now(&b, &http).await.unwrap();
        replication::set_progress(&*a.lock().await, 1, 0.2).unwrap();
        sync_now(&a, &http).await.unwrap();
        replication::set_progress(&*b.lock().await, 1, 0.8).unwrap();
        let before = server.lock().unwrap().body.clone();
        let writes = server.lock().unwrap().conditional_writes;
        assert!(matches!(
            sync_now(&b, &http).await,
            Err(AppError::Coded("webdavNeedsResolution"))
        ));
        assert_eq!(status(&b).await.unwrap().conflicts, 1);
        assert_eq!(server.lock().unwrap().body, before);
        assert_eq!(server.lock().unwrap().conditional_writes, writes);
        let conflict = conflicts(&b).await.unwrap().remove(0);
        resolve(&b, &conflict.kind, &conflict.key, "local")
            .await
            .unwrap();
        sync_now(&b, &http).await.unwrap();
        sync_now(&a, &http).await.unwrap();
        assert_eq!(status(&b).await.unwrap().conflicts, 0);
        assert_eq!(
            db::get_article(&*a.lock().await, 1)
                .unwrap()
                .reading_progress,
            0.8
        );
        assert_eq!(
            replication::records(&*a.lock().await).unwrap(),
            replication::records(&*b.lock().await).unwrap()
        );
        task.abort();
    }

    #[tokio::test]
    async fn refuses_unsafe_or_malformed_remote_and_keeps_local_library() {
        let (url, server, task) = server().await;
        let http = client();
        let a = database();
        seed(&a).await;
        connect(&a, &http, &url, "alice", "secret").await.unwrap();
        {
            let mut s = server.lock().unwrap();
            s.body = Some(b"not json".to_vec());
            s.missing_etag = true;
        }
        assert!(matches!(
            sync_now(&a, &http).await,
            Err(AppError::Coded("webdavEtagRequired"))
        ));
        server.lock().unwrap().missing_etag = false;
        assert!(matches!(
            sync_now(&a, &http).await,
            Err(AppError::Coded("webdavInvalidData"))
        ));
        server.lock().unwrap().body = Some(br#"{"version":999,"records":[]}"#.to_vec());
        assert!(matches!(
            sync_now(&a, &http).await,
            Err(AppError::Coded("webdavVersion"))
        ));
        assert!(db::get_article(&*a.lock().await, 1).unwrap().is_read);
        assert!(status(&a).await.unwrap().last_error.is_some());
        task.abort();
    }
    #[tokio::test]
    async fn failed_auth_does_not_save_credentials_and_failed_upload_can_retry() {
        let (url, server, task) = server().await;
        let http = client();
        let a = database();
        seed(&a).await;
        server.lock().unwrap().deny = true;
        assert!(matches!(
            connect(&a, &http, &url, "a", "p").await,
            Err(AppError::Coded("webdavAuthFailed"))
        ));
        assert!(!status(&a).await.unwrap().connected);
        server.lock().unwrap().deny = false;
        connect(&a, &http, &url, "a", "p").await.unwrap();
        server.lock().unwrap().fail_upload = true;
        assert!(sync_now(&a, &http).await.is_err());
        server.lock().unwrap().fail_upload = false;
        sync_now(&a, &http).await.unwrap();
        assert!(status(&a).await.unwrap().last_error.is_none());
        task.abort();
    }
    #[tokio::test]
    async fn migrates_legacy_and_only_transfers_changed_state_chunks() {
        let (url, server, task) = server().await;
        let http = client();
        let a = database();
        let b = database();
        seed(&a).await;
        {
            let c = a.lock().await;
            c.execute(
                "UPDATE articles SET body_text=?1 WHERE id=1",
                ["article body ".repeat(100_000)],
            )
            .unwrap();
            let mut s = server.lock().unwrap();
            s.body = Some(
                serde_json::to_vec(&Snapshot {
                    version: 5,
                    records: replication::records(&c).unwrap(),
                })
                .unwrap(),
            );
            s.revision = 1;
        }
        connect(&a, &http, &url, "u", "p").await.unwrap();
        sync_now(&a, &http).await.unwrap();
        let old: chunks::Manifest =
            serde_json::from_slice(server.lock().unwrap().body.as_ref().unwrap()).unwrap();
        assert_eq!(old.version, 7);
        connect(&b, &http, &url, "u", "p").await.unwrap();
        sync_now(&b, &http).await.unwrap();
        let (puts, gets) = {
            let s = server.lock().unwrap();
            (s.chunk_puts, s.chunk_gets)
        };
        sync_now(&a, &http).await.unwrap();
        sync_now(&b, &http).await.unwrap();
        {
            let s = server.lock().unwrap();
            assert_eq!((s.chunk_puts, s.chunk_gets), (puts, gets));
        }
        replication::set_progress(&*a.lock().await, 1, 0.73).unwrap();
        sync_now(&a, &http).await.unwrap();
        let new: chunks::Manifest =
            serde_json::from_slice(server.lock().unwrap().body.as_ref().unwrap()).unwrap();
        let article_hashes = |m: &chunks::Manifest| {
            m.chunks
                .iter()
                .filter(|p| p.kind == "article")
                .map(|p| p.hash.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(article_hashes(&old), article_hashes(&new));
        assert_eq!(server.lock().unwrap().chunk_puts, puts + 1);
        sync_now(&b, &http).await.unwrap();
        assert_eq!(server.lock().unwrap().chunk_gets, gets + 1);
        assert_eq!(
            db::get_article(&*b.lock().await, 1)
                .unwrap()
                .reading_progress,
            0.73
        );
        task.abort();
    }

    #[tokio::test]
    async fn corrupt_or_missing_chunks_do_not_partially_import() {
        let (url, server, task) = server().await;
        let http = client();
        let a = database();
        let b = database();
        seed(&a).await;
        connect(&a, &http, &url, "u", "p").await.unwrap();
        sync_now(&a, &http).await.unwrap();
        connect(&b, &http, &url, "u", "p").await.unwrap();
        let (path, bytes) = {
            let s = server.lock().unwrap();
            let (p, b) = s.objects.iter().next_back().unwrap();
            (p.clone(), b.clone())
        };
        server
            .lock()
            .unwrap()
            .objects
            .insert(path.clone(), vec![b'x'; bytes.len()]);
        assert!(matches!(
            sync_now(&b, &http).await,
            Err(AppError::Coded("webdavInvalidData"))
        ));
        assert_eq!(
            b.lock()
                .await
                .query_row("SELECT count(*) FROM feeds", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        server.lock().unwrap().objects.remove(&path);
        assert!(sync_now(&b, &http).await.is_err());
        assert_eq!(
            b.lock()
                .await
                .query_row("SELECT count(*) FROM feeds", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        server.lock().unwrap().objects.insert(path, bytes);
        sync_now(&b, &http).await.unwrap();
        assert_eq!(
            db::get_article(&*b.lock().await, 1).unwrap().title,
            "Article"
        );
        task.abort();
    }

    #[tokio::test]
    async fn manifest_compare_and_swap_failure_keeps_old_snapshot_and_resumes() {
        let (url, server, task) = server().await;
        let http = client();
        let a = database();
        let b = database();
        seed(&a).await;
        connect(&a, &http, &url, "u", "p").await.unwrap();
        sync_now(&a, &http).await.unwrap();
        let before = server.lock().unwrap().body.clone();
        replication::set_progress(&*a.lock().await, 1, 0.5).unwrap();
        // The root fails after all immutable chunks have been published.
        server.lock().unwrap().fail_root = true;
        assert!(sync_now(&a, &http).await.is_err());
        assert_eq!(server.lock().unwrap().body, before);
        connect(&b, &http, &url, "u", "p").await.unwrap();
        sync_now(&b, &http).await.unwrap();
        assert_eq!(
            db::get_article(&*b.lock().await, 1)
                .unwrap()
                .reading_progress,
            0.0
        );
        server.lock().unwrap().fail_root = false;
        sync_now(&a, &http).await.unwrap();
        sync_now(&b, &http).await.unwrap();
        assert_eq!(
            db::get_article(&*b.lock().await, 1)
                .unwrap()
                .reading_progress,
            0.5
        );
        task.abort();
    }

    #[tokio::test]
    #[ignore = "large disk-backed integration test: transfers over 1.3 GiB of logical article data"]
    async fn library_over_1_3_gib_round_trips_with_bounded_chunks() {
        let (url, server, task) = server().await;
        let http = client();
        let dir = std::env::temp_dir().join(format!("papr-large-sync-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = Mutex::new(db::open(&dir.join("a.db")).unwrap());
        let b = Mutex::new(db::open(&dir.join("b.db")).unwrap());
        {
            let mut c = a.lock().await;
            let tx = c.transaction().unwrap();
            tx.execute(
                "INSERT INTO feeds(feed_url,title) VALUES('https://example.com/rss','Large test')",
                [],
            )
            .unwrap();
            let text = "large library article ".repeat(50_000); // 1.1 MB per article
            for i in 0..1400 {
                tx.execute("INSERT INTO articles(feed_id,guid,title,body_text) VALUES(1,?1,'Large article',?2)",params![format!("large-{i:05}"),text]).unwrap();
            }
            tx.commit().unwrap();
        }
        println!("Large fixture seeded; starting initial upload");
        connect(&a, &http, &url, "u", "p").await.unwrap();
        sync_now(&a, &http).await.unwrap();
        println!("Initial upload completed; starting download/import on second device");
        let m: chunks::Manifest =
            serde_json::from_slice(server.lock().unwrap().body.as_ref().unwrap()).unwrap();
        let raw: usize = m.chunks.iter().map(|p| p.raw_bytes).sum();
        let zipped: usize = m.chunks.iter().map(|p| p.bytes).sum();
        assert!(raw > 13 * 1024 * 1024 * 1024 / 10);
        assert!(m.chunks.iter().all(|p| p.raw_bytes <= chunks::MAX_CHUNK));
        connect(&b, &http, &url, "u", "p").await.unwrap();
        sync_now(&b, &http).await.unwrap();
        assert_eq!(
            b.lock()
                .await
                .query_row(
                    "SELECT count(*),sum(length(body_text)) FROM articles",
                    [],
                    |r| Ok((r.get::<_, usize>(0)?, r.get::<_, usize>(1)?))
                )
                .unwrap(),
            (1400, "large library article ".len() * 50_000 * 1400)
        );
        let (puts, gets) = {
            let s = server.lock().unwrap();
            (s.chunk_puts, s.chunk_gets)
        };
        replication::set_progress(&*a.lock().await, 1, 0.42).unwrap();
        sync_now(&a, &http).await.unwrap();
        sync_now(&b, &http).await.unwrap();
        assert_eq!(server.lock().unwrap().chunk_puts, puts + 1);
        assert_eq!(server.lock().unwrap().chunk_gets, gets + 1);
        assert_eq!(
            db::get_article(&*b.lock().await, 1)
                .unwrap()
                .reading_progress,
            0.42
        );
        println!("Large sync: {raw} uncompressed bytes, {zipped} compressed bytes, {} chunks; subsequent progress edit transferred one chunk in each direction",m.chunks.len());
        drop(a);
        drop(b);
        task.abort();
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[tokio::test]
    async fn v6_folders_upgrade_to_scoped_paths_without_reuploading_articles() {
        use sha2::{Digest, Sha256};
        use std::io::Write;
        let (url, server, task) = server().await;
        let http = client();
        let a = database();
        let b = database();
        seed(&a).await;
        {
            let c = a.lock().await;
            let p = db::create_folder(&c, "Root").unwrap();
            let q = db::create_folder_in(&c, "Child", Some(p)).unwrap();
            db::move_feed(&c, 1, Some(q)).unwrap();
        }
        let mut old = replication::records(&*a.lock().await).unwrap();
        for r in &mut old {
            let basename = |key: &str| {
                serde_json::from_str::<Vec<String>>(key)
                    .unwrap()
                    .pop()
                    .unwrap()
            };
            if r.kind == "folder" {
                r.key = basename(&r.key);
                if let Some(p) = r.data["parent_id"].as_str() {
                    r.data["parent_id"] = serde_json::Value::String(basename(p));
                }
            }
            if r.kind == "feed" {
                if let Some(p) = r.data["folder_id"].as_str() {
                    r.data["folder_id"] = serde_json::Value::String(basename(p));
                }
            }
        }
        let mut manifest = chunks::Manifest::empty();
        manifest.version = 6;
        let mut article_hash = String::new();
        for r in old {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            let mut raw = serde_json::to_vec(&r).unwrap();
            raw.push(b'\n');
            encoder.write_all(&raw).unwrap();
            let body = encoder.finish().unwrap();
            let part = chunks::Part {
                hash: format!("{:x}", Sha256::digest(&body)),
                kind: r.kind.clone(),
                bucket: Sha256::digest(r.key.as_bytes())[0],
                bytes: body.len(),
                raw_bytes: raw.len(),
                records: 1,
            };
            if r.kind == "article" {
                article_hash = part.hash.clone();
            }
            server
                .lock()
                .unwrap()
                .objects
                .insert(format!("/papr/{}", chunks::path(&part)), body);
            manifest.chunks.push(part);
        }
        {
            let mut s = server.lock().unwrap();
            s.body = Some(serde_json::to_vec(&manifest).unwrap());
            s.revision = 1;
        }
        connect(&b, &http, &url, "u", "p").await.unwrap();
        sync_now(&b, &http).await.unwrap();
        let c = b.lock().await;
        let parent:String=c.query_row("SELECT p.name FROM feeds f JOIN folders leaf ON leaf.id=f.folder_id JOIN folders p ON p.id=leaf.parent_id",[],|r|r.get(0)).unwrap();
        assert_eq!(parent, "Root");
        drop(c);
        let current: chunks::Manifest =
            serde_json::from_slice(server.lock().unwrap().body.as_ref().unwrap()).unwrap();
        assert_eq!(current.version, 7);
        assert_eq!(
            current
                .chunks
                .iter()
                .find(|p| p.kind == "article")
                .unwrap()
                .hash,
            article_hash
        );
        task.abort();
    }
    #[test]
    fn validates_directory_urls() {
        assert_eq!(
            base_url(" https://example.com/dav/papr ").unwrap().as_str(),
            "https://example.com/dav/papr/"
        );
        for url in [
            "file:///tmp/test",
            "https://user:password@example.com/dav",
            "https://example.com/dav?token=secret",
            "https://example.com/#fragment",
        ] {
            assert!(base_url(url).is_err());
        }
    }
}
