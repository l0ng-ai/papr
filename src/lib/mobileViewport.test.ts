import { describe, expect, it } from "vitest";
import { mobileViewportHeight } from "./mobileViewport";

describe("iPhone application height", () => {
  it("keeps the full 14 Pro window when WebKit subtracts safe areas", () => {
    expect(mobileViewportHeight(852,759,false)).toBe(852);
    expect(mobileViewportHeight(852,759,true)).toBe(852);
  });
  it("resizes for the keyboard and restores the full window after dismissal", () => {
    expect(mobileViewportHeight(852,490,true)).toBe(490);
    expect(mobileViewportHeight(852,852,true)).toBe(852);
    expect(mobileViewportHeight(852,490,false)).toBe(852);
  });
  it("handles rotation, absent viewport APIs and pinch zoom", () => {
    expect(mobileViewportHeight(393,393,false)).toBe(393);
    expect(mobileViewportHeight(852,undefined,true)).toBe(852);
    expect(mobileViewportHeight(852,0,true)).toBe(852);
    expect(mobileViewportHeight(852,426,true,2)).toBe(852);
  });
});
