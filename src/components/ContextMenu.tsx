import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import Icon, { type IconName } from "./Icon";
import { useFloatingPosition } from "../hooks/useFloatingPosition";
import { useDismiss } from "../hooks/useDismiss";
import { useMenuKeyboard } from "../hooks/useMenuKeyboard";
import { useUi } from "../store";

export type MenuEntry =
  | {
      icon?: IconName;
      label: string;
      description?: string;
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
  const pos = useFloatingPosition(ref, x, y);

  useDismiss(ref, onClose);

  // Flag a menu as open for the whole app. The reader's native original-page
  // webview floats above the DOM, so it must suspend while a menu is up or it
  // covers the menu's right edge (issue #74). One signal here covers every
  // context menu, since they all render through this component.
  useEffect(() => {
    const setMenuOpen = useUi.getState().setMenuOpen;
    setMenuOpen(true);
    return () => setMenuOpen(false);
  }, []);

  // Focus management on open/close plus Arrow/Home/End/Enter navigation,
  // shared with the other role="menu" popovers.
  const onKeyDown = useMenuKeyboard(ref);

  return createPortal(
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
            <span className="ctx-label">{it.label}{it.description && <span className="ctx-description">{it.description}</span>}</span>
            {it.shortcut && <span className="ctx-shortcut">{it.shortcut}</span>}
          </div>
        ),
      )}
    </div>, document.body
  );
}
