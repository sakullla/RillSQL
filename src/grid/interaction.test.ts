import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { flushPromises, mount } from "@vue/test-utils";
import { nextTick } from "vue";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { resetExportTasks } from "../export/tasks";
import { resetCatalog } from "../catalog/store";
import { resetCloseGuard } from "../shell/close-guard";
import { toasts } from "../ui/toast";
import { rectangleSelection, selectionTsv, toTsv } from "./clipboard";
import { copyCellText, showCell } from "./display";
import DataGrid from "./DataGrid.vue";
import { customersFixture, viewFixture } from "./fixture";
import QueryResultGrid from "./QueryResultGrid.vue";
import { readSingleTable } from "./result-edit";
import { TableSession } from "./session";
import { selectExportGate, splitSql, sqlCell } from "./sql";
import type { TableTarget } from "./target";
import { rejectWrite, typeCaption } from "./typeset";
import type { Column, Row } from "./types";

const invoke = vi.hoisted(() => vi.fn());
const continueTabRun = vi.hoisted(() => vi.fn(async () => undefined));
const tabRun = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("../query/runtime", () => ({ continueTabRun, tabRun }));

const writeText = vi.fn(async () => undefined);

function column(name: string, typeName: string, flags: Partial<Column> = {}): Column {
  return {
    name,
    typeName,
    nullable: flags.nullable ?? true,
    primary: flags.primary ?? false,
    unique: flags.unique ?? false,
    auto: flags.auto ?? false,
  };
}

describe("grid display, copy and typed literals", () => {
  it("quotes only fields that contain a separator and keeps the null token", () => {
    expect(rectangleSelection([{ id: "1" }, { id: "2" }], [{ name: "a" }, { name: "b" }], { row: "2", column: "b" }, { row: "1", column: "a" }).map((point) => `${point.row}:${point.column}`)).toEqual(["1:a", "1:b", "2:a", "2:b"]);
    expect(rectangleSelection([{ id: "1" }], [{ name: "a" }], { row: "1", column: "a" }, { row: "missing", column: "a" })).toEqual([]);
    expect(toTsv([["NULL", ""], ["a\tb", "say \"hi\"\n"]])).toBe("NULL\t\n\"a\tb\"\t\"say \"\"hi\"\"\n\"");
    expect(toTsv([["\\N", ""]])).toBe("\\N\t");
    const text = selectionTsv([{ row: "1", column: "n" }, { row: "1", column: "e" }], (point) => copyCellText(point.column === "n" ? null : "", "empty"));
    expect(text).toBe("\t");
  });

  it("shows type names and keeps writeback inside the column family", () => {
    expect(showCell("1", { column: column("flag", "boolean") }).label).toBe("TRUE");
    expect(showCell("0", { column: column("flag", "bool") }).label).toBe("FALSE");
    expect(showCell("0", { column: column("n", "tinyint") }).label).toBe("0");
    expect(showCell("1", { column: column("n", "integer") }).label).toBe("1");
    expect(typeCaption(column("n", "tinyint"))).toBe("tinyint");
    expect(typeCaption(column("n", "integer"))).toBe("integer");
    expect(showCell("1.2300", { column: column("amount", "DECIMAL(10,4)") }).label).toBe("1.2300");
    expect(showCell("1.5", { column: column("score", "double precision") }).label).toBe("1.5 · 近似");
    expect(showCell("2024-05-06 00:00:00", { column: column("day", "date") }).label).toBe("2024-05-06");
    expect(showCell("2024-05-06 13:14:15", { column: column("clock", "time") }).label).toBe("13:14:15");
    expect(showCell("2024-05-06 13:14:15", { column: column("stamp", "datetime") }).label).toBe("2024-05-06 13:14:15");
    expect(showCell("2024-05-06 00:00:00", { column: column("mystery", "") }).label).toBe("2024-05-06 00:00:00");
    expect(typeCaption(column("mystery", ""))).toBe("类型未知");
    expect(typeCaption(column("mystery", "类型未知"))).toBe("类型未知");
    expect(rejectWrite(column("mystery", ""), "x")).toContain("类型未知");
    expect(rejectWrite(column("mystery", "类型未知"), null)).toContain("类型未知");
    expect(rejectWrite(column("shape", "geometry"), null)).toContain("几何");
    expect(rejectWrite(column("raw", "interval"), null)).toContain("不能判断写回方式");
    expect(showCell("{\"a\":1}", { column: column("doc", "json") }).label).toBe("{\"a\":1}");
    expect(copyCellText("{\"a\":1}", "NULL", column("doc", "json"))).toBe("{\"a\":1}");
    expect(copyCellText("6f4d", "NULL", column("id", "uuid"))).toBe("6f4d");
    const binary = showCell("abcd", { column: column("blob", "bytea") });
    expect(binary.label).toBe("二进制 · 2 字节");
    expect(binary.body).toBe("");
    expect(binary.label).not.toContain("\uFFFD");
    expect(showCell("abcd", { column: column("blob", "bytea"), opened: true }).body).toBe("abcd");
    expect(showCell("\u0001b:4", { column: column("blob", "bytea") }).label).toBe("二进制 · 4 字节");
    expect(copyCellText("abcd", "NULL", column("blob", "bytea"))).toBe("abcd");
    expect(copyCellText("\u0001b:4", "NULL", column("blob", "bytea"))).toBe("");
    expect(copyCellText("\u0001b:4", "NULL", column("note", "text"))).toBe("\u0001b:4");
    expect(selectionTsv([{ row: "1", column: "note" }, { row: "1", column: "blob" }], (point) => copyCellText("\u0001b:4", "empty", column(point.column, point.column === "blob" ? "bytea" : "text")))).toBe("\u0001b:4\t");
    expect(rejectWrite(column("blob", "bytea"), "zz")).toBe("这不是十六进制");
    expect(rejectWrite(column("note", "text"), "\u0001t:11:hello")).toBeNull();
    expect(copyCellText("\u0001t:11:hello", "NULL", column("note", "text"))).toBe("\u0001t:11:hello");
    expect(showCell("\u0001t:11:hello", { column: column("note", "text") }).label).toBe("\u0001t:11:hello");
    const synthetic = `\u0001t:3000:${"a".repeat(1024)}`;
    expect(rejectWrite(column("note", "text"), synthetic)).toBe("预览内容不能写回");
    expect(rejectWrite(column("note", "text"), synthetic, true)).toBeNull();
    const openedOriginal = showCell(synthetic, { column: column("note", "text"), opened: true, resolved: true });
    expect(openedOriginal.body).toBe(synthetic);
    expect(openedOriginal.label).toBe(synthetic);
    expect(copyCellText(synthetic, "NULL", column("note", "text"), true)).toBe(synthetic);
    expect(showCell(synthetic, { column: column("note", "text"), opened: true }).body).toBe("a".repeat(1024));
    expect(sqlCell(column("flag", "boolean"), "yes", "mysql")).toBe("TRUE");
    expect(sqlCell(column("blob", "bytea"), "0A0B", "postgres")).toBe("'\\x0a0b'");
    expect(sqlCell(column("blob", "blob"), "0A0B", "mysql")).toBe("X'0a0b'");
    const envelope = showCell(`\u0001t:6000:${"长".repeat(400)}`);
    expect(envelope.kind).toBe("large");
    expect(envelope.label).toContain("6000");
    expect(envelope.label).toContain("字节");
    expect(envelope.body.length).toBeLessThan(400);
    expect(envelope.label).not.toContain("\uFFFD");
    expect(showCell(null).label).not.toBe(showCell("").label);

    const store = customersFixture(2);
    store.schema.columns.push(column("shape", "geometry", { nullable: true }));
    store.schema.columns.push(column("blob", "bytea", { nullable: true }));
    const session = new TableSession(store, 10);
    expect(session.stageUpdate("1", "shape", null)).toContain("几何");
    expect(session.stageUpdate("1", "blob", "zz")).toContain("十六进制");
    expect(session.stageUpdate("1", "note", "\u0001t:11:hello")).toBeNull();
    session.discard();
    expect(session.stageUpdate("1", "note", `\u0001t:3000:${"a".repeat(1024)}`)).toBe("预览内容不能写回");
    expect(session.pendingCount).toBe(0);
    const located = session.rowById("1");
    if (located) located.resolved = true;
    expect(session.stageUpdate("1", "note", `\u0001t:3000:${"a".repeat(1024)}`)).toBeNull();
    session.discard();
    const view = new TableSession(viewFixture(), 10);
    expect(view.stageUpdate("1", "name", "改")).toContain("不能修改");
    expect(view.pendingCount).toBe(0);

    expect(selectExportGate("SELECT $$a;b$$", "postgres").ok).toBe(true);
    expect(splitSql("SELECT $$a;b$$", "postgres")).toHaveLength(1);
    expect(splitSql("SELECT 1 # c;\n", "mysql")).toHaveLength(1);
    expect(splitSql("SELECT 1; # comment\nSELECT 2", "mysql")).toHaveLength(2);
    expect(selectExportGate("SELECT 1; # comment\nSELECT 2", "mysql").ok).toBe(false);
    expect(splitSql("SELECT 1 # c;\nSELECT 2", "mysql")).toHaveLength(1);
    expect(splitSql("SELECT 1 # c;\nSELECT 2", "postgres")).toHaveLength(2);
    expect(readSingleTable("SELECT id FROM customers # ;\nWHERE id = 1", "mysql").ok).toBe(true);
    expect(readSingleTable("SELECT id FROM customers # ;\nWHERE id = 1", "postgres").ok).toBe(false);
  });
});

