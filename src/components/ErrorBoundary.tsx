// Catches render-time exceptions anywhere in the tree so an unexpected bug
// shows a recoverable fallback instead of a blank white window.

import { Component, type ReactNode } from "react";
import i18n from "../i18n";

interface Props {
  children: ReactNode;
}
interface State {
  crashed: boolean;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { crashed: false };

  static getDerivedStateFromError(): State {
    return { crashed: true };
  }

  componentDidCatch(error: unknown) {
    console.error("Unhandled render error:", error);
    // A crash during initial mount leaves the boot splash (z-index 9999)
    // covering everything — drop it so the fallback below is visible.
    document.getElementById("app-loading")?.remove();
  }

  render() {
    if (!this.state.crashed) return this.props.children;
    // Inline styles + i18n only — both are available before React renders,
    // so the fallback stands on its own even if app styles are implicated.
    return (
      <div
        role="alert"
        style={{
          position: "fixed",
          inset: 0,
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          justifyContent: "center",
          gap: 14,
          background: "#16140f",
          color: "#e8e3d8",
          fontFamily: "-apple-system, BlinkMacSystemFont, sans-serif",
          textAlign: "center",
          padding: 32,
        }}
      >
        <div style={{ fontSize: 16, fontWeight: 600 }}>
          {i18n.t("crash.title")}
        </div>
        <div style={{ fontSize: 13, opacity: 0.7 }}>{i18n.t("crash.body")}</div>
        <button
          onClick={() => location.reload()}
          style={{
            marginTop: 6,
            padding: "7px 16px",
            fontSize: 13,
            borderRadius: 7,
            border: "1px solid rgba(255,255,255,0.2)",
            background: "rgba(255,255,255,0.08)",
            color: "inherit",
            cursor: "pointer",
          }}
        >
          {i18n.t("crash.reload")}
        </button>
      </div>
    );
  }
}
