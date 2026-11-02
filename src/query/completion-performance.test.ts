import { expect, it } from "vitest";
import { catalogFixture } from "../catalog/fixtures";
import { completionPlan, metadataNote } from "./complete";

it("bounds completion candidates for a catalog with 10,000 tables", () => {
  const connection = catalogFixture()[0]!;
  connection.databases[0]!.schemas = [{
    name: "public", total: 10_000, error: null,
    objects: Array.from({ length: 10_000 }, (_, index) => ({
      name: `orders_${index}`, kind: "table" as const, estimatedRows: null, stale: false,
    })),
  }];
  const metadata = metadataNote(connection, connection.databases[0]!.name);
  const timings: number[] = [];
  for (let run = 0; run < 9; run += 1) {
    const start = performance.now();
    const plan = completionPlan({
      connection, database: connection.databases[0]!.name, prefix: "ord",
      context: { clause: "from" },
      metadata,
    });
    timings.push(performance.now() - start);
    expect(plan.items.length).toBeLessThanOrEqual(120);
    expect(plan.items[0]?.label).toBe("orders_0");
  }
  expect(timings.sort((a, b) => a - b)[4]).toBeLessThan(50);
});
