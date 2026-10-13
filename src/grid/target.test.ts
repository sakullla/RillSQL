import { describe, expect, it } from "vitest";
import type { CatalogConnection } from "../catalog/types";
import { resolveTableTarget } from "./target";

function connection(id: string, database: string, schema: string, table: string, engine: CatalogConnection["engine"] = "sqlite"): CatalogConnection {
  return {
    id,
    name: id,
    engine,
    env: "local",
    envLabel: "",
    status: "connected",
    defaultDatabase: database,
    foreignKeys: null,
    error: null,
    databases: [
      {
        name: database,
        error: null,
        schemas: [{ name: schema, error: null, total: 1, objects: [{ name: table, kind: "table", estimatedRows: null, stale: false }] }],
      },
    ],
  };
}

describe("table tab targets", () => {
  it("matches catalog prefixes, including empty schemas and colons in names", () => {
    const connections = [
      connection("a", "db", "", "notes"),
      connection("a:b", "shop:west", "public:prod", "orders:open", "postgres"),
    ];
    expect(resolveTableTarget("table:a:db::notes", connections)).toMatchObject({
      connectionId: "a",
      database: "db",
      schema: "",
      table: "notes",
      engine: "sqlite",
    });
    expect(resolveTableTarget("table:a:b:shop:west:public:prod:orders:open", connections)).toMatchObject({
      connectionId: "a:b",
      database: "shop:west",
      schema: "public:prod",
      table: "orders:open",
    });
    expect(resolveTableTarget("table:customers", connections)).toBeNull();
  });
});