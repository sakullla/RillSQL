import { describe, expect, it } from "vitest";
import { MODULE_ID } from "./index";

describe("query placeholder", () => {
  it("exposes the query module entry", () => {
    expect(MODULE_ID).toBe("query");
  });
});
