import { useLayoutEffect, useState, type RefObject } from "react";
import { clampToViewport, getOverlayViewport } from "../lib/viewport";

/** Measure the real content, cap its scrollable size, and reflow on keyboard,
 * rotation and async content changes. All coordinates use the same safe area. */
export function useFloatingPosition(
  ref: RefObject<HTMLElement | null>,
  x: number,
  y: number,
  placement: "at" | "above-center" | "below-flip" = "at",
) {
  const [position, setPosition] = useState({ left: x, top: y });
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const update = () => {
      const bounds = getOverlayViewport();
      element.style.maxWidth = `${Math.max(0, bounds.width - 16)}px`;
      element.style.maxHeight = `${Math.max(0, bounds.height - 16)}px`;
      // Entrance animations scale/translate the painted rect. Layout dimensions
      // keep the final, full-size overlay inside the safe edges as well.
      const rect = { width: element.offsetWidth, height: element.offsetHeight };
      let top = y;
      if (placement === "above-center") {
        top = y - rect.height - 8;
        if (top < bounds.top + 8) top = y + 22;
      } else if (placement === "below-flip" && top + rect.height > bounds.top + bounds.height - 8) {
        top = y - rect.height - 28;
      }
      const next = clampToViewport({
        x: placement === "above-center" ? x - rect.width / 2 : x,
        y: top, width: rect.width, height: rect.height, bounds,
      });
      setPosition((previous) => previous.left === next.left && previous.top === next.top ? previous : next);
    };
    update();
    const observer = new ResizeObserver(update);
    observer.observe(element);
    window.addEventListener("resize", update);
    window.addEventListener("papr-viewport-change", update);
    window.visualViewport?.addEventListener("resize", update);
    window.visualViewport?.addEventListener("scroll", update);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", update);
      window.removeEventListener("papr-viewport-change", update);
      window.visualViewport?.removeEventListener("resize", update);
      window.visualViewport?.removeEventListener("scroll", update);
    };
  }, [ref, x, y, placement]);
  return position;
}
