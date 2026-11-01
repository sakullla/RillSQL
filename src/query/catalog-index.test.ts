import { describe, expect, it } from "vitest";
import { catalogFixture } from "../catalog/fixtures";
import { catalogIndex, expandStar, resolveTable, sourcesFor } from "./catalog-index";

const atlas = catalogFixture()[0]!;

describe("catalog index", () => {
  it("lists databases, schemas and objects once per connection object", () => {
    const index = catalogIndex(atlas);
    expect(index.databases).toEqual(["atlas_dev", "atlas_audit"]);
    expect(index.schemas.get("atlas_dev")).toEqual(expect.arrayContaining(["public", "analytics"]));
    expect(index.byName.get("orders")?.length).toBeGreaterThan(0);
    expect(catalogIndex(atlas)).toBe(index);
    expect(catalogIndex({ ...atlas })).not.toBe(index);
  });

  it("resolves qualified and bare references without guessing ambiguity", () => {
    const index = catalogIndex(atlas);
    expect(resolveTable(index, { alias: "o", schema: "public", name: "orders" })?.name).toBe("orders");
    const unique = [...index.byName.values()].find((list) => list.length === 1);
    expect(resolveTable(index, { alias: "u", schema: null, name: unique![0]!.name })).toBe(unique![0]);
    expect(resolveTable(index, { alias: "x", schema: null, name: "missing" })).toBeNull();
  });

  it("maps FROM tables to sources and expands star in order", () => {
    const index = catalogIndex(atlas);
    const sources = sourcesFor(index, [{ alias: "o", schema: "public", name: "orders" }], () => ["id", "customer_id"]);
    expect(sources).toEqual([{ alias: "o", label: expect.stringContaining("orders"), columns: ["id", "customer_id"] }]);
    expect(expandStar([...sources, { alias: "c", label: "public.customers", columns: ["id", "name"] }])).toEqual(["id", "customer_id", "name"]);
  });

  it("uses the database name as qualifier when the engine has no schema", () => {
    const mysql = {
      ...atlas,
      engine: "mysql" as const,
      databases: [
        { name: "shop", error: null, schemas: [{ name: "", objects: [{ name: "orders", kind: "table" as const, estimatedRows: null, stale: false }], total: 1, error: null }] },
        { name: "archive", error: null, schemas: [{ name: "", objects: [{ name: "orders", kind: "table" as const, estimatedRows: null, stale: false }], total: 1, error: null }] },
      ],
    };
    const index = catalogIndex(mysql);
    expect(index.byQualified.has("shop.orders")).toBe(true);
    expect(resolveTable(index, { alias: "o", schema: "shop", name: "orders" })?.schema).toBe("shop");
    expect(resolveTable(index, { alias: "o", schema: null, name: "orders" }, "archive")?.schema).toBe("archive");
    expect(resolveTable(index, { alias: "o", schema: null, name: "orders" })).toBeNull();
  });
});
