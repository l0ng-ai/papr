import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import "./i18n";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { isMac, isMobile } from "./lib/platform";
// Bundle the three UI / reader fonts so Windows and Linux render with the
// same letterforms macOS sees, instead of falling through to Arial / DejaVu.
// Variable-weight woff2 — one file per family covers every weight the styles
// reference (450 / 500 / 550 / 600 / 650 / 700).
import "@fontsource-variable/inter-tight";
import "@fontsource-variable/jetbrains-mono";
import "@fontsource-variable/newsreader";
import "@fontsource-variable/newsreader/wght-italic.css";
import "./styles.css";

// Tag the document root before first paint so the mac-only titlebar chrome
// (overlay strip, sidebar padding for the traffic lights) is gated correctly
// from the very first frame — otherwise Win/Linux would briefly show 38px of
// dead space at the top before a layout-shifting effect runs.
document.documentElement.dataset.platform = isMac ? "mac" : "other";

// Tag the root as a mobile (iOS) build so the mobile-only CSS in styles.css
// (the stacked single-column shell, safe-area chrome, full-screen sheets) takes
// effect. Set once from the static platform detection and *only* when true, so
// on desktop the attribute is simply absent and every `[data-mobile]` rule is
// inert — the desktop layout renders exactly as before.
if (isMobile) document.documentElement.dataset.mobile = "true";

// Suppress the webview's default context menu — its "Reload / Back / Inspect"
// entries belong to a browser, not a finished app. Editable surfaces still get
// the native menu so paste / select-all / spellcheck stay available. The reader
// supplies its own context menu (copy / save image / …) in ContextMenu.tsx.
window.addEventListener("contextmenu", (e) => {
  const t = e.target as HTMLElement | null;
  if (t?.closest("input, textarea, [contenteditable=''], [contenteditable='true']")) return;
  e.preventDefault();
});

const queryClient = new QueryClient({
  defaultOptions: {
    queries: { staleTime: 30_000, refetchOnWindowFocus: false, retry: 1 },
  },
});

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <QueryClientProvider client={queryClient}>
        <App />
      </QueryClientProvider>
    </ErrorBoundary>
  </React.StrictMode>,
);
