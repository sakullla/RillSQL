import { describe, expect, it } from "vitest";
import { parseScope } from "./scope";

describe("statement scope", () => {
  it("reads the clause, aliases and bare table references", () => {
    const scope = parseScope("SELECT o.id FROM public.orders AS o JOIN customers c ON c.id = o.customer_id WHERE ");
    expect(scope.clause).toBe("where");
    expect(scope.tables.map((table) => table.alias)).toEqual(["o", "c"]);
    expect(scope.tables[0]).toMatchObject({ schema: "public", name: "orders" });
  });

  it("stops at ON so the join condition keeps its own context", () => {
    expect(parseScope("SELECT * FROM orders o JOIN customers c ON ").clause).toBe("on");
  });

  it("collects CTE names", () => {
    const scope = parseScope("WITH recent AS (SELECT 1), latest AS (SELECT 2) SELECT * FROM recent r ");
    expect(scope.ctes).toEqual(["recent", "latest"]);
    expect(scope.tables.map((table) => table.alias)).toContain("r");
  });

  it("hides aliases declared inside a closed subquery", () => {
    const scope = parseScope("SELECT * FROM (SELECT * FROM inner_t i) sub WHERE ");
    expect(scope.tables.map((table) => table.alias)).toEqual(["sub"]);
  });

  it("keeps outer aliases visible inside a subquery", () => {
    const scope = parseScope("SELECT * FROM orders o WHERE EXISTS (SELECT 1 FROM lines l WHERE l.order_id = o.id AND ");
    expect(scope.tables.map((table) => table.alias)).toEqual(["o", "l"]);
  });
});
