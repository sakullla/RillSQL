import { describe, expect, it } from "vitest";
import { MODULE_ID } from "./index";

describe("structure placeholder", () => {
  it("exposes the structure module entry", () => {
    expect(MODULE_ID).toBe("structure");
  });
});
