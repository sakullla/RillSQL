import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import type { CatalogConnection } from "../catalog/types";
import type { Column } from "../grid/types";
import DragImportDialog from "./DragImportDialog.vue";
import DragOverlay from "./DragOverlay.vue";
import DragPreview from "./DragPreview.vue";
import {
  connectDroppedFiles,
  createDroppedFileHandler,
  formatDropBytes,
  previewBudgetBytes,
  previewRowsWithin,
  routeDroppedFile,
  unsupportedDropText,
  type DroppedPreview,
} from "./dropped";
import ImportDialog from "./ImportDialog.vue";
import { parseImportCsv } from "./import";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

afterEach(() => {
  resetCatalog();
  invoke.mockReset();
});

const CSV_TEXT = "name,email\r\n甲,a@example.com\r\n乙,b@example.com\r\n丙,c@example.com";

function droppedPreview(overrides: Partial<DroppedPreview> = {}): DroppedPreview {
  const text = overrides.text ?? CSV_TEXT;
  return {
    path: "C:\\data\\rows.csv",
    format: "csv",
    text,
    fileSize: Buffer.byteLength(text, "utf8"),
    readBytes: Buffer.byteLength(text, "utf8"),
    truncated: false,
    ...overrides,
  };
}

describe("dropped file routing", () => {
  it("routes csv and json in, everything else out with the extension kept", () => {
    expect(routeDroppedFile("C:\\data\\rows.csv")).toEqual({ kind: "preview", format: "csv", fileName: "rows.csv" });
    expect(routeDroppedFile("/tmp/Events.JSON")).toEqual({ kind: "preview", format: "json", fileName: "Events.JSON" });
    expect(routeDroppedFile("C:\\data\\orders.xlsx")).toEqual({ kind: "unsupported", extension: "xlsx", fileName: "orders.xlsx" });
    expect(routeDroppedFile("/tmp/events.parquet")).toEqual({ kind: "unsupported", extension: "parquet", fileName: "events.parquet" });
    expect(routeDroppedFile("C:\\data\\no-extension")).toEqual({ kind: "unsupported", extension: "", fileName: "no-extension" });
  });

  it("names the unsupported format and points spreadsheets back to the import dialog", () => {
    expect(unsupportedDropText("xlsx")).toContain(".xlsx 文件");
    expect(unsupportedDropText("xlsx")).toContain("导入对话框");
    expect(unsupportedDropText("parquet")).toContain(".parquet 文件");
    expect(unsupportedDropText("parquet")).not.toContain("Excel");
    expect(unsupportedDropText("")).toContain("没有扩展名");
  });

  it("derives the preview budget from the previewKB setting and folds rows beyond it", () => {
    expect(previewBudgetBytes(1)).toBe(1024);
    expect(previewBudgetBytes(4)).toBe(4096);
    expect(previewBudgetBytes(0)).toBe(1024);
    const rows = [
      { name: "aaaaaaaaaa", email: "bbbbbbbbbb" },
      { name: "cccccccccc", email: "dddddddddd" },
      { name: "eeeeeeeeee", email: "ffffffffff" },
    ];
    // 每行约 21 字节：预算 30 字节只装得下第一行，且至少显示一行。
    expect(previewRowsWithin(rows, 30)).toEqual({ rows: [rows[0]], hidden: 2 });
    expect(previewRowsWithin(rows, 1024)).toEqual({ rows, hidden: 0 });
    expect(previewRowsWithin([], 1024)).toEqual({ rows: [], hidden: 0 });
    expect(formatDropBytes(512)).toBe("512 B");
    expect(formatDropBytes(2048)).toBe("2 KB");
    expect(formatDropBytes(3 * 1024 * 1024)).toBe("3.0 MB");
  });
});

