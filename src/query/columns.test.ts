import { afterEach, describe, expect, it, vi } from "vitest";
import type { CompletionObject } from "./complete";
import { cachedColumns, clearColumnCache, columnCacheSize, loadFields } from "./columns";

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
});
