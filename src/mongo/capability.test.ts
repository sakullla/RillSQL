import { describe, expect, it } from "vitest";
import { ENGINES, engineMeta } from "../engines";
import { targetOptions } from "../query/complete";
import { listPaletteTables, replaceCatalog, resetCatalog } from "../catalog/store";
import type { CatalogConnection } from "../catalog/types";

function mongoConnection(): CatalogConnection {
  return {
    id: "docs",
    name: "文档库",
    engine: "mongodb",
    env: "none",
    envLabel: "无标签",
    envTone: "none",
    status: "connected",
    defaultDatabase: "rillsql",
    foreignKeys: null,
    error: null,
    // 目录形状复用 MySQL 先例：库 → 空 schema → 集合（table）对象，行估算即文档计数。
    databases: [
      {
        name: "rillsql",
        error: null,
        schemas: [{ name: "", error: null, total: 2, objects: [
          { name: "orders", kind: "table", estimatedRows: 25, stale: false },
          { name: "customers", kind: "table", estimatedRows: 3, stale: false },
        ] }],
      },
    ],
  };
}

function postgresConnection(): CatalogConnection {
  return {
    id: "shop",
    name: "书店",
    engine: "postgres",
    env: "none",
    envLabel: "无标签",
    envTone: "none",
    status: "connected",
    defaultDatabase: "bookstore",
    foreignKeys: null,
    error: null,
    databases: [{ name: "bookstore", error: null, schemas: [{ name: "public", objects: [{ name: "orders", kind: "table", estimatedRows: 1, stale: false }], total: 1, error: null }] }],
  };
}

describe("engine capability gating (ADR-2 / R12 / R14)", () => {
  it("keeps ENGINES at exactly seven entries with mongodb as the only document engine (R14)", () => {
    expect(ENGINES).toHaveLength(7);
    expect(ENGINES.map((item) => item.id)).toEqual(["postgres", "mysql", "mariadb", "sqlserver", "sqlite", "redis", "mongodb"]);
    expect(engineMeta("mongodb")).toMatchObject({ label: "MongoDB", port: "27017", relational: false, document: true, kv: false });
    expect(ENGINES.filter((item) => item.document).map((item) => item.id)).toEqual(["mongodb"]);
    // R14 排除项：选择器不再有第八项（ClickHouse/Oracle 等不在清单里）。
    const ids = ENGINES.map((item) => String(item.id));
    expect(ids.some((id) => id === "clickhouse" || id === "oracle")).toBe(false);
    expect(new Set(ids).size).toBe(7);
  });

  it("keeps the SQL executor target list free of document engines", () => {
    const options = targetOptions([mongoConnection(), postgresConnection()]);
    expect(options.map((option) => option.connectionId)).toEqual(["shop"]);
    expect(options[0]!.engine).toBe("postgres");
  });

  it("does not list mongo collections in the quick-open table palette", () => {
    resetCatalog();
    replaceCatalog([mongoConnection(), postgresConnection()]);
    const tables = listPaletteTables();
    expect(tables.some((item) => item.label === "orders" && item.path.startsWith("文档库"))).toBe(false);
    expect(tables.some((item) => item.label === "orders" && item.path.startsWith("书店"))).toBe(true);
    resetCatalog();
  });

  it("keeps the mysql-style tree shape for collections (databases without schemas)", async () => {
    const { engineShape } = await import("../catalog/tree");
    expect(engineShape("mongodb")).toEqual({ databases: true, schemas: false, foreignKeys: false });
  });
});
