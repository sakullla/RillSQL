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

describe("catalog session reads", () => {
  it("refreshes the full read list and appends the next page", async () => {
    expect(refreshLimit(120, 100)).toBe(120);
    expect(refreshLimit(3, 100)).toBe(100);
    replaceCatalog(catalogFixture());
    const archive = limitsForRefresh(catalogConnections(), 100).find((item) => item.schema === "archive");
    expect(archive?.limit).toBe(120);

    invoke.mockResolvedValue(catalogFixture());
    await refreshFromBackend();
    const refresh = invoke.mock.calls.find((call) => call[0] === "catalog_refresh");
    expect(refresh?.[1].limits.find((item: { schema: string }) => item.schema === "archive").limit).toBe(120);
    expect(refresh?.[1].page).toBe(100);

    resetCatalog();
    setObjectPage(100);
    replaceCatalog(pagedCatalog());
    invoke.mockResolvedValue({ objects: auditTables(100, 20), total: 120, error: null });
    revealMore("atlas", "atlas_dev", "archive");
    await vi.waitFor(() => catalogConnections()[0].databases[0].schemas.find((schema) => schema.name === "archive")?.objects.length === 120);
    expect(invoke).toHaveBeenCalledWith(
      "catalog_page",
      expect.objectContaining({ connectionId: "atlas", database: "atlas_dev", schema: "archive", offset: 100, limit: 100 }),
    );
  });

  it("keeps objects when a read fails and does not present that as an empty schema", async () => {
    replaceCatalog(catalogFixture());
    invoke.mockRejectedValue(new Error("读取库表失败：down"));
    await refreshFromBackend();
    expect(catalogLoadError()).toContain("读取库表失败");
    expect(catalogConnections()[0].databases[0].schemas[0].objects.map((item) => item.name)).toEqual(["customers", "orders", "categories"]);

    const incoming = catalogFixture().map((connection) =>
      connection.id === "atlas"
        ? {
            ...connection,
            databases: connection.databases.map((database) => ({
              ...database,
              schemas: database.schemas.map((schema) =>
                schema.name === "public" && database.name === "atlas_dev"
                  ? { ...schema, objects: [], total: 0, error: "读取库表失败：permission denied" }
                  : schema,
              ),
            })),
          }
        : connection,
    );
    invoke.mockResolvedValue(incoming);
    await refreshFromBackend();
    const publicSchema = catalogConnections()[0].databases[0].schemas.find((schema) => schema.name === "public");
    expect(publicSchema?.error).toContain("permission denied");
    expect(publicSchema?.objects.map((item) => item.name)).toEqual(["customers", "orders", "categories"]);
  });

  it("reads again after an open session appears", async () => {
    vi.useFakeTimers();
    let ids: string[] = [];
    invoke.mockImplementation(async (command: string) => {
      if (command === "catalog_sessions") return ids;
      if (command === "catalog_refresh") return [];
      return { objects: [], total: 0, error: null };
    });
    await startCatalog();
    expect(invoke.mock.calls.filter((call) => call[0] === "catalog_refresh")).toHaveLength(1);
    ids = ["notes"];
    await vi.advanceTimersByTimeAsync(1000);
    expect(invoke.mock.calls.filter((call) => call[0] === "catalog_refresh").length).toBeGreaterThan(1);
  });
});
