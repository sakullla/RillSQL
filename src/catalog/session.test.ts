import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { auditTables, catalogFixture, pagedCatalog } from "./fixtures";
import {
  catalogConnections,
  catalogLoadError,
  limitsForRefresh,
  refreshFromBackend,
  refreshLimit,
  replaceCatalog,
  resetCatalog,
  revealMore,
  setObjectPage,
  startCatalog,
} from "./store";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke,
}));

afterEach(() => {
  resetCatalog();
  invoke.mockReset();
  vi.useRealTimers();
});

beforeEach(() => {
  setObjectPage(100);
});

function saved(id: string, engine: "postgres" | "mysql" | "sqlite", database: string | null) {
  return {
    id,
    name: id,
    engine,
    env: "dev",
    host: "127.0.0.1",
    port: 5432,
    database,
    username: "rillsql",
    filePath: null,
    tls: null,
    ssh: null,
    passwordSaved: true,
    sshPasswordSaved: false,
    sshHostKey: null,
  };
}

function outline(id: string, engine: "postgres" | "mysql" | "sqlite", databases: { name: string; schemas: string[] }[]) {
  return {
    id,
    name: id,
    engine,
    env: "dev",
    envLabel: "开发",
    envTone: "dev",
    status: "connected",
    defaultDatabase: databases[0]?.name ?? null,
    foreignKeys: engine === "sqlite" ? false : null,
    error: null,
    databases: databases.map((database) => ({
      name: database.name,
      error: null,
      schemas: database.schemas.map((name) => ({ name, objects: [], total: 0, error: null })),
    })),
  };
}

