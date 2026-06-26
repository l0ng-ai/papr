import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { useDismiss } from "../hooks/useDismiss";
import { reportError } from "../toast";
import { tagColor } from "../lib/tagColors";
import { clampToViewport } from "../lib/viewport";
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
  };

  const toggle = (tagId: number, on: boolean) =>
    api
      .setArticleTag(articleId, tagId, on)
      .then(sync)
      .catch((e) => reportError(e));

  const createAndAttach = async () => {
    const name = draft.trim();
    if (!name) return;
    try {
      const id = await api.createTag(name);
      await api.setArticleTag(articleId, id, true);
      setDraft("");
      sync();
    } catch (e) {
      reportError(e);
    }
  };

  // Clamp inside the viewport with the shared two-sided helper. The popover
  // is ~232px wide and ~320px tall; the 248/320 footprint plus the 0px margin
  // reproduces the historical pull-back while flooring the top-left corner so
  // a narrow/short window can't push the popover off-screen.
  const { left, top } = clampToViewport({ x, y, width: 248, height: 320, margin: 0 });

  return (
    <div className="tag-picker" ref={ref} style={{ left, top }}>
      <div className="tag-picker-head">{t("tagPicker.title")}</div>
      <div className="tag-picker-list">
        {(tags.data ?? []).map((tag) => {
          const on = attachedSet.has(tag.id);
          return (
            <div
              key={tag.id}
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
      <div className="tag-picker-create">
        <input
          value={draft}
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
        <button onClick={createAndAttach} disabled={!draft.trim()}>
          <Icon name="plus" size={13} />
        </button>
      </div>
    </div>
  );
}
