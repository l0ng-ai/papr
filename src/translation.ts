// Background article-translation jobs. A translation runs to completion
// independently of which article is on screen, so several can be in flight at
// once and switching articles never interrupts them. The reader subscribes to
// the job for the article it is showing; progress arrives per batch (not per
// token) so the webview's main thread stays responsive on a long article.

import { create } from "zustand";
import * as api from "./api";
import { reportError } from "./toast";

export type JobStatus = "translating" | "done" | "error";

export interface TranslationJob {
  status: JobStatus;
  /** Batches completed so far. */
  done: number;
  /** Total batches the body was split into (0 until the first event arrives). */
  total: number;
  /** Accumulated translated HTML, grown as batches arrive. */
  html: string;
  /** The target language code this job was started for. */
  lang: string;
}

interface TranslationState {
  jobs: Record<number, TranslationJob>;
  /** Start a background translation for an article into `lang`. A no-op if one
   *  is already running; a finished/errored job is replaced (e.g. to retry or
   *  translate into a newly chosen language). */
  translate: (articleId: number, lang: string) => void;
  /** Drop a job (e.g. after its cached result has been read back from the DB). */
  clear: (articleId: number) => void;
}

export const useTranslationJobs = create<TranslationState>((set, get) => {
  const patch = (articleId: number, fn: (j: TranslationJob) => TranslationJob) =>
    set((s) => {
      const cur = s.jobs[articleId];
      if (!cur) return s;
      return { jobs: { ...s.jobs, [articleId]: fn(cur) } };
    });

  return {
    jobs: {},

    translate: (articleId, lang) => {
      if (get().jobs[articleId]?.status === "translating") return;
      set((s) => ({
        jobs: {
          ...s.jobs,
          [articleId]: { status: "translating", done: 0, total: 0, html: "", lang },
        },
      }));

      api
        .aiTranslate(articleId, (e) => {
          if (e.type === "start") {
            patch(articleId, (j) => ({ ...j, total: e.data.total }));
          } else if (e.type === "batch") {
            patch(articleId, (j) => ({
              ...j,
              done: e.data.done,
              html: j.html + e.data.html,
            }));
          } else if (e.type === "done") {
            patch(articleId, (j) => ({ ...j, html: e.data.html, status: "done" }));
          }
        })
        .then(() => {
          // The `done` event normally flips the status; guard against it being
          // missed so a finished job never sticks on "translating".
          patch(articleId, (j) =>
            j.status === "translating" ? { ...j, status: "done" } : j,
          );
        })
        .catch((err) => {
          patch(articleId, (j) => ({ ...j, status: "error" }));
          reportError(err);
        });
    },

    clear: (articleId) =>
      set((s) => {
        if (!(articleId in s.jobs)) return s;
        const rest = { ...s.jobs };
        delete rest[articleId];
        return { jobs: rest };
      }),
  };
});
