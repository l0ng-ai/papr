import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { errorText } from "../lib/errors";
import { tagColor } from "../lib/tagColors";
import Icon from "./Icon";

interface Props {
  articleId: number;
  /** Ids of tags already attached to the article. */
  attached: number[];
  /** Anchor point (viewport coords) the popover opens from. */
  x: number;
  y: number;
  onClose: () => void;
  onToast: (msg: string) => void;
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
  onToast,
}: Props) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const ref = useRef<HTMLDivElement>(null);
  const [draft, setDraft] = useState("");

  const tags = useQuery({ queryKey: ["tags"], queryFn: api.listTags });
  const attachedSet = new Set(attached);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    const timer = window.setTimeout(() => {
      document.addEventListener("mousedown", onDown);
      window.addEventListener("keydown", onKey);
    }, 0);
    return () => {
      window.clearTimeout(timer);
      document.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

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
      .catch((e) => onToast(errorText(e)));

  const createAndAttach = async () => {
    const name = draft.trim();
    if (!name) return;
    try {
      const id = await api.createTag(name);
      await api.setArticleTag(articleId, id, true);
      setDraft("");
      sync();
    } catch (e) {
      onToast(errorText(e));
    }
  };

  // Clamp inside the viewport.
  const left = Math.min(x, window.innerWidth - 248);
  const top = Math.min(y, window.innerHeight - 320);

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
          onKeyDown={(e) => e.key === "Enter" && createAndAttach()}
          placeholder={t("tagPicker.createPlaceholder")}
        />
        <button onClick={createAndAttach} disabled={!draft.trim()}>
          <Icon name="plus" size={13} />
        </button>
      </div>
    </div>
  );
}
