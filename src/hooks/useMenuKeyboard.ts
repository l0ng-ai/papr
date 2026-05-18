// Keyboard support for floating `role="menu"` popovers.
//
// `ContextMenu` already implements this inline; the share / highlight-export
// popovers (`SendToMenu`, `ExportMenu`) were added later and shipped without
// it — opening them with the keyboard left focus nowhere, arrow keys did
// nothing, and closing dropped focus to `<body>`. This hook factors the
// behaviour out so those menus get parity with `ContextMenu`:
//   - moves focus to the first enabled `[role="menuitem"]` on open,
//   - restores focus to the trigger element on close,
//   - returns an `onKeyDown` handler for Arrow / Home / End navigation
//     (Enter / Space activation is left to the items' native <button>).

import { useEffect, type KeyboardEvent, type RefObject } from "react";

/**
 * @param ref   the menu container element.
 * @param ready when false the menu items have not rendered yet (e.g. an async
 *              load is pending) — focus is moved in only once it flips true.
 *              Defaults to true for menus whose items are present immediately.
 */
export function useMenuKeyboard(
  ref: RefObject<HTMLElement | null>,
  ready: boolean = true,
) {
  // Restore focus to the trigger element when the menu unmounts.
  useEffect(() => {
    const trigger = document.activeElement as HTMLElement | null;
    return () => trigger?.focus?.();
  }, []);

  // Focus the first enabled menu item once the items are on screen.
  useEffect(() => {
    if (!ready) return;
    const items = ref.current?.querySelectorAll<HTMLElement>('[role="menuitem"]');
    const first = Array.from(items ?? []).find(
      (el) => !(el as HTMLButtonElement).disabled,
    );
    (first ?? items?.[0])?.focus();
  }, [ref, ready]);

  /** Arrow / Home / End / Enter navigation over the (enabled) menu items. */
  const onKeyDown = (e: KeyboardEvent) => {
    const all = Array.from(
      ref.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [],
    );
    const items = all.filter((el) => !(el as HTMLButtonElement).disabled);
    if (items.length === 0) return;
    const idx = items.indexOf(document.activeElement as HTMLElement);
    const focusAt = (i: number) => {
      e.preventDefault();
      items[(i + items.length) % items.length]?.focus();
    };
    switch (e.key) {
      case "ArrowDown": focusAt(idx + 1); break;
      case "ArrowUp": focusAt(idx < 0 ? -1 : idx - 1); break;
      case "Home": focusAt(0); break;
      case "End": focusAt(items.length - 1); break;
      case "Enter":
      case " ": {
        // The menu items are real <button>s, which already fire `click` on
        // Enter/Space natively. Synthesising another `click()` here would
        // run the action twice (e.g. exporting highlights to Notion twice).
        // Only forward the key to a non-natively-activatable element.
        const el = document.activeElement as HTMLElement | null;
        if (el && el.tagName !== "BUTTON" && el.tagName !== "A") {
          e.preventDefault();
          el.click();
        }
        break;
      }
    }
  };

  return onKeyDown;
}
