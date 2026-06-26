// Props that silence the macOS WebKit autocorrect / autocapitalize / spell-check
// / autofill machinery on a free-text field. On macOS, Tauri renders through
// WKWebView, which forces these on by default: it underlines and rewrites text,
// auto-capitalizes the first letter, and pops a browser history dropdown — all
// of which fight the IME's own candidate window, especially for CJK input.
// Spread onto every free-text <input> (search boxes, names, URLs, credentials).
export const NO_AUTOCORRECT = {
  autoComplete: "off",
  autoCorrect: "off",
  autoCapitalize: "off",
  spellCheck: false,
} as const;
