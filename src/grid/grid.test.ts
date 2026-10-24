import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import { anyPending, resetCloseGuard, tabHasPending } from "../shell/close-guard";
import type { CatalogConnection } from "../catalog/types";
import { exportTasks, resetExportTasks } from "../export/tasks";
import DataGrid from "./DataGrid.vue";
import { commitChanges, matchesFilter, queryPage, viewLoadedRows } from "./data";
import { locateResultRows, readSingleTable, resolveEditableTable, ResultEditSession } from "./result-edit";
import { gridModel } from "../query/rows";
import { copyPlain, copySqlValue, showCell } from "./display";
import { customersFixture, nokeyFixture, viewFixture } from "./fixture";
import QueryResultGrid from "./QueryResultGrid.vue";
import { TableSession } from "./session";
import { compileWhere, selectExportGate } from "./sql";
import type { TableTarget } from "./target";
import { COMMIT_RULE, FILTER_OPS, opLabel, opNeedsValue, type Column } from "./types";

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
    const large = showCell("长".repeat(2000));
    expect(large.kind).toBe("large");
    expect(large.label).toContain("字节");
    expect(large.label).not.toContain("长".repeat(2000));
    expect(large.body.length).toBeLessThan(2000);
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
    expect(session.scopeLabel).toBe("筛选和排序作用于整张表");
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

  it("adds between and in operators with metadata, SQL and local matching", () => {
    const betweenMeta = FILTER_OPS.find((item) => item.id === "between");
    expect(betweenMeta?.label).toBe("介于");
    expect(betweenMeta?.needsValue).toBe(true);
    const inMeta = FILTER_OPS.find((item) => item.id === "in");
    expect(inMeta?.label).toBe("属于(in)");
    expect(inMeta?.needsValue).toBe(true);
    expect(opLabel("between")).toBe("介于");
    expect(opNeedsValue("in")).toBe(true);

    const store = customersFixture(20);
    const between = queryPage(store, { filters: [{ column: "id", op: "between", value: "5", second: "8" }], sort: null, offset: 0, limit: 100 });
    expect(between).toMatchObject({ ok: true, matched: 4 });
    if (between.ok) expect(between.sql).toContain("\"id\" BETWEEN 5 AND 8");
    const oneOf = queryPage(store, { filters: [{ column: "name", op: "in", values: ["小河", "000-first"] }], sort: null, offset: 0, limit: 100 });
    expect(oneOf).toMatchObject({ ok: true, matched: 3 });
    if (oneOf.ok) expect(oneOf.sql).toContain("\"name\" IN ('小河', '000-first')");
    const mysql = compileWhere({ ...store.schema, engine: "mysql" }, [{ column: "id", op: "between", value: "5", second: "8" }]);
    expect(mysql).toEqual({ ok: true, sql: "`id` BETWEEN 5 AND 8" });
    expect(compileWhere(store.schema, [{ column: "id", op: "between", value: "5" }])).toEqual({ ok: false, error: "between 需要两个值" });
    expect(compileWhere(store.schema, [{ column: "id", op: "in", values: [] }])).toEqual({ ok: false, error: "IN 筛选至少要一个值" });
    expect(queryPage(store, { filters: [{ column: "id", op: "between", value: "5" }], sort: null, offset: 0, limit: 10 })).toMatchObject({ ok: false, emptyMatch: false });
    expect(queryPage(store, { filters: [{ column: "id", op: "in", values: [] }], sort: null, offset: 0, limit: 10 })).toMatchObject({ ok: false, emptyMatch: false });

    const note = store.schema.columns[3]!;
    expect(matchesFilter(note, "b", { column: "note", op: "between", value: "a", second: "c" })).toBe(true);
    expect(matchesFilter(note, "d", { column: "note", op: "between", value: "a", second: "c" })).toBe(false);
    expect(matchesFilter(note, "b", { column: "note", op: "between", value: "a" })).toBe(false);
    expect(matchesFilter(note, "甲", { column: "note", op: "in", values: ["甲", "乙"] })).toBe(true);
    expect(matchesFilter(note, "丙", { column: "note", op: "in", values: ["甲", "乙"] })).toBe(false);
    expect(matchesFilter(note, null, { column: "note", op: "in", values: ["甲"] })).toBe(false);
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
    resetCloseGuard();
    resetCatalog();
  });

  it("reads an opened table through grid commands instead of the fixture", async () => {
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
    expect(wrapper.text()).toContain("已读取前 100 行");
    expect(wrapper.text()).toContain("筛选和排序作用于整张表");
    expect(wrapper.text()).not.toContain("服务器端");
    expect(wrapper.find("button[aria-label='继续读取']").exists()).toBe(true);
    expect(wrapper.find(".grid-foot .v-null").exists()).toBe(false);
    expect(wrapper.find(".v-null").text()).toBe("NULL");
    expect(wrapper.text()).toContain("空字符串");
    expect(wrapper.text()).toContain("字节");
    expect(wrapper.text()).not.toContain("长".repeat(2000));
    const pageCall = invoke.mock.calls.find((call) => call[0] === "grid_page");
    expect(pageCall?.[1]).toMatchObject({ request: { connectionId: "shop", table: "customers", limit: 100, offset: 0 } });
    exportTasks.unshift({
      id: "stuck",
      fileName: "orders.csv",
      path: "orders.csv",
      status: "running",
      doneRows: 0,
      totalRows: 3,
      partial: false,
      message: "正在从已打开的连接导出",
      residualPath: null,
    });
    const exportButton = wrapper.findAll("button").find((button) => button.text() === "导出");
    await exportButton!.trigger("click");
    const dir = mkdtempSync(join(tmpdir(), "rillsql-grid-view-"));
    const requested = join(dir, "customers.csv");
    await wrapper.get("input[aria-label='导出路径']").setValue(requested);
    const start = wrapper.findAll("button").filter((button) => button.text() === "导出").at(-1)!;
    await start.trigger("click");
    await vi.waitFor(() => {
      expect(exportTasks.find((task) => task.id !== "stuck")?.status).toBe("done");
    });
    expect(invoke.mock.calls.some((call) => call[0] === "grid_export")).toBe(false);
    const written = join(dir, "customers（部分）.csv");
    const body = readFileSync(written, "utf8");
    expect(body.startsWith("\uFEFF")).toBe(true);
    expect(body).toContain("客户1");
    expect(body).toContain("长".repeat(2000));
    // Saving this snapshot must not rewrite another export's status.
    expect(exportTasks.find((task) => task.id === "stuck")?.status).toBe("running");
    expect(exportTasks.find((task) => task.id !== "stuck")?.status).toBe("done");
    expect(wrapper.get("[role='status']").text()).toContain("已导出 100 行");
    expect(wrapper.get("[role='status']").text()).toContain("customers（部分）.csv");
    const again = wrapper.findAll("button").find((button) => button.text() === "导出");
    await again!.trigger("click");
    await wrapper.get("input[value='full']").setValue(true);
    await wrapper.get("input[aria-label='导出路径']").setValue(join(dir, "all.csv"));
    const exportAll = wrapper.findAll("button").filter((button) => button.text() === "导出").at(-1)!;
    await exportAll.trigger("click");
    await flushPromises();
    const exported = invoke.mock.calls.find((call) => call[0] === "grid_export");
    expect(exported?.[1]).toMatchObject({ request: { mode: "table-full", connectionId: "shop", table: "customers", limit: 0 } });
    expect(JSON.stringify(exported?.[1])).not.toContain("客户1");
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
    await wrapper.get("[data-cell='o:0:0:label']").trigger("dblclick");
    expect(wrapper.find("input[aria-label='编辑 label']").exists()).toBe(false);
    expect(wrapper.text()).toContain("这张表没有主键，无法唯一确定已有行，所以不能修改或删除它们。");
    expect(wrapper.text()).not.toContain("按 label 定位行");
    expect(wrapper.find(".key-chip").exists()).toBe(false);
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
    await wrapper.findAll("button").find((button) => button.text() === "确定")!.trigger("click");
    expect(wrapper.get(".pending-bar").text()).toContain("1 处更改未保存");
    await wrapper.findAll("button").find((button) => button.text() === "提交")!.trigger("click");
    expect(wrapper.find("[aria-label='待提交的更改']").exists()).toBe(true);
    await wrapper.findAll("button").find((button) => button.text() === "提交 1 处更改")!.trigger("click");
    await flushPromises();
    // 失败时面板保持打开，错误写在面板里，而不是看起来像提交成功。
    expect(wrapper.find("[aria-label='待提交的更改']").exists()).toBe(true);
    expect(wrapper.get("[data-commit-error]").text()).toContain("1 处失败");
    const bar = wrapper.get(".pending-bar");
    expect(bar.text()).toContain("1 处更改未保存");
    expect(bar.text()).toContain('duplicate key value violates unique constraint "customers_email_key"');
    expect(bar.text()).toContain("DETAIL: Key (email)=(u2@example.com) already exists.");
    const failed = wrapper.get("tr.row-failed");
    expect(failed.attributes("data-row-id")).toBe("1");
    expect(failed.attributes("title")).toBe(duplicate);
    await wrapper.findAll("button").find((button) => button.text() === "取消")!.trigger("click");
    expect(wrapper.find("[aria-label='待提交的更改']").exists()).toBe(false);
    wrapper.unmount();
  });

  it("edits with desktop keys and marks deleted rows clearly", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return customerSchema();
      if (command === "grid_page") {
        return {
          rows: [customerRow(1), customerRow(2)],
          truncated: false,
          sql: 'SELECT "id" FROM "public"."customers" LIMIT 10',
        };
      }
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:shop:app:public:customers", draft: "", pageSize: 10, target: customersTarget } });
    await flushPromises();
    const name = wrapper.get('[data-cell="1:name"]');
    await name.trigger("click");
    await name.trigger("keydown", { key: "Enter" });
    expect(wrapper.find("input[aria-label='编辑 name']").exists()).toBe(true);
    await wrapper.get("input[aria-label='编辑 name']").setValue("键盘改名");
    await wrapper.get("input[aria-label='编辑 name']").trigger("keydown", { key: "Escape" });
    expect(wrapper.find("input[aria-label='编辑 name']").exists()).toBe(false);
    expect(wrapper.get('[data-cell="1:name"]').text()).toBe("客户1");
    expect(wrapper.find(".pending-bar").exists()).toBe(false);
    const cell = wrapper.get('[data-cell="1:name"]');
    await cell.trigger("keydown", { key: "Enter" });
    const input = wrapper.get("input[aria-label='编辑 name']");
    await input.setValue("键盘改名");
    await input.trigger("keydown", { key: "Tab" });
    expect(wrapper.find("input[aria-label='编辑 name']").exists()).toBe(false);
    expect(wrapper.get('[data-cell="1:name"]').text()).toBe("键盘改名");
    expect(wrapper.get(".pending-bar").text()).toContain("1 处更改未保存");
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("1:email");
    await wrapper.findAll("button").find((button) => button.text() === "删除行")!.trigger("click");
    const row = wrapper.get("tr[data-row-id='1']");
    expect(row.classes()).toContain("row-delete");
    expect(wrapper.get('[data-cell="1:id"]').text()).toContain("将删除");
    expect(wrapper.get(".pending-bar").text()).toContain("1 处更改未保存");
    await wrapper.findAll("button").find((button) => button.text() === "放弃")!.trigger("click");
    expect(wrapper.get(".pending-bar").text()).toContain("确认放弃");
    expect(wrapper.find(".pending-bar").exists()).toBe(true);
    await wrapper.findAll("button").find((button) => button.text() === "确认放弃")!.trigger("click");
    expect(wrapper.find(".pending-bar").exists()).toBe(false);
    expect(wrapper.get('[data-cell="1:name"]').text()).toBe("客户1");
    expect(wrapper.get("tr[data-row-id='1']").classes()).not.toContain("row-delete");
    wrapper.unmount();
  });

  it("honours the advertised grid shortcuts and preview limit", async () => {
    localStorage.setItem("rillsql.shell", JSON.stringify({ previewKB: 4 }));
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return customerSchema();
      if (command === "grid_page") {
        return {
          rows: [
            { ...customerRow(1), cells: { ...customerRow(1).cells, note: "长".repeat(600) } },
            customerRow(2),
          ],
          truncated: false,
          sql: 'SELECT "id" FROM "public"."customers" LIMIT 10',
        };
      }
      throw new Error(command);
    });
    try {
      const wrapper = mount(DataGrid, { props: { tabId: "table:shortcuts", draft: "", pageSize: 10, target: customersTarget } });
      await flushPromises();
      // 4 KB 上限下，1800 字节的文本不应被折叠成“大字段”
      expect(wrapper.text()).not.toContain("大字段");
      const scroll = wrapper.get(".grid-scroll");
      await wrapper.get("[data-cell='1:name']").trigger("pointerdown");
      await scroll.trigger("keydown", { key: "Delete" });
      expect(wrapper.get("tr[data-row-id='1']").classes()).toContain("row-delete");
      expect(wrapper.get(".pending-bar").text()).toContain("1 处更改未保存");
      await scroll.trigger("keydown", { key: "a", ctrlKey: true });
      expect(wrapper.get(".grid-foot").text()).toContain("已选 8 格");
      await scroll.trigger("keydown", { key: "s", ctrlKey: true });
      expect(wrapper.find("[aria-label='待提交的更改']").exists()).toBe(true);
      expect(wrapper.get("[aria-label='待提交的更改']").text()).toContain("DELETE");
      await wrapper.findAll("button").find((button) => button.text() === "取消")!.trigger("click");
      wrapper.unmount();
    } finally {
      localStorage.clear();
    }
  });

  it("jumps to the referenced rows from the cell menu", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return customerSchema();
      if (command === "grid_page") {
        return {
          rows: [customerRow(1), customerRow(4)],
          truncated: false,
          sql: "SELECT customers",
        };
      }
      if (command === "ddl_describe") {
        return {
          foreignKeyList: [{
            name: "orders_note_fk",
            columns: ["note"],
            refSchema: "public",
            refTable: "customers",
            refColumns: ["note"],
            onDelete: "NO ACTION",
            onUpdate: "NO ACTION",
          }],
        };
      }
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:shop:app:public:customers", draft: "", pageSize: 10, target: customersTarget } });
    await flushPromises();
    // 表结构在单元格菜单第一次打开前不读。
    expect(invoke.mock.calls.some((call) => call[0] === "ddl_describe")).toBe(false);
    await wrapper.get("[data-cell='4:note']").trigger("contextmenu", { clientX: 24, clientY: 24 });
    await flushPromises();
    const jump = wrapper.get("[data-action='jump-reference']");
    expect(jump.text()).toContain("查看引用行 → public.customers");
    await jump.trigger("click");
    expect(wrapper.emitted("jump-reference")).toEqual([['SELECT *\nFROM "public"."customers"\nWHERE "note" = \'备注4\';', "public.customers"]]);
    expect(wrapper.find(".cell-menu").exists()).toBe(false);
    // NULL 的外键值拼不出等值条件，菜单里不出现跳转项。
    await wrapper.get("[data-cell='1:note']").trigger("contextmenu", { clientX: 24, clientY: 24 });
    await flushPromises();
    expect(wrapper.find("[data-action='jump-reference']").exists()).toBe(false);
    wrapper.unmount();
  });

  it("reports a failed structure read in the cell menu without blocking the basics", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return customerSchema();
      if (command === "grid_page") return { rows: [customerRow(1)], truncated: false, sql: "SELECT customers" };
      if (command === "ddl_describe") throw new Error("连接已断开");
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:shop:app:public:customers", draft: "", pageSize: 10, target: customersTarget } });
    await flushPromises();
    await wrapper.get("[data-cell='1:name']").trigger("contextmenu", { clientX: 24, clientY: 24 });
    await flushPromises();
    expect(wrapper.get(".cell-menu .menu-error").text()).toContain("读取表结构失败");
    expect(wrapper.find("[data-action='jump-reference']").exists()).toBe(false);
    expect(wrapper.get(".cell-menu").text()).toContain("复制");
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

  it("renders the read failure and empty-table states in the unified state pattern", async () => {
    // 失败态：err 软底卡（.grid-alert）带语义图标，role=alert
    invoke.mockRejectedValue(new Error("连接已断开，读取失败"));
    const broken = mount(DataGrid, { props: { tabId: "table:states-err", draft: "", target: { ...customersTarget, table: "broken" } } });
    await flushPromises();
    const alert = broken.get("p.grid-alert[role='alert']");
    expect(alert.text()).toContain("读取失败");
    expect(alert.find("svg.i").exists()).toBe(true);
    broken.unmount();

    // 空表态：两网格共用的 .grid-empty 呈现，区别于筛选无匹配
    invoke.mockReset();
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return customerSchema();
      if (command === "grid_page") return { rows: [], truncated: false, sql: "SELECT empty" };
      throw new Error(command);
    });
    const empty = mount(DataGrid, { props: { tabId: "table:states-empty", draft: "", pageSize: 10, target: customersTarget } });
    await flushPromises();
    expect(empty.get("[data-empty='table'] td").classes()).toContain("grid-empty");
    expect(empty.get("[data-empty='table']").text()).toContain("这张表没有行");
    expect(empty.find("[data-empty='filter']").exists()).toBe(false);
    empty.unmount();
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

  it("separates an empty table from a filter miss and can clear the filter", async () => {
    invoke.mockImplementation(async (command: string, args: { request?: { filters?: unknown[] } }) => {
      if (command === "grid_describe") return customerSchema();
      if (command === "grid_page") {
        const filters = args.request?.filters ?? [];
        if (filters.length > 0) return { rows: [], truncated: false, sql: "SELECT WHERE none" };
        return { rows: [customerRow(1), customerRow(2)], truncated: false, sql: "SELECT customers" };
      }
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:shop:app:public:customers", draft: "select unfinished", pageSize: 10, target: customersTarget } });
    await flushPromises();
    expect(wrapper.find("[data-empty]").exists()).toBe(false);
    expect(wrapper.text()).toContain("客户1");
    expect(tabHasPending("table:shop:app:public:customers")).toBe(false);
    expect(anyPending()).toBe(false);
    await wrapper.get("select[aria-label='筛选列']").setValue("name");
    await wrapper.get("input[aria-label='筛选值']").setValue("不存在");
    await wrapper.get("form.filter-form").trigger("submit");
    await flushPromises();
    expect(wrapper.get("[data-empty='filter']").text()).toContain("没有匹配行");
    expect(wrapper.find("[data-empty='table']").exists()).toBe(false);
    expect(wrapper.text()).not.toContain("这张表没有行");
    expect(wrapper.text()).toContain("name");
    await wrapper.get("[data-empty='filter'] button").trigger("click");
    await flushPromises();
    expect(wrapper.find("[data-empty]").exists()).toBe(false);
    expect(wrapper.text()).toContain("客户1");
    expect(wrapper.text()).toContain("客户2");
    wrapper.unmount();
  });

  it("submits between and in filters from the toolbar form", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return customerSchema();
      if (command === "grid_page") return { rows: [customerRow(1), customerRow(5)], truncated: false, sql: "SELECT customers" };
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:filter-ops", draft: "", pageSize: 10, target: customersTarget } });
    await flushPromises();
    await wrapper.get("select[aria-label='筛选列']").setValue("id");
    await wrapper.get("select[aria-label='筛选条件']").setValue("between");
    expect(wrapper.find("input[aria-label='筛选第二个值']").exists()).toBe(true);
    await wrapper.get("input[aria-label='筛选值']").setValue("1");
    await wrapper.get("input[aria-label='筛选第二个值']").setValue("9");
    await wrapper.get("form.filter-form").trigger("submit");
    await flushPromises();
    const betweenCall = invoke.mock.calls.filter((call) => call[0] === "grid_page").at(-1);
    expect(betweenCall?.[1]).toMatchObject({ request: { filters: [{ column: "id", op: "between", value: "1", second: "9" }] } });
    expect(wrapper.text()).toContain("1 ~ 9");
    await wrapper.get("button[aria-label='移除条件 1']").trigger("click");
    await flushPromises();
    await wrapper.get("select[aria-label='筛选条件']").setValue("in");
    expect(wrapper.find("input[aria-label='筛选值列表']").exists()).toBe(true);
    await wrapper.get("input[aria-label='筛选值列表']").setValue("1, 3 , ,5");
    await wrapper.get("form.filter-form").trigger("submit");
    await flushPromises();
    const inCall = invoke.mock.calls.filter((call) => call[0] === "grid_page").at(-1);
    expect(inCall?.[1]).toMatchObject({ request: { filters: [{ column: "id", op: "in", values: ["1", "3", "5"] }] } });
    expect(wrapper.text()).toContain("1, 3, 5");
    wrapper.unmount();
  });

  it("shows an empty base table as having no rows and still allows insert", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return customerSchema();
      if (command === "grid_page") return { rows: [], truncated: false, sql: "SELECT empty" };
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:empty", draft: "", pageSize: 10, target: customersTarget } });
    await flushPromises();
    expect(wrapper.get("[data-empty='table']").text()).toContain("这张表没有行");
    expect(wrapper.text()).not.toContain("没有匹配行");
    expect(wrapper.text()).not.toContain("读取失败");
    const add = wrapper.findAll("button").find((button) => button.text() === "新增行");
    expect(add!.attributes("disabled")).toBeUndefined();
    await wrapper.get("[data-empty='table'] button").trigger("click");
    expect(wrapper.find("[data-empty]").exists()).toBe(false);
    expect(wrapper.text()).toContain("新增");
    expect(tabHasPending("table:empty")).toBe(true);
    wrapper.unmount();
    expect(tabHasPending("table:empty")).toBe(false);
    expect(anyPending()).toBe(false);
  });

  it("keeps a view read-only when it has no rows", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return { ...customerSchema(), kind: "view" as const, name: "customer_view", key: [] };
      if (command === "grid_page") return { rows: [], truncated: false, sql: "SELECT view" };
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:view", draft: "select 1", pageSize: 10, target: { ...customersTarget, table: "customer_view" } } });
    await flushPromises();
    expect(wrapper.text()).toContain("视图是只读的");
    expect(wrapper.get("[data-empty='table']").text()).toContain("这个视图没有行");
    expect(wrapper.get("[data-empty='table']").find("button").exists()).toBe(false);
    const add = wrapper.findAll("button").find((button) => button.text() === "新增行");
    expect(add!.attributes("disabled")).toBeDefined();
    expect(tabHasPending("table:view")).toBe(false);
    wrapper.unmount();
  });

  it("retries a failed read without presenting it as an empty table", async () => {
    let failed = true;
    invoke.mockImplementation(async (command: string) => {
      if (failed) throw new Error("连接已断开，读取失败");
      if (command === "grid_describe") return customerSchema();
      if (command === "grid_page") return { rows: [customerRow(1)], truncated: false, sql: "SELECT recovered" };
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:retry", draft: "", target: customersTarget } });
    await flushPromises();
    expect(wrapper.text()).toContain("读取失败");
    expect(wrapper.text()).toContain("这不是无匹配结果");
    expect(wrapper.text()).not.toContain("没有匹配行");
    expect(wrapper.text()).not.toContain("这张表没有行");
    expect(wrapper.find("[data-empty]").exists()).toBe(false);
    failed = false;
    await wrapper.findAll("button").find((button) => button.text() === "重试")!.trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("客户1");
    expect(wrapper.text()).not.toContain("读取失败");
    expect(wrapper.find("[data-empty]").exists()).toBe(false);
    wrapper.unmount();
  });

  it("shows the catalog environment on the table and its commit preview", async () => {
    replaceCatalog([{
      ...shopCatalog("postgres", "app", [{ name: "public", tables: ["customers"] }]),
      env: "prod",
      envLabel: "生产",
      envTone: "prod" as const,
    }]);
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return customerSchema();
      if (command === "grid_page") return { rows: [customerRow(1), customerRow(2)], truncated: false, sql: "SELECT" };
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:prod", draft: "select 1", pageSize: 10, target: customersTarget } });
    await flushPromises();
    expect(wrapper.get("[data-env]").text()).toBe("生产");
    expect(wrapper.find(".v-null").text()).toBe("NULL");
    expect(wrapper.find(".v-empty").text()).toBe("空字符串");
    expect(tabHasPending("table:prod")).toBe(false);
    await wrapper.get("[data-cell='1:name']").trigger("dblclick");
    await wrapper.get("input[aria-label='编辑 name']").setValue("改名");
    await wrapper.findAll("button").find((button) => button.text() === "确定")!.trigger("click");
    expect(tabHasPending("table:prod")).toBe(true);
    expect(anyPending()).toBe(true);
    await wrapper.findAll("button").find((button) => button.text() === "提交")!.trigger("click");
    const confirm = wrapper.get("[data-confirm-target]");
    expect(confirm.text()).toContain("Shop");
    expect(confirm.text()).toContain("customers");
    expect(confirm.text()).toContain("生产");
    expect(invoke.mock.calls.some((call) => call[0] === "grid_commit")).toBe(false);
    await wrapper.findAll("button").find((button) => button.text() === "取消")!.trigger("click");
    expect(invoke.mock.calls.some((call) => call[0] === "grid_commit")).toBe(false);
    expect(tabHasPending("table:prod")).toBe(true);
    await wrapper.findAll("button").find((button) => button.text() === "放弃")!.trigger("click");
    expect(tabHasPending("table:prod")).toBe(true);
    await wrapper.findAll("button").find((button) => button.text() === "确认放弃")!.trigger("click");
    expect(tabHasPending("table:prod")).toBe(false);
    expect(anyPending()).toBe(false);
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
    expect(wrapper.text()).toContain("在已读取的 100 行中查找和排序");
    expect(wrapper.text()).toContain("已读取前 100 行");
    expect(wrapper.text()).toContain("结果没有读完");
    await wrapper.get("[aria-label='在已读取行中查找']").setValue("000-first");
    expect(wrapper.find("[data-row-id]").exists()).toBe(false);
    const exportButton = wrapper.findAll("button").find((button) => button.text() === "导出");
    await exportButton!.trigger("click");
    await wrapper.get("input[value='full']").setValue("full");
    expect(wrapper.text()).toContain("只有 SELECT 可以重新执行并完整导出");
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

function shopCatalog(engine: CatalogConnection["engine"], database: string, schemas: { name: string; tables: string[] }[]): CatalogConnection {
  return {
    id: "shop",
    name: "Shop",
    engine,
    env: "dev",
    envLabel: "开发",
    envTone: "dev",
    status: "connected",
    defaultDatabase: database,
    foreignKeys: null,
    error: null,
    databases: [{
      name: database,
      error: null,
      schemas: schemas.map((schema) => ({
        name: schema.name,
        error: null,
        total: schema.tables.length,
        objects: schema.tables.map((name) => ({ name, kind: "table" as const, estimatedRows: 1, stale: false })),
      })),
    }],
  };
}

describe("query result edits", () => {
  beforeEach(() => {
    invoke.mockReset();
    resetExportTasks();
    resetCatalog();
    resetCloseGuard();
  });

  afterEach(() => {
    resetCatalog();
  });

  function proof(sql: string) {
    const parsed = readSingleTable(sql);
    expect(parsed.ok).toBe(true);
    if (!parsed.ok) throw new Error(parsed.reason);
    return parsed.table;
  }

  function rejectReason(sql: string): string {
    const parsed = readSingleTable(sql);
    return parsed.ok ? "" : parsed.reason;
  }

  it("only locates a proved single-table select that contains its primary key", () => {
    expect(readSingleTable("SELECT * FROM orders JOIN items ON items.id = orders.id").ok).toBe(false);
    expect(rejectReason("SELECT status, COUNT(*) FROM orders GROUP BY status")).toContain("聚合");
    expect(rejectReason("SELECT DISTINCT status FROM orders")).toContain("DISTINCT");
    expect(rejectReason("SELECT id FROM orders UNION SELECT id FROM archive")).toContain("UNION");
    expect(rejectReason("SELECT id FROM (SELECT id FROM orders) q")).toContain("子查询");
    expect(rejectReason("DELETE FROM orders")).toContain("SELECT");
    expect(readSingleTable("SELECT id, name FROM ONLY orders").ok).toBe(false);
    const explicitAlias = readSingleTable("SELECT id, email AS name FROM customers");
    expect(explicitAlias.ok).toBe(true);
    if (explicitAlias.ok) {
      expect(explicitAlias.columns.baseColumns).toEqual(["id"]);
      expect(explicitAlias.columns.aliasColumns).toEqual(["name"]);
    }
    const implicitAlias = readSingleTable("SELECT email name FROM customers");
    expect(implicitAlias.ok).toBe(true);
    if (implicitAlias.ok) {
      expect(implicitAlias.columns.baseColumns).toEqual([]);
      expect(implicitAlias.columns.aliasColumns).toEqual(["name"]);
    }
    const star = readSingleTable("SELECT * FROM customers c");
    expect(star.ok).toBe(true);
    if (star.ok) expect(star.columns.star).toBe(true);
    const ambiguous = resolveEditableTable(proof("SELECT status FROM orders"), [
      { schema: "public", name: "orders", kind: "table" },
      { schema: "sales", name: "orders", kind: "table" },
    ], "app");
    expect(ambiguous.ok).toBe(false);
    if (!ambiguous.ok) expect(ambiguous.reason).toContain("多个模式");
    const named = resolveEditableTable(proof("SELECT status FROM public.orders"), [], "app", "postgres");
    expect(named).toEqual({ ok: true, schema: "public", table: "orders" });
    const folded = resolveEditableTable(proof("SELECT id, name FROM orders"), [
      { schema: "public", name: "Orders", kind: "table" },
      { schema: "public", name: "orders", kind: "table" },
    ], "app", "postgres");
    expect(folded.ok).toBe(false);
    if (!folded.ok) expect(folded.reason).toContain("大小写");
    const namedFolded = resolveEditableTable(proof("SELECT id FROM public.orders"), [
      { schema: "public", name: "orders", kind: "table" },
      { schema: "public", name: "Orders", kind: "table" },
    ], "app", "postgres");
    expect(namedFolded.ok).toBe(false);
    for (const engine of ["mysql", "mariadb"] as const) {
      const foreign = resolveEditableTable(proof("SELECT id, name FROM otherdb.customers"), [
        { schema: "", name: "customers", kind: "table" },
      ], "app", engine);
      expect(foreign.ok).toBe(false);
      if (!foreign.ok) expect(foreign.reason).toContain("限定名");
      const current = resolveEditableTable(proof("SELECT id, name FROM app.customers"), [
        { schema: "", name: "customers", kind: "table" },
      ], "app", engine);
      expect(current).toEqual({ ok: true, schema: "", table: "customers" });
      const unlisted = resolveEditableTable(proof("SELECT id FROM app.customers"), [], "app", engine);
      expect(unlisted.ok).toBe(false);
      const shopVersusShop = resolveEditableTable(proof("SELECT id, name FROM Shop.customers"), [
        { schema: "", name: "customers", kind: "table" },
      ], "shop", engine);
      expect(shopVersusShop.ok).toBe(false);
      if (!shopVersusShop.ok) expect(shopVersusShop.reason).toContain("限定名");
      const upperVersusShop = resolveEditableTable(proof("SELECT id, name FROM SHOP.customers"), [
        { schema: "", name: "customers", kind: "table" },
      ], "shop", engine);
      expect(upperVersusShop.ok).toBe(false);
      const exactShop = resolveEditableTable(proof("SELECT id, name FROM shop.customers"), [
        { schema: "", name: "customers", kind: "table" },
      ], "shop", engine);
      expect(exactShop).toEqual({ ok: true, schema: "", table: "customers" });
      const exactSpelling = resolveEditableTable(proof("SELECT id, name FROM Shop.customers"), [
        { schema: "", name: "customers", kind: "table" },
      ], "Shop", engine);
      expect(exactSpelling).toEqual({ ok: true, schema: "", table: "customers" });
    }
    const threePart = readSingleTable("SELECT id, name FROM app.public.customers");
    expect(threePart.ok).toBe(false);
    if (!threePart.ok) expect(threePart.reason).toContain("两部分");
    const attached = resolveEditableTable(proof("SELECT id FROM archive.customers"), [
      { schema: "", name: "customers", kind: "table" },
    ], "main", "sqlite");
    expect(attached.ok).toBe(false);
    if (!attached.ok) expect(attached.reason).toContain("限定名");
    const mainTable = resolveEditableTable(proof("SELECT id FROM main.customers"), [
      { schema: "", name: "customers", kind: "table" },
    ], "main", "sqlite");
    expect(mainTable).toEqual({ ok: true, schema: "", table: "customers" });
    const aliasProof = readSingleTable("SELECT id, email AS name FROM customers");
    const aliasRows = gridModel({ columns: ["id", "name"], rows: [["1", "a@example.com"]] });
    const aliasLocated = locateResultRows(customersFixture(1).schema, aliasRows.columns, aliasRows.rows, aliasProof.ok ? aliasProof.columns : null);
    expect(aliasLocated.ok).toBe(true);
    if (aliasLocated.ok && aliasProof.ok) {
      expect(aliasLocated.editable).toEqual(["id"]);
      const session = new ResultEditSession(customersFixture(1).schema, customersTarget, aliasLocated.rows, aliasLocated.editable);
      expect(session.stageUpdate("r0", "name", "写进名字")).toContain("不是基表列");
      expect(session.preview()).toEqual([]);
    }
    const hiddenKey = readSingleTable("SELECT email AS id FROM customers");
    const hiddenRows = gridModel({ columns: ["id"], rows: [["a@example.com"]] });
    const hiddenLocated = locateResultRows(customersFixture(1).schema, hiddenRows.columns, hiddenRows.rows, hiddenKey.ok ? hiddenKey.columns : null);
    expect(hiddenLocated.ok).toBe(false);
    const qualifiedColumn = readSingleTable("SELECT id, customers.name FROM customers c");
    expect(qualifiedColumn.ok && qualifiedColumn.columns.baseColumns).toEqual(["id", "name"]);
    const otherColumn = readSingleTable("SELECT id, other.name FROM customers");
    expect(otherColumn.ok && otherColumn.columns.baseColumns).toEqual(["id"]);
    const starProof = readSingleTable("SELECT * FROM customers");
    const starRows = gridModel({ columns: ["id", "name"], rows: [["1", "甲"]] });
    const starLocated = locateResultRows(customersFixture(1).schema, starRows.columns, starRows.rows, starProof.ok ? starProof.columns : null);
    expect(starLocated.ok).toBe(true);
    if (starLocated.ok) expect(starLocated.editable).toEqual(["id", "name"]);
    const model = gridModel({ columns: ["id", "name"], rows: [["1", "甲"], ["1", "乙"]] });
    const missingKey = locateResultRows({ ...customersFixture(1).schema, key: [] }, model.columns, model.rows);
    expect(missingKey.ok).toBe(false);
    if (!missingKey.ok) expect(missingKey.reason).toContain("没有主键");
    const view = locateResultRows(viewFixture().schema, model.columns, model.rows);
    expect(view.ok).toBe(false);
    if (!view.ok) expect(view.reason).toContain("视图");
    const duplicated = locateResultRows(customersFixture(1).schema, model.columns, model.rows);
    expect(duplicated.ok).toBe(false);
    if (!duplicated.ok) expect(duplicated.reason).toContain("无法唯一确定");
    const unique = gridModel({ columns: ["id", "status"], rows: [["7", "open"]] });
    const located = locateResultRows(customersFixture(1).schema, unique.columns, unique.rows);
    expect(located.ok).toBe(true);
    if (located.ok) expect(located.rows[0]?.located).toBe(true);
    expect(model.rows.every((row) => !row.located)).toBe(true);
  });

  it("previews a cell change and writes it through the existing commit", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return customerSchema();
      if (command === "grid_commit") {
        return { applied: [{ id: "u:r0", sql: "UPDATE", label: "修改 r0" }], failed: null, pendingIds: [], rolledBack: false, rule: COMMIT_RULE };
      }
      throw new Error(command);
    });
    const model = gridModel({ columns: ["id", "name", "email", "note"], columnTypes: ["integer", "text", "text", "text"], rows: [["1", "客户1", "a@example.com", null]] });
    const wrapper = mount(QueryResultGrid, {
      props: {
        tabId: "query-edit",
        statement: "SELECT id, name, email, note FROM public.customers",
        columns: model.columns,
        rows: model.rows,
        truncated: false,
        connectionId: "shop",
        engine: "postgres",
        database: "app",
        schema: "public",
      },
    });
    await flushPromises();
    expect(wrapper.attributes("data-editable")).toBe("true");
    expect(wrapper.text()).toContain("查询结果不能新增行");
    expect(wrapper.findAll("button").some((button) => button.text() === "新增行")).toBe(false);
    await wrapper.get("[data-cell='r0:name']").trigger("dblclick");
    await wrapper.get("input[aria-label='编辑 name']").setValue("新名字");
    await wrapper.findAll("button").find((button) => button.text() === "确定")!.trigger("click");
    expect(wrapper.get(".pending-bar").text()).toContain("1 处更改未保存");
    await wrapper.findAll("button").find((button) => button.text() === "提交")!.trigger("click");
    expect(wrapper.get("[aria-label='待提交的更改']").text()).toContain("UPDATE");
    await wrapper.findAll("button").find((button) => button.text() === "提交 1 处更改")!.trigger("click");
    await flushPromises();
    const commit = invoke.mock.calls.find((call) => call[0] === "grid_commit");
    expect(commit?.[1]).toMatchObject({ request: { table: "customers", schema: "public", changes: [{ kind: "update", id: "u:r0" }] } });
    expect(wrapper.get("[data-cell='r0:name']").text()).toContain("新名字");
    wrapper.unmount();
  });

  it("supports delete, select-all, SQL copy and commit shortcuts on editable results", async () => {
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return customerSchema();
      throw new Error(command);
    });
    const model = gridModel({
      columns: ["id", "name", "email"],
      columnTypes: ["integer", "text", "text"],
      rows: [["1", "甲", "a@example.com"], ["2", "乙", "b@example.com"]],
    });
    const wrapper = mount(QueryResultGrid, {
      props: {
        tabId: "query-shortcuts",
        statement: "SELECT id, name, email FROM public.customers",
        columns: model.columns,
        rows: model.rows,
        truncated: false,
        connectionId: "shop",
        engine: "postgres",
        database: "app",
        schema: "public",
      },
    });
    await flushPromises();
    const scroll = wrapper.get(".grid-scroll");
    await wrapper.get("[data-cell='r0:name']").trigger("pointerdown");
    await scroll.trigger("keydown", { key: "Delete" });
    expect(wrapper.get("tr[data-row-id='r0']").classes()).toContain("row-delete");
    expect(wrapper.get(".pending-bar").text()).toContain("1 处更改未保存");

    await scroll.trigger("keydown", { key: "a", ctrlKey: true });
    expect(wrapper.findAll(".cell-btn.range")).toHaveLength(6);
    await wrapper.get("[data-cell='r0:name']").trigger("pointerdown");
    await scroll.trigger("keydown", { key: "c", ctrlKey: true, shiftKey: true });
    await flushPromises();
    expect(writeText).toHaveBeenLastCalledWith("'甲'");

    await scroll.trigger("keydown", { key: "s", ctrlKey: true });
    expect(wrapper.find("[aria-label='待提交的更改']").exists()).toBe(true);
    await wrapper.findAll("button").find((button) => button.text() === "取消")!.trigger("click");
    wrapper.unmount();
  });

  it("shows the catalog environment on a result commit and ignores unsaved SQL", async () => {
    replaceCatalog([{
      ...shopCatalog("postgres", "app", [{ name: "public", tables: ["customers"] }]),
      env: "prod",
      envLabel: "生产",
      envTone: "prod" as const,
    }]);
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return customerSchema();
      throw new Error(command);
    });
    const model = gridModel({ columns: ["id", "name", "email", "note"], columnTypes: ["integer", "text", "text", "text"], rows: [["1", "客户1", "a@example.com", null], ["2", "客户2", "b@example.com", ""]] });
    const wrapper = mount(QueryResultGrid, {
      props: {
        tabId: "query-prod",
        statement: "SELECT id, name, email, note FROM public.customers",
        columns: model.columns,
        rows: model.rows,
        truncated: false,
        connectionId: "shop",
        engine: "postgres",
        database: "app",
        schema: "public",
      },
    });
    await flushPromises();
    expect(wrapper.attributes("data-editable")).toBe("true");
    expect(tabHasPending("query-prod")).toBe(false);
    expect(anyPending()).toBe(false);
    expect(wrapper.find(".v-null").text()).toBe("NULL");
    expect(wrapper.find(".v-empty").text()).toBe("空字符串");
    await wrapper.get("[aria-label='在已读取行中查找']").setValue("不会写入");
    expect(tabHasPending("query-prod")).toBe(false);
    await wrapper.get("[aria-label='在已读取行中查找']").setValue("");
    await wrapper.get("[data-cell='r0:name']").trigger("dblclick");
    await wrapper.get("input[aria-label='编辑 name']").setValue("新名字");
    await wrapper.findAll("button").find((button) => button.text() === "确定")!.trigger("click");
    expect(tabHasPending("query-prod")).toBe(true);
    await wrapper.findAll("button").find((button) => button.text() === "提交")!.trigger("click");
    const confirm = wrapper.get("[data-confirm-target]");
    expect(confirm.text()).toContain("Shop");
    expect(confirm.text()).toContain("customers");
    expect(confirm.text()).toContain("生产");
    expect(invoke.mock.calls.some((call) => call[0] === "grid_commit")).toBe(false);
    await wrapper.findAll("button").find((button) => button.text() === "取消")!.trigger("click");
    expect(invoke.mock.calls.some((call) => call[0] === "grid_commit")).toBe(false);
    expect(tabHasPending("query-prod")).toBe(true);
    wrapper.unmount();
    expect(tabHasPending("query-prod")).toBe(false);
    expect(anyPending()).toBe(false);
  });

  it("keeps a rejected row and later staged changes unsuccessful", async () => {
    const model = gridModel({
      columns: ["id", "name"],
      rows: [["1", "甲"], ["2", "乙"], ["3", "丙"]],
    });
    const located = locateResultRows(customersFixture(1).schema, model.columns, model.rows);
    expect(located.ok).toBe(true);
    if (!located.ok) return;
    const session = new ResultEditSession(customersFixture(1).schema, customersTarget, located.rows);
    session.stageUpdate("r0", "name", "甲改");
    session.stageUpdate("r1", "name", "乙改");
    session.stageUpdate("r2", "name", "丙改");
    invoke.mockResolvedValue({
      applied: [{ id: "u:r0", sql: "UPDATE", label: "修改 r0" }],
      failed: { id: "u:r1", sql: "UPDATE", label: "修改 r1", message: "check constraint failed" },
      pendingIds: ["u:r1", "u:r2"],
      rolledBack: false,
      rule: COMMIT_RULE,
    });
    const report = await session.commit();
    expect(report?.applied).toHaveLength(1);
    expect(report?.failed?.message).toContain("check constraint");
    expect(session.cellValue("r0", "name")).toBe("甲改");
    expect(session.failures.get("u:r1")).toContain("check constraint");
    expect(session.cellValue("r1", "name")).toBe("乙改");
    expect(session.cellValue("r2", "name")).toBe("丙改");
    expect(session.pendingCount).toBe(2);
    expect(session.committedCells().map((row) => row.name)).toEqual(["甲改", "乙", "丙"]);
  });

  it("stays read-only for a join, a view, and a table without a key", async () => {
    const join = mount(QueryResultGrid, {
      props: { tabId: "join", statement: "SELECT * FROM orders JOIN items ON items.id = orders.id", columns: [], rows: [], truncated: false, connectionId: "shop", engine: "postgres", database: "app" },
    });
    expect(join.text()).toContain("JOIN");
    expect(join.attributes("data-editable")).toBe("false");
    expect(invoke).not.toHaveBeenCalled();
    join.unmount();

    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return { ...customerSchema(), name: "customer_view", kind: "view" };
      throw new Error(command);
    });
    const view = mount(QueryResultGrid, {
      props: {
        tabId: "view",
        statement: "SELECT id, name FROM public.customer_view",
        columns: gridModel({ columns: ["id", "name"], rows: [["1", "只读"]] }).columns,
        rows: gridModel({ columns: ["id", "name"], rows: [["1", "只读"]] }).rows,
        truncated: false,
        connectionId: "shop",
        engine: "postgres",
        database: "app",
      },
    });
    await flushPromises();
    expect(view.text()).toContain("视图是只读的");
    expect(view.attributes("data-editable")).toBe("false");
    view.unmount();

    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return { ...customerSchema(), key: [], columns: customerSchema().columns.map((column) => ({ ...column, primary: false })) };
      throw new Error(command);
    });
    const bare = gridModel({ columns: ["id", "name"], rows: [["1", "甲"]] });
    const nokey = mount(QueryResultGrid, {
      props: { tabId: "nokey", statement: "SELECT id, name FROM public.notes", columns: bare.columns, rows: bare.rows, truncated: false, connectionId: "shop", engine: "postgres", database: "app" },
    });
    await flushPromises();
    expect(nokey.text()).toContain("没有主键");
    await nokey.get("[data-cell='r0:name']").trigger("dblclick");
    expect(nokey.find("input[aria-label='编辑 name']").exists()).toBe(false);
    nokey.unmount();
  });

  it("does not edit a select-list alias as the base column", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return customerSchema();
      throw new Error(command);
    });
    const model = gridModel({ columns: ["id", "name"], columnTypes: ["integer", "text"], rows: [["1", "a@example.com"]] });
    const wrapper = mount(QueryResultGrid, {
      props: {
        tabId: "alias",
        statement: "SELECT id, email AS name FROM public.customers",
        columns: model.columns,
        rows: model.rows,
        truncated: false,
        connectionId: "shop",
        engine: "postgres",
        database: "app",
      },
    });
    await flushPromises();
    expect(wrapper.attributes("data-editable")).toBe("true");
    await wrapper.get("[data-cell='r0:name']").trigger("dblclick");
    expect(wrapper.find("input[aria-label='编辑 name']").exists()).toBe(false);
    await wrapper.get("[data-cell='r0:id']").trigger("dblclick");
    expect(wrapper.find("input[aria-label='编辑 id']").exists()).toBe(true);
    wrapper.unmount();

    const implicit = mount(QueryResultGrid, {
      props: {
        tabId: "implicit-alias",
        statement: "SELECT id, email name FROM public.customers",
        columns: model.columns,
        rows: model.rows,
        truncated: false,
        connectionId: "shop",
        engine: "postgres",
        database: "app",
      },
    });
    await flushPromises();
    expect(implicit.attributes("data-editable")).toBe("true");
    await implicit.get("[data-cell='r0:name']").trigger("dblclick");
    expect(implicit.find("input[aria-label='编辑 name']").exists()).toBe(false);
    implicit.unmount();
  });

  it("exports loaded alias columns as JSON and INSERT", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return customerSchema();
      throw new Error(command);
    });
    const model = gridModel({ columns: ["customer_name"], rows: [["甲"]] });
    const wrapper = mount(QueryResultGrid, {
      props: {
        tabId: "alias-export",
        statement: "SELECT name AS customer_name FROM public.customers",
        columns: model.columns,
        rows: model.rows,
        truncated: false,
        connectionId: "shop",
        engine: "postgres",
        database: "app",
        schema: "sales",
      },
    });
    await flushPromises();
    expect(wrapper.attributes("data-editable")).toBe("false");
    await wrapper.findAll("button").find((button) => button.text() === "导出")!.trigger("click");
    await wrapper.get("input[value='json']").setValue(true);
    expect(wrapper.get("pre.ex-preview").text()).toContain("customer_name");
    expect(wrapper.get("pre.ex-preview").text()).toContain("甲");
    await wrapper.get("input[value='insert']").setValue(true);
    const preview = wrapper.get("pre.ex-preview").text();
    expect(preview).toContain('INSERT INTO "public"."customers"');
    expect(preview).toContain('"customer_name"');
    expect(preview).toContain("'甲'");
    expect(preview).not.toContain("DEFAULT VALUES");
    expect(preview).not.toContain('"sales"');
    wrapper.unmount();
  });

  it("clears the insert target when one table can no longer be proved", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return customerSchema();
      throw new Error(command);
    });
    const model = gridModel({ columns: ["id", "name"], rows: [["1", "甲"]] });
    const wrapper = mount(QueryResultGrid, {
      props: {
        tabId: "switch",
        statement: "SELECT id, name FROM public.customers",
        columns: model.columns,
        rows: model.rows,
        truncated: false,
        connectionId: "shop",
        engine: "postgres",
        database: "app",
        schema: "public",
      },
    });
    await flushPromises();
    await wrapper.findAll("button").find((button) => button.text() === "导出")!.trigger("click");
    await wrapper.get("input[value='insert']").setValue(true);
    expect(wrapper.get("pre.ex-preview").text()).toContain('INSERT INTO "public"."customers"');
    await wrapper.setProps({ statement: "SELECT id, name FROM orders JOIN items ON items.id = orders.id" });
    await flushPromises();
    expect(wrapper.attributes("data-editable")).toBe("false");
    expect(wrapper.text()).toContain("JOIN");
    expect(wrapper.get("pre.ex-preview").text()).not.toContain("INSERT INTO");
    expect(wrapper.findAll("button").filter((button) => button.text() === "导出").at(-1)!.attributes("disabled")).toBeDefined();
    wrapper.unmount();

    invoke.mockClear();
    replaceCatalog([shopCatalog("postgres", "app", [
      { name: "public", tables: ["customers"] },
      { name: "sales", tables: ["customers"] },
    ])]);
    const ambiguous = mount(QueryResultGrid, {
      props: {
        tabId: "ambiguous",
        statement: "SELECT id, name FROM customers",
        columns: model.columns,
        rows: model.rows,
        truncated: false,
        connectionId: "shop",
        engine: "postgres",
        database: "app",
        schema: "sales",
      },
    });
    await flushPromises();
    expect(invoke.mock.calls.some((call) => call[0] === "grid_describe")).toBe(false);
    expect(ambiguous.attributes("data-editable")).toBe("false");
    expect(ambiguous.text()).toContain("多个模式");
    await ambiguous.findAll("button").find((button) => button.text() === "导出")!.trigger("click");
    await ambiguous.get("input[value='insert']").setValue(true);
    expect(ambiguous.get("pre.ex-preview").text()).not.toContain("INSERT");
    expect(ambiguous.text()).not.toContain('"sales"');
    ambiguous.unmount();
  });

  it("stays read-only for case-variant tables and ONLY", async () => {
    replaceCatalog([shopCatalog("postgres", "app", [{ name: "public", tables: ["Orders", "orders", "only"] }])]);
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return { ...customerSchema(), name: "Orders" };
      throw new Error(command);
    });
    const model = gridModel({ columns: ["id", "name"], rows: [["1", "甲"]] });
    const folded = mount(QueryResultGrid, {
      props: {
        tabId: "case",
        statement: "SELECT id, name FROM orders",
        columns: model.columns,
        rows: model.rows,
        truncated: false,
        connectionId: "shop",
        engine: "postgres",
        database: "app",
        schema: "public",
      },
    });
    await flushPromises();
    expect(invoke).not.toHaveBeenCalled();
    expect(folded.attributes("data-editable")).toBe("false");
    expect(folded.text()).toContain("大小写");
    folded.unmount();

    const only = mount(QueryResultGrid, {
      props: {
        tabId: "only",
        statement: "SELECT id, name FROM ONLY orders",
        columns: model.columns,
        rows: model.rows,
        truncated: false,
        connectionId: "shop",
        engine: "postgres",
        database: "app",
      },
    });
    await flushPromises();
    expect(invoke).not.toHaveBeenCalled();
    expect(only.attributes("data-editable")).toBe("false");
    expect(only.text()).toContain("不能确定");
    only.unmount();
  });

  it("does not describe or insert the current table for another database qualifier", async () => {
    const model = gridModel({ columns: ["id", "name"], rows: [["1", "甲"]] });
    for (const engine of ["mysql", "mariadb"] as const) {
      invoke.mockReset();
      resetCatalog();
      replaceCatalog([shopCatalog(engine, "app", [{ name: "", tables: ["customers"] }])]);
      invoke.mockImplementation(async (command: string) => {
        if (command === "grid_describe") return { ...customerSchema(), engine, schema: "" };
        throw new Error(command);
      });
      const wrapper = mount(QueryResultGrid, {
        props: {
          tabId: `foreign-${engine}`,
          statement: "SELECT id, name FROM otherdb.customers",
          columns: model.columns,
          rows: model.rows,
          truncated: false,
          connectionId: "shop",
          engine,
          database: "app",
        },
      });
      await flushPromises();
      expect(invoke).not.toHaveBeenCalled();
      expect(wrapper.attributes("data-editable")).toBe("false");
      expect(wrapper.text()).toContain("限定名");
      await wrapper.findAll("button").find((button) => button.text() === "导出")!.trigger("click");
      await wrapper.get("input[value='insert']").setValue(true);
      expect(wrapper.get("pre.ex-preview").text()).not.toContain("INSERT");
      wrapper.unmount();
    }

    invoke.mockReset();
    resetCatalog();
    replaceCatalog([shopCatalog("sqlite", "main", [{ name: "", tables: ["customers"] }])]);
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return { ...customerSchema(), engine: "sqlite", schema: "" };
      throw new Error(command);
    });
    const attached = mount(QueryResultGrid, {
      props: {
        tabId: "archive",
        statement: "SELECT id, name FROM archive.customers",
        columns: model.columns,
        rows: model.rows,
        truncated: false,
        connectionId: "shop",
        engine: "sqlite",
        database: "main",
      },
    });
    await flushPromises();
    expect(invoke).not.toHaveBeenCalled();
    expect(attached.attributes("data-editable")).toBe("false");
    expect(attached.text()).toContain("限定名");
    attached.unmount();

    invoke.mockReset();
    replaceCatalog([shopCatalog("mysql", "app", [{ name: "", tables: ["customers"] }])]);
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return { ...customerSchema(), engine: "mysql", schema: "", name: "customers" };
      throw new Error(command);
    });
    const current = mount(QueryResultGrid, {
      props: {
        tabId: "current-db",
        statement: "SELECT id, name FROM app.customers",
        columns: model.columns,
        rows: model.rows,
        truncated: false,
        connectionId: "shop",
        engine: "mysql",
        database: "app",
      },
    });
    await flushPromises();
    expect(invoke.mock.calls.find((call) => call[0] === "grid_describe")?.[1]).toMatchObject({
      request: { database: "app", schema: "", table: "customers", engine: "mysql" },
    });
    expect(current.attributes("data-editable")).toBe("true");
    current.unmount();
  });

  it("keeps a database qualifier read-only unless its original spelling matches", async () => {
    const model = gridModel({ columns: ["id", "name"], rows: [["1", "甲"]] });
    const cases = [
      { database: "shop", qualifier: "Shop", editable: false },
      { database: "shop", qualifier: "SHOP", editable: false },
      { database: "shop", qualifier: "shop", editable: true },
      { database: "Shop", qualifier: "Shop", editable: true },
    ] as const;
    for (const engine of ["mysql", "mariadb"] as const) {
      for (const item of cases) {
        invoke.mockReset();
        resetCatalog();
        replaceCatalog([shopCatalog(engine, item.database, [{ name: "", tables: ["customers"] }])]);
        invoke.mockImplementation(async (command: string) => {
          if (command === "grid_describe") return { ...customerSchema(), engine, schema: "", database: item.database, name: "customers" };
          throw new Error(command);
        });
        const wrapper = mount(QueryResultGrid, {
          props: {
            tabId: `${engine}-${item.database}-${item.qualifier}`,
            statement: `SELECT id, name FROM ${item.qualifier}.customers`,
            columns: model.columns,
            rows: model.rows,
            truncated: false,
            connectionId: "shop",
            engine,
            database: item.database,
          },
        });
        await flushPromises();
        const described = invoke.mock.calls.find((call) => call[0] === "grid_describe");
        if (!item.editable) {
          expect(described).toBeUndefined();
          expect(wrapper.attributes("data-editable")).toBe("false");
          expect(wrapper.text()).toContain("限定名");
          await wrapper.findAll("button").find((button) => button.text() === "导出")!.trigger("click");
          await wrapper.get("input[value='insert']").setValue(true);
          expect(wrapper.get("pre.ex-preview").text()).not.toContain("INSERT");
        } else {
          expect(described?.[1]).toMatchObject({
            request: { database: item.database, schema: "", table: "customers", engine },
          });
          expect(wrapper.attributes("data-editable")).toBe("true");
          await wrapper.findAll("button").find((button) => button.text() === "导出")!.trigger("click");
          await wrapper.get("input[value='insert']").setValue(true);
          expect(wrapper.get("pre.ex-preview").text()).toContain("INSERT INTO `customers`");
        }
        wrapper.unmount();
      }
    }
  });

  it("stays read-only for a three-part PostgreSQL name and drops an unproved insert target", async () => {
    replaceCatalog([shopCatalog("postgres", "app", [
      { name: "app", tables: ["public"] },
      { name: "public", tables: ["customers"] },
    ])]);
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return { ...customerSchema(), schema: "app", name: "public" };
      throw new Error(command);
    });
    const model = gridModel({ columns: ["id", "name"], rows: [["1", "甲"]] });
    const triple = mount(QueryResultGrid, {
      props: {
        tabId: "three-part",
        statement: "SELECT id, name FROM app.public.customers",
        columns: model.columns,
        rows: model.rows,
        truncated: false,
        connectionId: "shop",
        engine: "postgres",
        database: "app",
        schema: "public",
      },
    });
    await flushPromises();
    expect(invoke).not.toHaveBeenCalled();
    expect(triple.attributes("data-editable")).toBe("false");
    expect(triple.text()).toContain("两部分");
    await triple.findAll("button").find((button) => button.text() === "导出")!.trigger("click");
    await triple.get("input[value='insert']").setValue(true);
    expect(triple.get("pre.ex-preview").text()).not.toContain("INSERT");
    expect(triple.text()).not.toContain('"app"."public"');
    triple.unmount();

    invoke.mockReset();
    invoke.mockRejectedValue(new Error("找不到表"));
    const failed = mount(QueryResultGrid, {
      props: {
        tabId: "describe-failed",
        statement: "SELECT id, name FROM public.customers",
        columns: model.columns,
        rows: model.rows,
        truncated: false,
        connectionId: "shop",
        engine: "postgres",
        database: "app",
        schema: "public",
      },
    });
    await flushPromises();
    expect(failed.attributes("data-editable")).toBe("false");
    expect(failed.text()).toContain("找不到表");
    await failed.findAll("button").find((button) => button.text() === "导出")!.trigger("click");
    await failed.get("input[value='insert']").setValue(true);
    expect(failed.get("pre.ex-preview").text()).not.toContain("INSERT");
    failed.unmount();
  });

  it("keeps identifiers around a comment in a qualifier and clears an unproved insert target", async () => {
    const block = readSingleTable("SELECT id, name FROM app.public/*c*/.customers");
    expect(block.ok).toBe(false);
    if (!block.ok) expect(block.reason).toContain("两部分");
    const line = readSingleTable("SELECT id, name FROM app.public--c\n.customers");
    expect(line.ok).toBe(false);
    if (!line.ok) expect(line.reason).toContain("两部分");
    expect(proof("SELECT id, name FROM public/*c*/.customers")).toEqual({
      schema: "public",
      table: "customers",
      qualifierSource: "public",
    });
    expect(proof("SELECT id, name FROM public--c\n.customers")).toEqual({
      schema: "public",
      table: "customers",
      qualifierSource: "public",
    });
    const aliased = readSingleTable("SELECT c.id FROM public/*c*/.customers c");
    expect(aliased.ok && aliased.columns.baseColumns).toEqual(["id"]);

    replaceCatalog([shopCatalog("postgres", "app", [
      { name: "app", tables: ["public"] },
      { name: "public", tables: ["customers"] },
    ])]);
    const model = gridModel({ columns: ["id", "name"], rows: [["1", "甲"]] });
    for (const [index, statement] of [
      "SELECT id, name FROM app.public/*c*/.customers",
      "SELECT id, name FROM app.public--c\n.customers",
    ].entries()) {
      invoke.mockReset();
      invoke.mockImplementation(async (command: string) => {
        if (command === "grid_describe") return { ...customerSchema(), schema: "app", name: "public" };
        throw new Error(command);
      });
      const wrapper = mount(QueryResultGrid, {
        props: {
          tabId: `comment-three-${index}`,
          statement,
          columns: model.columns,
          rows: model.rows,
          truncated: false,
          connectionId: "shop",
          engine: "postgres",
          database: "app",
          schema: "public",
        },
      });
      await flushPromises();
      expect(invoke).not.toHaveBeenCalled();
      expect(wrapper.attributes("data-editable")).toBe("false");
      expect(wrapper.text()).toContain("两部分");
      await wrapper.findAll("button").find((button) => button.text() === "导出")!.trigger("click");
      await wrapper.get("input[value='insert']").setValue(true);
      expect(wrapper.get("pre.ex-preview").text()).not.toContain("INSERT");
      expect(wrapper.text()).not.toContain('"app"."public"');
      wrapper.unmount();
    }

    for (const engine of ["mysql", "mariadb"] as const) {
      const commentedShop = proof("SELECT id, name FROM Shop/*c*/.customers");
      expect(commentedShop).toEqual({ schema: "shop", table: "customers", qualifierSource: "Shop" });
      const mismatched = resolveEditableTable(commentedShop, [
        { schema: "", name: "customers", kind: "table" },
      ], "shop", engine);
      expect(mismatched.ok).toBe(false);
      if (!mismatched.ok) expect(mismatched.reason).toContain("限定名");
      const exactShop = resolveEditableTable(proof("SELECT id, name FROM shop.customers"), [
        { schema: "", name: "customers", kind: "table" },
      ], "shop", engine);
      expect(exactShop).toEqual({ ok: true, schema: "", table: "customers" });
      const commentedExact = resolveEditableTable(proof("SELECT id, name FROM shop/*c*/.customers"), [
        { schema: "", name: "customers", kind: "table" },
      ], "shop", engine);
      expect(commentedExact).toEqual({ ok: true, schema: "", table: "customers" });

      invoke.mockReset();
      resetCatalog();
      replaceCatalog([shopCatalog(engine, "shop", [{ name: "", tables: ["customers"] }])]);
      invoke.mockImplementation(async (command: string) => {
        if (command === "grid_describe") return { ...customerSchema(), engine, schema: "", database: "shop", name: "customers" };
        throw new Error(command);
      });
      const foreign = mount(QueryResultGrid, {
        props: {
          tabId: `comment-shop-${engine}`,
          statement: "SELECT id, name FROM Shop/*c*/.customers",
          columns: model.columns,
          rows: model.rows,
          truncated: false,
          connectionId: "shop",
          engine,
          database: "shop",
        },
      });
      await flushPromises();
      expect(invoke).not.toHaveBeenCalled();
      expect(foreign.attributes("data-editable")).toBe("false");
      expect(foreign.text()).toContain("限定名");
      await foreign.findAll("button").find((button) => button.text() === "导出")!.trigger("click");
      await foreign.get("input[value='insert']").setValue(true);
      expect(foreign.get("pre.ex-preview").text()).not.toContain("INSERT");
      foreign.unmount();

      invoke.mockReset();
      invoke.mockImplementation(async (command: string) => {
        if (command === "grid_describe") return { ...customerSchema(), engine, schema: "", database: "shop", name: "customers" };
        throw new Error(command);
      });
      const current = mount(QueryResultGrid, {
        props: {
          tabId: `comment-shop-exact-${engine}`,
          statement: "SELECT id, name FROM shop/*c*/.customers",
          columns: model.columns,
          rows: model.rows,
          truncated: false,
          connectionId: "shop",
          engine,
          database: "shop",
        },
      });
      await flushPromises();
      expect(invoke.mock.calls.find((call) => call[0] === "grid_describe")?.[1]).toMatchObject({
        request: { database: "shop", schema: "", table: "customers", engine },
      });
      expect(current.attributes("data-editable")).toBe("true");
      await current.findAll("button").find((button) => button.text() === "导出")!.trigger("click");
      await current.get("input[value='insert']").setValue(true);
      expect(current.get("pre.ex-preview").text()).toContain("INSERT INTO `customers`");
      current.unmount();
    }
  });

  it("does not join identifiers that a comment alone separates, and clears the insert target", async () => {
    const line = readSingleTable("SELECT id, name FROM public.cu--\nstomers");
    expect(line.ok).toBe(false);
    if (!line.ok) expect(line.reason).toContain("不能确定");
    const block = readSingleTable("SELECT id, name FROM cu/*x*/stomers");
    expect(block.ok).toBe(false);
    if (!block.ok) expect(block.reason).toContain("不能确定");
    expect(proof("SELECT id, name FROM public.customers--\nWHERE id = 1")).toEqual({
      schema: "public",
      table: "customers",
      qualifierSource: "public",
    });
    expect(proof("SELECT id, name FROM shop.customers")).toEqual({
      schema: "shop",
      table: "customers",
      qualifierSource: "shop",
    });

    replaceCatalog([shopCatalog("postgres", "app", [{ name: "public", tables: ["customers"] }])]);
    const model = gridModel({ columns: ["id", "name"], rows: [["1", "甲"]] });
    for (const [index, statement] of [
      "SELECT id, name FROM public.cu--\nstomers",
      "SELECT id, name FROM cu/*x*/stomers",
    ].entries()) {
      invoke.mockReset();
      invoke.mockImplementation(async (command: string) => {
        if (command === "grid_describe") return customerSchema();
        throw new Error(command);
      });
      const wrapper = mount(QueryResultGrid, {
        props: {
          tabId: `comment-join-${index}`,
          statement,
          columns: model.columns,
          rows: model.rows,
          truncated: false,
          connectionId: "shop",
          engine: "postgres",
          database: "app",
          schema: "public",
        },
      });
      await flushPromises();
      expect(invoke).not.toHaveBeenCalled();
      expect(wrapper.attributes("data-editable")).toBe("false");
      expect(wrapper.text()).toContain("不能确定");
      await wrapper.findAll("button").find((button) => button.text() === "导出")!.trigger("click");
      await wrapper.get("input[value='insert']").setValue(true);
      expect(wrapper.get("pre.ex-preview").text()).not.toContain("INSERT");
      expect(wrapper.text()).not.toContain('"public"."customers"');
      wrapper.unmount();
    }
  });
});

