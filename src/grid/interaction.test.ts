import { flushPromises, mount } from "@vue/test-utils";
import { nextTick } from "vue";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { resetCatalog } from "../catalog/store";
import { resetCloseGuard } from "../shell/close-guard";
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
    expect(rejectWrite(column("blob", "bytea"), "zz")).toBe("这不是十六进制");
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
    expect(session.pendingCount).toBe(0);
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
    cells: { id: "1", note: "\u0001t:11:hello", blob: "\u0001b:2", shape: "POINT(1 2)" },
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
});
