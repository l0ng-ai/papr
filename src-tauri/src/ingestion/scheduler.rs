//! Feed refresh: a reusable `refresh_all` (driven by both the manual command
//! and the periodic timer) plus the background scheduler loop.

use crate::db;
use crate::error::AppResult;
use crate::ingestion::{fetch, parse};
use crate::models::RefreshProgress;
use crate::state::AppState;
use crate::{notify, sync, tray};
use std::sync::Arc;
use std::time::Duration;
use tauri::{ipc::Channel, AppHandle, Emitter, Manager};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Outcome of fetching one feed.
enum Outcome {
    NotModified,
    Updated {
        parsed: parse::ParsedFeed,
        etag: Option<String>,
        last_modified: Option<String>,
    },
    Failed(String),
}

async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
    etag: Option<String>,
    last_modified: Option<String>,
) -> Outcome {
    match fetch::conditional_get(client, url, etag.as_deref(), last_modified.as_deref()).await {
        Ok(fetch::Fetched::NotModified) => Outcome::NotModified,
        Ok(fetch::Fetched::Body {
            bytes,
            etag,
            last_modified,
        }) => match parse::parse_feed(&bytes, url) {
            Ok(parsed) => Outcome::Updated {
                parsed,
                etag,
                last_modified,
            },
            Err(e) => Outcome::Failed(e.to_string()),
        },
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// Read an integer setting, falling back to `default`.
fn int_setting(conn: &rusqlite::Connection, key: &str, default: i64) -> i64 {
    db::get_setting(conn, key)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Refresh every feed (bounded concurrency). Streams per-feed progress over
/// `progress` when provided, emits `feeds-updated`, fires a notification,
/// runs retention cleanup, syncs to FreshRSS, and returns the new-article count.
pub async fn refresh_all(
    app: &AppHandle,
    progress: Option<Channel<RefreshProgress>>,
    wait_if_busy: bool,
) -> AppResult<usize> {
    let state = app.state::<AppState>();

    // Only one refresh at a time: the manual command and the periodic
    // scheduler would otherwise duplicate every fetch. `wait_if_busy` callers
    // (OPML import) queue behind an in-flight run so their freshly added
    // feeds still get fetched; everyone else bows out cleanly.
    let _refresh_guard = if wait_if_busy {
        state.refresh_lock.lock().await
    } else {
        match state.refresh_lock.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                log::debug!("refresh already in progress; skipping this run");
                if let Some(p) = &progress {
                    let _ = p.send(RefreshProgress::Started { total: 0 });
                    let _ = p.send(RefreshProgress::Finished { new_articles: 0 });
                }
                return Ok(0);
            }
        }
    };

    let (feeds, concurrency, dedup, rules) = {
        let conn = state.db.lock().await;
        let feeds = db::feeds_to_refresh(&conn)?;
        let concurrency = int_setting(&conn, "net_concurrency", 6).clamp(1, 16) as usize;
        let dedup = db::get_setting(&conn, "dedup_enabled")
            .ok()
            .flatten()
            .map(|v| v == "1")
            .unwrap_or(false);
        let rules = db::active_rules(&conn).unwrap_or_default();
        (feeds, concurrency, dedup, rules)
    };
    if let Some(p) = &progress {
        let _ = p.send(RefreshProgress::Started { total: feeds.len() });
    }

    let sem = Arc::new(Semaphore::new(concurrency));
    let mut set: JoinSet<(i64, Outcome)> = JoinSet::new();
    for (id, url, etag, last_modified) in feeds {
        let client = state.http();
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire().await;
            (id, fetch_one(&client, &url, etag, last_modified).await)
        });
    }

    let mut total_new = 0usize;
    while let Some(joined) = set.join_next().await {
        let Ok((feed_id, outcome)) = joined else {
            continue;
        };
        let mut new_here = 0usize;
        let mut error: Option<String> = None;

        match outcome {
            Outcome::NotModified => {
                let conn = state.db.lock().await;
                let _ = db::touch_feed(&conn, feed_id);
            }
            Outcome::Failed(e) => {
                let conn = state.db.lock().await;
                let _ = db::set_feed_error(&conn, feed_id, &e);
                error = Some(e);
            }
            Outcome::Updated {
                parsed,
                etag,
                last_modified,
            } => {
                // Insert in bounded chunks, releasing the shared DB lock
                // between each so concurrent UI queries aren't starved while
                // a large feed (hundreds of items) is being ingested.
                for chunk in parsed.articles.chunks(64) {
                    let conn = state.db.lock().await;
                    for article in chunk {
                        match db::upsert_article(&conn, feed_id, article, dedup, &rules) {
                            Ok(true) => new_here += 1,
                            Ok(false) => {}
                            Err(e) => log::warn!(
                                "upsert_article failed (feed {feed_id}): {e}"
                            ),
                        }
                    }
                }
                let conn = state.db.lock().await;
                let _ = db::update_feed_meta(
                    &conn,
                    feed_id,
                    parsed.title.as_deref(),
                    parsed.site_url.as_deref(),
                    parsed.description.as_deref(),
                    parsed.icon.as_deref(),
                );
                let _ = db::set_feed_fetch_state(
                    &conn,
                    feed_id,
                    etag.as_deref(),
                    last_modified.as_deref(),
                    None,
                );
            }
        }

        total_new += new_here;
        if let Some(p) = &progress {
            let _ = p.send(RefreshProgress::FeedDone {
                feed_id,
                new_articles: new_here,
                error,
            });
        }
    }

    // Retention: drop old read articles when a finite window is configured.
    // The DELETE scans the whole table, so throttle it to once per day rather
    // than running on every refresh cycle.
    {
        let conn = state.db.lock().await;
        let retention = db::get_setting(&conn, "retention_days").ok().flatten();
        if let Some(days) = retention.and_then(|v| v.parse::<i64>().ok()) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let last_run = db::get_setting(&conn, "retention_last_run")
                .ok()
                .flatten()
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            if now - last_run >= 86_400 {
                match db::cleanup_old_articles(&conn, days) {
                    Ok(removed) => {
                        if removed > 0 {
                            log::info!("retention: removed {removed} old articles");
                        }
                        let _ =
                            db::set_setting(&conn, "retention_last_run", &now.to_string());
                    }
                    Err(e) => log::warn!("retention cleanup failed: {e}"),
                }
            }
        }
    }

    if let Some(p) = &progress {
        let _ = p.send(RefreshProgress::Finished {
            new_articles: total_new,
        });
    }
    let _ = app.emit("feeds-updated", total_new);

    notify::notify_new_articles(app, total_new).await;

    // Reconcile read/starred state with the sync server, if one is connected.
    if let Err(e) = sync::run_if_connected(app).await {
        log::warn!("sync failed: {e}");
    }

    notify::update_badge(app).await;
    tray::refresh(app).await;
    Ok(total_new)
}

/// Read the refresh interval (minutes) from settings, defaulting to 30.
async fn refresh_interval_minutes(app: &AppHandle) -> u64 {
    let state = app.state::<AppState>();
    let conn = state.db.lock().await;
    db::get_setting(&conn, "refresh_interval_min")
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .filter(|m| *m >= 5)
        .unwrap_or(30)
}

/// Spawn the background refresh loop. The app must stay resident (tray) for
/// this to run — macOS does not execute the process after the app is quit.
pub fn spawn_scheduler(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(8)).await;
        loop {
            if let Err(e) = refresh_all(&app, None, false).await {
                log::warn!("scheduled refresh failed: {e}");
            }
            let mins = refresh_interval_minutes(&app).await;
            tokio::time::sleep(Duration::from_secs(mins * 60)).await;
        }
    });
}
