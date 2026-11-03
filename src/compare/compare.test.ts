import { flushPromises, mount, type DOMWrapper } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import type { CatalogConnection } from "../catalog/types";
import { createSession } from "../shell/session";
import { shellCommands } from "../shell/commands";
import CompareDialog from "./CompareDialog.vue";
import { placeCompareScript } from "./place";

const invoke = vi.hoisted(() => vi.fn());

async function pick(control: Omit<DOMWrapper<Element>, "exists">, value: string) {
  await control.get(".listbox-trigger").trigger("click");
  const option = control.findAll(".listbox-option").find((item) => item.attributes("data-value") === value);
  if (!option) throw new Error(`missing option: ${value}`);
  await option.trigger("click");
  await flushPromises();
}

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
  it("disables connection controls when no relational connection is open", async () => {
    replaceCatalog([]);
    const wrapper = mount(CompareDialog);
    const control = wrapper.get("[data-side=baseline][data-field=connection]");
    const trigger = control.get(".listbox-trigger");
    expect(trigger.attributes("disabled")).toBeDefined();
    await trigger.trigger("click");
    expect(control.find(".listbox-pop").exists()).toBe(false);
    wrapper.unmount();
    resetCatalog();
  });

  it("offers a filter only when the connection list is long", async () => {
    replaceCatalog(Array.from({ length: 9 }, (_, index) => connection("mysql", `c${index}`, `库${index}`)));
    const wrapper = mount(CompareDialog);
    const control = wrapper.get("[data-side=baseline][data-field=connection]");
    await control.get(".listbox-trigger").trigger("click");
    expect(control.find(".listbox-filter").exists()).toBe(true);
    wrapper.unmount();
    resetCatalog();
  });

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
    await pick(wrapper.get("[data-side=baseline][data-field=connection]"), "my");
    await pick(wrapper.get("[data-side=target][data-field=connection]"), "maria");
    await pick(wrapper.get("[data-side=baseline][data-field=database]"), "app");
    await pick(wrapper.get("[data-side=target][data-field=database]"), "app");
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
    await pick(wrapper.get("[data-side=baseline][data-field=connection]"), "left");
    await pick(wrapper.get("[data-side=target][data-field=connection]"), "right");
    await pick(wrapper.get("[data-side=baseline][data-field=database]"), "app");
    await pick(wrapper.get("[data-side=target][data-field=database]"), "app");
    await pick(wrapper.get("[data-side=baseline][data-field=schema]"), "public");
    await pick(wrapper.get("[data-side=target][data-field=schema]"), "public");
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

  it("lists schemas for another database on the same postgres connection and compares without focusing", async () => {
    invoke.mockReset();
    invoke.mockImplementation(async (command: string) => {
      if (command === "compare_schemas") return ["sales"];
      if (command === "compare_structure") {
        return { refused: null, script: "CREATE TABLE sales.orders (id integer);\n", incomplete: false, notes: [], onlyBaseline: [], onlyTarget: [], changed: [], tables: [] };
      }
      return [];
    });
    replaceCatalog([
      {
        ...connection("postgres", "pg", "书店"),
        databases: [
          { name: "app", error: null, schemas: [{ name: "public", objects: [], total: 0, error: null }] },
          { name: "other", error: "当前会话连在 app，没有另开连接读取 other", schemas: [] },
        ],
      },
    ]);
    const wrapper = mount(CompareDialog);
    await pick(wrapper.get("[data-side=baseline][data-field=connection]"), "pg");
    await pick(wrapper.get("[data-side=target][data-field=connection]"), "pg");
    await pick(wrapper.get("[data-side=baseline][data-field=database]"), "app");
    await pick(wrapper.get("[data-side=target][data-field=database]"), "other");
    await flushPromises();
    await pick(wrapper.get("[data-side=baseline][data-field=schema]"), "public");
    const targetSchema = wrapper.get("[data-side=target][data-field=schema]");
    expect(targetSchema.find(".listbox-trigger").exists()).toBe(true);
    await pick(targetSchema, "sales");
    await wrapper.get("[data-action=compare-structure]").trigger("click");
    await flushPromises();
    expect(wrapper.find("[data-compare-refused]").exists()).toBe(false);
    const listed = invoke.mock.calls.filter((call) => call[0] === "compare_schemas");
    expect(listed).toHaveLength(1);
    expect(listed[0]?.[1]).toMatchObject({ request: { connectionId: "pg", database: "other" } });
    const compared = invoke.mock.calls.find((call) => call[0] === "compare_structure");
    expect(compared?.[1]).toMatchObject({
      request: {
        baseline: { connectionId: "pg", database: "app", schema: "public", engine: "postgres" },
        target: { connectionId: "pg", database: "other", schema: "sales", engine: "postgres" },
      },
    });
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("connections_focus_database");
    wrapper.unmount();
    resetCatalog();
  });

  it("accepts a typed schema when listing the other postgres database fails", async () => {
    invoke.mockReset();
    invoke.mockImplementation(async (command: string) => {
      if (command === "compare_schemas") throw new Error("临时读取会话失败");
      if (command === "compare_structure") {
        return { refused: null, script: "-- 未执行\n", incomplete: false, notes: [], onlyBaseline: [], onlyTarget: [], changed: [], tables: [] };
      }
      return [];
    });
    replaceCatalog([
      {
        ...connection("postgres", "pg", "书店"),
        databases: [
          { name: "app", error: null, schemas: [{ name: "public", objects: [], total: 0, error: null }] },
          { name: "other", error: "当前会话连在 app，没有另开连接读取 other", schemas: [] },
        ],
      },
    ]);
    const wrapper = mount(CompareDialog);
    await pick(wrapper.get("[data-side=baseline][data-field=connection]"), "pg");
    await pick(wrapper.get("[data-side=target][data-field=connection]"), "pg");
    await pick(wrapper.get("[data-side=baseline][data-field=database]"), "app");
    await pick(wrapper.get("[data-side=target][data-field=database]"), "other");
    await flushPromises();
    await pick(wrapper.get("[data-side=baseline][data-field=schema]"), "public");
    const targetSchema = wrapper.get("[data-side=target][data-field=schema]");
    expect(targetSchema.element.tagName).toBe("INPUT");
    expect(wrapper.get("[data-side=target][data-schema-hint]").text()).toContain("临时读取会话失败");
    await targetSchema.setValue("sales");
    await wrapper.get("[data-action=compare-structure]").trigger("click");
    await flushPromises();
    expect(wrapper.find("[data-compare-refused]").exists()).toBe(false);
    const compared = invoke.mock.calls.find((call) => call[0] === "compare_structure");
    expect(compared?.[1]).toMatchObject({ request: { target: { database: "other", schema: "sales" } } });
    wrapper.unmount();
    resetCatalog();
  });

  it("drops tables selected in the previous structure result", async () => {
    invoke.mockReset();
    let round = 0;
    invoke.mockImplementation(async (command: string) => {
      if (command === "compare_structure") {
        round += 1;
        const name = round === 1 ? "orders" : "customers";
        return {
          refused: null,
          script: "",
          incomplete: false,
          notes: [],
          onlyBaseline: [],
          onlyTarget: [],
          changed: [],
          tables: [{ name, onBoth: true, needsKeyChoice: false, keys: [] }],
        };
      }
      if (command === "compare_data") return { refused: null, script: "", incomplete: false, notes: [], diffs: [] };
      return [];
    });
    replaceCatalog([connection("mysql", "left", "基准"), connection("mysql", "right", "对照")]);
    const wrapper = mount(CompareDialog);
    await pick(wrapper.get("[data-side=baseline][data-field=connection]"), "left");
    await pick(wrapper.get("[data-side=target][data-field=connection]"), "right");
    await pick(wrapper.get("[data-side=baseline][data-field=database]"), "app");
    await pick(wrapper.get("[data-side=target][data-field=database]"), "app");
    await wrapper.get("[data-action=compare-structure]").trigger("click");
    await flushPromises();
    expect((wrapper.get("[data-table=orders]").element as HTMLInputElement).checked).toBe(true);
    await wrapper.get("[data-action=compare-structure]").trigger("click");
    await flushPromises();
    expect(wrapper.find("[data-table=orders]").exists()).toBe(false);
    expect((wrapper.get("[data-table=customers]").element as HTMLInputElement).checked).toBe(true);
    await wrapper.get("[data-action=compare-data]").trigger("click");
    await flushPromises();
    const dataCall = invoke.mock.calls.filter((call) => call[0] === "compare_data").at(-1);
    expect(dataCall?.[1]).toMatchObject({ request: { tables: [{ name: "customers", key: null }] } });
    wrapper.unmount();
    resetCatalog();
  });

  it("does not submit the previous schema while the next database is still loading", async () => {
    invoke.mockReset();
    let release: ((names: string[]) => void) | undefined;
    invoke.mockImplementation(async (command: string) => {
      if (command === "compare_schemas") {
        return await new Promise<string[]>((resolve) => {
          release = resolve;
        });
      }
      if (command === "compare_structure") {
        return { refused: null, script: "-- 未执行\n", incomplete: false, notes: [], onlyBaseline: [], onlyTarget: [], changed: [], tables: [] };
      }
      return [];
    });
    replaceCatalog([
      {
        ...connection("postgres", "pg", "书店"),
        databases: [
          { name: "app", error: null, schemas: [{ name: "public", objects: [], total: 0, error: null }] },
          { name: "other", error: "当前会话连在 app，没有另开连接读取 other", schemas: [] },
        ],
      },
    ]);
    const wrapper = mount(CompareDialog);
    await pick(wrapper.get("[data-side=baseline][data-field=connection]"), "pg");
    await pick(wrapper.get("[data-side=target][data-field=connection]"), "pg");
    await pick(wrapper.get("[data-side=baseline][data-field=database]"), "app");
    await pick(wrapper.get("[data-side=target][data-field=database]"), "app");
    await pick(wrapper.get("[data-side=baseline][data-field=schema]"), "public");
    await pick(wrapper.get("[data-side=target][data-field=schema]"), "public");
    await pick(wrapper.get("[data-side=target][data-field=database]"), "other");
    expect(wrapper.get("[data-side=target][data-field=schema]").attributes("data-schema-state")).toBe("loading");
    const compareStructure = wrapper.get("[data-action=compare-structure]");
    expect((compareStructure.element as HTMLButtonElement).disabled).toBe(true);
    (compareStructure.element as HTMLButtonElement).disabled = false;
    await compareStructure.trigger("click");
    expect(wrapper.get("[data-compare-refused]").text()).toContain("还在读取");
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("compare_structure");
    release?.(["sales"]);
    await flushPromises();
    const targetSchema = wrapper.get("[data-side=target][data-field=schema]");
    expect(targetSchema.find(".listbox-value.placeholder").exists()).toBe(true);
    await wrapper.get("[data-action=compare-structure]").trigger("click");
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("compare_structure");
    await pick(targetSchema, "sales");
    await wrapper.get("[data-action=compare-structure]").trigger("click");
    await flushPromises();
    const compared = invoke.mock.calls.find((call) => call[0] === "compare_structure");
    expect(compared?.[1]).toMatchObject({ request: { target: { database: "other", schema: "sales" } } });
    wrapper.unmount();
    resetCatalog();
  });

  it("drops a cached schema that is not one of the loaded choices", async () => {
    invoke.mockReset();
    invoke.mockImplementation(async (command: string) => {
      if (command === "compare_schemas") return ["sales"];
      if (command === "compare_structure") {
        return { refused: null, script: "-- 未执行\n", incomplete: false, notes: [], onlyBaseline: [], onlyTarget: [], changed: [], tables: [] };
      }
      return [];
    });
    replaceCatalog([
      {
        ...connection("postgres", "pg", "书店"),
        databases: [
          { name: "app", error: null, schemas: [{ name: "public", objects: [], total: 0, error: null }] },
          { name: "other", error: "当前会话连在 app，没有另开连接读取 other", schemas: [] },
        ],
      },
    ]);
    const wrapper = mount(CompareDialog);
    await pick(wrapper.get("[data-side=baseline][data-field=connection]"), "pg");
    await pick(wrapper.get("[data-side=target][data-field=connection]"), "pg");
    await pick(wrapper.get("[data-side=baseline][data-field=database]"), "app");
    await pick(wrapper.get("[data-side=target][data-field=database]"), "other");
    await flushPromises();
    await pick(wrapper.get("[data-side=baseline][data-field=schema]"), "public");
    await pick(wrapper.get("[data-side=target][data-field=database]"), "app");
    await pick(wrapper.get("[data-side=target][data-field=schema]"), "public");
    await pick(wrapper.get("[data-side=target][data-field=database]"), "other");
    await flushPromises();
    const targetSchema = wrapper.get("[data-side=target][data-field=schema]");
    await targetSchema.get(".listbox-trigger").trigger("click");
    const labels = targetSchema.findAll(".listbox-option").map((item) => item.text()).join(" ");
    expect(labels).toContain("sales");
    expect(labels).not.toContain("public");
    await targetSchema.get(".listbox-trigger").trigger("click");
    expect(targetSchema.find(".listbox-value.placeholder").exists()).toBe(true);
    await wrapper.get("[data-action=compare-structure]").trigger("click");
    expect(wrapper.get("[data-compare-refused]").text()).toContain("schema");
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("compare_structure");
    wrapper.unmount();
    resetCatalog();
  });

  it("summarizes counts, folds scripts, and restores focus on escape", async () => {
    const opener = document.createElement("button");
    document.body.appendChild(opener);
    opener.focus();
    invoke.mockReset();
    invoke.mockImplementation(async (command: string) => {
      if (command === "compare_structure") {
        return {
          refused: null,
          script: "CREATE TABLE orders (id integer);\n",
          incomplete: false,
          notes: [],
          onlyBaseline: [{ kind: "表", name: "orders", detail: "仅基准拥有" }],
          onlyTarget: [{ kind: "表", name: "legacy", detail: "仅对照拥有" }],
          changed: [{ kind: "列", name: "orders.note", detail: "类型不同" }],
          tables: [{ name: "orders", onBoth: true, needsKeyChoice: false, keys: [] }],
        };
      }
      if (command === "compare_data") {
        return { refused: null, script: "INSERT INTO orders (id) VALUES (1);\n", incomplete: false, notes: [], diffs: [] };
      }
      return [];
    });
    replaceCatalog([connection("mysql", "left", "基准"), connection("mysql", "right", "对照")]);
    const wrapper = mount(CompareDialog, { attachTo: document.body });
    await pick(wrapper.get("[data-side=baseline][data-field=connection]"), "left");
    await pick(wrapper.get("[data-side=target][data-field=connection]"), "right");
    await pick(wrapper.get("[data-side=baseline][data-field=database]"), "app");
    await pick(wrapper.get("[data-side=target][data-field=database]"), "app");
    await wrapper.get("[data-action=compare-structure]").trigger("click");
    await flushPromises();
    const summary = wrapper.get("[data-compare-summary]");
    expect(summary.text()).toContain("仅基准 1");
    expect(summary.text()).toContain("仅对照 1");
    expect(summary.text()).toContain("列差异 1");
    const list = wrapper.get("[data-compare-diffs]").element;
    expect(summary.element.compareDocumentPosition(list) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    const structureDetails = wrapper.get("[data-compare-script]").element.closest("details") as HTMLDetailsElement;
    expect(structureDetails.open).toBe(false);
    expect(structureDetails.textContent).toContain("结构脚本，可放入新查询");
    expect(structureDetails.contains(wrapper.get("[data-action=place-structure]").element)).toBe(true);
    expect(wrapper.get(".cmp-foot").element.contains(wrapper.get("[data-action=compare-structure]").element)).toBe(true);
    expect(wrapper.get(".cmp-body").element.contains(wrapper.get("[data-action=compare-structure]").element)).toBe(false);
    expect(wrapper.get("[data-action=compare-close]").classes()).not.toContain("primary");
    await wrapper.get("[data-action=compare-data]").trigger("click");
    await flushPromises();
    const dataDetails = wrapper.get("[data-data-script]").element.closest("details") as HTMLDetailsElement;
    expect(dataDetails.open).toBe(false);
    expect(dataDetails.textContent).toContain("数据脚本，可放入新查询");
    expect(dataDetails.contains(wrapper.get("[data-action=place-data]").element)).toBe(true);
    await wrapper.get(".cmp-sheet").trigger("keydown", { key: "Escape" });
    expect(wrapper.emitted("close")).toHaveLength(1);
    await nextTick();
    expect(document.activeElement).toBe(opener);
    wrapper.unmount();
    opener.remove();
    resetCatalog();
  });
});
