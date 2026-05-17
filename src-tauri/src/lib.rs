//! Papr — a local-first RSS reader. Tauri application entry point: opens the
//! database, wires shared state, installs the macOS tray, and starts the
//! background refresh scheduler.

mod ai;
mod commands;
mod db;
mod error;
mod extraction;
mod ingestion;
mod models;
mod notify;
mod opml;
mod sanitize;
mod state;
mod sync;
mod tray;

use state::AppState;
use std::fs;
use tauri::Manager;

/// Number of read-only connections in the UI query pool.
const READ_POOL_SIZE: usize = 4;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            // ── Database ──────────────────────────────────────────────
            let data_dir = app.path().app_data_dir().expect("resolve app data dir");
            fs::create_dir_all(&data_dir).ok();
            let db_path = data_dir.join("papr.db");
            let conn = db::open(&db_path).expect("open database");
            // On the very first launch, subscribe to a curated set of feeds.
            let seeded = db::seed_default_feeds(&conn).unwrap_or(false);
            // A small pool of read-only connections for UI queries — under WAL
            // they run concurrently with the writer, so the interface stays
            // responsive while a background refresh is writing.
            let readers: Vec<_> = (0..READ_POOL_SIZE)
                .map(|_| db::open_reader(&db_path).expect("open reader connection"))
                .collect();
            // The HTTP client honours the persisted proxy / timeout settings.
            let http = ingestion::fetch::build_client_from_settings(&conn);
            // Snapshot the state the tray menu needs (read before the
            // connection moves behind the async mutex).
            let lang = db::get_setting(&conn, "language")
                .ok()
                .flatten()
                .unwrap_or_default();
            let unread = db::count_unread(&conn).unwrap_or(0);
            let latest_fetch = db::latest_fetch(&conn).ok().flatten();

            app.manage(AppState::new(conn, readers, http));

            // ── macOS window vibrancy ─────────────────────────────────
            #[cfg(target_os = "macos")]
            {
                use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial};
                if let Some(window) = app.get_webview_window("main") {
                    let _ = apply_vibrancy(
                        &window,
                        NSVisualEffectMaterial::Sidebar,
                        None,
                        None,
                    );
                }
            }

            // ── Menu-bar tray (keeps the app resident for refreshes) ──
            tray::build(app.handle(), &lang, unread, latest_fetch.as_deref())?;

            // ── Background refresh scheduler ──────────────────────────
            ingestion::scheduler::spawn_scheduler(app.handle().clone());

            // After first-run seeding, fetch the default feeds right away.
            if seeded {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let _ = ingestion::scheduler::refresh_all(&handle, None).await;
                });
            }

            // Reflect the current unread count on the Dock badge at launch.
            let badge_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                notify::update_badge(&badge_handle).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_folders,
            commands::create_folder,
            commands::rename_folder,
            commands::delete_folder,
            commands::list_feeds,
            commands::add_feed,
            commands::delete_feed,
            commands::move_feed,
            commands::rename_feed,
            commands::refresh_feeds,
            commands::list_articles,
            commands::get_article,
            commands::mark_read,
            commands::mark_starred,
            commands::mark_read_later,
            commands::mark_all_read,
            commands::smart_counts,
            commands::extract_fulltext,
            commands::import_opml,
            commands::export_opml,
            commands::get_setting,
            commands::set_setting,
            commands::ai_summarize,
            commands::ai_ask,
            commands::ai_digest,
            commands::storage_stats,
            commands::cleanup_articles,
            commands::vacuum_db,
            commands::reset_settings,
            commands::clear_all_data,
            commands::apply_network_settings,
            commands::freshrss_connect,
            commands::freshrss_disconnect,
            commands::freshrss_status,
            commands::freshrss_sync,
            commands::refresh_tray,
            commands::list_tags,
            commands::create_tag,
            commands::rename_tag,
            commands::set_tag_color,
            commands::delete_tag,
            commands::set_article_tag,
            commands::reorder_tags,
            commands::list_rules,
            commands::create_rule,
            commands::update_rule,
            commands::delete_rule,
            commands::preview_rule,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Papr");
}
