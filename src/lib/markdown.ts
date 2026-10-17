// Render markdown produced by the AI features (summaries, Q&A, digests).
//
// The text is LLM output, but the LLM summarizes attacker-influenced feed
// content, so its markdown can carry raw HTML. `marked` does not sanitize,
// and the result is injected via dangerouslySetInnerHTML inside a Tauri
// webview — so every render is passed through an allowlist sanitizer first.

import { marked } from "marked";

marked.setOptions({ breaks: true, gfm: true });

// Elements removed wholesale, including their text content.
const DROP_TAGS = new Set([
  "script", "style", "iframe", "object", "embed", "noscript",
  "template", "link", "meta", "form", "input", "button", "svg",
]);

// Elements kept as-is. Anything else is unwrapped to its children.
const ALLOWED_TAGS = new Set([
  "p", "br", "hr", "span", "div", "blockquote", "pre", "code",
  "strong", "em", "b", "i", "u", "s", "del", "mark", "sub", "sup",
  "ul", "ol", "li", "a",
  "h1", "h2", "h3", "h4", "h5", "h6",
  "table", "thead", "tbody", "tfoot", "tr", "th", "td",
]);

// Per-tag attribute allowlist. Everything not listed (incl. on* handlers,
// style, srcset) is stripped.
const ALLOWED_ATTRS: Record<string, Set<string>> = {
  a: new Set(["href", "title"]),
  td: new Set(["colspan", "rowspan"]),
  th: new Set(["colspan", "rowspan", "scope"]),
};

const SAFE_HREF = /^(https?:|mailto:)/i;

function sanitizeElement(el: Element, images?: ReadonlySet<string>) {
  // Depth-first: process children before the element may be unwrapped.
  for (const child of Array.from(el.children)) sanitizeElement(child, images);

  const tag = el.tagName.toLowerCase();

  if (DROP_TAGS.has(tag)) {
    el.remove();
    return;
  }
  if (tag === "img") {
    const src = el.getAttribute("src") || "";
    if (!images?.has(src) || !/^https?:\/\//i.test(src)) { el.remove(); return; }
    const alt = el.getAttribute("alt") || "";
    for (const attr of Array.from(el.attributes)) el.removeAttribute(attr.name);
    el.setAttribute("src", src); el.setAttribute("alt", alt);
    el.setAttribute("loading", "lazy"); el.setAttribute("referrerpolicy", "no-referrer");
    return;
  }
  if (!ALLOWED_TAGS.has(tag)) {
    el.replaceWith(...Array.from(el.childNodes)); // unwrap, keep contents
    return;
  }

  const allowed = ALLOWED_ATTRS[tag];
  for (const attr of Array.from(el.attributes)) {
    if (!allowed || !allowed.has(attr.name.toLowerCase())) {
      el.removeAttribute(attr.name);
    }
  }
  if (tag === "a") {
    const href = (el.getAttribute("href") ?? "").trim();
    if (SAFE_HREF.test(href)) {
      el.setAttribute("rel", "noopener noreferrer nofollow");
    } else {
      el.removeAttribute("href");
    }
  }
}

export function renderMarkdown(text: string, images?: ReadonlySet<string>): string {
  const raw = marked.parse(text, { async: false });
  // Template contents are inert, including images: sanitize before attaching
  // them to a document so invented image URLs are never loaded while parsing.
  const doc = document.implementation.createHTMLDocument("");
  const template = doc.createElement("template");
  template.innerHTML = raw;
  for (const child of Array.from(template.content.children)) sanitizeElement(child, images);
  doc.body.append(...Array.from(template.content.childNodes));
  for (const extra of Array.from(doc.body.querySelectorAll("img")).slice(2)) extra.remove();
  // Link each validated image to its original source, outside the sanitizer so
  // only app-owned figure markup is added. Text captions never become HTML.
  for (const img of Array.from(doc.body.querySelectorAll("img"))) {
    const figure = doc.createElement("figure");
    const link = doc.createElement("a"); link.href = img.getAttribute("src")!;
    link.rel = "noopener noreferrer nofollow";
    img.replaceWith(figure); link.append(img); figure.append(link);
    if (img.getAttribute("alt")) { const caption = doc.createElement("figcaption"); caption.textContent = img.getAttribute("alt"); figure.append(caption); }
  }
  return doc.body.innerHTML;
}