const target: TableTarget = {
  connectionId: "shop",
  database: "app",
  schema: "public",
  table: "docs",
  engine: "postgres",
};

function schema() {
  return {
    engine: "postgres",
    database: "app",
    schema: "public",
    name: "docs",
    kind: "table",
    key: ["id"],
    columns: [
      column("id", "integer", { primary: true, nullable: false, auto: true }),
      column("note", "text", { nullable: true }),
      column("blob", "bytea", { nullable: true }),
      column("shape", "geometry", { nullable: true }),
    ],
  };
}

function row(): { id: string; cells: Record<string, string | null>; located: boolean; locatorColumns: string[]; locatorValues: string[] } {
  return {
    id: "1",
    cells: { id: "1", note: `\u0001t:3000:${"n".repeat(1024)}`, blob: "\u0001b:2", shape: "POINT(1 2)" },
    located: true,
    locatorColumns: ["id"],
    locatorValues: ["1"],
  };
}

describe("grid column width, clipboard and paging", () => {
  beforeEach(() => {
    invoke.mockReset();
    continueTabRun.mockClear();
    tabRun.mockReset();
    writeText.mockClear();
    resetExportTasks();
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    resetCloseGuard();
    resetCatalog();
  });

  it("keeps a dragged width, copies quoted tsv, and reads a located cell before copy", async () => {
    invoke.mockImplementation(async (command: string, args: { request?: { offset?: number; filters?: { column: string }[] } }) => {
      if (command === "grid_describe") return schema();
      if (command === "grid_page") {
        const filters = args.request?.filters ?? [];
        if (filters.some((filter) => filter.column === "id")) {
          return { rows: [{ ...row(), cells: { id: "1", note: "hello\tthere", blob: "abcd", shape: "POINT(1 2)" } }], truncated: false, sql: "SELECT full" };
        }
        return { rows: [row()], truncated: false, sql: "SELECT preview" };
      }
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:docs", draft: "", pageSize: 10, target } });
    await flushPromises();
    expect(wrapper.text()).toContain("integer");
    expect(wrapper.text()).toContain("bytea");
    expect(wrapper.text()).toContain("geometry");
    expect(wrapper.get("[data-cell='1:note']").text()).toContain("字节");
    expect(wrapper.get("[data-cell='1:note']").text()).not.toContain("there");
    expect(wrapper.get("[data-cell='1:blob']").text()).toContain("二进制");
    expect(wrapper.get("[data-cell='1:blob']").text()).not.toContain("\uFFFD");
    expect(wrapper.get("[data-cell='1:shape']").text()).toContain("POINT(1 2)");

    const handle = wrapper.get("[aria-label='调整 note 列宽']");
    handle.element.dispatchEvent(new PointerEvent("pointerdown", { clientX: 100, bubbles: true }));
    window.dispatchEvent(new PointerEvent("pointermove", { clientX: 180 }));
    window.dispatchEvent(new PointerEvent("pointerup"));
    await nextTick();
    const th = handle.element.closest("th") as HTMLElement;
    expect(th.style.width).toBe("80px");

    await wrapper.get("[data-cell='1:note']").trigger("pointerdown");
    await wrapper.get("[data-cell='1:blob']").trigger("pointerenter");
    expect(wrapper.findAll(".cell-btn.range").map((button) => button.attributes("data-cell"))).toEqual(["1:note", "1:blob"]);
    await wrapper.get("[data-cell='1:note']").trigger("contextmenu", { clientX: 12, clientY: 16 });
    expect(wrapper.get(".cell-menu").text()).toContain("复制");
    expect(wrapper.get(".cell-menu").text()).toContain("编辑");
    expect(wrapper.get(".cell-menu").text()).toContain("设为 NULL");
    await wrapper.get(".cell-menu").findAll("button").find((button) => button.text() === "复制")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenCalledWith("\"hello\tthere\"\tabcd");
    const reread = invoke.mock.calls.filter((call) => call[0] === "grid_page").at(-1)?.[1] as { request: { limit: number; offset: number; filters: { op: string; value: string }[] } };
    expect(reread.request).toMatchObject({ limit: 1, offset: 0 });
    expect(reread.request.filters[0]).toMatchObject({ op: "eq", value: "1" });
    expect(th.style.width).toBe("80px");

    await wrapper.get("[data-cell='1:shape']").trigger("contextmenu", { clientX: 20, clientY: 20 });
    await wrapper.get(".cell-menu").findAll("button").find((button) => button.text() === "设为 NULL")!.trigger("click");
    expect(wrapper.get(".cell-menu").text()).toContain("几何");
    expect(wrapper.find(".pending-bar").exists()).toBe(false);
    await wrapper.get("[data-cell='1:blob']").trigger("dblclick");
    await wrapper.get("input[aria-label='编辑 blob']").setValue("zz");
    await wrapper.get(".editor").findAll("button").find((button) => button.text() === "确定")!.trigger("click");
    expect(wrapper.text()).toContain("这不是十六进制");
    expect(wrapper.find(".pending-bar").exists()).toBe(false);
    wrapper.unmount();
  });

  it("shows live sum, count, avg, min and max for the selected range", async () => {
    const store = customersFixture(4);
    invoke.mockImplementation(async (command: string, args: { request?: { offset?: number; limit?: number } }) => {
      if (command === "grid_describe") return store.schema;
      if (command === "grid_page") {
        const offset = args.request?.offset ?? 0;
        const limit = args.request?.limit ?? 100;
        const rows = store.rows.slice(offset, offset + limit).map((row) => ({
          id: row.id,
          cells: { ...row.cells },
          located: row.located,
          locatorColumns: row.locator?.columns ?? [],
          locatorValues: row.locator?.values ?? [],
        }));
        return { rows, truncated: offset + limit < store.rows.length, sql: "SELECT customers" };
      }
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:customers-calc", draft: "", pageSize: 10, target } });
    await flushPromises();
    expect(wrapper.find(".calc-stat").exists()).toBe(false);
    await wrapper.get("[data-cell='1:id']").trigger("pointerdown");
    await wrapper.get("[data-cell='2:note']").trigger("pointerenter");
    window.dispatchEvent(new PointerEvent("pointerup"));
    await nextTick();
    // 两行 × 四列共 8 格；数值只有 id 1、2，其余是文本、u1@example.com、NULL 和空字符串。
    expect(wrapper.get(".calc-stat").attributes("role")).toBe("status");
    expect(wrapper.get(".calc-stat").text()).toBe("已选 8 格 · 数值 2 个 · 总和 3 · 平均 1.5 · 最小 1 · 最大 2");
    await wrapper.get("[data-cell='1:name']").trigger("pointerdown");
    await wrapper.get("[data-cell='2:name']").trigger("pointerenter");
    window.dispatchEvent(new PointerEvent("pointerup"));
    await nextTick();
    expect(wrapper.get(".calc-stat").text()).toBe("已选 2 格 · 数值 0 个");
    wrapper.unmount();
  });

  it("reads the full value before editing a located preview", async () => {
    invoke.mockImplementation(async (command: string, args: { request?: { filters?: { column: string }[] } }) => {
      if (command === "grid_describe") return schema();
      if (command === "grid_page") {
        const filters = args.request?.filters ?? [];
        if (filters.some((filter) => filter.column === "id")) {
          return { rows: [{ ...row(), cells: { id: "1", note: "hello\tthere", blob: "abcd", shape: "POINT(1 2)" } }], truncated: false, sql: "SELECT full" };
        }
        return { rows: [row()], truncated: false, sql: "SELECT preview" };
      }
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:docs-edit", draft: "", pageSize: 10, target } });
    await flushPromises();
    await wrapper.get("[data-cell='1:note']").trigger("dblclick");
    await flushPromises();
    const input = wrapper.get("input[aria-label='编辑 note']");
    expect((input.element as HTMLInputElement).value).toBe("hello\tthere");
    expect((input.element as HTMLInputElement).value).not.toContain("\u0001");
    const reread = invoke.mock.calls.filter((call) => call[0] === "grid_page").at(-1)?.[1] as { request: { limit: number; filters: { op: string; value: string }[] } };
    expect(reread.request.limit).toBe(1);
    expect(reread.request.filters[0]).toMatchObject({ op: "eq", value: "1" });
    wrapper.unmount();
  });

  it("edits protocol-shaped text when the located read returns that same text", async () => {
    const literal = "\u0001t:11:hello";
    const synthetic = `\u0001t:3000:${"a".repeat(1024)}`;
    let note = literal;
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return schema();
      if (command === "grid_page") return { rows: [{ ...row(), cells: { id: "1", note, blob: "abcd", shape: "POINT(1 2)" } }], truncated: false, sql: "SELECT page" };
      if (command === "grid_commit") return { applied: [{ id: "u:1", sql: "update", label: "修改" }], failed: null, pendingIds: [], rolledBack: false, rule: "按顺序" };
      throw new Error(command);
    });
    const literalGrid = mount(DataGrid, { props: { tabId: "table:docs-literal", draft: "", pageSize: 10, target } });
    await flushPromises();
    expect(literalGrid.get("[data-cell='1:note']").text()).toContain(literal);
    await literalGrid.get("[data-cell='1:note']").trigger("dblclick");
    await flushPromises();
    expect((literalGrid.get("input[aria-label='编辑 note']").element as HTMLInputElement).value).toBe(literal);
    await literalGrid.get(".editor").findAll("button").find((button) => button.text() === "确定")!.trigger("click");
    expect(literalGrid.text()).toContain("1 处更改未保存");
    literalGrid.unmount();

    note = synthetic;
    const same = mount(DataGrid, { props: { tabId: "table:docs-same", draft: "", pageSize: 10, target } });
    await flushPromises();
    await same.get("[data-cell='1:note']").trigger("dblclick");
    await flushPromises();
    expect((same.get("input[aria-label='编辑 note']").element as HTMLInputElement).value).toBe(synthetic);
    await same.get(".editor").findAll("button").find((button) => button.text() === "确定")!.trigger("click");
    await same.get(".pending-bar").findAll("button").find((button) => button.text() === "提交")!.trigger("click");
    await same.get(".sheet").findAll("button").find((button) => button.text().startsWith("提交"))!.trigger("click");
    await flushPromises();
    const commit = invoke.mock.calls.filter((call) => call[0] === "grid_commit").at(-1)?.[1] as { request: { changes: { sets: { value: string; resolved?: boolean }[] }[] } };
    expect(commit.request.changes[0]?.sets[0]).toMatchObject({ value: synthetic, resolved: true });
    same.unmount();
  });

  it("copies binary-envelope-shaped text as the original string", async () => {
    const shaped = "\u0001b:4";
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return schema();
      if (command === "grid_page") {
        return { rows: [{ ...row(), cells: { id: "1", note: shaped, blob: "abcd", shape: "POINT(1 2)" } }], truncated: false, sql: "SELECT page" };
      }
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:docs-binary-shape", draft: "", pageSize: 10, target } });
    await flushPromises();
    expect(wrapper.get("[data-cell='1:note']").text()).toContain(shaped);
    await wrapper.get("[data-cell='1:note']").trigger("contextmenu", { clientX: 4, clientY: 4 });
    await wrapper.get(".cell-menu").findAll("button").find((button) => button.text() === "查看值")!.trigger("click");
    await flushPromises();
    await wrapper.findAll("button").find((button) => button.text() === "复制值")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenCalledWith(shaped);
    await wrapper.get("[data-cell='1:note']").trigger("pointerdown");
    await wrapper.get("[data-cell='1:blob']").trigger("pointerenter");
    await wrapper.get("[data-cell='1:note']").trigger("contextmenu", { clientX: 4, clientY: 4 });
    await wrapper.get(".cell-menu").findAll("button").find((button) => button.text() === "复制")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenLastCalledWith(`${shaped}\tabcd`);
    wrapper.unmount();
  });

  it("shows and copies the full text when a null key already returned it", async () => {
    const synthetic = `\u0001t:3000:${"a".repeat(1024)}`;
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return schema();
      if (command === "grid_page") {
        return {
          rows: [{ id: "o:0:0", cells: { id: null, note: synthetic, blob: "abcd", shape: "POINT(1 2)" }, located: false, locatorColumns: [], locatorValues: [] }],
          truncated: false,
          sql: "SELECT page",
        };
      }
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:docs-null-key", draft: "", pageSize: 10, target } });
    await flushPromises();
    expect(wrapper.get("[data-cell='o:0:0:note']").text()).toContain("t:3000");
    await wrapper.get("[data-cell='o:0:0:note']").trigger("contextmenu", { clientX: 4, clientY: 4 });
    await wrapper.get(".cell-menu").findAll("button").find((button) => button.text() === "查看值")!.trigger("click");
    await flushPromises();
    await wrapper.findAll("button").find((button) => button.text() === "读取完整值")!.trigger("click");
    await flushPromises();
    expect((wrapper.get("textarea").element as HTMLTextAreaElement).value).toBe(synthetic);
    await wrapper.findAll("button").find((button) => button.text() === "复制值")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenCalledWith(synthetic);
    await wrapper.get("[data-cell='o:0:0:note']").trigger("contextmenu", { clientX: 4, clientY: 4 });
    await wrapper.get(".cell-menu").findAll("button").find((button) => button.text() === "复制")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenLastCalledWith(synthetic);
    wrapper.unmount();
  });

  it("shows and copies a protocol-shaped original once the value is complete", async () => {
    const synthetic = `\u0001t:3000:${"a".repeat(1024)}`;
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return { ...schema(), kind: "view", key: [] };
      if (command === "grid_page") {
        return { rows: [{ ...row(), located: false, locatorColumns: [], locatorValues: [], cells: { id: "1", note: synthetic, blob: "abcd", shape: "POINT(1 2)" } }], truncated: false, sql: "SELECT view" };
      }
      throw new Error(command);
    });
    const view = mount(DataGrid, { props: { tabId: "table:docs-protocol-view", draft: "", pageSize: 10, target } });
    await flushPromises();
    expect(view.get("[data-cell='1:note']").text()).toContain("t:3000");
    await view.get("[data-cell='1:note']").trigger("contextmenu", { clientX: 4, clientY: 4 });
    await view.get(".cell-menu").findAll("button").find((button) => button.text() === "查看值")!.trigger("click");
    await flushPromises();
    await view.findAll("button").find((button) => button.text() === "读取完整值")!.trigger("click");
    await flushPromises();
    expect((view.get("textarea").element as HTMLTextAreaElement).value).toBe(synthetic);
    await view.findAll("button").find((button) => button.text() === "复制值")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenCalledWith(synthetic);
    view.unmount();

    writeText.mockClear();
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return schema();
      if (command === "grid_page") return { rows: [{ ...row(), cells: { id: "1", note: synthetic, blob: "abcd", shape: "POINT(1 2)" } }], truncated: false, sql: "SELECT page" };
      throw new Error(command);
    });
    const located = mount(DataGrid, { props: { tabId: "table:docs-protocol-located", draft: "", pageSize: 10, target } });
    await flushPromises();
    expect(located.get("[data-cell='1:note']").text()).not.toContain("t:3000");
    await located.get("[data-cell='1:note']").trigger("contextmenu", { clientX: 4, clientY: 4 });
    await located.get(".cell-menu").findAll("button").find((button) => button.text() === "查看值")!.trigger("click");
    await flushPromises();
    await located.findAll("button").find((button) => button.text() === "读取完整值")!.trigger("click");
    await flushPromises();
    expect((located.get("textarea").element as HTMLTextAreaElement).value).toBe(synthetic);
    await located.findAll("button").find((button) => button.text() === "复制值")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenCalledWith(synthetic);
    await located.get("[data-cell='1:note']").trigger("contextmenu", { clientX: 4, clientY: 4 });
    await located.get(".cell-menu").findAll("button").find((button) => button.text() === "复制")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenLastCalledWith(synthetic);
    located.unmount();
  });

  it("exports located previews as the full text instead of the envelope", async () => {
    invoke.mockImplementation(async (command: string, args: { request?: { filters?: { column: string }[] } }) => {
      if (command === "grid_describe") return schema();
      if (command === "grid_page") {
        const filters = args.request?.filters ?? [];
        if (filters.some((filter) => filter.column === "id")) {
          return { rows: [{ ...row(), cells: { id: "1", note: "hello\tthere", blob: "abcd", shape: "POINT(1 2)" } }], truncated: false, sql: "SELECT full" };
        }
        return { rows: [row()], truncated: false, sql: "SELECT preview" };
      }
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:docs-export", draft: "", pageSize: 10, target } });
    await flushPromises();
    const dir = mkdtempSync(join(tmpdir(), "rillsql-grid-preview-"));
    await wrapper.get("button[aria-label='导出']").trigger("click");
    await wrapper.get("input[aria-label='导出路径']").setValue(join(dir, "docs.csv"));
    await wrapper.findAll("button").filter((button) => button.text() === "导出").at(-1)!.trigger("click");
    const written = join(dir, "docs.csv");
    await vi.waitFor(() => {
      expect(readFileSync(written, "utf8")).toContain("abcd");
    });
    const body = readFileSync(written, "utf8");
    expect(body).toContain("hello");
    expect(body).toContain("there");
    expect(body).toContain("abcd");
    expect(body).not.toContain("\u0001");
    rmSync(dir, { recursive: true, force: true });
    wrapper.unmount();
  });

  it("keeps loaded rows when the next page fails", async () => {
    invoke.mockImplementation(async (command: string, args: { request?: { offset?: number } }) => {
      if (command === "grid_describe") return schema();
      if (command === "grid_page") {
        if ((args.request?.offset ?? 0) > 0) throw new Error("下一页失败");
        return { rows: [row(), { ...row(), id: "2", cells: { ...row().cells, id: "2" }, locatorValues: ["2"] }], truncated: true, sql: "SELECT page" };
      }
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:docs-page", draft: "", pageSize: 2, target } });
    await flushPromises();
    expect(wrapper.text()).toContain("下一页失败");
    expect(wrapper.text()).toContain("POINT(1 2)");
    expect(wrapper.attributes("data-loaded")).toBe("2");
    expect(invoke.mock.calls.filter((call) => call[0] === "grid_page")).toHaveLength(2);
    wrapper.unmount();
  });

  it("does not stage edits from a read-only view", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") return { ...schema(), kind: "view", key: [] };
      if (command === "grid_page") return { rows: [{ ...row(), located: false, locatorColumns: [], locatorValues: [] }], truncated: false, sql: "SELECT view" };
      throw new Error(command);
    });
    const wrapper = mount(DataGrid, { props: { tabId: "table:docs-view", draft: "", pageSize: 10, target: { ...target, table: "docs_view" } } });
    await flushPromises();
    await wrapper.get("[data-cell='1:note']").trigger("contextmenu", { clientX: 8, clientY: 8 });
    await wrapper.get(".cell-menu").findAll("button").find((button) => button.text() === "编辑")!.trigger("click");
    expect(wrapper.get(".cell-menu").text()).toContain("只读");
    expect(wrapper.find(".pending-bar").exists()).toBe(false);
    expect(wrapper.find("input[aria-label='编辑 note']").exists()).toBe(false);
    wrapper.unmount();
  });

  it("continues the same query stream when the loaded rows already fill the window", async () => {
    tabRun.mockReturnValue({
      id: "run-1",
      status: "running",
      statements: [{ sql: "SELECT n FROM docs", truncated: false, rows: [{ n: "1" }] }],
    });
    const columns = [column("n", "integer")];
    const first: Row[] = [{ id: "r0", cells: { n: "1" }, located: false, locator: null }];
    const wrapper = mount(QueryResultGrid, {
      props: { tabId: "query-1", statement: "SELECT n FROM docs", columns, rows: first, truncated: true, engine: "mysql" },
    });
    await flushPromises();
    expect(wrapper.text()).not.toContain("没有行。");
    expect(wrapper.text()).toContain("integer");
    expect(continueTabRun).not.toHaveBeenCalled();
    const more: Row[] = [...first, { id: "r1", cells: { n: "2" }, located: false, locator: null }];
    await wrapper.setProps({ rows: more });
    await flushPromises();
    expect(continueTabRun).toHaveBeenCalledWith("query-1");
    expect(wrapper.text()).toContain("2");
    wrapper.unmount();
  });

  it("opens a protocol-shaped query value as the retrieved text", async () => {
    const synthetic = `\u0001t:3000:${"b".repeat(1024)}`;
    const wrapper = mount(QueryResultGrid, {
      props: {
        tabId: "query-protocol",
        statement: "SELECT note FROM docs",
        columns: [column("note", "text")],
        rows: [{ id: "r0", cells: { note: synthetic }, located: false, locator: null }],
        truncated: false,
        engine: "postgres",
      },
    });
    await flushPromises();
    expect(wrapper.get("[data-cell='r0:note']").text()).toContain("t:3000");
    await wrapper.get("[data-cell='r0:note']").trigger("contextmenu", { clientX: 4, clientY: 4 });
    await wrapper.get(".cell-menu").findAll("button").find((button) => button.text() === "查看值")!.trigger("click");
    await flushPromises();
    await wrapper.findAll("button").find((button) => button.text() === "读取完整值")!.trigger("click");
    expect((wrapper.get("textarea").element as HTMLTextAreaElement).value).toBe(synthetic);
    await wrapper.findAll("button").find((button) => button.text() === "复制值")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenCalledWith(synthetic);
    wrapper.unmount();
  });

  it("uses the query engine when a hash comment would otherwise split the export", async () => {
    const wrapper = mount(QueryResultGrid, {
      props: {
        tabId: "query-mysql",
        statement: "SELECT 1 # keep;\nFROM t",
        columns: [column("n", "integer")],
        rows: [{ id: "r0", cells: { n: "1" }, located: false, locator: null }],
        truncated: false,
        engine: "mysql",
      },
    });
    await flushPromises();
    await wrapper.findAll("button").find((button) => button.text() === "导出")!.trigger("click");
    await wrapper.get("input[value='full']").setValue(true);
    expect(wrapper.get("[data-export-notice]").text()).toContain("会重新执行这条 SELECT");
    expect(wrapper.find(".ex-alert").exists()).toBe(false);
    wrapper.unmount();
  });
});

const pointerTarget: TableTarget = { ...target, table: "customers" };

/** 指针事件不依赖真实布局：直接派发到命中的单元格，冒泡到窗口监听。 */
function pointerMove(el: Element, type = "pointermove") {
  el.dispatchEvent(new PointerEvent(type, { bubbles: true, cancelable: true }));
}

function rangeCells(wrapper: { findAll: (selector: string) => { attributes: (name: string) => string | undefined }[] }) {
  return wrapper.findAll(".cell-btn.range").map((button) => button.attributes("data-cell"));
}

describe("data grid pointer selection and on-demand value viewer", () => {
  beforeEach(() => {
    invoke.mockReset();
    continueTabRun.mockClear();
    tabRun.mockReset();
    writeText.mockClear();
    resetExportTasks();
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    resetCloseGuard();
    resetCatalog();
  });

  function mountCustomers(tabId: string) {
    const store = customersFixture(3);
    invoke.mockImplementation(async (command: string, args: { request?: { offset?: number; limit?: number } }) => {
      if (command === "grid_describe") return store.schema;
      if (command === "grid_page") {
        const offset = args.request?.offset ?? 0;
        const limit = args.request?.limit ?? 100;
        const rows = store.rows.slice(offset, offset + limit).map((row) => ({
          id: row.id,
          cells: { ...row.cells },
          located: row.located,
          locatorColumns: row.locator?.columns ?? [],
          locatorValues: row.locator?.values ?? [],
        }));
        return { rows, truncated: offset + limit < store.rows.length, sql: "SELECT customers" };
      }
      if (command === "ddl_describe") return { foreignKeyList: [] };
      throw new Error(command);
    });
    // 窗口级的 pointermove 监听只有在挂到文档上时才会收到冒泡的指针事件。
    return mount(DataGrid, { attachTo: document.body, props: { tabId, draft: "", pageSize: 10, target: pointerTarget } });
  }

  /** 右键菜单里的“查看值”是唯一打开查看器的入口。 */
  async function openViewer(wrapper: ReturnType<typeof mountCustomers>, cell: string) {
    await wrapper.get("[data-cell='" + cell + "']").trigger("contextmenu", { clientX: 8, clientY: 8 });
    await flushPromises();
    await wrapper.get(".cell-menu [data-action='view-value']").trigger("click");
    await flushPromises();
  }

  it("keeps the viewer closed for clicks, keyboard moves and drags", async () => {
    const wrapper = mountCustomers("table:viewer-closed");
    await flushPromises();
    expect(wrapper.find(".inspector").exists()).toBe(false);

    await wrapper.get("[data-cell='1:name']").trigger("pointerdown");
    await wrapper.get("[data-cell='1:name']").trigger("pointerup");
    await wrapper.get("[data-cell='1:name']").trigger("click");
    await flushPromises();
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("1:name");
    expect(wrapper.find(".inspector").exists()).toBe(false);

    await wrapper.get(".grid-scroll").trigger("keydown", { key: "ArrowRight" });
    await flushPromises();
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("1:email");
    expect(wrapper.find(".inspector").exists()).toBe(false);

    await wrapper.get("[data-cell='2:id']").trigger("pointerdown");
    pointerMove(wrapper.get("[data-cell='3:note']").element);
    await wrapper.get(".grid-scroll").trigger("keydown", { key: "ArrowDown" });
    await flushPromises();
    expect(wrapper.find(".inspector").exists()).toBe(false);
    expect(wrapper.find("textarea").exists()).toBe(false);
    wrapper.unmount();
  });

  it("opens the viewer from the context menu for the right-clicked cell", async () => {
    const wrapper = mountCustomers("table:viewer-menu");
    await flushPromises();
    await openViewer(wrapper, "2:email");
    const inspector = wrapper.get(".inspector");
    expect(inspector.attributes("aria-label")).toBe("值查看器");
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("2:email");
    expect(inspector.text()).toContain("email");
    expect((wrapper.get("textarea").element as HTMLTextAreaElement).value).toBe("u2@example.com");
    expect(wrapper.find("[aria-label='email 的值']").exists()).toBe(true);
    expect(wrapper.find(".cell-menu").exists()).toBe(false);
    // 查看器里的复制使用这次明确查看的单元格。
    await wrapper.findAll("button").find((button) => button.text() === "复制值")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenCalledWith("u2@example.com");
    wrapper.unmount();
  });

  it("closes an open viewer on the next ordinary selection and never reopens on cell change", async () => {
    const wrapper = mountCustomers("table:viewer-close");
    await flushPromises();
    await openViewer(wrapper, "1:name");
    expect(wrapper.find(".inspector").exists()).toBe(true);

    await wrapper.get(".inspector-close").trigger("click");
    await flushPromises();
    expect(wrapper.find(".inspector").exists()).toBe(false);

    // 关闭后换格、再换回来都不重新展开。
    await wrapper.get("[data-cell='2:name']").trigger("pointerdown");
    pointerMove(wrapper.get("[data-cell='1:name']").element);
    await wrapper.get("[data-cell='1:name']").trigger("pointerup");
    await flushPromises();
    expect(wrapper.find(".inspector").exists()).toBe(false);

    // 已打开时进行普通选择：查看器关闭，而不是立刻展示新值。
    await openViewer(wrapper, "1:name");
    expect(wrapper.find(".inspector").exists()).toBe(true);
    await wrapper.get("[data-cell='3:name']").trigger("pointerdown");
    await wrapper.get("[data-cell='3:name']").trigger("pointerup");
    await flushPromises();
    expect(wrapper.find(".inspector").exists()).toBe(false);
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("3:name");
    wrapper.unmount();
  });

  it("follows fast pointer moves, keeps the last region on an outside release and clears on cancel", async () => {
    const wrapper = mountCustomers("table:drag-lifecycle");
    await flushPromises();

    await wrapper.get("[data-cell='1:id']").trigger("pointerdown");
    // 一次跨两行三列的移动：不能只依赖指针经过的每个单元格。
    pointerMove(wrapper.get("[data-cell='3:note']").element);
    await nextTick();
    expect(rangeCells(wrapper)).toEqual([
      "1:id", "1:name", "1:email", "1:note",
      "2:id", "2:name", "2:email", "2:note",
      "3:id", "3:name", "3:email", "3:note",
    ]);
    expect(wrapper.find(".inspector").exists()).toBe(false);

    // 网格外释放：保留最后有效区域并结束拖动。
    window.dispatchEvent(new PointerEvent("pointerup"));
    await nextTick();
    expect(rangeCells(wrapper)).toHaveLength(12);
    pointerMove(wrapper.get("[data-cell='1:email']").element);
    await nextTick();
    expect(rangeCells(wrapper)).toHaveLength(12);
    expect(wrapper.find("input[aria-label='编辑 name']").exists()).toBe(false);
    expect(wrapper.find(".pending-bar").exists()).toBe(false);

    // pointercancel：结束拖动、不编辑、不复制，之后移动不再扩展。
    await wrapper.get("[data-cell='1:id']").trigger("pointerdown");
    pointerMove(wrapper.get("[data-cell='2:name']").element);
    await nextTick();
    expect(rangeCells(wrapper)).toEqual(["1:id", "1:name", "2:id", "2:name"]);
    window.dispatchEvent(new PointerEvent("pointercancel"));
    await nextTick();
    pointerMove(wrapper.get("[data-cell='3:note']").element);
    await nextTick();
    // 取消之后不再扩展：仍是取消前的两行两列。
    expect(rangeCells(wrapper)).toEqual(["1:id", "1:name", "2:id", "2:name"]);
    expect(writeText).not.toHaveBeenCalled();
    expect(wrapper.find(".inspector").exists()).toBe(false);
    wrapper.unmount();
  });

  it("commits the last dragged cell without entering editing", async () => {
    const wrapper = mountCustomers("table:drag-commit");
    await flushPromises();
    await wrapper.get("[data-cell='1:name']").trigger("pointerdown");
    pointerMove(wrapper.get("[data-cell='2:name']").element);
    pointerMove(wrapper.get("[data-cell='2:name']").element, "pointerup");
    await flushPromises();
    expect(wrapper.find("input[aria-label='编辑 name']").exists()).toBe(false);
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("2:name");
    expect(rangeCells(wrapper)).toEqual(["1:name", "2:name"]);
    // 拖选结束后双击仍然进入编辑。
    await wrapper.get("[data-cell='2:name']").trigger("dblclick");
    await flushPromises();
    expect(wrapper.find("input[aria-label='编辑 name']").exists()).toBe(true);
    wrapper.unmount();
  });

  it("copies exactly the rectangle the drag selected", async () => {
    const wrapper = mountCustomers("table:drag-copy");
    await flushPromises();
    await wrapper.get("[data-cell='1:id']").trigger("pointerdown");
    pointerMove(wrapper.get("[data-cell='2:name']").element);
    pointerMove(wrapper.get("[data-cell='2:name']").element, "pointerup");
    await flushPromises();
    await wrapper.get(".grid-scroll").trigger("keydown", { ctrlKey: true, key: "c" });
    await flushPromises();
    expect(writeText).toHaveBeenCalledWith("1\t小河\n2\t客户2");
    wrapper.unmount();
  });

  /** 查看器默认关闭：复制 SQL 值的成败都必须落在全局轻提示上。 */
  it("keeps SQL-value copy feedback visible while the viewer stays closed", async () => {
    const wrapper = mountCustomers("table:copy-toast");
    await flushPromises();
    await wrapper.get("[data-cell='1:name']").trigger("pointerdown");
    await wrapper.get("[data-cell='1:name']").trigger("pointerup");
    await flushPromises();
    expect(wrapper.find(".inspector").exists()).toBe(false);

    toasts.splice(0, toasts.length);
    await wrapper.get(".grid-scroll").trigger("keydown", { ctrlKey: true, shiftKey: true, key: "c" });
    await flushPromises();
    expect(writeText).toHaveBeenLastCalledWith("'小河'");
    expect(toasts.map((item) => item.text)).toEqual(["已复制 SQL 值"]);
    expect(toasts[0]?.tone).toBe("ok");
    expect(wrapper.find(".inspector").exists()).toBe(false);

    writeText.mockRejectedValueOnce(new Error("denied"));
    toasts.splice(0, toasts.length);
    await wrapper.get(".grid-scroll").trigger("keydown", { ctrlKey: true, shiftKey: true, key: "c" });
    await flushPromises();
    expect(toasts).toHaveLength(1);
    expect(toasts[0]?.tone).toBe("err");
    expect(toasts[0]?.text).toContain("复制失败");
    expect(wrapper.find(".inspector").exists()).toBe(false);
    wrapper.unmount();
  });

  it("closes an opened viewer on arrow-key navigation without reopening it", async () => {
    const wrapper = mountCustomers("table:viewer-keys");
    await flushPromises();
    await openViewer(wrapper, "1:name");
    expect(wrapper.find(".inspector").exists()).toBe(true);

    await wrapper.get(".grid-scroll").trigger("keydown", { key: "ArrowRight" });
    await flushPromises();
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("1:email");
    expect(wrapper.find(".inspector").exists()).toBe(false);

    await wrapper.get(".grid-scroll").trigger("keydown", { key: "ArrowDown" });
    await flushPromises();
    expect(wrapper.find(".inspector").exists()).toBe(false);

    // Shift+方向键扩展选区同样不重开查看器。
    await wrapper.get(".grid-scroll").trigger("keydown", { key: "ArrowDown", shiftKey: true });
    await flushPromises();
    expect(wrapper.find("button.cell-btn.range").exists()).toBe(true);
    expect(wrapper.find(".inspector").exists()).toBe(false);
    wrapper.unmount();
  });

  it("ignores a pointerup that follows pointercancel", async () => {
    const wrapper = mountCustomers("table:cancel-then-up");
    await flushPromises();
    await wrapper.get("[data-cell='1:id']").trigger("pointerdown");
    pointerMove(wrapper.get("[data-cell='2:name']").element);
    await nextTick();
    window.dispatchEvent(new PointerEvent("pointercancel"));
    await nextTick();
    // 取消后紧跟的 up 不能把选中提交到释放点，也不能进入编辑。
    pointerMove(wrapper.get("[data-cell='3:email']").element, "pointerup");
    await flushPromises();
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("1:id");
    expect(rangeCells(wrapper)).toEqual(["1:id", "1:name", "2:id", "2:name"]);
    expect(wrapper.find("input[aria-label='编辑 email']").exists()).toBe(false);
    expect(wrapper.find(".pending-bar").exists()).toBe(false);
    wrapper.unmount();
  });

  it("ends the drag when the window loses focus", async () => {
    const wrapper = mountCustomers("table:drag-blur");
    await flushPromises();
    await wrapper.get("[data-cell='1:id']").trigger("pointerdown");
    pointerMove(wrapper.get("[data-cell='2:name']").element);
    await nextTick();
    expect(rangeCells(wrapper)).toEqual(["1:id", "1:name", "2:id", "2:name"]);

    window.dispatchEvent(new Event("blur"));
    await nextTick();
    // 失焦后不再跟随指针，也不把当前单元格提交到释放点。
    pointerMove(wrapper.get("[data-cell='3:email']").element);
    pointerMove(wrapper.get("[data-cell='3:email']").element, "pointerup");
    await flushPromises();
    expect(rangeCells(wrapper)).toEqual(["1:id", "1:name", "2:id", "2:name"]);
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("1:id");
    wrapper.unmount();
  });

  it("keeps the dragged range and its copy consistent after a resize or scroll", async () => {
    const wrapper = mountCustomers("table:drag-resize");
    await flushPromises();
    await wrapper.get("[data-cell='1:id']").trigger("pointerdown");
    pointerMove(wrapper.get("[data-cell='2:name']").element);
    await nextTick();
    expect(rangeCells(wrapper)).toEqual(["1:id", "1:name", "2:id", "2:name"]);

    // 拖选期间改列宽：窗口上的 resize 监听不能把终点或选区改坏。
    const handle = wrapper.get("[aria-label='调整 name 列宽']");
    handle.element.dispatchEvent(new PointerEvent("pointerdown", { clientX: 100, bubbles: true }));
    window.dispatchEvent(new PointerEvent("pointermove", { clientX: 160 }));
    window.dispatchEvent(new PointerEvent("pointerup"));
    await flushPromises();
    expect((handle.element.closest("th") as HTMLElement).style.width).toBe("60px");
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("2:name");
    expect(rangeCells(wrapper)).toEqual(["1:id", "1:name", "2:id", "2:name"]);

    // 新一次拖动中滚动网格：小数据集的虚拟窗口不换行，命中仍落在指针所在单元格。
    await wrapper.get("[data-cell='1:id']").trigger("pointerdown");
    pointerMove(wrapper.get("[data-cell='1:id']").element);
    wrapper.get(".grid-scroll").element.dispatchEvent(new Event("scroll"));
    await nextTick();
    pointerMove(wrapper.get("[data-cell='2:email']").element);
    pointerMove(wrapper.get("[data-cell='2:email']").element, "pointerup");
    await flushPromises();
    expect(rangeCells(wrapper)).toEqual(["1:id", "1:name", "1:email", "2:id", "2:name", "2:email"]);
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("2:email");

    toasts.splice(0, toasts.length);
    await wrapper.get(".grid-scroll").trigger("keydown", { ctrlKey: true, key: "c" });
    await flushPromises();
    expect(writeText).toHaveBeenLastCalledWith("1\t小河\tu1@example.com\n2\t客户2\tu2@example.com");
    wrapper.unmount();
  });

  /** 真实指针会拿到捕获：target 固定在起始格，命中必须按坐标，而不是钉在锚点上。 */
  it("hits by coordinates while the pointer is captured and drops capture on release", async () => {
    const wrapper = mountCustomers("table:capture-hit");
    await flushPromises();
    const start = wrapper.get("[data-cell='1:id']").element as HTMLElement;
    const captured: number[] = [];
    const released: number[] = [];
    start.setPointerCapture = (id: number) => captured.push(id);
    start.releasePointerCapture = (id: number) => released.push(id);
    const original = document.elementFromPoint;
    let hit: Element | null = wrapper.get("[data-cell='3:email']").element;
    document.elementFromPoint = () => hit;

    start.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0, pointerId: 7, clientX: 10, clientY: 10 }));
    await nextTick();
    expect(captured).toEqual([7]);
    // 事件目标仍是起始格，只有坐标命中才能拖到 3:email。
    start.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 7, clientX: 300, clientY: 300 }));
    await nextTick();
    // 1:id → 3:email 是三行三列。
    expect(rangeCells(wrapper)).toHaveLength(9);
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("1:id");

    // 网格外释放：坐标没有命中时保留最后一个有效区域，不回落到起始格。
    hit = null;
    start.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, pointerId: 7, clientX: 900, clientY: 900 }));
    await flushPromises();
    expect(released).toEqual([7]);
    expect(rangeCells(wrapper)).toHaveLength(9);
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("3:email");

    // 捕获指针时浏览器把 click 发到起始格：不能把当前单元格拉回锚点。
    start.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flushPromises();
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("3:email");
    document.elementFromPoint = original;
    wrapper.unmount();
  });

  it("anchors a keyboard context menu on the target cell instead of the top-left corner", async () => {
    const wrapper = mountCustomers("table:menu-keyboard");
    await flushPromises();
    const cell = wrapper.get("[data-cell='2:name']").element as HTMLElement;
    cell.getBoundingClientRect = () =>
      ({ x: 120, y: 40, left: 120, top: 40, right: 240, bottom: 68, width: 120, height: 28, toJSON: () => ({}) }) as DOMRect;
    // 键盘触发（Shift+F10/菜单键）没有指针坐标：用单元格矩形定位。
    await wrapper.get("[data-cell='2:name']").trigger("contextmenu", { clientX: 0, clientY: 0 });
    await flushPromises();
    const style = wrapper.get(".cell-menu").attributes("style") ?? "";
    expect(style).toContain("left: 180px");
    expect(style).toContain("top: 54px");
    // 鼠标右键仍按指针坐标。
    await wrapper.get("[data-cell='1:id']").trigger("contextmenu", { clientX: 12, clientY: 34 });
    await flushPromises();
    const pointerStyle = wrapper.get(".cell-menu").attributes("style") ?? "";
    expect(pointerStyle).toContain("left: 12px");
    expect(pointerStyle).toContain("top: 34px");
    wrapper.unmount();
  });
});

