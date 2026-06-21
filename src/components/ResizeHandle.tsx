import { useCallback, useEffect, useRef } from "react";

interface Props {
  /** Pixel width of the pane this handle resizes, at drag start. */
  width: number;
  /** `right`: dragging right grows the pane (sidebar / list, handle on the
   *  pane's right edge). `left`: dragging left grows the pane (the AI drawer,
   *  handle on its left edge). */
  side: "left" | "right";
  /** Lower clamp for the resulting width (px). */
  min: number;
  /** Upper clamp for the resulting width (px). */
  max: number;
  /** Called continuously during the drag with the new, clamped width. */
  onResize: (width: number) => void;
  /** Accessible label for the separator. */
  label: string;
}

/**
 * A thin draggable separator between two panes. Implemented with native
 * pointer events (no third-party resize library) to stay lightweight and match
 * the project's hand-rolled interaction code.
 *
 * The drag is tracked from the pointer's start X and the pane's start width, so
 * a slow first frame can't desync the handle from the cursor. Pointer capture
 * keeps the drag alive even if the cursor outruns the 6px hit area, and the
 * body `col-resize` cursor + a no-select guard make the gesture feel native.
 */
export default function ResizeHandle({ width, side, min, max, onResize, label }: Props) {
  // Latest props in a ref so the move/up listeners (bound once per drag) always
  // read fresh values without re-binding mid-drag.
  const latest = useRef({ width, side, min, max, onResize });
  latest.current = { width, side, min, max, onResize };

  const onPointerDown = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    // Ignore secondary buttons so a right-click context menu can't start a drag.
    if (e.button !== 0) return;
    e.preventDefault();
    const startX = e.clientX;
    const { width: startW } = latest.current;
    const target = e.currentTarget;
    target.setPointerCapture(e.pointerId);

    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";

    const move = (ev: PointerEvent) => {
      const { side, min, max, onResize } = latest.current;
      const dx = ev.clientX - startX;
      // `right` panes grow as the cursor moves right; `left` panes (drawer on
      // the right) grow as it moves left.
      const raw = side === "right" ? startW + dx : startW - dx;
      onResize(Math.min(max, Math.max(min, raw)));
    };
    const up = (ev: PointerEvent) => {
      target.releasePointerCapture?.(ev.pointerId);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }, []);

  // Keyboard a11y: arrow keys nudge the boundary in 16px steps for users who
  // can't drag.
  const onKeyDown = useCallback((e: React.KeyboardEvent<HTMLDivElement>) => {
    const { width, side, min, max, onResize } = latest.current;
    const step = e.shiftKey ? 48 : 16;
    let delta = 0;
    if (e.key === "ArrowLeft") delta = side === "right" ? -step : step;
    else if (e.key === "ArrowRight") delta = side === "right" ? step : -step;
    else return;
    e.preventDefault();
    onResize(Math.min(max, Math.max(min, width + delta)));
  }, []);

  // Belt-and-braces: if this handle unmounts mid-drag (e.g. focus mode hides
  // the panes), make sure the global cursor/select overrides are cleared.
  useEffect(() => {
    return () => {
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
  }, []);

  return (
    <div
      className="resize-handle"
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      tabIndex={0}
      onPointerDown={onPointerDown}
      onKeyDown={onKeyDown}
    />
  );
}
