import { describe, expect, it } from "vitest";
import { listPaletteTables, MODULE_ID } from "./index";

describe("catalog placeholder", () => {
  it("keeps a compilable entry without pretending the tree is empty data", () => {
    expect(MODULE_ID).toBe("catalog");
    expect(listPaletteTables()).toEqual([]);
  });
});
