// Highlight & annotation layer for the Reader (feature F7).
//
// Owns three pieces of UI laid over the article body:
//   1. a floating colour toolbar shown when text is selected,
//   2. a popover for editing / deleting an existing highlight,
//   3. an export menu (Markdown copy/save, Obsidian, Readwise, Notion).
//
// The pure re-anchoring lives in `lib/anchor.ts`; the DOM wrapping in
// `lib/highlightDom.ts`. This component is the glue that calls them and the
// `create_highlight` / export Tauri commands.

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { reportError, withUndo } from "../toast";
import {
  applyHighlights,
  clearHighlights,
  plainText,
  selectionAnchor,
} from "../lib/highlightDom";
import { captureContext } from "../lib/anchor";
import { HIGHLIGHT_COLORS } from "../lib/highlightColors";
import { clampAxis } from "../lib/viewport";
import type { Highlight } from "../types";
import Icon from "./Icon";

interface Props {
  /** The article whose body is highlighted. */
  articleId: number;
  /** Ref to the rendered article-body div the highlights are applied into. */
  bodyRef: React.RefObject<HTMLDivElement | null>;
  /** The rendered body HTML — changes whenever the Reader swaps the body
   *  (extract toggle, extraction finishing) so highlights are re-applied to
   *  the fresh DOM. Compared by value, so a no-op render does not re-apply. */
  bodyVersion: string;
}

/** The floating colour toolbar's anchor point (viewport coordinates). */
interface ToolbarPos {
  x: number;
  y: number;
}

/** The currently-selected text pending a highlight. */
interface PendingSelection {
  quote: string;
  textOffset: number;
}