describe("catalog session reads", () => {
  it("asks once per schema with the loaded window and appends the next page", async () => {
    expect(refreshLimit(120, 100)).toBe(120);
    expect(refreshLimit(3, 100)).toBe(100);
    replaceCatalog(catalogFixture());
    const archive = limitsForRefresh(catalogConnections(), 100).find((item) => item.schema === "archive");
    expect(archive?.limit).toBe(120);

    invoke.mockImplementation(async (command: string, args?: { schema?: string; connectionId?: string; limit?: number }) => {
      if (command === "catalog_sessions") return ["atlas"];
      if (command === "connections_list") return [saved("atlas", "postgres", "atlas_dev")];
      if (command === "env_tags_list") return [];
      if (command === "catalog_refresh") return [outline("atlas", "postgres", [{ name: "atlas_dev", schemas: ["public", "archive"] }])];
      if (command === "catalog_page" && args?.schema === "archive") return { objects: auditTables(0, 120), total: 120, error: null };
      return { objects: [{ name: "customers", kind: "table", estimatedRows: 1, stale: false }], total: 1, error: null };
    });
    await refreshFromBackend();
    const pages = invoke.mock.calls.filter((call) => call[0] === "catalog_page");
    expect(pages.map((call) => call[1].schema)).toEqual(["public", "archive"]);
    expect(pages.find((call) => call[1].schema === "archive")?.[1].limit).toBe(120);
    expect(invoke.mock.calls.find((call) => call[0] === "catalog_refresh")?.[1].outline).toBe(true);

    resetCatalog();
    setObjectPage(100);
    replaceCatalog(pagedCatalog());
    invoke.mockResolvedValue({ objects: auditTables(100, 20), total: 120, error: null });
    revealMore("atlas", "atlas_dev", "archive");
    await vi.waitFor(() => {
      expect(catalogConnections()[0].databases[0].schemas.find((schema) => schema.name === "archive")?.objects.length).toBe(120);
    });
    expect(invoke).toHaveBeenCalledWith(
      "catalog_page",
      expect.objectContaining({ connectionId: "atlas", database: "atlas_dev", schema: "archive", offset: 100, limit: 100 }),
    );
  });

  it("pages past retained rows from a failed kind without skipping successful names", async () => {
    const base = catalogFixture();
    const schema = base[0]!.databases[0]!.schemas.find((item) => item.name === "archive")!;
    const functions = Array.from({ length: 15 }, (_, index) => ({
      name: `fn_${index}`,
      kind: "function" as const,
      estimatedRows: null,
      stale: false,
    }));
    schema.objects = [
      ...auditTables(0, 90),
      { name: "audit_old", kind: "table", estimatedRows: 1, stale: true },
      ...functions,
    ];
    schema.total = 100;
    schema.kindReports = [
      { kind: "table", total: 100, error: null },
      { kind: "function", total: 0, error: "permission denied" },
    ];
    replaceCatalog(base);
    invoke.mockResolvedValue({
      objects: auditTables(90, 10),
      total: 100,
      error: null,
      kindReports: schema.kindReports,
    });

    revealMore("atlas", "atlas_dev", "archive");
    expect(invoke).not.toHaveBeenCalled();
    revealMore("atlas", "atlas_dev", "archive");
    await vi.waitFor(() => {
      const loaded = catalogConnections()[0]!.databases[0]!.schemas.find((item) => item.name === "archive");
      expect(loaded?.objects.filter((item) => item.kind === "table" && !item.stale)).toHaveLength(100);
    });
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith(
      "catalog_page",
      expect.objectContaining({ connectionId: "atlas", database: "atlas_dev", schema: "archive", offset: 90, limit: 100 }),
    );
    const loaded = catalogConnections()[0]!.databases[0]!.schemas.find((item) => item.name === "archive")!;
    const names = loaded.objects.filter((item) => item.kind === "table" && !item.stale).map((item) => item.name);
    expect(names).toContain("audit_0089");
    expect(names).toContain("audit_0090");
    expect(new Set(names).size).toBe(100);
    expect(loaded.objects.filter((item) => item.kind === "function")).toHaveLength(15);
  });

  it("keeps objects when a read fails and does not present that as an empty schema", async () => {
    replaceCatalog(catalogFixture());
    invoke.mockRejectedValue(new Error("读取库表失败：down"));
    await refreshFromBackend();
    expect(catalogLoadError()).toContain("读取库表失败");
    expect(catalogConnections()[0].databases[0].schemas[0].objects.map((item) => item.name)).toEqual(["customers", "orders", "categories"]);

    invoke.mockImplementation(async (command: string, args?: { schema?: string }) => {
      if (command === "catalog_sessions") return ["atlas"];
      if (command === "connections_list") return [saved("atlas", "postgres", "atlas_dev")];
      if (command === "env_tags_list") return [];
      if (command === "catalog_refresh") return [outline("atlas", "postgres", [{ name: "atlas_dev", schemas: ["public"] }])];
      if (command === "catalog_page") return { objects: [], total: 0, error: "读取库表失败：permission denied", schema: args?.schema };
      return [];
    });
    await refreshFromBackend();
    const publicSchema = catalogConnections()[0].databases[0].schemas.find((schema) => schema.name === "public");
    expect(publicSchema?.error).toContain("permission denied");
    expect(publicSchema?.objects.map((item) => item.name)).toEqual(["customers", "orders", "categories"]);
  });

  it("shows the first schema before the next read on the same connection returns", async () => {
    let releaseSecond: (() => void) | undefined;
    const second = new Promise<void>((resolve) => {
      releaseSecond = resolve;
    });
    let secondStarted = false;
    invoke.mockImplementation(async (command: string, args?: { schema?: string }) => {
      if (command === "catalog_sessions") return ["atlas"];
      if (command === "connections_list") return [saved("atlas", "postgres", "app")];
      if (command === "env_tags_list") return [];
      if (command === "catalog_refresh") return [outline("atlas", "postgres", [{ name: "app", schemas: ["public", "analytics"] }])];
      if (command === "catalog_page" && args?.schema === "analytics") {
        secondStarted = true;
        await second;
        return { objects: [{ name: "metrics", kind: "table", estimatedRows: 1, stale: false }], total: 1, error: null };
      }
      return { objects: [{ name: "customers", kind: "table", estimatedRows: 2, stale: false }], total: 1, error: null, kindReports: [{ kind: "table", total: 1, error: null }] };
    });
    const pending = refreshFromBackend();
    await vi.waitFor(() => {
      expect(
        catalogConnections()[0]?.databases[0]?.schemas.some((schema) => schema.objects.some((item) => item.name === "customers")),
      ).toBe(true);
    });
    expect(
      catalogConnections()[0]?.databases[0]?.schemas.some((schema) => schema.objects.some((item) => item.name === "metrics")) ?? false,
    ).toBe(false);
    expect(catalogLoadError()).toBeNull();
    releaseSecond?.();
    await pending;
    expect(secondStarted).toBe(true);
    expect(catalogConnections()[0]?.databases[0]?.schemas.some((schema) => schema.objects.some((item) => item.name === "metrics"))).toBe(true);
  });

  it("reads again after an open session appears", async () => {
    vi.useFakeTimers();
    let ids: string[] = [];
    invoke.mockImplementation(async (command: string) => {
      if (command === "catalog_sessions") return ids;
      if (command === "connections_list") return [];
      if (command === "env_tags_list") return [];
      if (command === "catalog_refresh") return [];
      return { objects: [], total: 0, error: null };
    });
    await startCatalog();
    expect(invoke.mock.calls.filter((call) => call[0] === "connections_list")).toHaveLength(1);
    ids = ["notes"];
    await vi.advanceTimersByTimeAsync(1000);
    expect(invoke.mock.calls.filter((call) => call[0] === "connections_list").length).toBeGreaterThan(1);
  });
});
