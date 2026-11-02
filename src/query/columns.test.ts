import { afterEach, describe, expect, it, vi } from "vitest";
import type { CompletionObject } from "./complete";
import { cachedColumns, clearColumnCache, columnCacheSize, invalidateColumns, loadFields, loadForeignKeys } from "./columns";
import { catalogFixture } from "../catalog/fixtures";
import { replaceCatalog, resetCatalog } from "../catalog/store";

const api = vi.hoisted(() => ({ gridDescribe: vi.fn() }));
vi.mock("../grid/api", () => api);

function object(name: string, schema = "public"): CompletionObject {
  return { label: `${schema}.${name}`, name, schema, kind: "table" };
}

function columnsFor(name: string) {
  return { columns: [{ name: `${name}_id` }, { name: "created_at" }] };
}

describe("column cache", () => {
  afterEach(() => {
    clearColumnCache();
    resetCatalog();
    api.gridDescribe.mockReset();
  });

  it("shares one describe between concurrent requests for the same table", async () => {
    let release!: (value: unknown) => void;
    api.gridDescribe.mockImplementation(() => new Promise((resolve) => { release = resolve; }));
    const first = loadFields({ connectionId: "db", database: "app", engine: "postgres", object: object("orders") });
    const second = loadFields({ connectionId: "db", database: "app", engine: "postgres", object: object("orders") });
    expect(api.gridDescribe).toHaveBeenCalledTimes(1);
    release(columnsFor("orders"));
    const [a, b] = await Promise.all([first, second]);
    expect(a.columns).toEqual(["orders_id", "created_at"]);
    expect(b).toBe(a);
  });

  it("keeps at most 200 tables and evicts the least recently used", async () => {
    api.gridDescribe.mockImplementation((request: { table: string }) => Promise.resolve(columnsFor(request.table)));
    for (let index = 0; index < 200; index += 1) {
      await loadFields({ connectionId: "db", database: "app", engine: "postgres", object: object(`t${index}`) });
    }
    await loadFields({ connectionId: "db", database: "app", engine: "postgres", object: object("t0") });
    await loadFields({ connectionId: "db", database: "app", engine: "postgres", object: object("t200") });
    expect(columnCacheSize()).toBe(200);
    const calls = api.gridDescribe.mock.calls.length;
    await loadFields({ connectionId: "db", database: "app", engine: "postgres", object: object("t1") });
    expect(api.gridDescribe.mock.calls.length).toBe(calls + 1);
    await loadFields({ connectionId: "db", database: "app", engine: "postgres", object: object("t0") });
    expect(api.gridDescribe.mock.calls.length).toBe(calls + 1);
  });

  it("joins an in-flight request even when the caller asks for a refresh", async () => {
    let release!: (value: unknown) => void;
    api.gridDescribe.mockImplementation(() => new Promise((resolve) => { release = resolve; }));
    const first = loadFields({ connectionId: "db", database: "app", engine: "postgres", object: object("orders") });
    const refreshed = loadFields({ connectionId: "db", database: "app", engine: "postgres", object: object("orders"), refresh: true });
    expect(api.gridDescribe).toHaveBeenCalledTimes(1);
    release(columnsFor("orders"));
    await Promise.all([first, refreshed]);
    expect(api.gridDescribe).toHaveBeenCalledTimes(1);
  });

  it("invalidates a connection prefix without touching other connections", async () => {
    api.gridDescribe.mockImplementation((request: { table: string }) => Promise.resolve(columnsFor(request.table)));
    await loadFields({ connectionId: "db", database: "app", engine: "postgres", object: object("orders") });
    await loadFields({ connectionId: "db2", database: "app", engine: "postgres", object: object("orders") });
    invalidateColumns("db\u0000");
    expect(columnCacheSize()).toBe(1);
    expect(cachedColumns("db", "app")).toEqual([]);
    expect(cachedColumns("db2", "app").length).toBe(2);
  });

  it("keeps a failure for the next lookup and flattens only successful tables", async () => {
    api.gridDescribe.mockImplementation((request: { table: string }) => (request.table === "broken" ? Promise.reject(new Error("denied")) : Promise.resolve(columnsFor(request.table))));
    await loadFields({ connectionId: "db", database: "app", engine: "postgres", object: object("orders") });
    const broken = await loadFields({ connectionId: "db", database: "app", engine: "postgres", object: object("broken") });
    expect(broken.error).toContain("denied");
    const calls = api.gridDescribe.mock.calls.length;
    await loadFields({ connectionId: "db", database: "app", engine: "postgres", object: object("broken") });
    expect(api.gridDescribe.mock.calls.length).toBe(calls);
    expect(cachedColumns("db", "app").map((item) => item.label)).toEqual(["orders_id", "created_at"]);
  });

  it("shares and caches foreign keys, including unavailable metadata", async () => {
    let release!: (value: unknown) => void;
    api.gridDescribe.mockImplementation(() => new Promise((resolve) => { release = resolve; }));
    const input = { connectionId: "db", database: "app", engine: "postgres" as const, object: object("orders") };
    const first = loadForeignKeys(input);
    const second = loadForeignKeys(input);
    expect(api.gridDescribe).toHaveBeenCalledTimes(1);
    expect(api.gridDescribe.mock.calls[0]![1]).toEqual({ includeForeignKeys: true });
    const keys = [{ name: "fk_customer", columns: ["customer_id"], refTable: "customers", refSchema: "public", refColumns: ["id"] }];
    release({ ...columnsFor("orders"), foreignKeys: keys });
    expect(await first).toEqual(keys);
    expect(await second).toBe(await loadForeignKeys(input));
    expect(api.gridDescribe).toHaveBeenCalledTimes(1);
    invalidateColumns("db\u0000");
    api.gridDescribe.mockRejectedValue(new Error("denied"));
    expect(await loadForeignKeys(input)).toEqual([]);
    expect(await loadForeignKeys(input)).toEqual([]);
    expect(api.gridDescribe).toHaveBeenCalledTimes(2);
  });

  it("bounds the foreign-key LRU cache and refreshes evicted tables", async () => {
    api.gridDescribe.mockResolvedValue({ foreignKeys: [] });
    const input = { connectionId: "db", database: "app", engine: "postgres" as const };
    for (let index = 0; index < 200; index += 1) await loadForeignKeys({ ...input, object: object(`t${index}`) });
    await loadForeignKeys({ ...input, object: object("t0") });
    await loadForeignKeys({ ...input, object: object("t200") });
    await loadForeignKeys({ ...input, object: object("t0") });
    expect(api.gridDescribe).toHaveBeenCalledTimes(201);
    await loadForeignKeys({ ...input, object: object("t1") });
    expect(api.gridDescribe).toHaveBeenCalledTimes(202);
  });

  it.each(["clear", "prefix"])("does not repopulate either cache after %s invalidation", async (mode) => {
    const releases: ((value: unknown) => void)[] = [];
    api.gridDescribe.mockImplementation(() => new Promise((resolve) => { releases.push(resolve); }));
    const input = { connectionId: "db", database: "app", engine: "postgres" as const, object: object("orders") };
    const oldFields = loadFields(input);
    const oldKeys = loadForeignKeys(input);
    if (mode === "clear") clearColumnCache();
    else invalidateColumns("db\u0000");
    const newFields = loadFields(input);
    const newKeys = loadForeignKeys(input);
    expect(api.gridDescribe).toHaveBeenCalledTimes(4);
    releases[2]!({ columns: [{ name: "fresh" }] });
    releases[3]!({ foreignKeys: [{ name: "fresh" }] });
    await Promise.all([newFields, newKeys]);
    releases[0]!(columnsFor("stale"));
    releases[1]!({ foreignKeys: [{ name: "stale" }] });
    await Promise.all([oldFields, oldKeys]);
    expect((await loadFields(input)).columns).toEqual(["fresh"]);
    expect((await loadForeignKeys(input))[0]!.name).toBe("fresh");
    expect(api.gridDescribe).toHaveBeenCalledTimes(4);
  });

  it("invalidates completion metadata when the catalog refreshes or disconnects", async () => {
    const catalog = catalogFixture();
    replaceCatalog(catalog);
    api.gridDescribe.mockResolvedValue({ ...columnsFor("orders"), foreignKeys: [] });
    const input = { connectionId: catalog[0]!.id, database: "app", engine: "postgres" as const, object: object("orders") };
    await Promise.all([loadFields(input), loadForeignKeys(input)]);
    replaceCatalog([...catalog]);
    await Promise.all([loadFields(input), loadForeignKeys(input)]);
    expect(api.gridDescribe).toHaveBeenCalledTimes(2);
    replaceCatalog(catalogFixture());
    await Promise.all([loadFields(input), loadForeignKeys(input)]);
    expect(api.gridDescribe).toHaveBeenCalledTimes(4);
    resetCatalog();
    expect(columnCacheSize()).toBe(0);
  });
});
