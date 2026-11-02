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

  it("keeps a late prefix match ahead of more than 1,000 fuzzy matches", () => {
    const items = [...Array.from({ length: 2_000 }, (_, index) => `x_name_${index}`), "name"];
    expect(rankBy(items, "name", (item) => item, 3)).toEqual(["name", "x_name_0", "x_name_1"]);
  });

  it("stops consuming candidates once the prefix quota is full", () => {
    let visited = 0;
    function* candidates() {
      for (let index = 0; index < 10_000; index += 1) {
        visited += 1;
        yield `name_${index}`;
      }
    }
    expect(rankBy(candidates(), "name", (item) => item, 40)).toHaveLength(40);
    expect(visited).toBe(40);
  });
});
