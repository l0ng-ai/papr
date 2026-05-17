//! System notifications for new articles and the Dock unread badge.

use crate::db;
use crate::state::AppState;
use chrono::{Local, Timelike};
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

/// Read a `"1"` / `"0"` boolean setting, falling back to `default`.
fn flag(conn: &rusqlite::Connection, key: &str, default: bool) -> bool {
    db::get_setting(conn, key)
        .ok()
        .flatten()
        .map(|v| v == "1" || v == "true")
        .unwrap_or(default)
}

/// Refresh the Dock badge to the unread count (or clear it when disabled).
pub async fn update_badge(app: &AppHandle) {
    let count = {
        let state = app.state::<AppState>();
        let conn = state.db.lock().await;
        if !flag(&conn, "notify_badge", true) {
            0
        } else {
            db::count_unread(&conn).unwrap_or(0)
        }
    };
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_badge_count(if count > 0 { Some(count) } else { None });
    }
}

/// Localised "new articles" notification body for the active UI language.
fn new_articles_body(lang: &str, count: usize) -> String {
    // Defaults to English — the app's fallback language — when unset.
    match lang {
        "zh" => format!("{count} 篇新文章"),
        "ja" => format!("新着記事 {count} 件"),
        _ => format!("{count} new article{}", if count == 1 { "" } else { "s" }),
    }
}

/// Show a "new articles" notification, honouring the notification preferences
/// (enabled, sound, night do-not-disturb window).
pub async fn notify_new_articles(app: &AppHandle, count: usize) {
    if count == 0 {
        return;
    }
    let (sound, lang) = {
        let state = app.state::<AppState>();
        let conn = state.db.lock().await;
        if !flag(&conn, "notify_enabled", true) {
            return;
        }
        if flag(&conn, "notify_dnd_night", false) {
            let hour = Local::now().hour();
            if hour >= 22 || hour < 8 {
                return;
            }
        }
        let lang = db::get_setting(&conn, "language")
            .ok()
            .flatten()
            .unwrap_or_default();
        (flag(&conn, "notify_sound", false), lang)
    };

    let mut builder = app
        .notification()
        .builder()
        .title("Papr")
        .body(new_articles_body(&lang, count));
    if sound {
        builder = builder.sound("default");
    }
    let _ = builder.show();
}
