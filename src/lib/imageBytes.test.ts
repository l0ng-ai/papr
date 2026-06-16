import { describe, expect, it } from "vitest";
import { imageBytes } from "./imageBytes";

describe("imageBytes", () => {
  it("keeps Uint8Array responses as bytes", () => {
    const bytes = new Uint8Array([0xff, 0xd8, 0xff]);
    expect(imageBytes(bytes)).toBe(bytes);
  });

  it("wraps ArrayBuffer responses", () => {
    const input = new Uint8Array([1, 2, 3]).buffer;
    expect(Array.from(imageBytes(input))).toEqual([1, 2, 3]);
  });

  it("converts number-array IPC responses without stringifying", () => {
    expect(Array.from(imageBytes([0xff, 0xd8, 0xff]))).toEqual([
      0xff, 0xd8, 0xff,
    ]);
  });
});
