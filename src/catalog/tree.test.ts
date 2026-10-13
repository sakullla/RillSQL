import { afterEach, describe, expect, it } from "vitest";
import { MODULE_ID } from "./index";
import { createRefreshFixture, catalogFixture } from "./fixtures";
import { listPaletteTables, refreshCatalog, replaceCatalog, resetCatalog, setRefreshSource } from "./store";
import { engineShape, objectPath, reconcileObjects, refreshSchema, schemaFailed, schemaIsEmpty, searchObjects } from "./tree";
import type { CatalogObject, SchemaNode } from "./types";

function row(name: string, stale = false): CatalogObject {
  return { name, kind: "table", estimatedRows: 1, stale };
}

afterEach(() => {
  resetCatalog();
});

describe("catalog tree", () => {
  it("keeps the module id and separates engine levels", () => {
    expect(MODULE_ID).toBe("catalog");
    expect(engineShape("postgres")).toEqual({ databases: true, schemas: true, foreignKeys: false });
    expect(engineShape("mysql")).toEqual({ databases: true, schemas: false, foreignKeys: false });
    expect(engineShape("mariadb")).toEqual(engineShape("mysql"));
    expect(engineShape("sqlite")).toEqual({ databases: false, schemas: false, foreignKeys: true });
  });

  it("puts connection, database and schema on the same-name path and skips closed connections", () => {
    const found = searchObjects(catalogFixture(), "customers");
    expect(found.hits.map((hit) => hit.path)).toEqual(["Atlas / atlas_dev / public", "Atlas / atlas_dev / analytics"]);
    expect(found.skipped).toEqual(["Analytics"]);
    expect(found.hits.some((hit) => hit.name === "events")).toBe(false);
    expect(objectPath("本地笔记", "main", "")).toBe("本地笔记 / main");
    expect(objectPath("Shop", "shop_staging", "")).toBe("Shop / shop_staging");
  });

  it("marks a missing object stale and drops it on the next refresh", () => {
    const first = reconcileObjects([row("orders"), row("categories")], [row("orders")]);
    expect(first.map((item) => [item.name, item.stale])).toEqual([
      ["orders", false],
      ["categories", true],
    ]);
    const second = reconcileObjects(first, [row("orders")]);
    expect(second.map((item) => item.name)).toEqual(["orders"]);
  });

  it("keeps the previous objects when a schema read fails", () => {
    const previous: SchemaNode = { name: "restricted", objects: [row("secret")], total: 1, error: null };
    const failed = refreshSchema(previous, { name: "restricted", objects: [], total: 0, error: "permission denied for schema restricted" });
    expect(schemaFailed(failed)).toBe(true);
    expect(schemaIsEmpty(failed)).toBe(false);
    expect(failed.objects.map((item) => item.name)).toEqual(["secret"]);
    const empty: SchemaNode = { name: "empty", objects: [], total: 0, error: null };
    expect(schemaIsEmpty(empty)).toBe(true);
    expect(schemaFailed(empty)).toBe(false);
  });

  it("omits disconnected and stale objects from the palette", () => {
    const fixture = createRefreshFixture();
    replaceCatalog(catalogFixture());
    expect(listPaletteTables().some((item) => item.label === "events")).toBe(false);
    const customers = listPaletteTables().filter((item) => item.label === "customers");
    expect(customers.map((item) => item.path)).toEqual(["Atlas / atlas_dev / public", "Atlas / atlas_dev / analytics"]);
    fixture.hide("atlas", "atlas_dev", "public", "categories");
    setRefreshSource(() => fixture.snapshot());
    refreshCatalog();
    expect(listPaletteTables().some((item) => item.label === "categories")).toBe(false);
    expect(listPaletteTables().some((item) => item.path === "Atlas / atlas_dev / public" && item.label === "orders")).toBe(true);
  });
});
