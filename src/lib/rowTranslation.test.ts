import { describe, expect, it } from "vitest";
import { resolveRowTranslation } from "./rowTranslation";
import type { ListTranslationJob } from "../listTranslation";

const source = { title: "Original title", snippet: "Original snippet" };

const job = (over: Partial<ListTranslationJob>): ListTranslationJob => ({
  status: "done",
  articleId: 1,
  lang: "zh",
  engine: "llm",
  sourceTitle: source.title,
  sourceSnippet: source.snippet,
  title: null,
  snippet: null,
  ...over,
});

describe("resolveRowTranslation", () => {
  it("shows the original untouched when the mode is off, even if a job is done", () => {
    const rt = resolveRowTranslation(
      source,
      job({ status: "done", title: "译标题", snippet: "译摘要" }),
      "off",
      "unknown",
    );
    expect(rt).toMatchObject({
      hasTranslation: false,
      isTranslating: false,
      error: null,
      title: "Original title",
      snippet: "Original snippet",
    });
  });

  it("replaces title and snippet with the translation once done in auto mode", () => {
    const rt = resolveRowTranslation(
      source,
      job({ status: "done", title: "译标题", snippet: "译摘要" }),
      "auto",
      "unknown",
    );
    expect(rt.hasTranslation).toBe(true);
    expect(rt.title).toBe("译标题");
    expect(rt.snippet).toBe("译摘要");
  });

  it("falls back to the original text for any empty translated field", () => {
    const rt = resolveRowTranslation(
      source,
      job({ status: "done", title: "", snippet: "译摘要" }),
      "auto",
      "unknown",
    );
    // Empty translated title → original title; snippet still translated.
    expect(rt.hasTranslation).toBe(true);
    expect(rt.title).toBe("Original title");
    expect(rt.snippet).toBe("译摘要");
  });

  it("reports queued and translating jobs as in-flight without altering text", () => {
    for (const status of ["queued", "translating"] as const) {
      const rt = resolveRowTranslation(source, job({ status }), "auto", "unknown");
      expect(rt.isTranslating).toBe(true);
      expect(rt.hasTranslation).toBe(false);
      expect(rt.title).toBe("Original title");
    }
  });

  it("surfaces the job error, using the fallback when none is attached", () => {
    expect(
      resolveRowTranslation(source, job({ status: "error", error: "boom" }), "auto", "unknown")
        .error,
    ).toBe("boom");
    expect(
      resolveRowTranslation(source, job({ status: "error" }), "auto", "unknown").error,
    ).toBe("unknown");
  });

  it("treats a missing job as plain original text", () => {
    const rt = resolveRowTranslation(source, undefined, "auto", "unknown");
    expect(rt).toMatchObject({
      hasTranslation: false,
      isTranslating: false,
      error: null,
      title: "Original title",
      snippet: "Original snippet",
    });
  });
});
