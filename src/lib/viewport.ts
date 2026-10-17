// Shared viewport-clamping helper for floating overlays (context menus,
// popovers, hover previews, pickers).
//
// Every `position: fixed` overlay anchored at an arbitrary coordinate must be
// pulled back inside the viewport, otherwise it spills off the right/bottom
// edge. But the pull-back alone is not enough: when the overlay is wider or
// taller than the viewport itself, the pulled-back coordinate goes negative
// and the overlay is shoved off the *top/left* corner instead — clipping its
// first items with no way to reach them.
//
// This bug was independently rediscovered and re-fixed several times across
// the codebase because each new overlay reimplemented the clamp inline.
// Centralising it here means a new overlay gets the correct two-sided clamp
// for free.
//
// The correct order is: first pull back from the far edge, then floor at the
// near margin. The floor wins, so an oversized overlay is pinned to the
// top-left margin (visible from the start) rather than the bottom-right.

export interface ViewportBounds {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** Visible layout coordinates, after excluding device cutouts and the keyboard. */
export function insetViewport(
  viewport: ViewportBounds,
  insets: { top: number; right: number; bottom: number; left: number },
): ViewportBounds {
  return {
    left: viewport.left + insets.left,
    top: viewport.top + insets.top,
    width: Math.max(0, viewport.width - insets.left - insets.right),
    height: Math.max(0, viewport.height - insets.top - insets.bottom),
  };
}

let insetProbe: HTMLDivElement | undefined;

export function getOverlayViewport(): ViewportBounds {
  if (typeof window === "undefined") return { left: 0, top: 0, width: 0, height: 0 };
  const full = { left: 0, top: 0, width: window.innerWidth, height: window.innerHeight };
  if (document.documentElement.dataset.platform !== "mobile") return full;
  // A computed padding resolves env() to pixels, unlike reading a custom
  // property directly. Reuse this invisible, inert probe across all overlays.
  if (!insetProbe?.isConnected) {
    insetProbe = document.createElement("div");
    insetProbe.setAttribute("aria-hidden", "true");
    insetProbe.style.cssText = "position:fixed;top:0;left:0;visibility:hidden;pointer-events:none;width:0;height:0;padding:var(--safe-top,0px) var(--safe-right,0px) var(--safe-bottom,0px) var(--safe-left,0px)";
    document.body.appendChild(insetProbe);
  }
  const padding = getComputedStyle(insetProbe);
  const root = document.documentElement;
  const keyboard = root.dataset.keyboardOpen === "true";
  const visual = window.visualViewport;
  const height = parseFloat(root.style.getPropertyValue("--app-height")) || full.height;
  return insetViewport({
    ...full,
    top: keyboard ? visual?.offsetTop ?? 0 : 0,
    height,
  }, {
    top: parseFloat(padding.paddingTop) || 0,
    right: parseFloat(padding.paddingRight) || 0,
    bottom: parseFloat(padding.paddingBottom) || 0,
    left: parseFloat(padding.paddingLeft) || 0,
  });
}

export interface ClampInput {
  /** Anchor x coordinate (viewport-relative, e.g. mouse or getBoundingClientRect). */
  x: number;
  /** Anchor y coordinate (viewport-relative). */
  y: number;
  /** Overlay width in px (a measured rect.width, or a hardcoded card width). */
  width: number;
  /** Overlay height in px. */
  height: number;
  /**
   * Gap kept between the overlay and the viewport edges, in px. Doubles as the
   * minimum left/top coordinate (the floor). Most overlays use 8; a couple
   * historically use 0 — pass the value that matches the call site.
   */
  margin?: number;
  /** Viewport width. Defaults to `window.innerWidth`; injectable for tests. */
  viewportWidth?: number;
  /** Viewport height. Defaults to `window.innerHeight`; injectable for tests. */
  viewportHeight?: number;
  /** Safe visible bounds; defaults to the current device's usable viewport. */
  bounds?: ViewportBounds;
}

export interface ClampResult {
  left: number;
  top: number;
}

/**
 * Clamp a single axis: pull back from the far edge so the overlay fits, then
 * floor at the near margin. The floor is applied last so it wins when the
 * overlay is larger than the viewport.
 */
export function clampAxis(
  coord: number,
  size: number,
  viewport: number,
  margin: number,
): number {
  return Math.max(margin, Math.min(coord, viewport - size - margin));
}

/**
 * Clamp an overlay's anchor coordinate so the whole overlay stays inside the
 * viewport on both axes. Returns the `{ left, top }` to apply as fixed-position
 * styles.
 */
export function clampToViewport({
  x,
  y,
  width,
  height,
  margin = 8,
  viewportWidth,
  viewportHeight,
  bounds = viewportWidth != null || viewportHeight != null
    ? { left: 0, top: 0, width: viewportWidth ?? (typeof window !== "undefined" ? window.innerWidth : 0), height: viewportHeight ?? (typeof window !== "undefined" ? window.innerHeight : 0) }
    : getOverlayViewport(),
}: ClampInput): ClampResult {
  return {
    left: bounds.left + clampAxis(x - bounds.left, width, bounds.width, margin),
    top: bounds.top + clampAxis(y - bounds.top, height, bounds.height, margin),
  };
}
