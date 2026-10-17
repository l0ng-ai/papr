import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { useDismiss } from "../hooks/useDismiss";
import { reportError } from "../toast";
import { flatTags, tagPath } from "../lib/tags";
import { tagColor } from "../lib/tagColors";
import { useFloatingPosition } from "../hooks/useFloatingPosition";
import { isMobile } from "../lib/platform";
import { NO_AUTOCORRECT } from "../lib/inputProps";
import Icon from "./Icon";

interface Props {
  articleId: number;
  /** Ids of tags already attached to the article. */
  attached: number[];
  /** Anchor point (viewport coords) the popover opens from. */
  x: number;
  y: number;
  onClose: () => void;
}

/**
 * Floating tag editor: toggle existing tags on an article, or create a new
 * one and attach it in a single step. Stays open across toggles.
 */
export default function TagPicker({
  articleId,
  attached,
  x,
  y,
  onClose,
}: Props) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const ref = useRef<HTMLDivElement>(null);
  const [draft, setDraft] = useState("");
  const [creating, setCreating] = useState(false);
  const [parentId, setParentId] = useState<number | null>(null);

  const tags = useQuery({ queryKey: ["tags"], queryFn: api.listTags });
  const attachedSet = new Set(attached);

  // Tabbing past the popover's last control dismisses it, the way a click
  // outside does — otherwise it floats over the page, orphaned from the
  // keyboard.
  useDismiss(ref, onClose, { onFocusOut: true });

  // Move focus into the popover on open so it is keyboard-reachable, and
  // restore it to the trigger (the toolbar tag button) on close.
  useEffect(() => {
    const trigger = document.activeElement as HTMLElement | null;
    ref.current
      ?.querySelector<HTMLElement>('[role="button"], input')
      ?.focus();
    return () => trigger?.focus?.();
  }, []);

  const sync = () => {
    qc.invalidateQueries({ queryKey: ["article", articleId] });
    qc.invalidateQueries({ queryKey: ["tags"] });
    qc.invalidateQueries({ queryKey: ["articles"] });
    qc.invalidateQueries({ queryKey: ["search"] });
    qc.invalidateQueries({ queryKey: ["cp-search"] });
  };

  const toggle = (tagId: number, on: boolean) =>
    api
      .setArticleTag(articleId, tagId, on)
      .then(sync)
      .catch((e) => reportError(e));

  const createAndAttach = async () => {
    const name = draft.trim();
    if (!name || creating) return;
    setCreating(true);
    try {
      const ids = await api.createTags(name, parentId);
      await Promise.all(ids.map((id) => api.setArticleTag(articleId, id, true)));
      setDraft("");
      sync();
    } catch (e) {
      reportError(e);
    } finally { setCreating(false); }
  };

  const position = useFloatingPosition(ref, x, y);

  return createPortal(
    <div className="tag-picker" role="dialog" aria-label={t("tagPicker.title")} ref={ref} style={position}>
      <div className="tag-picker-head"><span>{t("tagPicker.title")}</span>
        {isMobile && <button className="tag-picker-close" aria-label={t("common.close")} onClick={onClose}><Icon name="x" size={18} /></button>}
      </div>
      <div className="tag-picker-list">
        {flatTags(tags.data ?? []).map(({ tag, depth }) => {
          const on = attachedSet.has(tag.id);
          return (
            <div
              key={tag.id}
              style={{ paddingLeft: 12 + depth * 14 }}
              title={tagPath(tag.id, tags.data ?? [])}
              className={`tag-picker-row ${on ? "on" : ""}`}
              role="button"
              tabIndex={0}
              aria-pressed={on}
              onClick={() => toggle(tag.id, !on)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  toggle(tag.id, !on);
                }
              }}
            >
              <span
                className="tag-dot"
                style={{ background: tagColor(tag.color) }}
              />
              <span className="tag-picker-name">{tag.name}</span>
              {on && <Icon name="check" size={13} />}
            </div>
          );
        })}
        {(tags.data ?? []).length === 0 && (
          <div className="tag-picker-empty">{t("tagPicker.empty")}</div>
        )}
      </div>
      <select className="tag-picker-parent" aria-label={t("tagPicker.parent")} value={parentId ?? ""} onChange={(e) => setParentId(e.target.value ? Number(e.target.value) : null)}>
        <option value="">{t("tagPicker.root")}</option>
        {flatTags(tags.data ?? []).map(({ tag }) => <option key={tag.id} value={tag.id}>{tagPath(tag.id, tags.data ?? [])}</option>)}
      </select>
      <p className="tag-picker-path-hint">{t("tagPicker.pathHint")}</p>
      <div className="tag-picker-create">
        <input
          value={draft}
          disabled={creating}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            // Skip the Enter that confirms an IME candidate (CJK input) so a
            // half-composed draft isn't turned into a tag.
            if (e.key === "Enter" && !e.nativeEvent.isComposing) createAndAttach();
          }}
          placeholder={t("tagPicker.createPlaceholder")}
          aria-label={t("tagPicker.createPlaceholder")}
          {...NO_AUTOCORRECT}
        />
        <button aria-label={t("sidebar.newTagTitle")} onClick={createAndAttach} disabled={creating || !draft.trim()}>
          <Icon name="plus" size={13} />
        </button>
      </div>
    </div>, document.body
  );
}
