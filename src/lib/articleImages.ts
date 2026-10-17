import type { ArticleDetail } from "../types";
/** Exact, normalized URLs already supplied by the article. AI output cannot
 * add external image URLs to this set. Relative feed URLs use the article URL. */
export function articleImageUrls(article: Pick<ArticleDetail, "contentHtml" | "extractedHtml" | "imageUrl" | "url">): Set<string> {
  const urls = new Set<string>();
  const add = (src: string) => {
    try { const u = new URL(src, article.url || undefined); if (["https:","http:"].includes(u.protocol) && !u.username && !u.password) urls.add(u.href); } catch { /* malformed source */ }
  };
  const doc = new DOMParser().parseFromString(article.extractedHtml || article.contentHtml || "", "text/html");
  doc.querySelectorAll("img[src]").forEach((img) => add(img.getAttribute("src") || ""));
  if (article.imageUrl) add(article.imageUrl);
  return urls;
}
