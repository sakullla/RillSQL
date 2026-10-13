import { describe, expect, it } from "vitest";
import { listPaletteConnections, MODULE_ID } from "./index";

describe("connections placeholder", () => {
  it("keeps a compilable entry without inventing saved connections", () => {
    expect(MODULE_ID).toBe("connections");
    expect(listPaletteConnections()).toEqual([]);
  });
});
