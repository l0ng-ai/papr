//! Menu-bar tray: a localised, live-updating status menu.
//!
//! The icon is a monochrome template glyph (adapts to the light/dark menu
//! bar); the menu shows the unread count and last-refresh time and offers
//! quick actions. It is rebuilt after every refresh and on language change.

use crate::db;
use crate::ingestion::scheduler;
use crate::models::ArticleQuery;
use crate::notify;
use crate::state::AppState;
use chrono::{NaiveDateTime, Utc};
use tauri::image::Image;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, Wry};

const TRAY_ID: &str = "papr-tray";
const ICON: &[u8] = include_bytes!("../icons/tray.png");

/// Format an elapsed-time string for a stored `datetime('now')` timestamp.
fn ago(lang: &str, iso: &str) -> String {
    let Ok(naive) = NaiveDateTime::parse_from_str(iso, "%Y-%m-%d %H:%M:%S") else {
        return iso.to_string();
    };
    let secs = (Utc::now() - naive.and_utc()).num_seconds().max(0);
    let (mins, hours, days) = (secs / 60, secs / 3600, secs / 86400);
    match lang {
        "zh" => {
            if mins < 1 {
                "刚刚".into()
            } else if mins < 60 {
                format!("{mins} 分钟前")
            } else if hours < 24 {
                format!("{hours} 小时前")
            } else {
                format!("{days} 天前")
            }
        }
        "ja" => {
            if mins < 1 {
                "たった今".into()
            } else if mins < 60 {
                format!("{mins} 分前")
            } else if hours < 24 {
                format!("{hours} 時間前")
            } else {
                format!("{days} 日前")
            }
        }
        _ => {
            if mins < 1 {
                "just now".into()
            } else if mins < 60 {
                format!("{mins}m ago")
            } else if hours < 24 {
                format!("{hours}h ago")
            } else {
                format!("{days}d ago")
            }
        }
    }
}

struct Labels {
    unread: String,
    refreshed: String,
    open: &'static str,
    refresh: &'static str,
    mark_all: &'static str,
    settings: &'static str,
    quit: &'static str,
}

fn labels(lang: &str, unread: i64, last: Option<&str>) -> Labels {
    let unread_line = match lang {
        "zh" => {
            if unread > 0 {
                format!("{unread} 篇未读")
            } else {
                "暂无未读".into()
            }
        }
        "ja" => {
            if unread > 0 {
                format!("未読 {unread} 件")
            } else {
                "未読なし".into()
            }
        }
        _ => {
            if unread > 0 {
                format!("{unread} unread")
            } else {
                "No unread articles".into()
            }
        }
    };
    let refreshed_line = match (lang, last) {
        ("zh", Some(t)) => format!("上次刷新：{}", ago("zh", t)),
        ("zh", None) => "尚未刷新".into(),
        ("ja", Some(t)) => format!("最終更新：{}", ago("ja", t)),
        ("ja", None) => "未更新".into(),
        (_, Some(t)) => format!("Updated {}", ago("en", t)),
        (_, None) => "Never refreshed".into(),
    };
    let (open, refresh, mark_all, settings, quit) = match lang {
        "zh" => (
            "打开 Papr",
            "立即刷新全部",
            "全部标为已读",
            "设置…",
            "退出 Papr",
        ),
        "ja" => (
            "Papr を開く",
            "今すぐすべて更新",
            "すべて既読にする",
            "設定…",
            "Papr を終了",
        ),
        _ => (
            "Open Papr",
            "Refresh All Now",
            "Mark All as Read",
            "Settings…",
            "Quit Papr",
        ),
    };
    Labels {
        unread: unread_line,
        refreshed: refreshed_line,
        open,
        refresh,
        mark_all,
        settings,
        quit,
    }
}

