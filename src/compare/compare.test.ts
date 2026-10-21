import { flushPromises, mount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import type { CatalogConnection } from "../catalog/types";
import { createSession } from "../shell/session";
import { shellCommands } from "../shell/commands";
import CompareDialog from "./CompareDialog.vue";
import { placeCompareScript } from "./place";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke,
}));

function connection(engine: CatalogConnection["engine"], id: string, name: string): CatalogConnection {
  return {
    id,
    name,
    engine,
    env: "dev",
    envLabel: "开发",
    envTone: "dev",
    status: "connected",
    defaultDatabase: "app",
    foreignKeys: null,
    error: null,
    databases: [{ name: "app", error: null, schemas: [{ name: "public", objects: [], total: 0, error: null }] }],
  };
}

describe("compare", () => {
  it("adds a command palette entry and places the script in a new query without executing it", () => {
    const commands = shellCommands({
      openQuery: () => undefined,
      openStructure: () => undefined,
      showPage: () => undefined,
      toggleSidebar: () => undefined,
      setTheme: () => undefined,
      openHelp: () => undefined,
      requestSqlite: () => undefined,
      saveQuery: () => undefined,
      showSavedQueries: () => undefined,
      openCompare: () => undefined,
    });
    expect(commands.some((item) => item.label === "对比库")).toBe(true);
    const session = createSession();
    const sql = "CREATE TABLE orders (id integer);\n";
    placeCompareScript(session.openQueryDraft, sql, {
      connectionId: "pg",
      connectionName: "书店",
      database: "app",
      engine: "postgres",
      status: "connected",
    });
    expect(session.tabs.value.find((tab) => tab.kind === "query")?.draft).toBe(sql);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("refuses a cross-engine compare and does not offer to run the script", async () => {
    replaceCatalog([connection("mysql", "my", "商店"), connection("mariadb", "maria", "商店旧库")]);
    const wrapper = mount(CompareDialog);
    await wrapper.get("[data-side=baseline][data-field=connection]").setValue("my");
    await wrapper.get("[data-side=target][data-field=connection]").setValue("maria");
    await wrapper.get("[data-side=baseline][data-field=database]").setValue("app");
    await wrapper.get("[data-side=target][data-field=database]").setValue("app");
    await wrapper.get("[data-action=compare-structure]").trigger("click");
    expect(wrapper.get("[data-compare-refused]").text()).toContain("引擎不同");
    expect(wrapper.find("[data-action=place-structure]").exists()).toBe(false);
    expect(wrapper.text()).not.toContain("例程表单");
    expect(wrapper.find("[data-action=execute]").exists()).toBe(false);
    expect(invoke).not.toHaveBeenCalled();
    wrapper.unmount();
    resetCatalog();
  });

  it("shows an unexecuted structure script and can place it into a query", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "compare_structure") {
        return {
          refused: null,
          script: 'CREATE TABLE "public"."orders" (\n  "id" integer\n);',
          incomplete: false,
          notes: [],
          onlyBaseline: [{ kind: "表", name: "orders", detail: "仅基准拥有" }],
          onlyTarget: [],
          changed: [],
          tables: [],
        };
      }
      return [];
    });
    replaceCatalog([connection("postgres", "left", "基准"), connection("postgres", "right", "对照")]);
    const wrapper = mount(CompareDialog);
    await wrapper.get("[data-side=baseline][data-field=connection]").setValue("left");
    await wrapper.get("[data-side=target][data-field=connection]").setValue("right");
    await wrapper.get("[data-side=baseline][data-field=database]").setValue("app");
    await wrapper.get("[data-side=target][data-field=database]").setValue("app");
    await wrapper.get("[data-side=baseline][data-field=schema]").setValue("public");
    await wrapper.get("[data-side=target][data-field=schema]").setValue("public");
    await wrapper.get("[data-action=compare-structure]").trigger("click");
    await flushPromises();
    expect(wrapper.get("[data-compare-script]").element).toHaveProperty("readOnly", true);
    expect(wrapper.get("[data-compare-diffs]").text()).toContain("orders");
    await wrapper.get("[data-action=place-structure]").trigger("click");
    const placed = wrapper.emitted("place")?.[0];
    expect(String(placed?.[0])).toContain("orders");
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("query_start");
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("ddl_execute");
    wrapper.unmount();
    resetCatalog();
  });
});
