import { describe, expect, it } from "vitest";
import { MODULE_ID } from "./index";

describe("grid placeholder", () => {
  it("exposes the grid module entry", () => {
    expect(MODULE_ID).toBe("grid");
  });
});