describe("drag drop event handling", () => {
  it("lights the overlay on enter and over, clears it on leave", () => {
    const hovering: boolean[] = [];
    const handler = createDroppedFileHandler({ setState: () => {}, setHovering: (value) => hovering.push(value) });
    handler({ type: "enter", paths: ["C:\\a.csv"] });
    handler({ type: "over" });
    handler({ type: "leave" });
    expect(hovering).toEqual([true, true, false]);
  });

  it("rejects unsupported drops without reading the file", async () => {
    const read = vi.fn();
    const states: unknown[] = [];
    const handler = createDroppedFileHandler({ setState: (state) => states.push(state), setHovering: () => {}, read });
    handler({ type: "drop", paths: ["C:\\data\\orders.xlsx"] });
    await flushPromises();
    expect(read).not.toHaveBeenCalled();
    expect(states).toEqual([{ kind: "unsupported", extension: "xlsx", fileName: "orders.xlsx" }]);
  });

  it("reads a dropped csv and hands the parsed rows to the state", async () => {
    const states: unknown[] = [];
    const hovering: boolean[] = [];
    const handler = createDroppedFileHandler({
      setState: (state) => states.push(state),
      setHovering: (value) => hovering.push(value),
      read: async () => droppedPreview(),
    });
    handler({ type: "enter", paths: ["C:\\data\\rows.csv"] });
    handler({ type: "drop", paths: ["C:\\data\\rows.csv"] });
    await flushPromises();
    expect(hovering).toEqual([true, false]);
    const ready = states.at(-1) as { kind: string; fileName: string; parsed: { rows: unknown[] } };
    expect(ready.kind).toBe("ready");
    expect(ready.fileName).toBe("rows.csv");
    expect(ready.parsed.rows).toHaveLength(3);
  });

  it("surfaces read failures and broken json as explicit errors", async () => {
    const states: unknown[] = [];
    const handler = createDroppedFileHandler({ setState: (state) => states.push(state), setHovering: () => {}, read: async () => {
      throw new Error("无法读取拖放的文件：系统找不到指定的文件。");
    } });
    handler({ type: "drop", paths: ["C:\\data\\absent.csv"] });
    await flushPromises();
    expect(states.at(-1)).toMatchObject({ kind: "error", message: expect.stringContaining("无法读取") });

    states.length = 0;
    const broken = createDroppedFileHandler({
      setState: (state) => states.push(state),
      setHovering: () => {},
      read: async () => droppedPreview({ path: "C:\\data\\broken.json", format: "json", text: "{\"name\":\"甲\"}" }),
    });
    broken({ type: "drop", paths: ["C:\\data\\broken.json"] });
    await flushPromises();
    expect(states.at(-1)).toMatchObject({ kind: "error", message: expect.stringContaining("JSON 必须是由对象组成的数组") });
  });

  it("does not subscribe outside the tauri runtime", async () => {
    expect(await connectDroppedFiles(() => {})).toBeNull();
  });
});

describe("DragOverlay", () => {
  it("only renders while a drag hovers", async () => {
    const wrapper = mount(DragOverlay, { props: { active: false } });
    expect(wrapper.find(".drop-overlay").exists()).toBe(false);
    await wrapper.setProps({ active: true });
    expect(wrapper.get(".drop-overlay").text()).toContain("松开以预览文件");
    wrapper.unmount();
  });
});

describe("DragPreview", () => {
  function readyState(budgetBytes: number, preview: DroppedPreview = droppedPreview()) {
    const parsed = parseImportCsv(preview.text);
    if (!parsed.ok) throw new Error("fixture csv must parse");
    return mount(DragPreview, { props: { open: true, budgetBytes, state: { kind: "ready", fileName: "rows.csv", preview, parsed: parsed.data } } });
  }

  it("renders the first rows within the budget and notes the folded rest", () => {
    const wrapper = readyState(1024);
    expect(wrapper.get("[data-drop-meta]").text()).toContain("rows.csv");
    expect(wrapper.get("[data-drop-meta]").text()).toContain("CSV");
    expect(wrapper.get("[data-drop-count]").text()).toContain("解析出 3 行，显示前 3 行");
    expect(wrapper.findAll(".dp-table tbody tr")).toHaveLength(3);
    expect(wrapper.find("[data-drop-truncated]").exists()).toBe(false);
    wrapper.unmount();
  });

  it("folds rows beyond a small budget but keeps the full parse for import", () => {
    const wrapper = readyState(20);
    expect(wrapper.get("[data-drop-count]").text()).toContain("显示前 1 行");
    expect(wrapper.findAll(".dp-table tbody tr")).toHaveLength(1);
    wrapper.unmount();
  });

  it("warns when the file exceeded the read limit", () => {
    const wrapper = readyState(1024, droppedPreview({ truncated: true, fileSize: 4 * 1024 * 1024, readBytes: 2 * 1024 * 1024 }));
    expect(wrapper.get("[data-drop-truncated]").text()).toContain("只读取了前 2.0 MB");
    wrapper.unmount();
  });

  it("cancel closes with no write and continue hands the text to the import flow", async () => {
    const wrapper = readyState(1024);
    await wrapper.get("[data-action='drop-cancel']").trigger("click");
    expect(wrapper.emitted("cancel")).toHaveLength(1);
    await wrapper.get("[data-action='drop-continue']").trigger("click");
    const seed = wrapper.emitted("proceed")?.[0]?.[0];
    expect(seed).toEqual({ format: "csv", text: CSV_TEXT, fileName: "rows.csv" });
    wrapper.unmount();
  });

  it("shows the unsupported notice without a continue entry", () => {
    const wrapper = mount(DragPreview, { props: { open: true, budgetBytes: 1024, state: { kind: "unsupported", extension: "parquet", fileName: "events.parquet" } } });
    expect(wrapper.get("[data-drop-notice]").text()).toContain("不支持拖放 .parquet 文件");
    expect(wrapper.find("[data-action='drop-continue']").exists()).toBe(false);
    wrapper.unmount();
  });
});