fn build_menu(
    app: &AppHandle,
    lang: &str,
    unread: i64,
    last: Option<&str>,
) -> tauri::Result<Menu<Wry>> {
    let l = labels(lang, unread, last);
    // The two status lines are disabled — they are read-only labels.
    let status = MenuItem::with_id(app, "tray_unread", &l.unread, false, None::<&str>)?;
    let refreshed =
        MenuItem::with_id(app, "tray_refreshed", &l.refreshed, false, None::<&str>)?;
    let open = MenuItem::with_id(app, "tray_open", l.open, true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "tray_refresh", l.refresh, true, None::<&str>)?;
    let mark = MenuItem::with_id(app, "tray_markall", l.mark_all, true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "tray_settings", l.settings, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "tray_quit", l.quit, true, None::<&str>)?;
    let s1 = PredefinedMenuItem::separator(app)?;
    let s2 = PredefinedMenuItem::separator(app)?;
    let s3 = PredefinedMenuItem::separator(app)?;
    let s4 = PredefinedMenuItem::separator(app)?;
    Menu::with_items(
        app,
        &[
            &status, &refreshed, &s1, &open, &s2, &refresh, &mark, &s3, &settings, &s4,
            &quit,
        ],
    )
}

fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn handle_event(app: &AppHandle, event: MenuEvent) {
    match event.id.as_ref() {
        "tray_open" => show_window(app),
        "tray_refresh" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ =
                    scheduler::refresh_all(&app, None, false, scheduler::RefreshScope::All)
                        .await;
            });
        }
        "tray_markall" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                {
                    let state = app.state::<AppState>();
                    let conn = state.db.lock().await;
                    let _ = db::mark_all_read(
                        &conn,
                        &ArticleQuery::All,
                        db::is_freshrss_connected(&conn),
                    );
                }
                let _ = app.emit("feeds-updated", 0);
                notify::update_badge(&app).await;
                refresh(&app).await;
            });
        }
        "tray_settings" => {
            show_window(app);
            let _ = app.emit("tray-open-settings", ());
        }
        "tray_quit" => app.exit(0),
        _ => {}
    }
}

/// Install the tray icon at startup. `lang`, `unread` and `last` are read from
/// the database by the caller (the connection is not yet behind the mutex).
pub fn build(
    app: &AppHandle,
    lang: &str,
    unread: i64,
    last: Option<&str>,
) -> tauri::Result<()> {
    let menu = build_menu(app, lang, unread, last)?;
    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(ICON)?)
        .icon_as_template(true)
        .tooltip("Papr")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(handle_event)
        .build(app)?;
    if unread > 0 {
        let _ = tray.set_title(Some(unread.to_string()));
    }
    Ok(())
}

/// Rebuild the tray menu and menu-bar count from the current database state.
/// Called after refreshes, mark-all-read, data clears and language changes.
pub async fn refresh(app: &AppHandle) {
    let (lang, unread, last) = {
        let state = app.state::<AppState>();
        let conn = state.read().await;
        (
            db::get_setting(&conn, "language")
                .ok()
                .flatten()
                .unwrap_or_default(),
            db::count_unread(&conn).unwrap_or(0),
            db::latest_fetch(&conn).ok().flatten(),
        )
    };
    // Tray mutations touch AppKit, which on macOS must happen on the main
    // thread. Callers reach here from the async runtime (commands and the
    // background scheduler), where `set_menu`/`set_title` would silently
    // no-op — so hop onto the main thread before touching the tray.
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        let Some(tray) = app.tray_by_id(TRAY_ID) else {
            return;
        };
        if let Ok(menu) = build_menu(&app, &lang, unread, last.as_deref()) {
            let _ = tray.set_menu(Some(menu));
        }
        // Pass an empty string rather than `None` to clear the count: on
        // macOS `set_title(None)` leaves the previous title in place, so a
        // drop to zero would otherwise keep showing the stale number.
        let _ = tray.set_title(Some(if unread > 0 {
            unread.to_string()
        } else {
            String::new()
        }));
    });
}
