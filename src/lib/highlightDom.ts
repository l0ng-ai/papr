// DOM glue for the highlight layer (feature F7).
//
// `anchor.ts` resolves a highlight to a *character range* within the article's
// rendered plain text. This module bridges that abstract range back to the
// live DOM: it walks the body's text nodes, maps plain-text offsets to
// (node, offset) pairs, and wraps the matching run in <mark> elements.
//
// Kept apart from `anchor.ts` so the anchoring algorithm stays DOM-free and
// node-testable; this module is exercised in the running webview.

import { findAnchor, type HighlightAnchor } from "./anchor";
import { highlightBg } from "./highlightColors";
import type { Highlight } from "../types";

/** A text node plus the running plain-text offset at which it starts. */
interface TextSpan {
  node: Text;
  start: number;
}

/**
 * Collect every text node under `root` in document order, each tagged with
 * the cumulative character offset where it begins. The concatenation of the
 * node values is the "plain text" the anchor offsets are measured against.
 *
 * Text inside an existing <mark> is skipped — re-applying highlights should
 * not nest marks, and a fresh render never has any.
 */
function collectTextSpans(root: HTMLElement): { spans: TextSpan[]; text: string } {
  const spans: TextSpan[] = [];
  let text = "";
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      const parent = (node as Text).parentElement;
      if (parent && parent.closest("mark[data-hl]")) {
        return NodeFilter.FILTER_REJECT;
      }
      return NodeFilter.FILTER_ACCEPT;
    },
  });
  let n = walker.nextNode();
  while (n) {
    const t = n as Text;
    spans.push({ node: t, start: text.length });
    text += t.data;
    n = walker.nextNode();
  }
  return { spans, text };
}

/** The plain text the anchor offsets in stored highlights refer to. */
export function plainText(root: HTMLElement): string {
  return collectTextSpans(root).text;
}

/**
 * Wrap the character range `[start, end)` of the plain text in <mark>
 * elements. A range crossing several text nodes is split into one <mark> per
 * node so block structure is preserved. Returns the created marks.
 */
function wrapRange(
  spans: TextSpan[],
  start: number,
  end: number,
  hl: Highlight,
): HTMLElement[] {
  const marks: HTMLElement[] = [];
  for (const span of spans) {
    const nodeEnd = span.start + span.node.data.length;
    if (nodeEnd <= start || span.start >= end) continue; // no overlap

    const localStart = Math.max(0, start - span.start);
    const localEnd = Math.min(span.node.data.length, end - span.start);
    if (localEnd <= localStart) continue;

    const range = document.createRange();
    range.setStart(span.node, localStart);
    range.setEnd(span.node, localEnd);

    const mark = document.createElement("mark");
    mark.dataset.hl = String(hl.id);
    mark.style.backgroundColor = highlightBg(hl.color);
    mark.style.borderRadius = "2px";
    mark.style.cursor = "pointer";
    if (hl.note.trim()) mark.dataset.note = "1";
    try {
      range.surroundContents(mark);
      marks.push(mark);
    } catch {
      // surroundContents throws if the range partially selects a non-text
      // node — skip that fragment rather than breaking the whole article.
    }
  }
  return marks;
}

/**
 * Re-apply every stored highlight to a freshly rendered article body. Existing
 * marks are stripped first so this is idempotent. Returns the highlight ids
 * that could not be anchored (their quote is gone from the current text).
 */
export function applyHighlights(root: HTMLElement, highlights: Highlight[]): number[] {
  clearHighlights(root);
  const orphaned: number[] = [];
  // Each wrap mutates the DOM, so re-collect spans per highlight.
  for (const hl of highlights) {
    const { spans, text } = collectTextSpans(root);
    const anchor: HighlightAnchor = {
      quote: hl.quote,
      prefix: hl.prefix,
      suffix: hl.suffix,
      textOffset: hl.textOffset,
    };
    const range = findAnchor(text, anchor);
    if (!range) {
      orphaned.push(hl.id);
      continue;
    }
    wrapRange(spans, range.start, range.end, hl);
  }
  return orphaned;
}

/** Remove every highlight <mark>, leaving the body text intact. */
export function clearHighlights(root: HTMLElement): void {
  root.querySelectorAll("mark[data-hl]").forEach((mark) => {
    const parent = mark.parentNode;
    if (!parent) return;
    while (mark.firstChild) parent.insertBefore(mark.firstChild, mark);
    parent.removeChild(mark);
    parent.normalize(); // re-merge the split text nodes
  });
}

/**
 * Map a DOM point (`container`, `offset`) to an offset within the plain text
 * that `collectTextSpans` produces. `atEnd` decides the fallback when the
 * point lands inside a skipped <mark> or past every span: an unresolved start
 * snaps forward (0 or the next span's start), an unresolved end snaps to the
 * tail of the text — so a range that brackets a <mark> still yields a sane,
 * mark-free slice.
 */
function pointToOffset(
  spans: TextSpan[],
  container: Node,
  offset: number,
  atEnd: boolean,
): number {
  for (const span of spans) {
    if (span.node === container) {
      // Common case: the point sits inside this collected text node.
      return span.start + Math.min(offset, span.node.data.length);
    }
  }
  // The point is not inside a collected node (an element boundary, or inside
  // a skipped <mark>). Use document order to place it relative to the spans.
  for (const span of spans) {
    const pos = container.compareDocumentPosition(span.node);
    // The span follows the point — the point falls at this span's start.
    if (pos & Node.DOCUMENT_POSITION_FOLLOWING) return span.start;
  }
  const last = spans[spans.length - 1];
  return atEnd && last ? last.start + last.node.data.length : 0;
}

/**
 * Describe the current text selection inside `root` as a highlight anchor:
 * its quoted text and plain-text offset. Returns `null` when the selection is
 * empty or falls outside the article body.
 *
 * Both the offset *and* the quote are taken from the same plain text
 * `plainText` / `applyHighlights` use — i.e. text inside existing <mark>
 * elements is skipped. Using `Range.toString()` for the quote would instead
 * include marked text, so a selection that brackets an existing highlight
 * would store a quote that is not a substring of the anchoring basis and could
 * never be re-anchored on reopen.
 */
export function selectionAnchor(
  root: HTMLElement,
): { quote: string; textOffset: number } | null {
  const sel = window.getSelection();
  if (!sel || sel.isCollapsed || sel.rangeCount === 0) return null;
  const range = sel.getRangeAt(0);
  if (!root.contains(range.commonAncestorContainer)) return null;

  // Resolve both ends of the selection against the mark-free plain text, then
  // slice the quote out of that same basis — so the quote, the offset and the
  // text `findAnchor` searches all agree even across an existing highlight.
  const { spans, text } = collectTextSpans(root);
  const textOffset = pointToOffset(spans, range.startContainer, range.startOffset, false);
  const endOffset = pointToOffset(spans, range.endContainer, range.endOffset, true);
  const quote = text.slice(textOffset, Math.max(textOffset, endOffset));
  if (!quote.trim()) return null;

  return { quote, textOffset };
}
