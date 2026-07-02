//! The desktop refresh scheduler: a Tauri-aware wrapper around the UI-free
//! [`papr_core::ingestion::refresh::refresh_core`].
//!
//! `refresh_all` adds what only the desktop app needs — single-flight locking,
//! a progress channel to the webview, new-article notifications, FreshRSS sync
//! and tray/badge updates — while the actual fetch/parse/ingest pipeline lives
//! in `papr-core` so the agent CLI can drive the same code headlessly.

use crate::error::AppResult;
use crate::ingestion::refresh;
use crate::models::RefreshProgress;
use crate::state::AppState;
use crate::{db, notify, sync, tray};
use std::time::Duration;
use tauri::{ipc::Channel, AppHandle, Emitter, Manager};

// Re-exported so existing callers (`commands`, `tray`) keep referring to
// `scheduler::RefreshScope` / `scheduler::NEWSLETTER_POLL_TIMEOUT_SECS`.
pub use refresh::{RefreshScope, NEWSLETTER_POLL_TIMEOUT_SECS};

/// Refresh feeds selected by `scope`, streaming per-feed progress over
/// `progress` when provided, then running the desktop-only tail: emit
/// `feeds-updated`, notify, reconcile with FreshRSS, and refresh the tray.
/// Returns the new-article count.
pub async fn refresh_all(
    app: &AppHandle,
    progress: Option<Channel<RefreshProgress>>,
    wait_if_busy: bool,
    scope: RefreshScope,
) -> AppResult<usize> {
    let state = app.state::<AppState>();

    // Only one refresh at a time: the manual command and the periodic scheduler
    // would otherwise duplicate every fetch. `wait_if_busy` callers (OPML
    // import) queue behind an in-flight run so their freshly added feeds still
    // get fetched; everyone else bows out cleanly.
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

    let client = state.http();
    let sync_connected = {
        let conn = state.read().await;
        db::is_freshrss_connected(&conn)
    };
    if sync_connected {
        log::info!("refresh: sync server connected; skipping local feed fetch");
        if let Some(p) = &progress {
            let _ = p.send(RefreshProgress::Started { total: 0 });
        }
        let synced = match sync::sync_now(&state.db, &client).await {
            Ok(n) => n,
            Err(e) => {
                log::warn!("sync failed: {e}");
                0
            }
        };
        let _ = app.emit("feeds-updated", synced);
        if let Some(p) = &progress {
            let _ = p.send(RefreshProgress::Finished {
                new_articles: synced,
            });
        }
        notify::update_badge(app).await;
        tray::refresh(app).await;
        return Ok(synced);
    }

    let summary = refresh::refresh_core(&state.db, &client, scope, |event| {
        if let Some(p) = &progress {
            let _ = p.send(event);
        }
    })
    .await?;

    // Background scheduler with nothing due this cycle: bow out before the
    // heavier tail (sync, notifications, tray) so an idle tick is genuinely idle.
    if !summary.ran {
        return Ok(0);
    }

    let total_new = summary.new_articles;
    let _ = app.emit("feeds-updated", total_new);

    notify::notify_new_articles(app, total_new).await;

    // Reconcile read/starred state with the sync server, if one is connected.
    // A sync mutates article state and may add feeds, so emit `feeds-updated`
    // again afterwards — the first emit fired before the sync touched the DB.
    match sync::run_if_connected(&state.db, &client).await {
        Ok(true) => {
            let _ = app.emit("feeds-updated", 0);
        }
        Ok(false) => {}
        Err(e) => log::warn!("sync failed: {e}"),
    }

    notify::update_badge(app).await;
    tray::refresh(app).await;
    Ok(total_new)
}

/// How often the background loop wakes to look for *due* feeds. Each tick is a
/// cheap indexed query (and an early return when nothing is due), so a short,
/// fixed cadence keeps per-feed intervals honoured to within one tick.
const SCHEDULER_TICK: Duration = Duration::from_secs(60);

/// Spawn the background refresh loop. The app must stay resident (tray) for this
/// to run — macOS does not execute the process after the app is quit.
pub fn spawn_scheduler(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(8)).await;
        loop {
            if let Err(e) = refresh_all(&app, None, false, RefreshScope::Due).await {
                log::warn!("scheduled refresh failed: {e}");
            }
            tokio::time::sleep(SCHEDULER_TICK).await;
        }
    });
}
