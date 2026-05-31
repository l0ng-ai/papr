import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import "./i18n";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { isMac } from "./lib/platform";
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

// Suppress the webview's default context menu — its "Reload / Back / Inspect"
// entries belong to a browser, not a finished app. Editable surfaces still get
// the native menu so paste / select-all / spellcheck stay available.
//
// Two reader cases also keep the native menu, because there the webview offers
// genuinely useful, content-only entries (no Reload/Back/Inspect): a right-click
// on an article image (Save Image / Copy Image / Copy Image Address) and a
// right-click over a text selection (Copy / Look Up / Translate). Everywhere
// else the app's own context menu takes over.
window.addEventListener("contextmenu", (e) => {
  const t = e.target as HTMLElement | null;
  if (t?.closest("input, textarea, [contenteditable=''], [contenteditable='true']")) return;
  if (t?.closest(".reader-content") && (t.closest("img") || hasSelection())) return;
  e.preventDefault();
});

/** Whether the user currently has a non-collapsed text selection. */
function hasSelection(): boolean {
  const sel = window.getSelection();
  return !!sel && !sel.isCollapsed && sel.toString().trim().length > 0;
}

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
