// Unit tests for the shared viewport-clamping helper.
// `viewport.ts` is pure (viewport dimensions are injectable), so it is fully
// exercisable in node without a DOM.

import { describe, it, expect } from "vitest";
import { clampAxis, clampToViewport } from "./viewport";

describe("clampAxis", () => {
  it("leaves a coordinate that already fits untouched", () => {
    // Overlay 100 wide at x=200, viewport 1000, margin 8 — fits comfortably.
    expect(clampAxis(200, 100, 1000, 8)).toBe(200);
  });

  it("pulls back when the overlay overflows the far edge", () => {
    // x=950 + width 100 spills past viewport 1000 → pull back to 1000-100-8.
    expect(clampAxis(950, 100, 1000, 8)).toBe(892);
  });

  it("floors at the margin when the coordinate is negative", () => {
    expect(clampAxis(-50, 100, 1000, 8)).toBe(8);
  });

  it("floors at margin 0 when configured", () => {
    expect(clampAxis(-50, 100, 1000, 0)).toBe(0);
  });

  it("lets the floor win when the overlay is larger than the viewport", () => {
    // Overlay 1200 wide, viewport 1000 — pull-back target is 1000-1200-8=-208,
    // but the floor pins it to the margin so the top/left stays visible.
    expect(clampAxis(500, 1200, 1000, 8)).toBe(8);
  });
});

describe("clampToViewport", () => {
  it("places an overlay that fits without adjustment", () => {
    expect(
      clampToViewport({
        x: 100,
        y: 120,
        width: 200,
        height: 150,
        margin: 8,
        viewportWidth: 1000,
        viewportHeight: 800,
      }),
    ).toEqual({ left: 100, top: 120 });
  });

  it("pulls back from the right and bottom edges", () => {
    expect(
      clampToViewport({
        x: 980,
        y: 790,
        width: 200,
        height: 150,
        margin: 8,
        viewportWidth: 1000,
        viewportHeight: 800,
      }),
    ).toEqual({ left: 792, top: 642 });
  });

  it("floors at the top-left when the anchor is off-screen negative", () => {
    expect(
      clampToViewport({
        x: -100,
        y: -40,
        width: 200,
        height: 150,
        margin: 8,
        viewportWidth: 1000,
        viewportHeight: 800,
      }),
    ).toEqual({ left: 8, top: 8 });
  });

  it("pins an oversized overlay to the margin (floor beats pull-back)", () => {
    // Overlay both wider and taller than the viewport — without a floor the
    // pulled-back coordinate would be negative, clipping the first items.
    expect(
      clampToViewport({
        x: 50,
        y: 50,
        width: 1200,
        height: 900,
        margin: 8,
        viewportWidth: 1000,
        viewportHeight: 800,
      }),
    ).toEqual({ left: 8, top: 8 });
  });

  it("honours a zero margin (TagPicker-style call site)", () => {
    expect(
      clampToViewport({
        x: -30,
        y: 900,
        width: 248,
        height: 320,
        margin: 0,
        viewportWidth: 1000,
        viewportHeight: 800,
      }),
    ).toEqual({ left: 0, top: 480 });
  });

  it("defaults the margin to 8 when omitted", () => {
    expect(
      clampToViewport({
        x: -100,
        y: -100,
        width: 100,
        height: 100,
        viewportWidth: 1000,
        viewportHeight: 800,
      }),
    ).toEqual({ left: 8, top: 8 });
  });

  it("clamps each axis independently", () => {
    // Fits horizontally, overflows vertically.
    expect(
      clampToViewport({
        x: 100,
        y: 790,
        width: 200,
        height: 150,
        margin: 8,
        viewportWidth: 1000,
        viewportHeight: 800,
      }),
    ).toEqual({ left: 100, top: 642 });
  });
});
