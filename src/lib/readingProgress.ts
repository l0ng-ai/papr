/** Reading-progress bar fraction (0..1) for the reader's scroll container.
 *
 * The naive `scrollTop / (scrollHeight - clientHeight)` jerks *backward* while
 * reading: a long article's images (hero + body) carry no intrinsic size and
 * load lazily, so `scrollHeight` keeps growing as you scroll down. Each scroll
 * tick then divides by a bigger denominator, dropping the fraction even though
 * the reader only moved forward — the bar visibly slides left.
 *
 * Fix: only let the bar regress when the user is genuinely scrolling *up*
 * (`scrollTop` decreased). While scrolling down or holding still, clamp to the
 * furthest point reached so a growing `scrollHeight` can't push it backward.
 */
export function readingProgress(
  prev: number,
  scrollTop: number,
  lastScrollTop: number,
  scrollHeight: number,
  clientHeight: number,
): number {
  const max = scrollHeight - clientHeight;
  // Clamp to [0,1]: macOS overscroll makes scrollTop briefly negative, and a
  // negative fraction would mirror the bar via `scaleX(...)`.
  const raw = max > 0 ? Math.max(0, Math.min(1, scrollTop / max)) : 0;
  return scrollTop < lastScrollTop ? raw : Math.max(prev, raw);
}
