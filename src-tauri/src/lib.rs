//! Papr — a local-first RSS reader. Tauri application entry point: opens the
//! database, wires shared state, installs the macOS tray, and starts the
//! background refresh scheduler.

// The data layer, ingestion building blocks, sanitization and OPML now live in
// `papr-core` (shared with the agent CLI). Re-export them under their original
// crate paths so the rest of the app keeps referring to `crate::db`,
// `crate::ingestion`, etc. unchanged.
pub use papr_core::{ai, db, error, extraction, ingestion, models, opml, sanitize, sync};

mod backing;
mod commands;
mod notify;
// The in-app original-page child webview is built on Tauri's `unstable`
// multi-webview API, which only exists on desktop — mobile has no child-webview
// support — so the module (and its commands) compile out of the mobile builds.
#[cfg(desktop)]
mod page_view;
// The tauri-coupled refresh scheduler (progress channels, AppHandle) — built on
// top of `papr_core::ingestion`. Was `ingestion::scheduler` before the split.
mod scheduler;
mod state;
mod translate;
// The macOS menu-bar tray keeps the desktop app resident for background
// refreshes; there is no tray on mobile, so the module compiles out there.
#[cfg(desktop)]
mod tray;

use ingestion::discovery::{self, DeepLink};
use state::AppState;
use std::fs;
use tauri::{Emitter, Manager};

/// Handle every URL delivered through the `papr://` deep-link scheme. A
/// `papr://subscribe?url=…` link focuses the main window and emits a
/// `deep-link-subscribe` event the frontend listens for to open the
/// Add-feed dialog prefilled with the feed URL. Unrecognised links are
/// ignored. Pure parsing lives in [`discovery::parse_deep_link`].
///
/// A cold-start link is delivered to this handler from inside `setup()` —
/// *before* the webview has loaded and registered its `deep-link-subscribe`
/// listener — so a bare `emit` would be dropped on the floor and the Add-feed
/// dialog would never open. The URL is therefore also buffered in `AppState`;
/// the frontend drains that buffer once on mount, which catches the cold-start
/// case. A live link, arriving after the listener exists, is delivered by the
/// `emit`; its buffered copy is simply never drained (the mount has long
/// passed) and is discarded with the process.
fn handle_deep_links(app: &tauri::AppHandle, urls: &[String]) {
    for raw in urls {
        if let Some(DeepLink::Subscribe { url }) = discovery::parse_deep_link(raw) {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                // `unminimize` only exists on desktop — mobile windows are never
                // minimised, so bringing the app forward is `show` + `set_focus`.
                #[cfg(desktop)]
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            app.state::<AppState>().set_pending_deep_link(url.clone());
            let _ = app.emit("deep-link-subscribe", url);
        }
    }
}

