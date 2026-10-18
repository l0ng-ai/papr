// Client-side formula typesetting for the reader (feature F9).
//
// Some feeds typeset their formulas before publishing and ship the result as
// MathML — KaTeX's `mathml` output (e.g. dreams.plus), or MathJax's assistive
// copy hidden next to its own rendering. `sanitize` in papr-core keeps that
// markup only as a *carrier*: the LaTeX source rides along in
// `<annotation encoding="application/x-tex">`, while the visual half is either
// absent (MathML-only output) or unrenderable (KaTeX's `htmlAndMathml` output
// depends on inline styles no sanitizer can preserve). So the whole formula is
// re-typeset here from the bundle's own KaTeX, which also covers formulas a
// feed left as raw LaTeX in prose ($$…$$, \[…\], \(…\), \begin{…}).
//
// Everything is bundled: no CDN, no network. The app's CSP allows inline
// styles and this is offline-capable like the rest of the reader.
//
// Rendering with `output: "html"` (rather than the default `htmlAndMathml`)
// matters for the reader's text layer: KaTeX's MathML copy is `aria-hidden`
// but still holds a duplicate of the formula's tokens, which would double
// every formula in the plain text `highlightDom.ts` anchors highlights
// against. The html half keeps a single, copy-friendly copy of the glyphs;
// the wrapper's `aria-label` restores what `aria-hidden` removed.

import katex from "katex";
import renderMathInElement from "katex/contrib/auto-render";

/** Shared KaTeX options. `strict: "ignore"` lets a feed's stray Unicode or
 *  legacy TeX through instead of failing the whole formula, and `trust: false`
 *  keeps `\href` / `\includegraphics` / `\htmlStyle` from injecting URLs,
 *  styles or handlers. */
const RENDER_OPTIONS = {
  output: "html",
  throwOnError: false,
  strict: "ignore",
  trust: false,
} as const;

/** The delimiters `auto-render` scans for in prose. These are KaTeX's own
 *  defaults; `$…$` is deliberately not one of them — a lone `$` is far too
 *  common in ordinary feed text to treat as math. */
const AUTO_RENDER_OPTIONS = {
  ...RENDER_OPTIONS,
  // Never rescan MathML we could not re-typeset (it is kept for the browser's
  // native renderer), nor the KaTeX output this module just produced.
  ignoredTags: ["script", "noscript", "style", "textarea", "pre", "code", "option", "math"],
  ignoredClasses: ["katex", "katex-display", "katex-mathml", "katex-html"],
  // A malformed formula must not spam the console; KaTeX still renders the
  // source in place for the reader to see.
  errorCallback: () => {},
};

/** The LaTeX source a MathML formula carries, or `null` when it has none.
 *  Mirrors `sanitize::is_tex_encoding`: the encoding must look like a TeX
 *  media type — `application/x-tex`, sometimes `application/x-latex` — and a
 *  naive `includes("tex")` would also match `text/plain`, whose content is a
 *  plain-text *rendering* of the formula, not TeX. */
function texFrom(math: Element): string | null {
  for (const ann of math.querySelectorAll("annotation, annotation-xml")) {
    const encoding = (ann.getAttribute("encoding") ?? "").trim().toLowerCase();
    if (encoding.startsWith("text/") || !encoding.includes("tex")) continue;
    const tex = ann.textContent?.trim();
    if (tex) return tex;
  }
  return null;
}

/** The wrapper a formula should be swapped out for a fresh render: KaTeX's
 *  (which also holds the visual half), or MathJax's container. */
const WRAPPER_SELECTOR = "mjx-container, .katex-display, .katex, .katex-mathml";

/** The *outermost* wrapper around `math` — the part to replace.
 *
 *  `closest` alone would return the nearest match, which is wrong for a page
 *  KaTeX typeset itself (`htmlAndMathml`, what "read full text" stores):
 *  `katex-display > katex > katex-mathml > math` would swap only the inner
 *  `.katex`, leaving a fresh `.katex-display` nested inside the old one and
 *  doubling its margins. Walk out through contiguous wrappers so the whole
 *  block goes at once. */
function wrapperFor(math: Element): Element {
  let host = math.closest(WRAPPER_SELECTOR) ?? math;
  for (let p = host.parentElement; p && p.matches(WRAPPER_SELECTOR); p = p.parentElement) {
    host = p;
  }
  return host;
}

/** Whether a formula should be typeset as a block. Reads every carrier shape:
 *  MathML's own `display`, MathJax's `mjx-container`, and KaTeX's wrapper. */
function isDisplayMode(math: Element, host: Element): boolean {
  if (math.getAttribute("display") === "block") return true;
  if (host.getAttribute("display") === "true") return true;
  return host.classList.contains("katex-display");
}

/** Keep only the MathML when a formula cannot be re-typeset. The carrier's
 *  other content is either the feed's own KaTeX visual half — which the
 *  sanitizer's stripped inline styles leave as overlapping glyphs — or
 *  MathJax's CHTML markup, whose own tags the sanitizer unwraps into a run of
 *  bare glyphs. The MathML beside it is a complete rendering on its own, so
 *  promote it into the carrier's place and let the browser typeset it. */
function fallbackToMathML(host: Element, math: Element): void {
  if (host !== math) host.replaceWith(math);
}

/**
 * Typeset every formula in `root` in place. Idempotent enough to re-run over
 * an already-processed body: the MathML carriers are gone after the first pass
 * and KaTeX's own output is excluded from the auto-render pass.
 *
 * A formula with no TeX source, or one this KaTeX build cannot parse, is left
 * as sanitized MathML for the webview's native renderer rather than failed.
 */
export function renderMath(root: HTMLElement): void {
  // Snapshot: replacing a carrier detaches any `math` nested inside it, so the
  // live NodeList would skip nodes as we go.
  for (const math of Array.from(root.querySelectorAll("math"))) {
    // An earlier promotion in this loop can detach a later node (two formulas
    // sharing one carrier, which a feed occasionally emits); nothing left to
    // do for it.
    if (!root.contains(math)) continue;
    // Swap the whole wrapper the feed (or page) built around the formula, not
    // just the `<math>` inside it — that wrapper also holds the visual half.
    const host = wrapperFor(math);
    const tex = texFrom(math);
    if (!tex) {
      fallbackToMathML(host, math);
      continue;
    }

    const rendered = document.createElement("span");
    try {
      // `throwOnError: true` only here, to detect a formula this build can't
      // parse and fall back to the MathML; the reader never sees the throw.
      katex.render(tex, rendered, {
        ...RENDER_OPTIONS,
        throwOnError: true,
        displayMode: isDisplayMode(math, host),
      });
    } catch {
      fallbackToMathML(host, math);
      continue;
    }

    // `output: "html"` hides the only copy of the formula from assistive tech,
    // so carry the source back on the wrapper.
    rendered.setAttribute("role", "math");
    rendered.setAttribute("aria-label", tex);
    host.replaceWith(rendered);
  }

  renderMathInElement(root, AUTO_RENDER_OPTIONS);
}
