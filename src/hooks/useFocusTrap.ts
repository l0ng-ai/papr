// Contain keyboard focus within a modal dialog while it is open, so Tab can't
// walk into the (visually obscured) background behind it.

import { useEffect, type RefObject } from "react";

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), ' +
  'select:not([disabled]), textarea:not([disabled]), ' +
  '[tabindex]:not([tabindex="-1"])';

/**
 * True when `el` can actually receive focus right now. The `FOCUSABLE`
 * selector matches by tag/attribute alone, so it also picks up elements that
 * are not visible — e.g. the `display:none` hidden file `<input>` the
 * Settings dialog uses for OPML import. `.focus()` on such an element is a
 * silent no-op, so if it were treated as the trap's first/last item the
 * Tab/Shift+Tab wrap-around would land focus nowhere. `getClientRects()` is
 * empty for any element rendered with `display:none` (and for one inside a
 * collapsed ancestor), which reliably excludes them.
 */
function isVisible(el: HTMLElement): boolean {
  return el.getClientRects().length > 0;
}

/**
 * Trap Tab / Shift+Tab inside `ref`'s subtree while active.
 *
 * `enabled` lets a component that stays mounted but toggles its modal on and
 * off (rather than mounting fresh each time) re-arm the trap: the effect
 * re-runs when it flips, by which point `ref` points at the now-rendered node.
 */
export function useFocusTrap(
  ref: RefObject<HTMLElement | null>,
  enabled = true,
) {
  useEffect(() => {
    if (!enabled) return;
    const el = ref.current;
    if (!el) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Tab") return;
      // Only visible elements can take focus — a hidden match (the dialog's
      // `display:none` file input) as first/last would break the wrap-around.
      const items = Array.from(
        el.querySelectorAll<HTMLElement>(FOCUSABLE),
      ).filter(isVisible);
      if (items.length === 0) return;
      const first = items[0];
      const last = items[items.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };
    el.addEventListener("keydown", onKey);
    return () => el.removeEventListener("keydown", onKey);
  }, [ref, enabled]);
}
