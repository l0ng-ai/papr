// Unit tests for the pure `isUrlOnlySnippet` heuristic. Exercisable in a plain
// node environment — the helper takes the snippet text as its only argument.

import { describe, it, expect } from "vitest";
import { isUrlOnlySnippet } from "./snippetNoise";

describe("isUrlOnlySnippet", () => {
  it("flags the Hacker News two-URL boilerplate", () => {
    const hn =
      "Article URL: https://example.com/some-post Comments URL: https://news.ycombinator.com/item?id=42";
    expect(isUrlOnlySnippet(hn)).toBe(true);
  });

  it("flags a single labelled URL", () => {
    expect(isUrlOnlySnippet("Article URL: https://example.com/x")).toBe(true);
  });

  it("flags a bare URL with no label", () => {
    expect(isUrlOnlySnippet("https://example.com/a/very/long/path")).toBe(true);
  });

  it("flags a bare www URL", () => {
    expect(isUrlOnlySnippet("www.example.com/read-more")).toBe(true);
  });

  it("keeps a real sentence that merely contains a URL", () => {
    const s =
      "A deep dive into the new API design, with benchmarks at https://example.com/bench";
    expect(isUrlOnlySnippet(s)).toBe(false);
  });

  it("keeps a real sentence introduced by a URL label", () => {
    const s =
      "Source URL: the maintainers have shipped a rewrite that halves cold-start time";
    expect(isUrlOnlySnippet(s)).toBe(false);
  });

  it("keeps a normal prose snippet with no links at all", () => {
    expect(
      isUrlOnlySnippet("The quick brown fox jumps over the lazy dog."),
    ).toBe(false);
  });

  it("keeps short-but-real prose right at the threshold", () => {
    // "Read more at" -> 10 non-whitespace chars survive the URL strip -> kept.
    expect(isUrlOnlySnippet("Read more at https://example.com")).toBe(false);
  });

  it("treats empty / null / whitespace as not-noise (nothing to hide)", () => {
    expect(isUrlOnlySnippet("")).toBe(false);
    expect(isUrlOnlySnippet(null)).toBe(false);
    expect(isUrlOnlySnippet(undefined)).toBe(false);
    expect(isUrlOnlySnippet("   \n  ")).toBe(false);
  });
});
