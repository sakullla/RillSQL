import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import * as csvWrite from "../export/write";
import { exportTasks, resetExportTasks } from "../export/tasks";
import DataGrid from "./DataGrid.vue";
import { commitChanges, queryPage, viewLoadedRows } from "./data";
import { copyPlain, copySqlValue, showCell } from "./display";
import { customersFixture, nokeyFixture, viewFixture } from "./fixture";
import QueryResultGrid from "./QueryResultGrid.vue";
import { TableSession } from "./session";
import { selectExportGate } from "./sql";
import type { TableTarget } from "./target";
import { COMMIT_RULE, type Column } from "./types";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke,
}));

describe("grid batches, nulls and server filters", () => {
  it("loads one batch and keeps the truncated rows for a later read", () => {
    const session = new TableSession(customersFixture(130), 100);
    expect(session.loaded).toHaveLength(100);
    expect(session.truncated).toBe(true);
    expect(session.sql).toContain("LIMIT 100");
    expect(session.loadMore()).toBeNull();
    expect(session.loaded).toHaveLength(130);
    expect(session.truncated).toBe(false);
    expect(session.loaded[0]?.cells.note).toBeNull();
    expect(session.loaded[1]?.cells.note).toBe("");
  });

  it("distinguishes null, empty text, copy and large fields", () => {
    const note = customersFixture(4).schema.columns[3];
    expect(showCell(null).kind).toBe("null");
    expect(showCell("").label).toBe("空字符串");
    expect(showCell("长".repeat(2000)).kind).toBe("large");
    expect(showCell("长".repeat(2000)).label).not.toContain("长".repeat(200));
    expect(copyPlain(null, "NULL")).toBe("NULL");
    expect(copyPlain(null, "empty")).toBe("");
    expect(copyPlain(null, "N")).toBe("\\N");
    expect(copyPlain("", "NULL")).toBe("");
    expect(copySqlValue(null, note)).toBe("NULL");
    expect(copySqlValue("", note)).toBe("''");
  });

  it("filters and sorts on the whole table, not the loaded page", () => {
    const session = new TableSession(customersFixture(130), 100);
    expect(session.addFilter({ column: "id", op: "gt", value: "100" })).toBeNull();
    expect(session.loaded.map((row) => row.cells.id)).toEqual(Array.from({ length: 30 }, (_, index) => String(101 + index)));
    expect(session.sql).toContain('> 100');
    expect(session.sql).not.toContain(" OR ");
    session.replaceFilters([{ column: "name", op: "eq", value: "a' OR '1'='1" }]);
    expect(session.sql).toContain("'a'' OR ''1''=''1'");
    expect(session.loaded).toHaveLength(0);
    expect(session.error).toBeNull();
    session.replaceFilters([]);
    session.cycleSort("name");
    expect(session.loaded[0]?.cells.name).toBe("000-first");
    expect(session.sql).toContain("ORDER BY");
    expect(session.scopeLabel).toBe("服务器端 · 整张表");
  });

  it("keeps null and empty string filters apart", () => {
    const session = new TableSession(customersFixture(130), 100);
    session.addFilter({ column: "note", op: "null" });
    expect(session.loaded.map((row) => row.cells.note)).toEqual([null]);
    expect(session.sql).toContain("IS NULL");
    session.replaceFilters([{ column: "note", op: "empty" }]);
    expect(session.loaded.map((row) => row.cells.note)).toEqual([""]);
    expect(session.sql).toContain("= ''");
    session.replaceFilters([{ column: "name", op: "contains", value: "小河" }]);
    expect(session.sql).toContain("ILIKE");
    expect(session.loaded.length).toBeGreaterThan(1);
  });

  it("reports a failed read instead of an empty match and keeps the filters", () => {
    const session = new TableSession(customersFixture(8), 5);
    session.addFilter({ column: "name", op: "eq", value: "小河" });
    const kept = session.loaded.length;
    session.store.readError = "连接已断开";
    session.addFilter({ column: "id", op: "eq", value: "1" });
    expect(session.error).toContain("连接已断开");
    expect(session.filters).toHaveLength(2);
    expect(session.loaded).toHaveLength(kept);
    const brokenStore = customersFixture(3);
    brokenStore.readError = "读取失败";
    const broken = new TableSession(brokenStore, 10);
    expect(broken.loaded).toHaveLength(0);
    expect(broken.error).toContain("读取失败");
    expect(queryPage(broken.store, { filters: broken.filters, sort: null, offset: 0, limit: 10 })).toMatchObject({ ok: false, emptyMatch: false });
  });
});

