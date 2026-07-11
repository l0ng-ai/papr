// Pure helper for spotting article-list snippets that carry no real prose —
// only link boilerplate. Kept free of any React/Tauri imports so it is
// unit-testable in a plain node environment (see snippetNoise.test.ts).

// How many non-whitespace characters of *real* text must survive the URL/label
// strip for a snippet to count as meaningful. Below this the snippet is judged
// pure link noise. ~10 clears "Read more at <url>" (10 chars of prose remain)
// while catching the bare "Article URL: … Comments URL: …" case (0 remain).
const MIN_MEANINGFUL_CHARS = 10;

/**
 * Is this list-row snippet nothing but link boilerplate?
 *
 * The canonical offender is Hacker News' firehose, whose RSS <description> is
 * literally `Article URL: https://… Comments URL: https://…`. Rendered verbatim
 * in a two-line row snippet it's pure noise, so on mobile we skip drawing it.
 *
 * Conservative by construction: we only ever *remove* URL tokens and the short
 * "<word> URL:" / "<word> Link:" labels that introduce them, then ask whether
 * any real text survived. A normal snippet that merely *contains* a URL keeps
 * well over `MIN_MEANINGFUL_CHARS` of leftover prose and is therefore kept —
 * we deliberately under-match rather than risk hiding a real snippet.
 */
export function isUrlOnlySnippet(text: string | null | undefined): boolean {
  if (!text || !text.trim()) return false;
  let rest = text;
  // 1. Drop whole URLs — http/https tokens and bare `www.host` forms. `\S+`
  //    consumes the URL up to the next space, including any trailing slash or
  //    query string, so nothing URL-shaped is left to count as prose.
  rest = rest.replace(/(?:https?:\/\/|www\.)\S+/gi, " ");
  // 2. Drop the short labels that introduce those URLs: "Article URL:",
  //    "Comments URL:", "Source Link:", or a bare "URL:" / "Link:". Anchored to
  //    a single optional leading word so we don't chew into ordinary sentences.
  rest = rest.replace(/\b(?:\w+\s+)?(?:URL|Link)\s*:/gi, " ");
  // 3. Whatever's left, minus whitespace, is the surviving prose.
  const meaningful = rest.replace(/\s+/g, "");
  return meaningful.length < MIN_MEANINGFUL_CHARS;
}
