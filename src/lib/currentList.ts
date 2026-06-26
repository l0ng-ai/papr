// Read the article list currently shown in the middle pane straight from the
// React Query cache — used by keyboard shortcuts and reader navigation so they
// always agree with what the user sees.

import type { QueryClient } from "@tanstack/react-query";
import { useUi } from "../store";
import type { ArticleSummary } from "../types";

export function readCurrentItems(qc: QueryClient): ArticleSummary[] {
  const { query, unreadOnly, sortOldest, listAnchor } = useUi.getState();
  const inf = qc.getQueryData([
    "articles",
    query,
    unreadOnly,
    sortOldest,
    listAnchor,
  ]) as { pages: ArticleSummary[][] } | undefined;
  return inf?.pages.flat() ?? [];
}