describe("query result pointer selection and on-demand value viewer", () => {
  beforeEach(() => {
    writeText.mockClear();
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
  });

  function mountResult(tabId: string) {
    return mount(QueryResultGrid, {
      // 窗口级的 pointermove 监听只有在挂到文档上时才会收到冒泡的指针事件。
      attachTo: document.body,
      props: {
        tabId,
        statement: "SELECT * FROM orders JOIN items ON items.id = orders.id",
        columns: [column("id", "integer"), column("name", "text")],
        rows: [
          { id: "r0", cells: { id: "1", name: "甲" }, located: false, locator: null },
          { id: "r1", cells: { id: "2", name: "乙" }, located: false, locator: null },
        ],
        truncated: false,
        engine: "postgres",
      },
    });
  }

  it("shows the query viewer only when the context menu asks for it", async () => {
    const wrapper = mountResult("query-viewer-menu");
    await flushPromises();
    await wrapper.get("[data-cell='r1:name']").trigger("pointerdown");
    await wrapper.get("[data-cell='r1:name']").trigger("pointerup");
    await flushPromises();
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("r1:name");
    expect(wrapper.find(".inspector").exists()).toBe(false);

    await wrapper.get("[data-cell='r0:name']").trigger("contextmenu", { clientX: 6, clientY: 6 });
    await flushPromises();
    expect(wrapper.find(".inspector").exists()).toBe(false);
    await wrapper.get(".cell-menu [data-action='view-value']").trigger("click");
    await flushPromises();
    expect(wrapper.get(".inspector").attributes("aria-label")).toBe("查询值查看器");
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("r0:name");
    expect((wrapper.get("[aria-label='name 的值']").element as HTMLTextAreaElement).value).toBe("甲");

    // 关闭后换格不重开。
    await wrapper.get(".inspector").findAll("button").find((button) => button.text() === "关闭")!.trigger("click");
    await flushPromises();
    expect(wrapper.find(".inspector").exists()).toBe(false);
    await wrapper.get("[data-cell='r1:id']").trigger("pointerdown");
    await wrapper.get("[data-cell='r1:id']").trigger("pointerup");
    await flushPromises();
    expect(wrapper.find(".inspector").exists()).toBe(false);

    // 已打开时普通选择关闭查看器。
    await wrapper.get("[data-cell='r0:name']").trigger("contextmenu", { clientX: 6, clientY: 6 });
    await flushPromises();
    await wrapper.get(".cell-menu [data-action='view-value']").trigger("click");
    await flushPromises();
    expect(wrapper.find(".inspector").exists()).toBe(true);
    await wrapper.get("[data-cell='r1:name']").trigger("pointerdown");
    await wrapper.get("[data-cell='r1:name']").trigger("pointerup");
    await flushPromises();
    expect(wrapper.find(".inspector").exists()).toBe(false);
    wrapper.unmount();
  });

  it("copies a whole row, a whole column and a WHERE condition from the cell menu", async () => {
    const wrapper = mountResult("query-menu-copy-shapes");
    await flushPromises();
    /* 只读结果（无连接）：不出现删除标记项。 */
    await wrapper.get("[data-cell='r1:name']").trigger("contextmenu", { clientX: 6, clientY: 6 });
    expect(wrapper.get(".cell-menu").text()).toContain("复制整行");
    expect(wrapper.get(".cell-menu").text()).toContain("复制整列");
    expect(wrapper.get(".cell-menu").text()).toContain("复制为 WHERE 条件");
    expect(wrapper.get(".cell-menu").text()).not.toContain("标记删除行");
    await wrapper.get(".cell-menu").findAll("button").find((button) => button.text() === "复制整行")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenLastCalledWith("2\t乙");

    await wrapper.get("[data-cell='r1:name']").trigger("contextmenu", { clientX: 6, clientY: 6 });
    await wrapper.get(".cell-menu").findAll("button").find((button) => button.text() === "复制整列")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenLastCalledWith("甲\n乙");

    await wrapper.get("[data-cell='r1:name']").trigger("contextmenu", { clientX: 6, clientY: 6 });
    await wrapper.get(".cell-menu").findAll("button").find((button) => button.text() === "复制为 WHERE 条件")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenLastCalledWith("name = '乙'");

    // 数字列同样按字面量条件复制。
    await wrapper.get("[data-cell='r0:id']").trigger("contextmenu", { clientX: 6, clientY: 6 });
    await wrapper.get(".cell-menu").findAll("button").find((button) => button.text() === "复制为 WHERE 条件")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenLastCalledWith("id = 1");
    wrapper.unmount();
  });

  it("keeps the dragged rectangle across fast moves and pointer cancel", async () => {
    const wrapper = mountResult("query-drag-lifecycle");
    await flushPromises();
    await wrapper.get("[data-cell='r0:id']").trigger("pointerdown");
    pointerMove(wrapper.get("[data-cell='r1:name']").element);
    await nextTick();
    expect(rangeCells(wrapper)).toEqual(["r0:id", "r0:name", "r1:id", "r1:name"]);
    expect(wrapper.find(".inspector").exists()).toBe(false);

    pointerMove(wrapper.get("[data-cell='r1:name']").element, "pointerup");
    await flushPromises();
    expect(rangeCells(wrapper)).toHaveLength(4);
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("r1:name");

    await wrapper.get("[data-cell='r0:id']").trigger("pointerdown");
    pointerMove(wrapper.get("[data-cell='r1:id']").element);
    await nextTick();
    expect(rangeCells(wrapper)).toEqual(["r0:id", "r1:id"]);
    window.dispatchEvent(new PointerEvent("pointercancel"));
    await nextTick();
    pointerMove(wrapper.get("[data-cell='r1:name']").element);
    await nextTick();
    // 取消之后不再扩展成两行两列。
    expect(rangeCells(wrapper)).toEqual(["r0:id", "r1:id"]);
    expect(wrapper.find(".inspector").exists()).toBe(false);
    wrapper.unmount();
  });

  /** 查看器默认关闭：快捷键和右键“复制 SQL 值”都必须有全局可见回执。 */
  it("toasts SQL-value copy from the shortcut and from the context menu", async () => {
    const wrapper = mountResult("query-copy-toast");
    await flushPromises();
    await wrapper.get("[data-cell='r0:name']").trigger("pointerdown");
    await wrapper.get("[data-cell='r0:name']").trigger("pointerup");
    await flushPromises();
    expect(wrapper.find(".inspector").exists()).toBe(false);

    toasts.splice(0, toasts.length);
    await wrapper.get(".grid-scroll").trigger("keydown", { ctrlKey: true, shiftKey: true, key: "c" });
    await flushPromises();
    expect(writeText).toHaveBeenLastCalledWith("'甲'");
    expect(toasts.map((item) => item.text)).toEqual(["已复制 SQL 值"]);
    expect(toasts[0]?.tone).toBe("ok");
    expect(wrapper.find(".inspector").exists()).toBe(false);

    toasts.splice(0, toasts.length);
    await wrapper.get("[data-cell='r1:name']").trigger("contextmenu", { clientX: 6, clientY: 66 });
    await flushPromises();
    await wrapper.get(".cell-menu").findAll("button").find((button) => button.text() === "复制为 SQL 值")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenLastCalledWith("'乙'");
    expect(toasts[0]?.text).toBe("已复制 SQL 值");
    expect(wrapper.find(".inspector").exists()).toBe(false);

    writeText.mockRejectedValueOnce(new Error("denied"));
    toasts.splice(0, toasts.length);
    await wrapper.get(".grid-scroll").trigger("keydown", { ctrlKey: true, shiftKey: true, key: "c" });
    await flushPromises();
    expect(toasts).toHaveLength(1);
    expect(toasts[0]?.tone).toBe("err");
    expect(toasts[0]?.text).toContain("复制失败");
    wrapper.unmount();
  });

  it("closes an opened query viewer on arrow-key navigation without reopening it", async () => {
    const wrapper = mountResult("query-viewer-keys");
    await flushPromises();
    await wrapper.get("[data-cell='r0:name']").trigger("contextmenu", { clientX: 6, clientY: 66 });
    await flushPromises();
    await wrapper.get(".cell-menu [data-action='view-value']").trigger("click");
    await flushPromises();
    expect(wrapper.find(".inspector").exists()).toBe(true);

    await wrapper.get(".grid-scroll").trigger("keydown", { key: "ArrowRight" });
    await flushPromises();
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("r1:id");
    expect(wrapper.find(".inspector").exists()).toBe(false);

    await wrapper.get(".grid-scroll").trigger("keydown", { key: "ArrowDown" });
    await flushPromises();
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("r0:id");
    expect(wrapper.find(".inspector").exists()).toBe(false);

    // Shift+方向键扩展选区同样不重开查看器。
    await wrapper.get(".grid-scroll").trigger("keydown", { key: "ArrowRight", shiftKey: true });
    await flushPromises();
    expect(wrapper.find("button.cell-btn.range").exists()).toBe(true);
    expect(wrapper.find(".inspector").exists()).toBe(false);
    wrapper.unmount();
  });

  it("ignores a pointerup that follows pointercancel", async () => {
    const wrapper = mountResult("query-cancel-then-up");
    await flushPromises();
    await wrapper.get("[data-cell='r0:id']").trigger("pointerdown");
    pointerMove(wrapper.get("[data-cell='r1:id']").element);
    await nextTick();
    window.dispatchEvent(new PointerEvent("pointercancel"));
    await nextTick();
    // 取消后紧跟的 up 不能把当前单元格提交到释放点。
    pointerMove(wrapper.get("[data-cell='r1:name']").element, "pointerup");
    await flushPromises();
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("r0:id");
    expect(rangeCells(wrapper)).toEqual(["r0:id", "r1:id"]);
    expect(wrapper.find("input[aria-label='编辑 name']").exists()).toBe(false);
    expect(wrapper.find(".inspector").exists()).toBe(false);
    wrapper.unmount();
  });

  it("ends the drag when the window loses focus", async () => {
    const wrapper = mountResult("query-drag-blur");
    await flushPromises();
    await wrapper.get("[data-cell='r0:id']").trigger("pointerdown");
    pointerMove(wrapper.get("[data-cell='r1:id']").element);
    await nextTick();
    expect(rangeCells(wrapper)).toEqual(["r0:id", "r1:id"]);

    window.dispatchEvent(new Event("blur"));
    await nextTick();
    pointerMove(wrapper.get("[data-cell='r1:name']").element);
    pointerMove(wrapper.get("[data-cell='r1:name']").element, "pointerup");
    await flushPromises();
    expect(rangeCells(wrapper)).toEqual(["r0:id", "r1:id"]);
    expect(wrapper.get("button.cell-btn.on").attributes("data-cell")).toBe("r0:id");
    wrapper.unmount();
  });

  it("anchors a keyboard context menu on the target cell instead of the top-left corner", async () => {
    const wrapper = mountResult("query-menu-keyboard");
    await flushPromises();
    const cell = wrapper.get("[data-cell='r1:name']").element as HTMLElement;
    cell.getBoundingClientRect = () =>
      ({ x: 40, y: 80, left: 40, top: 80, right: 160, bottom: 108, width: 120, height: 28, toJSON: () => ({}) }) as DOMRect;
    await wrapper.get("[data-cell='r1:name']").trigger("contextmenu", { clientX: 0, clientY: 0 });
    await flushPromises();
    const style = wrapper.get(".cell-menu").attributes("style") ?? "";
    expect(style).toContain("left: 100px");
    expect(style).toContain("top: 94px");

    await wrapper.get("[data-cell='r0:id']").trigger("contextmenu", { clientX: 22, clientY: 44 });
    await flushPromises();
    const pointerStyle = wrapper.get(".cell-menu").attributes("style") ?? "";
    expect(pointerStyle).toContain("left: 22px");
    expect(pointerStyle).toContain("top: 44px");
    wrapper.unmount();
  });
});

