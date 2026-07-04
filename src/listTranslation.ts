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
  sourceTitle: string;
  sourceSnippet: string | null;
  title: string | null;
  snippet: string | null;
  error?: string;
}

interface ListTranslationState {
  jobs: Record<string, ListTranslationJob>;
  enqueueVisible: (articles: ArticleSummary[], lang: string, engine: string) => void;
}

const MAX_CONCURRENT = 3;
// Cap the retained job map. The reader's body-translation store prunes via
// `clear` once its result is read back; this store has no single read-back
// point (every visible row reads its job on each render), so instead it evicts
// the oldest *settled* jobs when the map grows past this bound. Completed
// results are also persisted in the DB preview cache, so an evicted row simply
// re-resolves from that cache on its next pass rather than losing anything.
const MAX_JOBS = 400;
const keyFor = (articleId: number, lang: string, engine: string) =>
  `${articleId}:${lang}:${engine}`;

export const listTranslationKey = keyFor;

// Drop the oldest done/error jobs once the map exceeds MAX_JOBS; queued and
// in-flight jobs are never evicted (a running job must survive to record its
// result). Object key insertion order approximates least-recently-added.
const pruneJobs = (
  jobs: Record<string, ListTranslationJob>,
): Record<string, ListTranslationJob> => {
  const keys = Object.keys(jobs);
  const overflow = keys.length - MAX_JOBS;
  if (overflow <= 0) return jobs;
  const evictable = keys.filter((k) => {
    const s = jobs[k].status;
    return s === "done" || s === "error";
  });
  if (evictable.length === 0) return jobs;
  const drop = new Set(evictable.slice(0, Math.min(overflow, evictable.length)));
  const next: Record<string, ListTranslationJob> = {};
  for (const k of keys) if (!drop.has(k)) next[k] = jobs[k];
  return next;
};

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
            ...(matchesJobSource(s.jobs[nextKey], next)
              ? {
                  [nextKey]: {
                    ...s.jobs[nextKey],
                    status: "done",
                    title: result.title,
                    snippet: result.snippet,
                    lang: result.lang,
                    engine: result.engine,
                  },
                }
              : {}),
          },
        }));
      })
      .catch((err) => {
        const message = errorText(err);
        set((s) => ({
          jobs: {
            ...s.jobs,
            ...(matchesJobSource(s.jobs[nextKey], next)
              ? {
                  [nextKey]: {
                    ...s.jobs[nextKey],
                    status: "error",
                    error: message,
                  },
                }
              : {}),
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
        const sourceTitle = article.title;
        const sourceSnippet = article.snippet ?? null;
        const existing = current[key];
        const sourceChanged =
          existing?.sourceTitle !== sourceTitle ||
          existing?.sourceSnippet !== sourceSnippet;
        if (existing && existing.status !== "error" && !sourceChanged) continue;
        additions[key] = {
          ...(existing ?? {
            articleId: article.id,
            title: null,
            snippet: null,
          }),
          sourceTitle,
          sourceSnippet,
          status: "queued",
          lang: targetLang,
          engine,
          error: undefined,
        };
        changed = true;
      }

      if (!changed) return;
      set((s) => ({ jobs: pruneJobs({ ...s.jobs, ...additions }) }));
      runNext();
    },
  };
});

const matchesJobSource = (
  current: ListTranslationJob | undefined,
  expected: ListTranslationJob,
) =>
  current?.articleId === expected.articleId &&
  current.lang === expected.lang &&
  current.engine === expected.engine &&
  current.sourceTitle === expected.sourceTitle &&
  current.sourceSnippet === expected.sourceSnippet;
