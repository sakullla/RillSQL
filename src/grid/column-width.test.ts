import { describe, expect, it } from "vitest";
import { autofitWidth, clampColWidth, MAX_COL_WIDTH, MIN_COL_WIDTH } from "./column-width";

describe("column width helpers", () => {
  it("clamps widths into the 48-360 range", () => {
    expect(clampColWidth(10)).toBe(MIN_COL_WIDTH);
    expect(clampColWidth(10)).toBe(48);
    expect(clampColWidth(120)).toBe(120);
    expect(clampColWidth(9999)).toBe(MAX_COL_WIDTH);
    expect(clampColWidth(9999)).toBe(360);
    expect(clampColWidth(Number.NaN)).toBe(48);
    expect(clampColWidth(120.6)).toBe(121);
  });

  it("autofits to the longest sample within the clamp range", () => {
    const wide = autofitWidth(["ab", "hello world"]);
    const narrow = autofitWidth(["a", "b"]);
    expect(wide).toBeGreaterThanOrEqual(MIN_COL_WIDTH);
    expect(wide).toBeLessThanOrEqual(MAX_COL_WIDTH);
    expect(wide).toBeGreaterThan(narrow);
    expect(autofitWidth(["长".repeat(500)])).toBe(MAX_COL_WIDTH);
  });
});
