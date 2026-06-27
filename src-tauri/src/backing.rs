//! Native window + webview backing colour (macOS resize-flash fix).
//!
//! On macOS wry builds the WKWebView with `drawsBackground = YES` — an opaque
//! WHITE backing — unless its `transparent` cargo feature is on, which we don't
//! enable. During a *live* window resize the renderer lags the new frame and the
//! freshly-exposed strip at the dragged edge shows white. On a dark theme that
//! is the "fast-resize edge flash".
//!
//! The textbook fix (drawsBackground = NO + a themed NSWindow colour) is enough
//! for a *default* window — the reference demo proves it. It is NOT enough here:
//! our window is `titleBarStyle: "Overlay"` + `hiddenTitle`, i.e. macOS
//! `fullSizeContentView`. That arrangement puts opaque container NSViews between
//! the (now non-opaque) webview and the window, and during a resize the exposed
//! strip shows whichever of those is opaque — by default an uninitialised WHITE
//! backing — *before* the NSWindow colour is ever reached.
//!
//! So we paint, in the theme colour, every native surface that strip can show:
//!   • `drawsBackground = NO` + `setOpaque: NO` — the page never paints its own
//!     white backdrop and lets lower layers composite through.
//!   • `underPageBackgroundColor` — the overscroll / resize *gutter* (macOS 12+),
//!     which otherwise stays stuck on the light window-config colour.
//!   • the CALayer of the webview **and every ancestor up to the window's
//!     contentView** — this is what actually kills the white strip under the
//!     Overlay titlebar. We stop at contentView so the titlebar frame
//!     (NSThemeFrame) is left untouched.
//!   • `NSWindow.backgroundColor` — the final layer behind everything.
//!
//! Called once in `setup()` with the saved theme's colour (before the first
//! frame) and re-asserted from the frontend on every theme change via the
//! `set_native_backing` command. See tauri-apps/tauri#14288.

/// Pin every native surface a live resize can expose to `(r, g, b)`.
#[cfg(target_os = "macos")]
pub fn apply(window: &tauri::WebviewWindow, r: u8, g: u8, b: u8) {
    let _ = window.with_webview(move |webview| unsafe {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;
        use objc2_app_kit::{NSColor, NSWindow};
        use objc2_foundation::{NSNumber, NSString};
        use objc2_web_kit::WKWebView;

        let wk: &WKWebView = &*webview.inner().cast::<WKWebView>();
        let wk_obj = wk as *const WKWebView as *mut AnyObject;

        // 1) never paint the page's own (white) backdrop; let lower layers show.
        let no = NSNumber::numberWithBool(false);
        let key = NSString::from_str("drawsBackground");
        let _: () = msg_send![wk_obj, setValue: &*no, forKey: &*key];
        let _: () = msg_send![wk_obj, setOpaque: false];

        let color = NSColor::colorWithSRGBRed_green_blue_alpha(
            r as f64 / 255.0,
            g as f64 / 255.0,
            b as f64 / 255.0,
            1.0,
        );
        let cg = color.CGColor();

        // 2) the overscroll / live-resize gutter (macOS 12+).
        let _: () = msg_send![wk_obj, setUnderPageBackgroundColor: &*color];

        let ns_window: &NSWindow = &*webview.ns_window().cast::<NSWindow>();
        let win_obj = ns_window as *const NSWindow as *mut AnyObject;
        let content_view: *mut AnyObject = msg_send![win_obj, contentView];

        // 3) paint the CALayer of the webview and every ancestor up to (and
        //    including) the window's contentView — the opaque white container
        //    layers the Overlay titlebar leaves between webview and window.
        let mut v = wk_obj;
        while !v.is_null() {
            let _: () = msg_send![v, setWantsLayer: true];
            let layer: *mut AnyObject = msg_send![v, layer];
            if !layer.is_null() {
                let _: () = msg_send![layer, setBackgroundColor: &*cg];
            }
            if v == content_view {
                break; // stop before the titlebar frame (NSThemeFrame)
            }
            v = msg_send![v, superview];
        }

        // 4) the final layer behind everything.
        ns_window.setBackgroundColor(Some(&color));
    });
}

/// No native backing to manage off macOS — the webview is opaque there and the
/// window-config colour already covers its own resize gutter.
#[cfg(not(target_os = "macos"))]
pub fn apply(_window: &tauri::WebviewWindow, _r: u8, _g: u8, _b: u8) {}
