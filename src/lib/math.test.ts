// @vitest-environment happy-dom
// DOM tests for the math typesetting pass (feature F9, `src/lib/math.ts`).
//
// The fixtures are the *sanitized* form of real feed markup — see the probe in
// papr-core's sanitize tests: unquoted attributes are quoted, `xmlns` is
// dropped, and `&` is re-escaped as `&amp;`.
//
// Why the dynamic imports: KaTeX reads `document.compatMode` once, when its
// module is evaluated, and replaces `render` with a thrower when the document
// is in quirks mode. happy-dom boots without a doctype, so the loaded modules
// would see quirks mode and refuse to render. The app's index.html *does* ship
// a doctype, so `beforeAll` restores the same `CSS1Compat` mode the webview runs
// in, and only then imports KaTeX / the module under test.
//
// (happy-dom also parses `<math>` into the XHTML namespace rather than the
// MathML one. The module keys off tag names, never namespaces, so the fixtures
// exercise the same code path either way.)

import { beforeAll, describe, expect, it } from "vitest";

let renderMath: (root: HTMLElement) => void;
let katex: typeof import("katex").default;

beforeAll(async () => {
  Object.defineProperty(document, "compatMode", {
    value: "CSS1Compat",
    configurable: true,
  });
  katex = (await import("katex")).default;
  renderMath = (await import("./math")).renderMath;
});

/** Real `dreams.plus` inline formula, after sanitize. */
const INLINE =
  '<p>它用一个凸函数 <span class="katex"><math><semantics><mrow><mi>f</mi></mrow>' +
  '<annotation encoding="application/x-tex">f</annotation></semantics></math></span> 来统一描述。</p>';

/** Real `dreams.plus` display formula, after sanitize. */
const DISPLAY =
  '<span class="katex"><math display="block"><semantics><mtable><mtr><mtd><mstyle displaystyle="true">' +
  '<mrow><mfrac><mrow><mi>p</mi><mo stretchy="false">(</mo><mi>x</mi><mo stretchy="false">)</mo></mrow>' +
  '<mrow><mi>q</mi><mo stretchy="false">(</mo><mi>x</mi><mo stretchy="false">)</mo></mrow></mfrac>' +
  '<mo fence="true">)</mo></mrow><mi>d</mi><mi>x</mi><mo separator="true">,</mo></mstyle></mtd></mtr></mtable>' +
  '<annotation encoding="application/x-tex">\\begin{aligned} D_f(P\\|Q) &amp;=\\int q(x)\\, f\\!\\left(\\frac{p(x)}{q(x)}\\right)dx, \\end{aligned}</annotation>' +
  "</semantics></math></span>";

/** A page KaTeX typeset itself (`htmlAndMathml`), as "read full text" stores
 *  it: `katex-display > katex`, a `.katex-mathml` carrier, and the page's own
 *  visual half — which the sanitizer's stripped inline styles leave as
 *  meaningless glyph runs. `STALE` stands in for that visual half. */
const SITE_DISPLAY =
  '<span class="katex-display"><span class="katex"><span class="katex-mathml">' +
  '<math display="block"><semantics><mrow><msup><mi>x</mi><mn>2</mn></msup></mrow>' +
  '<annotation encoding="application/x-tex">x^2</annotation></semantics></math></span>' +
  '<span class="katex-html" aria-hidden="true">STALE</span></span></span>';

function mount(html: string): HTMLElement {
  const root = document.createElement("div");
  root.className = "article-body";
  root.innerHTML = html;
  renderMath(root);
  return root;
}

describe("renderMath", () => {
  it("replaces an inline MathML carrier with a KaTeX render", () => {
    const root = mount(INLINE);
    expect(root.querySelector("math")).toBeNull();
    const wrapper = root.querySelector(".katex");
    expect(wrapper).not.toBeNull();
    expect(wrapper!.classList.contains("katex-display")).toBe(false);
    expect(wrapper!.querySelector(".katex-html")).not.toBeNull();
    // The surrounding prose is untouched.
    expect(root.textContent).toContain("来统一描述。");
  });

  it("keeps the display wrapper and block mode for display math", () => {
    const root = mount(DISPLAY);
    const wrapper = root.querySelector(".katex-display");
    expect(wrapper).not.toBeNull();
    expect(wrapper!.querySelector(".katex-html")).not.toBeNull();
  });

  it("typesets a page-rendered block without nesting display wrappers", () => {
    const root = mount(SITE_DISPLAY);
    expect(root.querySelector("math")).toBeNull();
    // One display block, not a fresh one nested inside the page's own.
    expect(root.querySelectorAll(".katex-display").length).toBe(1);
    expect(root.querySelector(".katex-display .katex-display")).toBeNull();
    expect(root.querySelector(".katex-display > .katex")).not.toBeNull();
    // The page's stale visual half goes with the wrapper it lived in.
    expect(root.textContent).not.toContain("STALE");
    expect(root.querySelector(".katex-html")).not.toBeNull();
  });

  it("carries the TeX source on the wrapper for assistive tech", () => {
    const root = mount(INLINE);
    const wrapper = root.querySelector('[role="math"]');
    expect(wrapper).not.toBeNull();
    expect(wrapper!.getAttribute("aria-label")).toBe("f");
  });

  it("typesets raw LaTeX delimiters left in prose", () => {
    const root = mount("<p>inline \\(x^2\\) and block $$y^2$$ end</p>");
    expect(root.querySelectorAll(".katex").length).toBeGreaterThanOrEqual(2);
    expect(root.querySelector(".katex-display")).not.toBeNull();
  });

  it("leaves single-dollar text alone", () => {
    const root = mount("<p>costs $5 and $10 today</p>");
    expect(root.querySelector(".katex")).toBeNull();
    expect(root.textContent).toBe("costs $5 and $10 today");
  });

  it("keeps MathML that has no TeX source, dropping a broken visual half", () => {
    const root = mount(
      '<span class="katex"><span class="katex-html"><span class="mord">x</span></span>' +
        "<math><mrow><mi>x</mi><mo>=</mo><mn>1</mn></mrow></math></span>",
    );
    expect(root.querySelector("math")).not.toBeNull();
    expect(root.querySelector(".katex-html")).toBeNull();
  });

  it("ignores non-formula text with backslashes", () => {
    const root = mount("<p>C:\\Users\\me and a \\backslash</p>");
    expect(root.querySelector(".katex")).toBeNull();
  });

  it("is idempotent", () => {
    const root = mount(DISPLAY);
    const first = root.innerHTML;
    renderMath(root);
    expect(root.innerHTML).toBe(first);
  });
});

describe("KaTeX build sanity", () => {
  // Guards the rendering options the module relies on: `output: "html"` must
  // still produce a `.katex` wrapper with a `.katex-html` half.
  it("renders with output: html", () => {
    const holder = document.createElement("span");
    katex.render("x^2", holder, {
      output: "html",
      throwOnError: false,
      strict: "ignore",
      trust: false,
    });
    expect(holder.querySelector(".katex-html")).not.toBeNull();
  });
});
