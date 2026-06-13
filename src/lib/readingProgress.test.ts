import { describe, expect, it } from "vitest";
import { readingProgress } from "./readingProgress";

const CLIENT = 500;

describe("readingProgress", () => {
  it("does not regress when a lazy image grows scrollHeight mid-scroll", () => {
    // tick 1: scrolled down a bit, body not fully loaded yet.
    let p = readingProgress(0, 100, 0, 1000, CLIENT); // max 500 → 0.20
    expect(p).toBeCloseTo(0.2);

    // tick 2: scrolled further down, but an image below just loaded and
    // doubled scrollHeight. Naive math would give 200/1500 = 0.133 — a
    // backward jump. The clamp holds the bar at its furthest point.
    p = readingProgress(p, 200, 100, 2000, CLIENT);
    expect(p).toBeCloseTo(0.2); // held, not 0.133
    // Guard against silently reintroducing the bug.
    expect(p).toBeGreaterThan(200 / (2000 - CLIENT));
  });

  it("advances normally when content height is stable", () => {
    let p = readingProgress(0, 100, 0, 1000, CLIENT); // 0.20
    p = readingProgress(p, 250, 100, 1000, CLIENT); // 0.50
    expect(p).toBeCloseTo(0.5);
  });

  it("regresses when the user actually scrolls up", () => {
    let p = readingProgress(0, 400, 0, 1000, CLIENT); // 0.80
    p = readingProgress(p, 150, 400, 1000, CLIENT); // scrolling up → 0.30
    expect(p).toBeCloseTo(0.3);
  });

  it("clamps to 1 at the foot and 0 with nothing to scroll", () => {
    expect(readingProgress(0, 600, 0, 1100, CLIENT)).toBe(1); // 600/600, capped
    expect(readingProgress(0, 0, 0, 400, CLIENT)).toBe(0); // content < viewport
  });

  it("never goes negative on overscroll (scrollTop < 0)", () => {
    // macOS rubber-band at the top: scrollTop dips negative while scrolling up.
    expect(readingProgress(0.1, -30, 0, 1000, CLIENT)).toBe(0);
  });
});