describe("grid edits", () => {
  it("lets a base table insert without a key, and blocks update or delete", () => {
    const session = new TableSession(nokeyFixture(), 10);
    expect(session.canInsert).toBe(true);
    expect(session.keyLabel).toContain("只能新增");
    expect(session.stageUpdate("a", "label", "改")).toContain("不能修改");
    expect(session.deleteRow("a")).toContain("不能删除");
    expect(session.store.rows).toHaveLength(3);
    session.addRow();
    const draft = session.inserts[0]!;
    session.stageInsert(draft.id, "label", "新");
    const report = session.commit();
    expect(report.rolledBack).toBe(false);
    expect(report.failed).toBeNull();
    expect(report.rule).toBe(COMMIT_RULE);
    expect(report.applied[0]?.sql.startsWith("INSERT")).toBe(true);
    expect(report.applied[0]?.sql).not.toContain("BEGIN");
    const saved = session.store.rows.find((row) => row.cells.label === "新");
    expect(saved?.located).toBe(false);
    expect(session.canUpdate(saved)).toBe(false);
    expect(session.canDeleteRow(saved!.id)).toBe(false);
  });

  it("stops on the failing statement and leaves earlier writes in place", () => {
    const session = new TableSession(customersFixture(4), 10);
    expect(session.stageUpdate("1", "name", "已改")).toBeNull();
    expect(session.stageUpdate("2", "email", "u1@example.com")).toBeNull();
    expect(session.deleteRow("3")).toBeNull();
    const before = session.store.rows.map((row) => ({ ...row.cells }));
    const report = session.commit();
    expect(report.rolledBack).toBe(false);
    expect(report.applied).toHaveLength(1);
    expect(report.failed?.message).toContain("duplicate key");
    expect(report.pendingIds).toEqual(["u:2", "d:3"]);
    expect(session.store.rows.find((row) => row.id === "1")?.cells.name).toBe("已改");
    expect(session.store.rows.find((row) => row.id === "2")?.cells.email).toBe(before[1]?.email);
    expect(session.store.rows.some((row) => row.id === "3")).toBe(true);
    expect(session.edits.has("2")).toBe(true);
    expect(session.deletes).toContain("3");
    expect(session.failures.get("u:2")).toContain("duplicate key");
    session.discard();
    expect(session.pendingCount).toBe(0);
    expect(session.store.rows.find((row) => row.id === "1")?.cells.name).toBe("已改");
  });

  it("refuses a delete that would match more than one row", () => {
    const store = customersFixture(1);
    const twin = { ...store.rows[0]!, id: "dup", cells: { ...store.rows[0]!.cells }, locator: store.rows[0]!.locator, located: true };
    store.rows.push(twin);
    const report = commitChanges(store, [{ kind: "delete", id: "d:1", rowId: "1" }]);
    expect(report.applied).toHaveLength(0);
    expect(report.failed?.message).toContain("影响 2 行");
    expect(report.rolledBack).toBe(false);
    expect(store.rows).toHaveLength(2);
  });

  it("does not edit a view, and a located insert stays editable", () => {
    const view = new TableSession(viewFixture(), 10);
    expect(view.canInsert).toBe(false);
    const session = new TableSession(customersFixture(2), 10);
    session.addRow();
    const draft = session.inserts[0]!;
    session.stageInsert(draft.id, "name", "新增");
    session.stageInsert(draft.id, "email", "new@example.com");
    const report = session.commit();
    expect(report.failed).toBeNull();
    const saved = session.store.rows.find((row) => row.cells.email === "new@example.com");
    expect(saved?.located).toBe(true);
    expect(session.canUpdate(saved)).toBe(true);
    expect(selectExportGate("SELECT 1").ok).toBe(true);
  });
});

