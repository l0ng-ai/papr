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
