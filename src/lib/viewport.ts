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
// This bug was independently rediscovered and re-fixed five times across the
// codebase (iterations 139, 156, 161, 194, 196) because each new overlay
// reimplemented the clamp inline. Centralising it here means a new overlay
// gets the correct two-sided clamp for free.
//
// The correct order is: first pull back from the far edge, then floor at the
// near margin. The floor wins, so an oversized overlay is pinned to the
// top-left margin (visible from the start) rather than the bottom-right.

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
  viewportWidth = typeof window !== "undefined" ? window.innerWidth : 0,
  viewportHeight = typeof window !== "undefined" ? window.innerHeight : 0,
}: ClampInput): ClampResult {
  return {
    left: clampAxis(x, width, viewportWidth, margin),
    top: clampAxis(y, height, viewportHeight, margin),
  };
}
