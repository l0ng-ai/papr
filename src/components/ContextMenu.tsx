import { useEffect, useLayoutEffect, useRef, useState } from "react";
import Icon, { type IconName } from "./Icon";
import { clampToViewport } from "../lib/viewport";

export type MenuEntry =
  | {
      icon?: IconName;
      label: string;
      shortcut?: string;
      danger?: boolean;
      onClick: () => void;
    }
  | { separator: true }
  | {
      /** A row of colour swatches — for picking a tag colour. */
      swatches: { value: string; color: string }[];
      current: string;
      onPick: (value: string) => void;
    };

interface Props {
  x: number;
  y: number;
  items: MenuEntry[];
  onClose: () => void;
}

/** Floating context menu, clamped inside the viewport — design `.ctx-menu`. */
export default function ContextMenu({ x, y, items, onClose }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    // The menu is anchored at the mouse cursor, so it can open anywhere. The
    // shared clamp pulls it back from the right/bottom edges and floors it at
    // the 8px margin, so a menu taller/wider than the window stays reachable.
    setPos(clampToViewport({ x, y, width: r.width, height: r.height, margin: 8 }));
  }, [x, y]);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    const t = window.setTimeout(() => {
      document.addEventListener("mousedown", onDown);
      window.addEventListener("keydown", onKey);
    }, 0);
    return () => {
      window.clearTimeout(t);
      document.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  // Move keyboard focus into the menu on open and restore it to whatever was
  // focused (the right-clicked row) when the menu closes.
  useEffect(() => {
    const trigger = document.activeElement as HTMLElement | null;
    ref.current?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
    return () => trigger?.focus?.();
  }, []);

  /** Arrow / Home / End / Enter navigation over the menu items. */
  const onKeyDown = (e: React.KeyboardEvent) => {
    const menuitems = Array.from(
      ref.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [],
    );
    if (menuitems.length === 0) return;
    const idx = menuitems.indexOf(document.activeElement as HTMLElement);
    const focusAt = (i: number) => {
      e.preventDefault();
      menuitems[(i + menuitems.length) % menuitems.length]?.focus();
    };
    switch (e.key) {
      case "ArrowDown": focusAt(idx + 1); break;
      case "ArrowUp": focusAt(idx < 0 ? -1 : idx - 1); break;
      case "Home": focusAt(0); break;
      case "End": focusAt(menuitems.length - 1); break;
      case "Enter":
      case " ": {
        // The colour-swatch items are real <button>s, which already fire
        // `click` on Enter/Space natively. Synthesising another `click()`
        // here would run `onPick` twice (a double tag-recolour mutation).
        // The plain `ctx-item` rows are <div>s and *do* need the synthetic
        // click — so only forward the key to non-natively-activatable
        // elements, mirroring `useMenuKeyboard`.
        const el = document.activeElement as HTMLElement | null;
        if (el && el.tagName !== "BUTTON" && el.tagName !== "A") {
          e.preventDefault();
          el.click();
        }
        break;
      }
    }
  };

  return (
    <div
      className="ctx-menu"
      ref={ref}
      role="menu"
      style={{ left: pos.left, top: pos.top }}
      onClick={(e) => e.stopPropagation()}
      onKeyDown={onKeyDown}
    >
      {items.map((it, i) =>
        "separator" in it ? (
          <div key={i} className="ctx-sep" role="separator" />
        ) : "swatches" in it ? (
          <div key={i} className="ctx-swatches">
            {it.swatches.map((sw) => (
              <button
                key={sw.value}
                className={`ctx-swatch ${sw.value === it.current ? "on" : ""}`}
                role="menuitem"
                tabIndex={-1}
                style={{ background: sw.color }}
                aria-label={sw.value}
                aria-pressed={sw.value === it.current}
                onClick={() => {
                  it.onPick(sw.value);
                  onClose();
                }}
              />
            ))}
          </div>
        ) : (
          <div
            key={i}
            className="ctx-item"
            role="menuitem"
            tabIndex={-1}
            style={it.danger ? { color: "oklch(0.55 0.17 28)" } : undefined}
            onClick={() => {
              it.onClick();
              onClose();
            }}
          >
            <span className="ctx-ico">
              {it.icon && <Icon name={it.icon} size={13} />}
            </span>
            {it.label}
            {it.shortcut && <span className="ctx-shortcut">{it.shortcut}</span>}
          </div>
        ),
      )}
    </div>
  );
}
