import { describe, expect, it } from "vitest";
import { MODULE_ID } from "./index";

describe("history placeholder", () => {
  it("exposes the history module entry", () => {
    expect(MODULE_ID).toBe("history");
  });
});