/// Number of read-only connections in the UI query pool.
const READ_POOL_SIZE: usize = 4;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // `mut` is used only by the desktop-plugin block below; on mobile that block
    // is compiled out, so the binding is never reassigned there.
    #[cfg_attr(not(desktop), allow(unused_mut))]
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_os::init())
        // Deep links are cross-platform (custom-scheme on desktop, universal
        // links on mobile), so the plugin stays on every target.
        .plugin(tauri_plugin_deep_link::init());

    // Desktop-only plugins — none has a mobile counterpart, so they compile out
    // of the mobile builds (and their permissions are absent from the mobile
    // capability set):
    //   • window-state persists window geometry — mobile has no OS windows.
    //   • autostart installs a macOS LaunchAgent — the OS owns app launch on
    //     mobile.
    //   • updater/process drive in-app updates from the GitHub release feed —
    //     the App Store handles updates on mobile.
    #[cfg(desktop)]
    {
        builder = builder
            .plugin(tauri_plugin_window_state::Builder::default().build())
            .plugin(tauri_plugin_autostart::init(
                tauri_plugin_autostart::MacosLauncher::LaunchAgent,
                None,
            ))
            .plugin(tauri_plugin_updater::Builder::new().build())
            .plugin(tauri_plugin_process::init());
    }

    builder
        .setup(|app| {
            // ── Database ──────────────────────────────────────────────
            let data_dir = app.path().app_data_dir().expect("resolve app data dir");
            fs::create_dir_all(&data_dir).ok();
            let db_path = data_dir.join("papr.db");
            let conn = db::open(&db_path).expect("open database");
            // A small pool of read-only connections for UI queries — under WAL
            // they run concurrently with the writer, so the interface stays
            // responsive while a background refresh is writing.
            let readers: Vec<_> = (0..READ_POOL_SIZE)
                .map(|_| db::open_reader(&db_path).expect("open reader connection"))
                .collect();
            // The HTTP client honours the persisted proxy / timeout settings.
            let http = ingestion::fetch::build_client_from_settings(&conn);
            // Snapshot the state the tray menu needs (read before the
            // connection moves behind the async mutex). Desktop-only — the tray
            // is the sole consumer, so these reads compile out on mobile.
            #[cfg(desktop)]
            let lang = db::get_setting(&conn, "language")
                .ok()
                .flatten()
                .unwrap_or_default();
            #[cfg(desktop)]
            let unread = db::count_unread(&conn).unwrap_or(0);
            #[cfg(desktop)]
            let latest_fetch = db::latest_fetch(&conn).ok().flatten();
            // The persisted appearance (palette + mode), mirrored from the
            // frontend store. Used just below to paint the native window in the
            // matching colour before the webview's first frame. `theme` is the
            // legacy pre-6-theme key, kept for installs that predate the split.
            let palette = db::get_setting(&conn, "palette").ok().flatten();
            let mode = db::get_setting(&conn, "mode").ok().flatten();
            let theme = db::get_setting(&conn, "theme").ok().flatten();
            let dark_shade = db::get_setting(&conn, "dark_shade").ok().flatten();

            app.manage(AppState::new(conn, readers, http));

            // ── papr:// deep links (feature F6) ───────────────────────
            // Registered after `app.manage` so the handler can always reach
            // `AppState` to buffer a cold-start link. Links opened while the
            // app is already running arrive here directly; a cold-start link
            // is delivered the same way once the event loop starts.
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    let urls: Vec<String> =
                        event.urls().iter().map(|u| u.to_string()).collect();
                    handle_deep_links(&handle, &urls);
                });
                // On Linux/Windows dev builds, register the scheme at runtime
                // so `papr://` resolves without a full bundle install.
                #[cfg(any(windows, target_os = "linux"))]
                {
                    let _ = app.deep_link().register("papr");
                }
            }

            // ── Themed native backing (kills the macOS resize flash) ──
            // `tauri.conf.json` hardcodes a light window background and wry
            // leaves the WKWebView painting an opaque white backing (it only
            // disables that under its `transparent` feature, which we don't
            // build). So between window creation and the webview's first paint —
            // and in the strip a live resize exposes — a dark-theme user sees a
            // white flash. `backing::apply` repaints every native surface that
            // can show through (drawsBackground / underPageBackgroundColor /
            // NSWindow). Done here before the first frame; the frontend
            // re-asserts it on every theme change. See backing.rs / tauri#14288.
            {
                // Effective mode: explicit `mode`, else the legacy `theme` key
                // ("dark" → dark) for installs that predate the palette/mode
                // split, else — for a fresh install that hasn't mirrored any
                // preference yet — follow the OS scheme, so a dark-OS first
                // launch (default "system" mode) doesn't flash a light window.
                let is_dark = match mode.as_deref() {
                    Some("dark") => true,
                    Some("light") => false,
                    _ => match theme.as_deref() {
                        Some("dark") => true,
                        Some("light") => false,
                        _ => app
                            .get_webview_window("main")
                            .map(|w| matches!(w.theme(), Ok(tauri::Theme::Dark)))
                            .unwrap_or(false),
                    },
                };
                let pal = palette.as_deref().unwrap_or("paper");
                // Each colour matches that theme's `--reader` in styles.css and
                // the frontend's BACKING map (App.tsx). Keep them in sync so the
                // cold-start frame matches before the frontend re-asserts
                // `set_native_backing`.
                let (r, g, b) = match (pal, is_dark) {
                    ("frost", false) => (0xFF, 0xFF, 0xFF),
                    ("frost", true) => (0x1D, 0x1F, 0x23),
                    ("contrast", false) => (0xFF, 0xFF, 0xFF),
                    ("contrast", true) => (0x00, 0x00, 0x00),
                    // Paper — dark honours the legacy dark-shade key.
                    (_, true) => match dark_shade.as_deref() {
                        Some("dimmer") => (0x1C, 0x17, 0x15),
                        Some("black") => (0x15, 0x10, 0x0F),
                        _ => (0x1D, 0x1E, 0x1F),
                    },
                    (_, false) => (0xFB, 0xF9, 0xF3),
                };
                if let Some(win) = app.get_webview_window("main") {
                    backing::apply(&win, r, g, b);
                }
            }

            // ── Menu-bar tray (keeps the app resident for refreshes) ──
            // Desktop-only: there is no tray on mobile, where the OS keeps the
            // app schedulable itself.
            #[cfg(desktop)]
            tray::build(app.handle(), &lang, unread, latest_fetch.as_deref())?;

            // ── Background refresh scheduler ──────────────────────────
            scheduler::spawn_scheduler(app.handle().clone());

            // Reflect the current unread count on the Dock badge at launch.
            let badge_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                notify::update_badge(&badge_handle).await;
            });

            // ── One-time card-thumbnail backfill ──────────────────────
            // Articles ingested before the body-image fallback have no
            // thumbnail even when their HTML embeds one. Adopt that first
            // image once, for existing rows. The HTML parse is heavy, so it
            // runs on a blocking thread against a throwaway reader; only the
            // quick UPDATE batch takes the writer lock.
            let bf_handle = app.handle().clone();
            let bf_db_path = db_path.clone();
            tauri::async_runtime::spawn(async move {
                let updates = tauri::async_runtime::spawn_blocking(move || {
                    let conn = db::open_reader(&bf_db_path).ok()?;
                    if db::get_setting(&conn, "card_image_backfill_v2")
                        .ok()
                        .flatten()
                        .is_some()
                    {
                        return None;
                    }
                    Some(db::card_image_backfill_scan(&conn).unwrap_or_default())
                })
                .await
                .ok()
                .flatten();
                let Some(updates) = updates else { return };
                let state = bf_handle.state::<AppState>();
                let conn = state.db.lock().await;
                let _ = db::apply_card_images(&conn, &updates);
                let _ = db::set_setting(&conn, "card_image_backfill_v2", "1");
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
            commands::search_feed_directory,
            commands::delete_feed,
            commands::move_feed,
            commands::set_feed_refresh_interval,
            commands::set_feed_auto_translate,
            commands::rename_feed,
            commands::refresh_feeds,
            commands::list_articles,
            commands::article_index,
            commands::get_article,
            commands::mark_read,
            commands::mark_starred,
            commands::mark_read_later,
            commands::mark_all_read,
            commands::smart_counts,
            commands::extract_fulltext,
            commands::fetch_image,
            commands::import_opml,
            commands::export_opml,
            commands::get_setting,
            commands::set_setting,
            commands::ai_summarize,
            commands::ai_ask,
            commands::ai_digest,
            commands::ai_translate,
            commands::translate_article_preview,
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
            #[cfg(desktop)]
            commands::refresh_tray,
            commands::set_native_backing,
            commands::take_pending_deep_link,
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
            commands::apply_rule_to_existing,
            commands::add_newsletter_source,
            commands::list_newsletter_sources,
            commands::remove_newsletter_source,
            commands::create_highlight,
            commands::list_highlights,
            commands::list_all_highlights,
            commands::update_highlight_note,
            commands::set_highlight_color,
            commands::delete_highlight,
            #[cfg(desktop)]
            page_view::open_page_view,
            #[cfg(desktop)]
            page_view::set_page_view_bounds,
            #[cfg(desktop)]
            page_view::set_page_view_visible,
            #[cfg(desktop)]
            page_view::close_page_view,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Papr");
}
