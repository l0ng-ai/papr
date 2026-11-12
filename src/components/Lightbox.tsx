import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import Icon from "./Icon";

interface Props {
  /** The working (already-loaded, possibly proxied) src of every image in the
   *  article, in reading order. */
  srcs: string[];
  /** Index of the image the user clicked. */
  index: number;
  onClose: () => void;
}

// Zoom bounds. Min < 1 lets the user zoom out below the fit-to-viewport size;
// max caps how far in before the pixels blur into mush.
const MIN_SCALE = 0.5;
const MAX_SCALE = 4;

const clamp = (n: number, lo: number, hi: number) =>
  Math.min(hi, Math.max(lo, n));

/** Full-screen image viewer (issue #87). Opens from a click on an article-body
 *  image and cycles through every image in the article via the on-screen arrows
 *  or the ← / → keys. The wheel zooms (anchored to the cursor), and the image
 *  can be dragged to pan once zoomed in. Videos are intentionally excluded —
 *  sanitize forces `controls` on every `<video>`, so they already have native
 *  fullscreen. */
export default function Lightbox({ srcs, index, onClose }: Props) {
  const { t } = useTranslation();
  const [i, setI] = useState(index);
  const many = srcs.length > 1;

  const rootRef = useRef<HTMLDivElement>(null);
  const imgRef = useRef<HTMLImageElement>(null);
  // The current zoom/pan transform. Kept in a ref and applied straight to the
  // <img> style — wheel events fire far faster than React can re-render, so
  // state would lag the pointer. Only a change of shown image (below) resets it.
  const view = useRef({ scale: 1, tx: 0, ty: 0 });
  const drag = useRef<{
    startX: number;
    startY: number;
    tx0: number;
    ty0: number;
  } | null>(null);
  // Set on a drag's pointerup so the click that immediately follows (retargeted
  // to the image by pointer capture) can't bubble up and close the lightbox.
  const justDragged = useRef(false);

  // Return to the fit view. Called when the shown image changes, so the next
  // image never renders (even for a frame) at the previous one's zoom level.
  const resetView = () => {
    view.current = { scale: 1, tx: 0, ty: 0 };
    drag.current = null;
    const el = imgRef.current;
    if (el) {
      el.style.transform = "";
      el.style.cursor = "";
    }
  };

  const prev = useCallback(() => {
    resetView();
    setI((v) => (v - 1 + srcs.length) % srcs.length);
  }, [srcs.length]);
  const next = useCallback(() => {
    resetView();
    setI((v) => (v + 1) % srcs.length);
  }, [srcs.length]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      else if (e.key === "ArrowLeft") prev();
      else if (e.key === "ArrowRight") next();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, prev, next]);

  const applyView = (next: { scale: number; tx: number; ty: number }) => {
    view.current = next;
    const el = imgRef.current;
    if (el) {
      el.style.transform = `translate3d(${next.tx}px, ${next.ty}px, 0) scale(${next.scale})`;
      el.style.cursor = "grab";
    }
  };

  // Zoom to `s1` while keeping the point under (x, y) stationary. With
  // `transform-origin: 0 0`, an element point at screen offset
  // (x - rect.left, y - rect.top) from the image's box must keep that offset
  // under the cursor, so the translation is moved by (1 - ratio) × it.
  const zoomAt = (s1: number, x: number, y: number) => {
    const img = imgRef.current;
    if (!img) return;
    const v = view.current;
    const target = clamp(s1, MIN_SCALE, MAX_SCALE);
    const ratio = target / v.scale;
    if (ratio === 1) return;
    const rect = img.getBoundingClientRect();
    applyView({
      scale: target,
      tx: v.tx + (1 - ratio) * (x - rect.left),
      ty: v.ty + (1 - ratio) * (y - rect.top),
    });
  };

  // The wheel handler needs `passive: false` so it can stop the reader behind
  // the lightbox scrolling (and ctrl+wheel page-zoom); React attaches wheel
  // listeners passively, so this one is bound natively. It only reads refs, so
  // `zoomAtRef` just keeps the latest instance.
  const zoomAtRef = useRef(zoomAt);
  useEffect(() => {
    zoomAtRef.current = zoomAt;
  });
  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      // Line-mode wheel events carry tiny deltas (e.g. 3); scale them up.
      const dy = e.deltaMode === 1 ? e.deltaY * 16 : e.deltaY;
      const v = view.current;
      zoomAtRef.current(v.scale * Math.exp(-dy * 0.0015), e.clientX, e.clientY);
    };
    root.addEventListener("wheel", onWheel, { passive: false });
    return () => root.removeEventListener("wheel", onWheel);
  }, []);

  // Reset zoom/pan whenever the shown image changes (arrows, keys).
  useEffect(() => {
    resetView();
  }, [i]);

  // Dragging pans the image — freely, to any position, at any zoom level.
  const onPointerDown = (e: React.PointerEvent<HTMLImageElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    const v = view.current;
    drag.current = { startX: e.clientX, startY: e.clientY, tx0: v.tx, ty0: v.ty };
    e.currentTarget.style.cursor = "grabbing";
    e.stopPropagation();
  };
  const onPointerMove = (e: React.PointerEvent<HTMLImageElement>) => {
    const d = drag.current;
    if (!d) return;
    const v = view.current;
    applyView({
      ...v,
      tx: d.tx0 + (e.clientX - d.startX),
      ty: d.ty0 + (e.clientY - d.startY),
    });
    e.currentTarget.style.cursor = "grabbing";
    e.stopPropagation();
  };
  const onPointerUp = (e: React.PointerEvent<HTMLImageElement>) => {
    const d = drag.current;
    drag.current = null;
    if (d) {
      // Only swallow the click that trails a *real* drag; a plain click's own
      // click event is already stopped by the image handler.
      if (Math.hypot(e.clientX - d.startX, e.clientY - d.startY) > 4) {
        justDragged.current = true;
      }
      applyView(view.current);
      try {
        e.currentTarget.releasePointerCapture(e.pointerId);
      } catch {
        /* capture may already be gone */
      }
      e.stopPropagation();
    }
  };

  const stop = (e: React.MouseEvent) => e.stopPropagation();

  // Clicking the image never closes the lightbox; clear the drag flag so the
  // click that follows a drag doesn't bubble up to the backdrop.
  const onImgClick = (e: React.MouseEvent) => {
    justDragged.current = false;
    stop(e);
  };

  return (
    <div
      className="lightbox"
      ref={rootRef}
      onClick={() => {
        // A click trailing a drag (pointer capture retargets it) would land on
        // the backdrop only when capture wasn't honoured — swallow it rather
        // than closing the viewer the moment a pan ends.
        if (justDragged.current) {
          justDragged.current = false;
          return;
        }
        onClose();
      }}
    >
      <button
        className="lightbox-btn lightbox-close"
        aria-label={t("reader.lightboxClose")}
        onClick={onClose}
      >
        <Icon name="x" size={20} />
      </button>

      {many && (
        <button
          className="lightbox-btn lightbox-prev"
          aria-label={t("reader.lightboxPrev")}
          onClick={(e) => {
            stop(e);
            prev();
          }}
        >
          <Icon name="chevron-right" size={26} />
        </button>
      )}

      <img
        ref={imgRef}
        className="lightbox-img"
        src={srcs[i]}
        alt=""
        onClick={onImgClick}
        onDoubleClick={(e) => {
          // Already zoomed in → back to fit; otherwise jump to 2×.
          const v = view.current;
          zoomAt(v.scale > 1.001 ? 1 : 2, e.clientX, e.clientY);
        }}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        draggable={false}
      />

      {many && (
        <button
          className="lightbox-btn lightbox-next"
          aria-label={t("reader.lightboxNext")}
          onClick={(e) => {
            stop(e);
            next();
          }}
        >
          <Icon name="chevron-right" size={26} />
        </button>
      )}

      {many && (
        <div className="lightbox-counter" onClick={stop}>
          {i + 1} / {srcs.length}
        </div>
      )}

      <div className="lightbox-hint">{t("reader.lightboxHint")}</div>
    </div>
  );
}