describe("query results stay on the loaded rows", () => {
  it("does not sort or find rows that were not loaded", () => {
    const store = customersFixture(130);
    const loaded = store.rows.slice(0, 100);
    const found = viewLoadedRows(loaded, store.schema.columns, "000-first", null);
    expect(found.rows).toHaveLength(0);
    expect(found.note).toBe("仅作用于已读取的 100 行");
    const sorted = viewLoadedRows(loaded, store.schema.columns, "", { column: "name", direction: "asc" });
    expect(sorted.rows.some((row) => row.cells.name === "000-first")).toBe(false);
    expect(sorted.rows).toHaveLength(100);
  });
});

const customersTarget: TableTarget = {
  connectionId: "shop",
  database: "app",
  schema: "public",
  table: "customers",
  engine: "postgres",
};

const notesTarget: TableTarget = {
  connectionId: "local",
  database: "main",
  schema: "",
  table: "notes",
  engine: "sqlite",
};

function column(name: string, flags: Partial<Column> = {}): Column {
  return {
    name,
    typeName: "text",
    nullable: flags.nullable ?? true,
    primary: flags.primary ?? false,
    unique: flags.unique ?? false,
    auto: flags.auto ?? false,
  };
}

function customerSchema() {
  return {
    engine: "postgres",
    database: "app",
    schema: "public",
    name: "customers",
    kind: "table",
    key: ["id"],
    columns: [
      column("id", { primary: true, unique: true, auto: true, nullable: false }),
      column("name", { nullable: false }),
      column("email", { nullable: false, unique: true }),
      column("note"),
    ],
  };
}

function customerRow(id: number) {
  const note = id === 1 ? null : id === 2 ? "" : id === 3 ? "长".repeat(2000) : `备注${id}`;
  return {
    id: String(id),
    cells: { id: String(id), name: `客户${id}`, email: `u${id}@example.com`, note },
    located: true,
    locatorColumns: ["id"],
    locatorValues: [String(id)],
  };
}

