import { useEffect, useLayoutEffect, useRef, useState } from "react";
import Icon, { type IconName } from "./Icon";

export type MenuEntry =
  | {
      icon?: IconName;
      label: string;
      shortcut?: string;
      danger?: boolean;
      onClick: () => void;
    }
  | { separator: true };

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
    let left = x;
    let top = y;
    if (left + r.width > window.innerWidth - 8) left = window.innerWidth - r.width - 8;
    if (top + r.height > window.innerHeight - 8) top = window.innerHeight - r.height - 8;
    setPos({ left, top });
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
      case " ":
        e.preventDefault();
        (document.activeElement as HTMLElement)?.click();
        break;
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
