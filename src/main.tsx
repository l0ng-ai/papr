import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import "./i18n";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { isMac } from "./lib/platform";
import "./styles.css";

// Tag the document root before first paint so the mac-only titlebar chrome
// (overlay strip, sidebar padding for the traffic lights) is gated correctly
// from the very first frame — otherwise Win/Linux would briefly show 38px of
// dead space at the top before a layout-shifting effect runs.
document.documentElement.dataset.platform = isMac ? "mac" : "other";

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
