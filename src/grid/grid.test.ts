import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import DataGrid from "./DataGrid.vue";
import { commitChanges, queryPage, viewLoadedRows } from "./data";
import { copyPlain, copySqlValue, showCell } from "./display";
import { customersFixture, nokeyFixture, viewFixture } from "./fixture";
import QueryResultGrid from "./QueryResultGrid.vue";
import { TableSession } from "./session";
import { selectExportGate } from "./sql";
import { COMMIT_RULE } from "./types";

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

describe("grid views", () => {
  it("shows a truncated batch, nulls and the server scope for a table tab", async () => {
    const wrapper = mount(DataGrid, { props: { tabId: "table:customers", draft: "" } });
    expect(wrapper.attributes("data-tab-id")).toBe("table:customers");
    expect(wrapper.attributes("data-draft")).toBe("");
    expect(wrapper.text()).toContain("已读取");
    expect(wrapper.text()).toContain("100");
    expect(wrapper.text()).toContain("还有未加载数据");
    expect(wrapper.text()).toContain("服务器端 · 整张表");
    expect(wrapper.find(".v-null").text()).toBe("NULL");
    expect(wrapper.text()).toContain("空字符串");
    expect(wrapper.text()).not.toContain("长".repeat(200));
    await wrapper.get("button[aria-label='继续读取']").trigger("click");
    expect(wrapper.attributes("data-loaded")).toBe("130");
    expect(wrapper.attributes("data-truncated")).toBe("false");
    wrapper.unmount();
  });

  it("disables changes for rows that cannot be located", () => {
    const wrapper = mount(DataGrid, { props: { tabId: "table:nokey", draft: "x" } });
    const add = wrapper.findAll("button").find((button) => button.text() === "新增行");
    const remove = wrapper.findAll("button").find((button) => button.text() === "删除行");
    expect(add!.attributes("disabled")).toBeUndefined();
    expect(remove!.attributes("disabled")).toBeDefined();
    expect(wrapper.text()).toContain("无主键 · 只能新增");
    wrapper.unmount();
  });

  it("shows a read failure instead of no matches", () => {
    const wrapper = mount(DataGrid, { props: { tabId: "table:broken", draft: "" } });
    expect(wrapper.text()).toContain("读取失败");
    expect(wrapper.text()).toContain("这不是无匹配结果");
    expect(wrapper.text()).not.toContain("没有匹配行");
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
});