export default function HighlightLayer({
  articleId,
  bodyRef,
  bodyVersion,
}: Props) {
  const { t } = useTranslation();
  const [highlights, setHighlights] = useState<Highlight[]>([]);
  const [toolbar, setToolbar] = useState<ToolbarPos | null>(null);
  const pendingRef = useRef<PendingSelection | null>(null);
  // The highlight whose edit popover is open, plus where to anchor it. Stores
  // the id (not a snapshot) so the popover always reflects the live highlight
  // — recolouring it re-renders the popover's active swatch immediately.
  const [editing, setEditing] = useState<{ hlId: number; x: number; y: number } | null>(
    null,
  );

  // The live highlight backing the open edit popover, looked up fresh from the
  // current set — so a recolour (which reloads `highlights`) is reflected in
  // the popover's active swatch. `null` once the highlight is deleted, which
  // also tears the popover down.
  const editingHl = editing
    ? (highlights.find((h) => h.id === editing.hlId) ?? null)
    : null;

  // Load the article's stored highlights.
  const reload = () => {
    api
      .listHighlights(articleId)
      .then(setHighlights)
      .catch(() => setHighlights([]));
  };
  useEffect(() => {
    setHighlights([]);
    reload();
    setEditing(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [articleId]);

  // Re-apply highlights to the body whenever the highlight set or the body
  // HTML changes. `bodyVersion` flips when the Reader swaps the body markup.
  //
  // The <mark> overlay is injected into DOM that React owns via
  // `dangerouslySetInnerHTML`, so anything that re-populates the body —
  // React resetting its innerHTML, or the body element only filling in
  // *after* this effect first runs (the reopen race that left highlights
  // blank until the next edit) — silently drops every mark. A
  // MutationObserver re-applies them whenever the body's child list changes;
  // `bodyVersion` alone misses a body rebuilt with identical markup and the
  // initial mount ordering.
  useEffect(() => {
    const el = bodyRef.current;
    if (!el) return;

    let obs: MutationObserver | null = null;
    const apply = () => {
      // Suspend observation while we mutate so our own <mark> edits do not
      // re-trigger the callback (which would loop).
      obs?.disconnect();
      if (highlights.length === 0) clearHighlights(el);
      else applyHighlights(el, highlights);
      obs?.observe(el, { childList: true, subtree: true });
    };

    obs = new MutationObserver(apply);
    apply();
    return () => obs?.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [highlights, bodyVersion]);

  // Show the colour toolbar when the user finishes a selection in the body.
  useEffect(() => {
    const el = bodyRef.current;
    if (!el) return;
    const onMouseUp = () => {
      // Defer so the browser has committed the selection.
      window.setTimeout(() => {
        const sel = selectionAnchor(el);
        if (!sel) {
          pendingRef.current = null;
          setToolbar(null);
          return;
        }
        pendingRef.current = sel;
        const range = window.getSelection()?.getRangeAt(0);
        const rect = range?.getBoundingClientRect();
        if (rect) {
          setToolbar({ x: rect.left + rect.width / 2, y: rect.top });
        }
      }, 0);
    };
    el.addEventListener("mouseup", onMouseUp);
    return () => el.removeEventListener("mouseup", onMouseUp);
  }, [bodyRef]);

  // Clicking an existing highlight <mark> opens its edit popover.
  useEffect(() => {
    const el = bodyRef.current;
    if (!el) return;
    const onClick = (e: MouseEvent) => {
      const mark = (e.target as HTMLElement).closest("mark[data-hl]");
      if (!mark) return;
      e.preventDefault();
      e.stopPropagation();
      const id = Number((mark as HTMLElement).dataset.hl);
      if (!highlights.some((h) => h.id === id)) return;
      const r = mark.getBoundingClientRect();
      setEditing({ hlId: id, x: r.left, y: r.bottom + 6 });
    };
    el.addEventListener("click", onClick, true);
    return () => el.removeEventListener("click", onClick, true);
  }, [bodyRef, highlights]);

  const createHighlight = async (color: string) => {
    const el = bodyRef.current;
    const pending = pendingRef.current;
    if (!el || !pending) return;
    const ctx = captureContext(plainText(el), pending.textOffset, pending.textOffset + pending.quote.length);
    try {
      await api.createHighlight({
        articleId,
        quote: pending.quote,
        prefix: ctx.prefix,
        suffix: ctx.suffix,
        textOffset: pending.textOffset,
        color,
        note: "",
      });
      window.getSelection()?.removeAllRanges();
      setToolbar(null);
      pendingRef.current = null;
      reload();
    } catch (e) {
      reportError(e);
    }
  };

  // Deleting a highlight runs behind an Undo window: it leaves the overlay at
  // once, but is only removed from the database ~6s later unless the user
  // takes it back. A snapshot is kept so Undo can restore it in reading order.
  const deleteHighlight = (hl: Highlight) => {
    withUndo({
      text: t("highlights.deleted"),
      apply: () => setHighlights((prev) => prev.filter((h) => h.id !== hl.id)),
      commit: () => {
        api.deleteHighlight(hl.id).catch(reportError);
      },
      revert: () =>
        setHighlights((prev) =>
          [...prev, hl].sort((a, b) => a.textOffset - b.textOffset),
        ),
    });
  };

  return (
    <>
      {toolbar && (
        <SelectionToolbar
          pos={toolbar}
          onPick={createHighlight}
          onDismiss={() => setToolbar(null)}
        />
      )}
      {editingHl && editing && (
        <HighlightPopover
          key={editingHl.id}
          hl={editingHl}
          x={editing.x}
          y={editing.y}
          onClose={() => setEditing(null)}
          onChanged={reload}
          onDelete={deleteHighlight}
        />
      )}
    </>
  );
}

/* ── floating colour toolbar ─────────────────────────────────── */
function SelectionToolbar({
  pos,
  onPick,
  onDismiss,
}: {
  pos: ToolbarPos;
  onPick: (color: string) => void;
  onDismiss: () => void;
}) {
  const { t } = useTranslation();
  const ref = useRef<HTMLDivElement>(null);
  const [place, setPlace] = useState({ left: pos.x, top: pos.y });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    // X is a plain two-sided viewport clamp; Y keeps its custom flip-below
    // behaviour (the toolbar prefers to sit above the selection).
    const left = clampAxis(pos.x - r.width / 2, r.width, window.innerWidth, 8);
    let top = pos.y - r.height - 8;
    if (top < 8) top = pos.y + 22; // flip below the selection if clipped
    setPlace({ left, top });
  }, [pos]);

  // A scroll or an outside click ends the selection toolbar.
  useEffect(() => {
    const onScroll = () => onDismiss();
    window.addEventListener("scroll", onScroll, true);
    return () => window.removeEventListener("scroll", onScroll, true);
  }, [onDismiss]);

  return (
    <div
      ref={ref}
      className="hl-toolbar"
      role="toolbar"
      aria-label={t("highlights.toolbarLabel")}
      style={{ left: place.left, top: place.top }}
      // Keep the text selection alive while the toolbar is clicked.
      onMouseDown={(e) => e.preventDefault()}
    >
      {HIGHLIGHT_COLORS.map((c) => (
        <button
          key={c.key}
          className="hl-swatch"
          style={{ background: c.swatch }}
          title={t(`highlights.color.${c.key}`)}
          aria-label={t(`highlights.color.${c.key}`)}
          onClick={() => onPick(c.key)}
        />
      ))}
    </div>
  );
}

/* ── existing-highlight edit popover ─────────────────────────── */
function HighlightPopover({
  hl,
  x,
  y,
  onClose,
  onChanged,
  onDelete,
}: {
  hl: Highlight;
  x: number;
  y: number;
  onClose: () => void;
  onChanged: () => void;
  onDelete: (hl: Highlight) => void;
}) {
  const { t } = useTranslation();
  const ref = useRef<HTMLDivElement>(null);
  const [note, setNote] = useState(hl.note);
  // Latest note, read by the once-bound outside-click handler so the listener
  // need not re-subscribe on every keystroke.
  const noteRef = useRef(note);
  noteRef.current = note;
  const [place, setPlace] = useState({ left: x, top: y });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    // X is a plain two-sided viewport clamp; Y keeps its custom flip-above
    // behaviour (when the popover would overflow the bottom it jumps above
    // the anchor instead of merely being pulled back).
    const left = clampAxis(x, r.width, window.innerWidth, 8);
    let top = y;
    if (top + r.height > window.innerHeight - 8) top = y - r.height - 28;
    setPlace({ left, top: Math.max(8, top) });
  }, [x, y]);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) save();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    const tm = window.setTimeout(() => {
      document.addEventListener("mousedown", onDown);
      window.addEventListener("keydown", onKey);
    }, 0);
    return () => {
      window.clearTimeout(tm);
      document.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const save = async () => {
    const current = noteRef.current;
    if (current !== hl.note) {
      try {
        await api.updateHighlightNote(hl.id, current);
        onChanged();
      } catch (e) {
        reportError(e);
      }
    }
    onClose();
  };

  const recolor = async (color: string) => {
    try {
      await api.setHighlightColor(hl.id, color);
      onChanged();
    } catch (e) {
      reportError(e);
    }
  };

  // The delete itself — including its Undo window — is owned by the parent;
  // here we just hand off the highlight and dismiss the popover.
  const remove = () => {
    onDelete(hl);
    onClose();
  };

  return (
    <div
      ref={ref}
      className="hl-popover"
      role="dialog"
      aria-label={t("highlights.editTitle")}
      style={{ left: place.left, top: place.top }}
    >
      <blockquote className="hl-quote">{hl.quote}</blockquote>
      <div className="hl-swatch-row">
        {HIGHLIGHT_COLORS.map((c) => (
          <button
            key={c.key}
            className={`hl-swatch ${hl.color === c.key ? "active" : ""}`}
            style={{ background: c.swatch }}
            title={t(`highlights.color.${c.key}`)}
            aria-label={t(`highlights.color.${c.key}`)}
            onClick={() => recolor(c.key)}
          />
        ))}
      </div>
      <textarea
        className="hl-note-input"
        placeholder={t("highlights.notePlaceholder")}
        value={note}
        autoFocus
        onChange={(e) => setNote(e.target.value)}
      />
      <div className="hl-popover-actions">
        <button className="s-btn danger" onClick={remove}>
          <Icon name="trash" size={12} /> {t("common.delete")}
        </button>
        <button className="s-btn primary" onClick={save}>
          {t("common.done")}
        </button>
      </div>
    </div>
  );
}
