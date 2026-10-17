//! Dedicated native player: WKWebView loadHTMLString(baseURL:) supplies the
//! registered bundle identity required by YouTube (error 153). No Tauri bridge
//! or library data is exposed to this untrusted web view.
use tauri::Manager;

fn player_html(id: &str, identity: &str) -> Result<String, String> {
    if id.len() != 11
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("Invalid YouTube video ID".into());
    }
    let identity = url::Url::parse(identity).map_err(|_| "Invalid application identity")?;
    if identity.scheme() != "https" {
        return Err("Invalid application identity".into());
    }
    let mut embed =
        url::Url::parse(&format!("https://www.youtube-nocookie.com/embed/{id}")).unwrap();
    embed
        .query_pairs_mut()
        .append_pair("autoplay", "1")
        .append_pair("playsinline", "1")
        .append_pair("origin", identity.as_str())
        .append_pair("widget_referrer", identity.as_str());
    Ok(format!(
        r#"<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover"><meta name="referrer" content="strict-origin-when-cross-origin"><style>html,body{{margin:0;height:100%;background:#111;color:white;font:17px system-ui}}body{{box-sizing:border-box;padding:env(safe-area-inset-top,0px) env(safe-area-inset-right,0px) env(safe-area-inset-bottom,0px) env(safe-area-inset-left,0px);display:flex;flex-direction:column}}header{{flex:0 0 auto;padding:12px;display:flex;justify-content:flex-end}}button{{font:inherit;color:white;background:#333;border:0;border-radius:8px;padding:12px 20px;min-height:44px}}iframe{{border:0;width:100%;flex:1;min-height:200px}}@media(min-width:700px){{header{{padding-top:38px}}}}</style></head><body><header><button onclick="window.webkit.messageHandlers.paprVideoClose.postMessage('close')">关闭 / Close</button></header><iframe title="YouTube" src="{}" referrerpolicy="strict-origin-when-cross-origin" allow="autoplay; encrypted-media; picture-in-picture; fullscreen" allowfullscreen></iframe></body></html>"#,
        embed.as_str().replace('&', "&amp;")
    ))
}

