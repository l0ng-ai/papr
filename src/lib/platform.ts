// Static platform detection for the webview's host OS.
//
// Tauri's window decorations differ per platform: on macOS the title bar is
// overlaid (Overlay + hiddenTitle + custom trafficLightPosition) and the
// frontend draws its own drag region; on Windows and Linux the native title
// bar handles dragging, and the extra 38px-tall .titlebar would be a dead
// strip above the content. We branch on this constant so the mac-only chrome
// stays out of the way everywhere else.
export const isMobile = typeof navigator !== "undefined" &&
  (/iPhone|iPad|iPod|Android/.test(navigator.userAgent) || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1));
export const isMac = !isMobile &&
  typeof navigator !== "undefined" &&
  /Mac|iPhone|iPad|iPod/.test(navigator.platform || navigator.userAgent || "");

// The primary modifier glyph for the current platform. Shown in <kbd> chips,
// command-palette hints, the shortcuts cheat sheet — anywhere a label sits
// next to (or instead of) the actual key.
export const modKey = isMac ? "⌘" : "Ctrl";

// Render a full modifier+key combo the way each platform spells it: macOS
// concatenates the glyph (⌘K), Windows / Linux use the "Ctrl+K" form.
export const modCombo = (key: string) => (isMac ? `⌘${key}` : `Ctrl+${key}`);
