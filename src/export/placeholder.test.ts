import { describe, expect, it } from "vitest";
import { MODULE_ID } from "./index";

describe("export placeholder", () => {
  it("exposes the export module entry", () => {
    expect(MODULE_ID).toBe("export");
  });
});
