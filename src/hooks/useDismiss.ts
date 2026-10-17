// Outside-click / Escape dismissal for floating popovers (context menus,
// pickers, share menus). Every popover reimplemented the same effect: a
// `pointerdown` listener that closes on a click/touch outside `ref`, an Escape
// `keydown` listener, and a `setTimeout(0)` so the very click/keypress that
// opened the popover does not immediately close it. Centralised here.

import { useEffect, type RefObject } from "react";

interface Options {
  /** Also dismiss when focus leaves the popover subtree (Tab past the last
   *  control). A null relatedTarget is ignored — left to the click/Escape
   *  paths — since focus falling to <body> is not a deliberate move out. */
  onFocusOut?: boolean;
}

/** Dismiss `ref`'s popover on an outside click, Escape, or (optionally) a
 *  Tab out of its subtree. */
export function useDismiss(
  ref: RefObject<HTMLElement | null>,
  onClose: () => void,
  { onFocusOut = false }: Options = {},
) {
  useEffect(() => {
    const onDown = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    const onBlur = (e: FocusEvent) => {
      const next = e.relatedTarget as Node | null;
      if (next && !ref.current?.contains(next)) onClose();
    };
    const tm = window.setTimeout(() => {
      document.addEventListener("pointerdown", onDown);
      window.addEventListener("keydown", onKey);
      if (onFocusOut) document.addEventListener("focusout", onBlur);
    }, 0);
    return () => {
      window.clearTimeout(tm);
      document.removeEventListener("pointerdown", onDown);
      window.removeEventListener("keydown", onKey);
      document.removeEventListener("focusout", onBlur);
    };
  }, [ref, onClose, onFocusOut]);
}
