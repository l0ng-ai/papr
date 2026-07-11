// Static platform detection for the webview's host OS.
//
// Tauri's window decorations differ per platform: on macOS the title bar is
// overlaid (Overlay + hiddenTitle + custom trafficLightPosition) and the
// frontend draws its own drag region; on Windows and Linux the native title
// bar handles dragging, and the extra 38px-tall .titlebar would be a dead
// strip above the content. We branch on this constant so the mac-only chrome
// stays out of the way everywhere else.
export const isMac =
  typeof navigator !== "undefined" &&
  /Mac|iPhone|iPad|iPod/.test(navigator.platform || navigator.userAgent || "");

// The primary modifier glyph for the current platform. Shown in <kbd> chips,
// command-palette hints, the shortcuts cheat sheet — anywhere a label sits
// next to (or instead of) the actual key.
export const modKey = isMac ? "⌘" : "Ctrl";

// Render a full modifier+key combo the way each platform spells it: macOS
// concatenates the glyph (⌘K), Windows / Linux use the "Ctrl+K" form.
export const modCombo = (key: string) => (isMac ? `⌘${key}` : `Ctrl+${key}`);

// Whether the same web bundle is running inside a Tauri *mobile* (iOS) webview
// rather than a desktop window. This matters because a slew of desktop-only
// Tauri commands and plugins (the tray, autostart, the updater/process plugins,
// the native window backing, the in-app original-page child webview) are simply
// not registered on mobile — calling them there throws. We branch on this to
// skip or no-op those paths so the shared frontend loads cleanly on a phone.
//
// The detection is a *synchronous* user-agent test, on purpose: some gating
// happens during module init (api wrappers) where an async round-trip to the
// Rust `plugin-os` isn't available yet, and a static UA read is also decoupled
// from whether that plugin is registered on a given build. Pulled out as a pure
// function so the platform logic is unit-testable without a live `navigator`.
//
// iPhone/iPod carry their name in the UA outright. iPadOS 13+ masquerades as a
// desktop "Macintosh", so it is told apart only by its touch points — a real
// Mac reports maxTouchPoints 0, an iPad reports > 1.
export function detectMobile(ua: string, maxTouchPoints = 0): boolean {
  if (/iPhone|iPod/.test(ua)) return true;
  if (/iPad/.test(ua)) return true;
  if (/Macintosh/.test(ua) && maxTouchPoints > 1) return true;
  return false;
}

export const isMobile =
  typeof navigator !== "undefined" &&
  detectMobile(navigator.userAgent || "", navigator.maxTouchPoints ?? 0);

export const isDesktop = !isMobile;