describe("DragImportDialog", () => {
  const columns: Column[] = [
    { name: "id", typeName: "integer", nullable: false, primary: true, unique: true, auto: true },
    { name: "name", typeName: "text", nullable: false, primary: false, unique: false, auto: false },
    { name: "email", typeName: "text", nullable: false, primary: false, unique: false, auto: false },
  ];

  function dragCatalog(): CatalogConnection[] {
    return [{
      id: "shop",
      name: "Shop",
      engine: "postgres",
      env: "none",
      envLabel: "",
      envTone: "none",
      status: "connected",
      defaultDatabase: "app",
      foreignKeys: null,
      error: null,
      databases: [{
        name: "app",
        error: null,
        schemas: [{ name: "public", objects: [{ name: "customers", kind: "table", estimatedRows: 3, stale: false }], total: 1, error: null }],
      }],
    }];
  }

  it("points out when no connection is open and close cancels", async () => {
    const wrapper = mount(DragImportDialog, { props: { seed: { format: "csv", text: CSV_TEXT, fileName: "rows.csv" } } });
    expect(wrapper.text()).toContain("还没有已打开的连接");
    await wrapper.get(".btn").trigger("click");
    expect(wrapper.emitted("close")).toHaveLength(1);
    expect(invoke).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("continues a dropped csv into the existing import mapping and writes row by row", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") {
        return { engine: "postgres", schema: "public", name: "customers", kind: "table", columns, key: ["id"] };
      }
      if (command === "grid_commit") {
        return {
          applied: [
            { id: "import-1", sql: "INSERT", label: "新增 1 行" },
            { id: "import-2", sql: "INSERT", label: "新增 1 行" },
            { id: "import-3", sql: "INSERT", label: "新增 1 行" },
          ],
          failed: null,
          pendingIds: [],
          rolledBack: false,
          rule: "按顺序逐条提交，遇到错误即停止；已成功的语句不撤销。",
        };
      }
      throw new Error(`unexpected ${command}`);
    });
    replaceCatalog(dragCatalog());
    const wrapper = mount(DragImportDialog, { props: { seed: { format: "csv", text: CSV_TEXT, fileName: "rows.csv" } } });
    await pickTable(wrapper);
    await flushPromises();
    const dialog = wrapper.getComponent(ImportDialog);
    expect(dialog.get("[data-dropped-file]").text()).toContain("rows.csv");
    expect(dialog.text()).toContain("将写入 3 行");
    expect(dialog.text()).toContain("Shop");
    await dialog.get(".btn.primary").trigger("click");
    await flushPromises();
    const call = invoke.mock.calls.find((item) => item[0] === "grid_commit");
    expect(call?.[1].request.changes).toHaveLength(3);
    expect(JSON.stringify(call?.[1].request.changes)).toContain("a@example.com");
    wrapper.unmount();
  });

  it("writes nothing when the import is cancelled after picking a table", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") {
        return { engine: "postgres", schema: "public", name: "customers", kind: "table", columns, key: ["id"] };
      }
      throw new Error(`unexpected ${command}`);
    });
    replaceCatalog(dragCatalog());
    const wrapper = mount(DragImportDialog, { props: { seed: { format: "csv", text: CSV_TEXT, fileName: "rows.csv" } } });
    await pickTable(wrapper);
    await flushPromises();
    const dialog = wrapper.getComponent(ImportDialog);
    expect(dialog.text()).toContain("将写入 3 行");
    await dialog.findAll("button").find((button) => button.text() === "取消")!.trigger("click");
    expect(wrapper.emitted("close")).toHaveLength(1);
    expect(invoke.mock.calls.find((item) => item[0] === "grid_commit")).toBeUndefined();
    wrapper.unmount();
  });

  async function pickTable(wrapper: ReturnType<typeof mountDrag>) {
    const trigger = wrapper.get("[role=\"combobox\"][aria-label=\"导入的表\"]");
    (trigger.element as HTMLElement).click();
    await nextTick();
    const match = wrapper.findAll("[role=\"option\"]").find((node) => node.text() === "customers");
    expect(match, "customers option").toBeTruthy();
    (match!.element as HTMLElement).click();
    await nextTick();
  }

  function mountDrag() {
    return mount(DragImportDialog, { props: { seed: { format: "csv", text: CSV_TEXT, fileName: "rows.csv" } } });
  }
});