describe("query result keyboard navigation", () => {
  beforeEach(() => {
    invoke.mockReset();
    resetExportTasks();
    resetCatalog();
    resetCloseGuard();
  });

  function mountReadOnly(keys: string) {
    const model = gridModel({ columns: ["id", "name"], rows: [["1", "甲"], ["2", "乙"]] });
    const wrapper = mount(QueryResultGrid, {
      props: {
        tabId: keys,
        statement: "SELECT * FROM orders JOIN items ON items.id = orders.id",
        columns: model.columns,
        rows: model.rows,
        truncated: false,
      },
    });
    return wrapper;
  }

  it("moves the selected cell with arrows and Tab, wrapping at the edges", async () => {
    const wrapper = mountReadOnly("keys-move");
    await flushPromises();
    expect(wrapper.attributes("data-editable")).toBe("false");
    await wrapper.get("[data-cell='r0:id']").trigger("pointerdown");
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("r0:id");
    const scroll = wrapper.get(".grid-scroll");
    await scroll.trigger("keydown", { key: "ArrowRight" });
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("r0:name");
    await scroll.trigger("keydown", { key: "ArrowDown" });
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("r1:name");
    await scroll.trigger("keydown", { key: "Tab" });
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("r0:id");
    await scroll.trigger("keydown", { key: "ArrowLeft" });
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("r1:name");
    await scroll.trigger("keydown", { key: "ArrowUp" });
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("r0:name");
    wrapper.unmount();
  });

  it("does not start editing a read-only session with Enter or F2", async () => {
    const wrapper = mountReadOnly("keys-enter");
    await flushPromises();
    await wrapper.get("[data-cell='r0:id']").trigger("pointerdown");
    await wrapper.get(".grid-scroll").trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect(wrapper.find("input[aria-label='编辑 id']").exists()).toBe(false);
    await wrapper.get(".grid-scroll").trigger("keydown", { key: "F2" });
    await flushPromises();
    expect(wrapper.find("input[aria-label='编辑 id']").exists()).toBe(false);
    expect(wrapper.attributes("data-editable")).toBe("false");
    wrapper.unmount();
  });

  it("still copies the selected cell with Ctrl+C", async () => {
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    const wrapper = mountReadOnly("keys-copy");
    await flushPromises();
    await wrapper.get("[data-cell='r1:name']").trigger("pointerdown");
    await wrapper.get(".grid-scroll").trigger("keydown", { ctrlKey: true, key: "c" });
    await flushPromises();
    expect(writeText).toHaveBeenCalledWith("乙");
    expect(wrapper.text()).toContain("已复制");
    wrapper.unmount();
  });
});
