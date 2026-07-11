//! System notifications for new articles and the Dock unread badge.

use crate::db;
use crate::db::setting_flag as flag;
use crate::state::AppState;
use chrono::{Local, Timelike};
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

/// Refresh the Dock badge to the unread count (or clear it when disabled).
///
/// The badge is a desktop-only window surface (`set_badge_count` — the macOS
/// Dock / Windows taskbar). Mobile has no equivalent window-level badge API, so
/// on those targets this is a no-op.
pub async fn update_badge(app: &AppHandle) {
    #[cfg(desktop)]
    {
        let count = {
            let state = app.state::<AppState>();
            let conn = state.read().await;
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
    #[cfg(not(desktop))]
    let _ = app;
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
    // Skip the notification when the window is already focused — the new
    // articles just appear in the list; an OS notification would only
    // interrupt content the user is actively looking at.
    let focused = app
        .get_webview_window("main")
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(false);
    if focused {
        return;
    }
    let (sound, lang) = {
        let state = app.state::<AppState>();
        let conn = state.read().await;
        if !flag(&conn, "notify_enabled", true) {
            return;
        }
        if flag(&conn, "notify_dnd_night", false) {
            let hour = Local::now().hour();
            if !(8..22).contains(&hour) {
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
