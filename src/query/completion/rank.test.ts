import { describe, expect, it } from "vitest";
import { matchKind, rankBy } from "./rank";

describe("completion ranking", () => {
  it("prefers prefix matches over subsequence matches", () => {
    expect(matchKind("customer_id", "cus")).toBe(0);
    expect(matchKind("customer_id", "ctm")).toBe(1);
    expect(matchKind("customer_id", "xyz")).toBe(-1);
  });

  it("keeps the input order inside the same match class", () => {
    const ranked = rankBy(["b_name", "a_name", "name"], "name", (item) => item);
    expect(ranked).toEqual(["name", "b_name", "a_name"]);
  });
});