describe("grid views", () => {
  beforeEach(() => {
    invoke.mockReset();
    resetExportTasks();
  });

  it("reads an opened table through grid commands instead of the fixture", async () => {
    expect(readFileSync(join(process.cwd(), "src/grid/DataGrid.vue"), "utf8")).not.toContain("fixtureForTab");
    expect(readFileSync(join(process.cwd(), "src/grid/QueryResultGrid.vue"), "utf8")).not.toContain("readRows");
    invoke.mockImplementation(async (command: string, args: { request?: { offset?: number; limit?: number; table?: string } }) => {
      if (command === "grid_describe") return customerSchema();
      if (command === "grid_page") {
        const offset = args.request?.offset ?? 0;
        const limit = args.request?.limit ?? 100;
        const rows = Array.from({ length: 130 }, (_, index) => customerRow(index + 1)).slice(offset, offset + limit);
        return { rows, truncated: offset + limit < 130, sql: `SELECT "id" FROM "public"."customers" LIMIT ${limit}` };
      }
      if (command === "grid_count") return { matched: 130 };
      if (command === "grid_export") {
        return { status: "done", success: true, writtenRows: 100, path: args.request && "path" in args.request ? String((args.request as { path?: string }).path) : "", residualPath: null, message: "已导出", partial: true };
      }
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:shop:app:public:customers", draft: "", pageSize: 100, target: customersTarget } });
    await flushPromises();
    expect(invoke.mock.calls.map((call) => call[0])).toEqual(expect.arrayContaining(["grid_describe", "grid_page"]));
    expect(wrapper.attributes("data-tab-id")).toBe("table:shop:app:public:customers");
    expect(wrapper.attributes("data-draft")).toBe("");
    expect(wrapper.text()).toContain("已读取");
    expect(wrapper.text()).toContain("100");
    expect(wrapper.text()).toContain("还有未加载数据");
    expect(wrapper.text()).toContain("服务器端 · 整张表");
    expect(wrapper.find(".v-null").text()).toBe("NULL");
    expect(wrapper.text()).toContain("空字符串");
    expect(wrapper.text()).not.toContain("长".repeat(200));
    const pageCall = invoke.mock.calls.find((call) => call[0] === "grid_page");
    expect(pageCall?.[1]).toMatchObject({ request: { connectionId: "shop", table: "customers", limit: 100, offset: 0 } });
    const writeSpy = vi.spyOn(csvWrite, "writeCsvFile");
    const exportButton = wrapper.findAll("button").find((button) => button.text() === "导出");
    await exportButton!.trigger("click");
    const dir = mkdtempSync(join(tmpdir(), "rillsql-grid-view-"));
    await wrapper.get("input[aria-label='导出路径']").setValue(join(dir, "customers.csv"));
    const start = wrapper.findAll("button").filter((button) => button.text() === "导出").at(-1)!;
    await start.trigger("click");
    await flushPromises();
    const exported = invoke.mock.calls.find((call) => call[0] === "grid_export");
    expect(exported?.[1]).toMatchObject({ request: { mode: "table-loaded", connectionId: "shop", table: "customers", truncated: true, limit: 100 } });
    expect(JSON.stringify(exported?.[1])).toContain("（部分）");
    expect(writeSpy).not.toHaveBeenCalled();
    writeSpy.mockRestore();
    rmSync(dir, { recursive: true, force: true });
    await wrapper.get("button[aria-label='继续读取']").trigger("click");
    await flushPromises();
    expect(wrapper.attributes("data-loaded")).toBe("130");
    expect(wrapper.attributes("data-truncated")).toBe("false");
    wrapper.unmount();
  });

  it("disables changes for rows that cannot be located", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") {
        return {
          engine: "sqlite",
          database: "main",
          schema: "",
          name: "notes",
          kind: "table",
          key: [],
          columns: [column("label")],
        };
      }
      if (command === "grid_page") {
        return {
          rows: [
            { id: "o:0:0", cells: { label: "重复" }, located: false, locatorColumns: [], locatorValues: [] },
            { id: "o:0:1", cells: { label: "重复" }, located: false, locatorColumns: [], locatorValues: [] },
          ],
          truncated: false,
          sql: 'SELECT "label" FROM "notes" LIMIT 10',
        };
      }
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:local:main::notes", draft: "x", pageSize: 10, target: notesTarget } });
    await flushPromises();
    const add = wrapper.findAll("button").find((button) => button.text() === "新增行");
    const remove = wrapper.findAll("button").find((button) => button.text() === "删除行");
    expect(add!.attributes("disabled")).toBeUndefined();
    expect(remove!.attributes("disabled")).toBeDefined();
    expect(wrapper.text()).toContain("无主键 · 只能新增");
    expect(invoke.mock.calls.some((call) => call[0] === "grid_describe")).toBe(true);
    wrapper.unmount();
  });

  it("shows the database error when a commit stops on a constraint", async () => {
    const duplicate = 'duplicate key value violates unique constraint "customers_email_key"\nDETAIL: Key (email)=(u2@example.com) already exists.';
    invoke.mockImplementation(async (command: string, args: { request?: { changes?: { id: string }[] } }) => {
      if (command === "grid_describe") return customerSchema();
      if (command === "grid_page") {
        return {
          rows: [customerRow(1), customerRow(2)],
          truncated: false,
          sql: 'SELECT "id" FROM "public"."customers" LIMIT 10',
        };
      }
      if (command === "grid_commit") {
        const changes = args.request?.changes ?? [];
        const id = changes[0]?.id ?? "missing";
        return {
          applied: [],
          failed: { id, sql: "UPDATE customers SET email = 'u2@example.com'", label: id, message: duplicate },
          pendingIds: changes.map((change) => change.id),
          rolledBack: false,
          rule: COMMIT_RULE,
        };
      }
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:shop:app:public:customers", draft: "", pageSize: 10, target: customersTarget } });
    await flushPromises();
    const email = wrapper.findAll("button.cell-btn").find((button) => button.text() === "u1@example.com");
    await email!.trigger("dblclick");
    await wrapper.get("input[aria-label='编辑 email']").setValue("u2@example.com");
    await wrapper.findAll("button").find((button) => button.text() === "暂存")!.trigger("click");
    await wrapper.findAll("button").find((button) => button.text() === "预览并提交")!.trigger("click");
    await wrapper.findAll("button").find((button) => button.text() === "提交变更")!.trigger("click");
    await flushPromises();
    expect(wrapper.find("[aria-label='提交数据变更']").exists()).toBe(false);
    const bar = wrapper.get(".pending-bar");
    expect(bar.text()).toContain("1 行提交失败，仍留在暂存区");
    expect(bar.text()).toContain('duplicate key value violates unique constraint "customers_email_key"');
    expect(bar.text()).toContain("DETAIL: Key (email)=(u2@example.com) already exists.");
    const failed = wrapper.get("tr.row-failed");
    expect(failed.attributes("data-row-id")).toBe("1");
    expect(failed.attributes("title")).toBe(duplicate);
    wrapper.unmount();
  });

  it("shows a read failure instead of no matches", async () => {
    invoke.mockRejectedValue(new Error("连接已断开，读取失败"));
    const wrapper = mount(DataGrid, { props: { tabId: "table:shop:app:public:broken", draft: "", target: { ...customersTarget, table: "broken" } } });
    await flushPromises();
    expect(wrapper.text()).toContain("读取失败");
    expect(wrapper.text()).toContain("这不是无匹配结果");
    expect(wrapper.text()).not.toContain("没有匹配行");
    wrapper.unmount();
  });

  it("keeps an unbound table tab empty instead of showing fixture rows", async () => {
    const wrapper = mount(DataGrid, { props: { tabId: "table:customers", draft: "" } });
    await flushPromises();
    expect(wrapper.attributes("data-tab-id")).toBe("table:customers");
    expect(wrapper.attributes("data-draft")).toBe("");
    expect(wrapper.text()).toContain("没有绑定已打开的表，不能用示例数据代替。");
    expect(invoke).not.toHaveBeenCalled();
    expect(wrapper.text()).not.toContain("客户1");
    wrapper.unmount();
  });

  it("limits query find and sort to loaded rows and blocks a non-select full export", async () => {
    const store = customersFixture(130);
    const wrapper = mount(QueryResultGrid, {
      props: {
        tabId: "query-1",
        statement: "DELETE FROM customers",
        columns: store.schema.columns,
        rows: store.rows.slice(0, 100),
        truncated: true,
      },
    });
    expect(wrapper.text()).toContain("仅作用于已读取的 100 行");
    expect(wrapper.text()).toContain("还有未加载数据");
    await wrapper.get("[aria-label='在已读取行中查找']").setValue("000-first");
    expect(wrapper.find("[data-row-id]").exists()).toBe(false);
    const exportButton = wrapper.findAll("button").find((button) => button.text() === "导出");
    await exportButton!.trigger("click");
    await wrapper.get("input[value='full']").setValue("full");
    expect(wrapper.text()).toContain("只有 SELECT 可以重新执行并全量导出");
    wrapper.unmount();
  });

  it("re-executes a select on the open connection and does not write loaded rows as the full file", async () => {
    const store = customersFixture(4);
    const secret = "仅已加载的秘密";
    const rows = store.rows.slice(0, 2).map((row) => ({ ...row, cells: { ...row.cells, note: secret } }));
    invoke.mockResolvedValue({
      status: "done",
      success: true,
      writtenRows: 4,
      path: "q.csv",
      residualPath: null,
      message: "已导出 4 行。",
      partial: false,
    });
    const wrapper = mount(QueryResultGrid, {
      props: {
        tabId: "query-1",
        statement: "SELECT id, note FROM customers",
        columns: store.schema.columns,
        rows,
        truncated: true,
        connectionId: "shop",
        engine: "postgres",
        database: "app",
        schema: "public",
      },
    });
    await wrapper.get("button").trigger("click");
    const open = wrapper.findAll("button").find((button) => button.text() === "导出");
    await open!.trigger("click");
    await wrapper.get("input[value='full']").setValue(true);
    await wrapper.get("input[type='checkbox']").setValue(true);
    const dir = mkdtempSync(join(tmpdir(), "rillsql-query-full-"));
    await wrapper.get("input[aria-label='导出路径']").setValue(join(dir, "query.csv"));
    const start = wrapper.findAll("button").filter((button) => button.text() === "导出").at(-1)!;
    await start.trigger("click");
    await flushPromises();
    const call = invoke.mock.calls.find((item) => item[0] === "grid_export");
    expect(call?.[1]).toMatchObject({
      request: { mode: "query", connectionId: "shop", statement: "SELECT id, note FROM customers", acknowledged: true },
    });
    expect(JSON.stringify(call?.[1])).not.toContain(secret);
    expect(exportTasks[0]?.status).toBe("done");
    rmSync(dir, { recursive: true, force: true });
    wrapper.unmount();
  });
});
