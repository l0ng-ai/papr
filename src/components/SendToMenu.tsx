// "Send to…" share menu (feature F8).
//
// A small popover listing the configured share targets — Pocket, Instapaper,
// Kindle, Notion — for one article. Used both from the Reader toolbar and the
// article-list context menu. Only targets that have complete credentials
// (reported by `share_targets`) are enabled; the rest are shown disabled with
// a hint so the user knows where to configure them.

import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { useDismiss } from "../hooks/useDismiss";
import { useMenuKeyboard } from "../hooks/useMenuKeyboard";
import { reportError } from "../toast";
import { clampToViewport } from "../lib/viewport";
import type { ShareTarget, ShareTargets } from "../types";
import Icon, { type IconName } from "./Icon";

interface Props {
  articleId: number;
  /** Anchor position (viewport coordinates) for the popover. */
  x: number;
  y: number;
  onClose: () => void;
  onToast: (msg: string) => void;
}

/** The four targets, in display order, with their icon + i18n key suffix. */
const TARGETS: { key: ShareTarget; icon: IconName }[] = [
  { key: "pocket", icon: "bookmark" },
  { key: "instapaper", icon: "text" },
  { key: "kindle", icon: "headphones" },
  { key: "notion", icon: "list" },
];

export default function SendToMenu({ articleId, x, y, onClose, onToast }: Props) {
  const { t } = useTranslation();
  const ref = useRef<HTMLDivElement>(null);
  const [place, setPlace] = useState({ left: x, top: y });
  const [targets, setTargets] = useState<ShareTargets | null>(null);
  const [busy, setBusy] = useState<ShareTarget | null>(null);
  const onKeyDown = useMenuKeyboard(ref, targets != null);

  // Clamp inside the viewport. `targets` is in the deps because the menu
  // starts as a single short "loading" row and grows to four target rows once
  // `share_targets` resolves — measuring only on mount would clamp the short
  // placeholder, leaving the taller real menu overflowing (and its lower
  // targets clipped) when it opens near the bottom edge. Re-measure once the
  // real rows have rendered.
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPlace(clampToViewport({ x, y, width: r.width, height: r.height, margin: 8 }));
  }, [x, y, targets]);

  // Load which targets are configured.
  useEffect(() => {
    api
      .shareTargets()
      .then(setTargets)
      .catch(() => setTargets({ pocket: false, instapaper: false, kindle: false, notion: false }));
  }, []);

  useDismiss(ref, onClose);

  const send = async (target: ShareTarget) => {
    setBusy(target);
    try {
      await api.sendArticle(articleId, target);
      onToast(t("sendTo.sent", { target: t(`sendTo.${target}`) }));
      onClose();
    } catch (e) {
      reportError(e);
    } finally {
      setBusy(null);
    }
  };

  const anyConfigured =
    targets != null && TARGETS.some(({ key }) => targets[key]);

  return (
    <div
      ref={ref}
      className="ctx-menu send-to-menu"
      role="menu"
      aria-label={t("sendTo.heading")}
      style={{ left: place.left, top: place.top }}
      onKeyDown={onKeyDown}
    >
      <div className="hl-export-head">{t("sendTo.heading")}</div>
      {targets == null ? (
        <div className="ctx-item" role="presentation" aria-disabled>
          {t("common.loading")}
        </div>
      ) : (
        TARGETS.map(({ key, icon }) => {
          const ok = targets[key];
          return (
            <button
              key={key}
              className="ctx-item"
              role="menuitem"
              disabled={!ok || busy != null}
              title={ok ? undefined : t("sendTo.notConfigured")}
              onClick={() => send(key)}
            >
              <Icon name={icon} size={13} /> {t(`sendTo.${key}`)}
              {!ok && (
                <span className="ctx-shortcut">{t("sendTo.setupHint")}</span>
              )}
            </button>
          );
        })
      )}
      {targets != null && !anyConfigured && (
        <div className="hl-export-head" style={{ borderTop: "1px solid var(--hair)" }}>
          {t("sendTo.noneConfigured")}
        </div>
      )}
    </div>
  );
}
