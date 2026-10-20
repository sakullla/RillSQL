import { EditorView } from "@codemirror/view";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import QueryTab from "./QueryTab.vue";
import { MAX_SQL_FILE_BYTES, saveSqlFile, sqlFileProblem, sqlSaveName, SQL_FILE_ACCEPT } from "./file-io";

const { invoke, save } = vi.hoisted(() => ({ invoke: vi.fn(), save: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save }));

const createObjectURL = vi.fn(() => "blob:sql");
const revokeObjectURL = vi.fn();

function mountTab(draft = "SELECT 1"): VueWrapper {
  return mount(QueryTab, {
    props: {
      tabId: "query-file",
      draft,
      connectionId: null,
      connectionName: null,
      database: null,
      engine: null,
      connectionStatus: null,
      notice: null,
    },
  });
}

function pickFile(wrapper: VueWrapper, file: File): Promise<void> {
  const input = wrapper.get("[data-action='open-file-input']").element as HTMLInputElement;
  Object.defineProperty(input, "files", { configurable: true, value: { 0: file, length: 1 } });
  return wrapper.get("[data-action='open-file-input']").trigger("change");
}

function editorOf(wrapper: VueWrapper): EditorView {
  return EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
}

beforeEach(() => {
  vi.resetAllMocks();
  createObjectURL.mockReturnValue("blob:sql");
  URL.createObjectURL = createObjectURL;
  URL.revokeObjectURL = revokeObjectURL;
});

afterEach(() => {
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
});

describe("query file io", () => {
  it("offers only sql and plain text files when opening", async () => {
    expect(SQL_FILE_ACCEPT).toBe(".sql,.txt,text/plain");
    const wrapper = mountTab();
    await nextTick();
    const input = wrapper.get("[data-action='open-file-input']");
    expect(input.attributes("accept")).toBe(".sql,.txt,text/plain");
    expect(input.attributes("type")).toBe("file");
    expect(wrapper.get("[data-action='open-file']").text()).toBe("打开");
    wrapper.unmount();
  });

  it("builds the default save name from the date", () => {
    expect(sqlSaveName(new Date("2026-10-20T08:09:10Z"))).toBe("query-2026-10-20.sql");
  });

  it("rejects files over the 2 MB limit before reading them", async () => {
    expect(sqlFileProblem(new File(["x"], "ok.sql"))).toBeNull();
    const big = new File(["select 1"], "big.sql");
    Object.defineProperty(big, "size", { value: MAX_SQL_FILE_BYTES + 1 });
    expect(sqlFileProblem(big)).toContain("超过 2 MB 上限");

    const wrapper = mountTab("SELECT 1");
    await nextTick();
    await pickFile(wrapper, big);
    await flushPromises();
    expect(wrapper.get("[role='alert']").text()).toContain("超过 2 MB 上限，没有载入");
    expect(editorOf(wrapper).state.doc.toString()).toBe("SELECT 1");
    expect(wrapper.emitted("update:draft")).toBeUndefined();
    wrapper.unmount();
  });

  it("loads a chosen file into the current tab as a draft without running it", async () => {
    const wrapper = mountTab("SELECT 1");
    await nextTick();
    await pickFile(wrapper, new File(["SELECT count(*) FROM orders;"], "daily.sql", { type: "text/plain" }));
    await flushPromises();
    expect(editorOf(wrapper).state.doc.toString()).toBe("SELECT count(*) FROM orders;");
    expect(wrapper.emitted("update:draft")?.at(-1)).toEqual(["query-file", "SELECT count(*) FROM orders;"]);
    expect(wrapper.emitted("target")).toBeUndefined();
    expect(invoke).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("saves the draft through the save dialog and native IPC", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    save.mockResolvedValue("C:\\sql\\daily.sql");
    invoke.mockImplementation(async (command: string) => (command === "query_saved_list" ? [] : undefined));
    const wrapper = mountTab("SELECT 99");
    await nextTick();
    await wrapper.get("[data-action='saved']").trigger("click");
    await wrapper.get("[data-action='save-file']").trigger("click");
    await flushPromises();
    expect(save).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
      title: "保存 SQL 文件",
      defaultPath: expect.stringMatching(/^query-\d{4}-\d{2}-\d{2}\.sql$/),
      filters: [{ name: "SQL 文件", extensions: ["sql"] }],
    }));
    expect(invoke.mock.calls.filter((call) => call[0] === "export_write_loaded"))
      .toEqual([["export_write_loaded", { path: "C:\\sql\\daily.sql", contents: "SELECT 99" }]]);
    expect(wrapper.text()).toContain("已保存到 C:\\sql\\daily.sql");
    wrapper.unmount();
  });

  it("keeps the draft untouched when the save dialog is cancelled or fails to open", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    save.mockResolvedValue(null);
    const cancelled = await saveSqlFile({ defaultPath: "query.sql", contents: "SELECT 1" });
    expect(cancelled).toEqual({ status: "cancelled", message: "" });
    expect(invoke).not.toHaveBeenCalled();

    save.mockRejectedValue(new Error("保存对话框不可用"));
    const failed = await saveSqlFile({ defaultPath: "query.sql", contents: "SELECT 1" });
    expect(failed.status).toBe("failed");
    expect(failed.message).toContain("保存对话框不可用");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("reports a native write failure without claiming success", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    save.mockResolvedValue("report.sql");
    invoke.mockRejectedValue("文件被占用");
    const outcome = await saveSqlFile({ defaultPath: "query.sql", contents: "SELECT 1" });
    expect(outcome.status).toBe("failed");
    expect(outcome.message).toContain("文件被占用");
  });

  it("falls back to a browser download outside the native app", async () => {
    const appended: HTMLAnchorElement[] = [];
    const appendSpy = vi.spyOn(document.body, "append").mockImplementation((...nodes: (Node | string)[]) => {
      for (const node of nodes) if (node instanceof HTMLAnchorElement) appended.push(node);
    });
    const outcome = await saveSqlFile({ defaultPath: "query-2026-10-20.sql", contents: "SELECT 1" });
    appendSpy.mockRestore();
    expect(outcome).toEqual({ status: "saved", message: "已开始下载 query-2026-10-20.sql。" });
    expect(save).not.toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalled();
    expect(createObjectURL).toHaveBeenCalledTimes(1);
    expect(appended).toHaveLength(1);
    expect(appended[0]?.download).toBe("query-2026-10-20.sql");
  });

  it("downloads the draft from the save popover in the browser build", async () => {
    invoke.mockImplementation(async (command: string) => (command === "query_saved_list" ? [] : undefined));
    const wrapper = mountTab("SELECT 7");
    await nextTick();
    await wrapper.get("[data-action='saved']").trigger("click");
    await wrapper.get("[data-action='save-file']").trigger("click");
    await flushPromises();
    expect(save).not.toHaveBeenCalled();
    expect(invoke.mock.calls.filter((call) => call[0] === "export_write_loaded")).toEqual([]);
    expect(createObjectURL).toHaveBeenCalledTimes(1);
    expect(wrapper.text()).toContain("已开始下载 query-");
    wrapper.unmount();
  });
});
