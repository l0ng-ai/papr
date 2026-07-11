// Unit tests for the pure platform-detection helper. `detectMobile` takes the
// user-agent (and touch-point count) as arguments, so it is exercisable in a
// plain node environment without a live `navigator`.

import { describe, it, expect } from "vitest";
import { detectMobile } from "./platform";

const IPHONE =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
const IPAD_LEGACY =
  "Mozilla/5.0 (iPad; CPU OS 12_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/12.0 Mobile/15E148 Safari/604.1";
// iPadOS 13+ reports a desktop "Macintosh" UA, told apart only by touch points.
const IPADOS_DESKTOP_UA =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";
const MAC =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";
const WINDOWS =
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

describe("detectMobile", () => {
  it("detects an iPhone", () => {
    expect(detectMobile(IPHONE, 5)).toBe(true);
  });

  it("detects a legacy iPad UA", () => {
    expect(detectMobile(IPAD_LEGACY, 5)).toBe(true);
  });

  it("detects an iPadOS device masquerading as Macintosh via touch points", () => {
    expect(detectMobile(IPADOS_DESKTOP_UA, 5)).toBe(true);
  });

  it("does not treat a real Mac (no touch points) as mobile", () => {
    expect(detectMobile(MAC, 0)).toBe(false);
  });

  it("does not treat Windows as mobile", () => {
    expect(detectMobile(WINDOWS, 0)).toBe(false);
  });
});