#[tauri::command]
pub async fn open_youtube_player(app: tauri::AppHandle, video_id: String) -> Result<(), String> {
    // Validate before dispatching any native UI work.
    player_html(&video_id, "https://com.thomas.papr/")?;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let main = app
            .get_webview_window("main")
            .ok_or("Main view unavailable")?;
        main.with_webview(move |view| unsafe { apple::open(view.inner(), &video_id) })
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        Err("Native player unavailable on this platform".into())
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple {
    use objc2::{
        define_class, msg_send, rc::Retained, runtime::ProtocolObject, MainThreadMarker,
        MainThreadOnly,
    };
    use objc2_foundation::{NSBundle, NSObject, NSObjectProtocol, NSString, NSURL};
    #[cfg(target_os = "macos")]
    use objc2_web_kit::WKWebView;
    use objc2_web_kit::{
        WKAudiovisualMediaTypes, WKScriptMessage, WKScriptMessageHandler, WKUserContentController,
        WKWebViewConfiguration,
    };
    // objc2-web-kit 0.3 exposes WKWebView only on macOS. Declare the small
    // UIKit surface used here, with the same native class and ownership rules.
    #[cfg(target_os = "ios")]
    objc2::extern_class!(
        #[unsafe(super(objc2_ui_kit::UIView, objc2_ui_kit::UIResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "WKWebView"]
        struct WKWebView;
    );
    #[cfg(target_os = "ios")]
    impl WKWebView {
        objc2::extern_methods!(
            #[unsafe(method(initWithFrame:configuration:))]
            unsafe fn initWithFrame_configuration(
                this: objc2::rc::Allocated<Self>,
                frame: objc2_core_foundation::CGRect,
                configuration: &WKWebViewConfiguration,
            ) -> Retained<Self>;
            #[unsafe(method(stopLoading))]
            unsafe fn stopLoading(&self);
            #[unsafe(method(loadHTMLString:baseURL:))]
            unsafe fn loadHTMLString_baseURL(
                &self,
                html: &NSString,
                base: Option<&NSURL>,
            ) -> Option<Retained<objc2_web_kit::WKNavigation>>;
            #[unsafe(method(scrollView))]
            unsafe fn scrollView(&self) -> Retained<objc2_ui_kit::UIScrollView>;
        );
    }
    use std::cell::RefCell;
    thread_local! {static PLAYER:RefCell<Option<Retained<WKWebView>>>=const{RefCell::new(None)};}
    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind=MainThreadOnly]
        struct PaprVideoCloseHandler;
        unsafe impl NSObjectProtocol for PaprVideoCloseHandler {}
        unsafe impl WKScriptMessageHandler for PaprVideoCloseHandler {
            #[unsafe(method(userContentController:didReceiveScriptMessage:))]
            unsafe fn receive(
                &self,
                _controller: &WKUserContentController,
                message: &WKScriptMessage,
            ) {
                if !message.frameInfo().isMainFrame() {
                    return;
                }
                PLAYER.with(|slot| {
                    if let Some(view) = slot.borrow_mut().take() {
                        view.stopLoading();
                        view.removeFromSuperview();
                    }
                });
            }
        }
    );
    pub unsafe fn open(main: *mut std::ffi::c_void, video_id: &str) {
        // Re-signing an IPA can change its installed Bundle ID.
        let bundle = NSBundle::mainBundle()
            .bundleIdentifier()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "com.thomas.papr".into());
        let identity = format!("https://{}/", bundle.to_ascii_lowercase());
        let html = super::player_html(video_id, &identity)
            .expect("validated video and installed app identifier");
        let mtm = MainThreadMarker::new().expect("WKWebView dispatch must be on the main thread");
        PLAYER.with(|slot| {
            if let Some(view) = slot.borrow_mut().take() {
                view.stopLoading();
                view.removeFromSuperview();
            }
        });
        let main = &*main.cast::<WKWebView>();
        let configuration = WKWebViewConfiguration::new(mtm);
        configuration.setMediaTypesRequiringUserActionForPlayback(WKAudiovisualMediaTypes::empty());
        #[cfg(target_os = "ios")]
        {
            let _: () = msg_send![&*configuration,setAllowsInlineMediaPlayback:true];
        }
        let handler: Retained<PaprVideoCloseHandler> =
            msg_send![PaprVideoCloseHandler::alloc(mtm), init];
        configuration
            .userContentController()
            .addScriptMessageHandler_name(
                ProtocolObject::from_ref(&*handler),
                &NSString::from_str("paprVideoClose"),
            );
        let player = WKWebView::initWithFrame_configuration(
            WKWebView::alloc(mtm),
            main.bounds(),
            &configuration,
        );
        let _: () = msg_send![&*player,setAutoresizingMask:18usize]; // flexible width and height on AppKit/UIKit
        #[cfg(target_os = "ios")]
        player.scrollView().setContentInsetAdjustmentBehavior(
            objc2_ui_kit::UIScrollViewContentInsetAdjustmentBehavior::Never,
        );
        main.addSubview(&player);
        player.loadHTMLString_baseURL(
            &NSString::from_str(&html),
            NSURL::URLWithString(&NSString::from_str(&identity)).as_deref(),
        );
        PLAYER.with(|slot| *slot.borrow_mut() = Some(player));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_and_video_id_are_bounded_and_the_player_is_isolated() {
        let s = player_html("M7lc1UVf-VE", "https://com.thomas.papr/").unwrap();
        assert!(s.contains("origin=https%3A%2F%2Fcom.thomas.papr%2F"));
        assert!(s.contains("strict-origin-when-cross-origin"));
        assert!(s.contains("safe-area-inset-bottom"));
        assert!(!s.contains("__TAURI"));
        for id in ["x", "1234567890<", "1234567890/"] {
            assert!(player_html(id, "https://com.thomas.papr/").is_err());
        }
    }
}
