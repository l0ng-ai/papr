//! In-app original-page view (issue #49).
//!
//! A child webview overlaid on the main window's reading area, so feeds that
//! only ship a summary — or sites that refuse to be framed via
//! `X-Frame-Options` (V2EX, most news/community sites) — still render inside
//! Papr. An `<iframe>` cannot do this: the remote server rejects the frame and
//! the browser gives no reliable failure signal across origins. A native child
//! webview is a real WebView, so framing headers do not apply.
//!
//! The child webview is NOT in the DOM — it floats above the main webview — so
//! the frontend measures the reading area and feeds us logical (CSS-px) bounds,
//! and re-sends them whenever the window resizes.
//!
//! Security: the `page-view` label appears in no capability (see
//! `capabilities/default.json`, scoped to `["main"]`), so even though Tauri
//! injects its IPC bridge, the ACL denies every command — remote pages cannot
//! reach the backend.
//!
//! Built on Tauri's `unstable` child-webview API; see `Cargo.toml`.

use tauri::{
    webview::WebviewBuilder, AppHandle, LogicalPosition, LogicalSize, Manager, WebviewUrl,
};

/// Label of the single, reused original-page child webview.
const LABEL: &str = "page-view";

fn parse_url(url: &str) -> Result<url::Url, String> {
    url.parse().map_err(|_| format!("invalid url: {url}"))
}

/// Show the original page at `url` over the given reading-area rectangle.
/// Reuses the existing child webview when already open (just navigate + move),
/// so switching articles doesn't flash a teardown/rebuild.
#[tauri::command]
pub async fn open_page_view(
    app: AppHandle,
    url: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let parsed = parse_url(&url)?;
    let position = LogicalPosition::new(x, y);
    let size = LogicalSize::new(width, height);

    if let Some(view) = app.get_webview(LABEL) {
        view.navigate(parsed).map_err(|e| e.to_string())?;
        view.set_position(position).map_err(|e| e.to_string())?;
        view.set_size(size).map_err(|e| e.to_string())?;
        return Ok(());
    }

    // `get_window` / `add_child` are part of the `unstable` feature. The main
    // WebviewWindow's underlying Window shares its "main" label.
    let window = app.get_window("main").ok_or("main window not found")?;
    let builder = WebviewBuilder::new(LABEL, WebviewUrl::External(parsed));
    window
        .add_child(builder, position, size)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Reposition/resize the open page view (window resized, sidebar toggled, …).
/// No-op when the view isn't open.
#[tauri::command]
pub async fn set_page_view_bounds(
    app: AppHandle,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    if let Some(view) = app.get_webview(LABEL) {
        view.set_position(LogicalPosition::new(x, y))
            .map_err(|e| e.to_string())?;
        view.set_size(LogicalSize::new(width, height))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Show or hide the open page view without tearing it down. A transient
/// overlay (context menu, modal, AI drawer) only needs the native webview out
/// of the way for a moment — hiding keeps the loaded page alive, so dismissing
/// the overlay reveals it instantly instead of reloading the whole page.
/// No-op when the view isn't open.
#[tauri::command]
pub async fn set_page_view_visible(app: AppHandle, visible: bool) -> Result<(), String> {
    if let Some(view) = app.get_webview(LABEL) {
        if visible {
            view.show().map_err(|e| e.to_string())?;
        } else {
            view.hide().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Tear down the page view (left web mode, switched away, reader unmounted).
#[tauri::command]
pub async fn close_page_view(app: AppHandle) -> Result<(), String> {
    if let Some(view) = app.get_webview(LABEL) {
        view.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}
