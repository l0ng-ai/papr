// Pure view-model for a single article-list row's translation state. Kept free
// of any Tauri/API imports (type-only imports below are erased at build time) so
// the row-display branching is unit-testable in a plain node environment and the
// row JSX in ArticleList can stay declarative.

import type { ArticleSummary } from "../types";
import type { ListTranslationJob } from "../listTranslation";

export interface RowTranslation {
  /** A translated title/snippet is being shown, so the row can offer the
   *  original text as a hover `title`. */
  hasTranslation: boolean;
  /** The row's translation is queued or in flight. */
  isTranslating: boolean;
  /** Resolved error message to surface on the row, or null. */
  error: string | null;
  /** The title to render (translated when available, else the original). */
  title: string;
  /** The snippet to render (translated when available, else the original). */
  snippet: string | null;
}

/**
 * Decide what one list row should display given its (maybe-absent) translation
 * job and the current list-translation mode. In "off" mode the original title
 * and snippet always win; in "auto" mode a completed job's text replaces them
 * and queued/in-flight/error states are surfaced.
 */
export const resolveRowTranslation = (
  source: Pick<ArticleSummary, "title" | "snippet">,
  job: ListTranslationJob | undefined,
  mode: "off" | "auto",
  unknownError: string,
): RowTranslation => {
  const show = mode === "auto";
  const translated =
    job?.status === "done" && (!!job.title || !!job.snippet);
  const hasTranslation = show && translated;
  return {
    hasTranslation,
    isTranslating:
      show && (job?.status === "queued" || job?.status === "translating"),
    error: show && job?.status === "error" ? job.error || unknownError : null,
    title: hasTranslation ? job!.title || source.title : source.title,
    snippet: hasTranslation ? job!.snippet || source.snippet : source.snippet,
  };
};
