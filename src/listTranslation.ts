// Viewport-driven list-row translation jobs. Unlike the reader's body
// translation store, these jobs translate only title/snippet previews and are
// scheduled as virtualized rows become visible.

import { create } from "zustand";
import * as api from "./api";
import { errorText } from "./lib/errors";
import { reportError } from "./toast";
import type { ArticlePreviewTranslation, ArticleSummary } from "./types";

export type ListTranslationStatus = "queued" | "translating" | "done" | "error";

export interface ListTranslationJob {
  status: ListTranslationStatus;
  articleId: number;
  lang: string;
  engine: string;
  title: string | null;
  snippet: string | null;
  error?: string;
}

interface ListTranslationState {
  jobs: Record<string, ListTranslationJob>;
  enqueueVisible: (articles: ArticleSummary[], lang: string, engine: string) => void;
}

const MAX_CONCURRENT = 3;
const keyFor = (articleId: number, lang: string, engine: string) =>
  `${articleId}:${lang}:${engine}`;

export const listTranslationKey = keyFor;

export const useListTranslation = create<ListTranslationState>((set, get) => {
  const runNext = () => {
    const state = get();
    const active = Object.values(state.jobs).filter((j) => j.status === "translating").length;
    if (active >= MAX_CONCURRENT) return;

    const nextKey = Object.keys(state.jobs).find((key) => state.jobs[key].status === "queued");
    if (!nextKey) return;

    const next = state.jobs[nextKey];
    set((s) => ({
      jobs: {
        ...s.jobs,
        [nextKey]: { ...next, status: "translating" },
      },
    }));

    api
      .translateArticlePreview(next.articleId, next.lang, next.engine)
      .then((result: ArticlePreviewTranslation) => {
        set((s) => ({
          jobs: {
            ...s.jobs,
            [nextKey]: {
              ...s.jobs[nextKey],
              status: "done",
              title: result.title,
              snippet: result.snippet,
              lang: result.lang,
              engine: result.engine,
            },
          },
        }));
      })
      .catch((err) => {
        const message = errorText(err);
        set((s) => ({
          jobs: {
            ...s.jobs,
            [nextKey]: { ...s.jobs[nextKey], status: "error", error: message },
          },
        }));
        reportError(err);
      })
      .finally(runNext);

    runNext();
  };

  return {
    jobs: {},

    enqueueVisible: (articles, lang, engine) => {
      const targetLang = lang.trim();
      if (!targetLang) return;

      let changed = false;
      const additions: Record<string, ListTranslationJob> = {};
      const current = get().jobs;
      for (const article of articles) {
        const key = keyFor(article.id, targetLang, engine);
        if (current[key]) continue;
        additions[key] = {
          status: "queued",
          articleId: article.id,
          lang: targetLang,
          engine,
          title: null,
          snippet: null,
        };
        changed = true;
      }

      if (!changed) return;
      set((s) => ({ jobs: { ...s.jobs, ...additions } }));
      runNext();
    },
  };
});
