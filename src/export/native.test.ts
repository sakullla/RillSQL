import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ExportDialog from "./ExportDialog.vue";
import TaskCenter from "./TaskCenter.vue";
import { DEFAULT_CSV, renderCsv } from "./csv";
import { planExport } from "./plan";
import { exportTasks, resetExportTasks, runExportTask } from "./tasks";

const { invoke, save } = vi.hoisted(() => ({ invoke: vi.fn(), save: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save }));

beforeEach(() => {
  vi.resetAllMocks();
  Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
});
afterEach(() => {
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
  resetExportTasks();
});

function dialog() {
  return mount(ExportDialog, { props: { open: true, origin: "table", truncated: false,
    fileStem: "customers", loadedCount: 2, pending: 0, columns: ["id", "name"], previewRows: [] } });
}
const path = "C:\\exports\\客户.csv";
const options = () => ({
  plan: planExport({ origin: "table", scope: "loaded", truncated: false, fileName: path }),
  acknowledged: false, path, columns: ["id", "name"],
  rows: [{ id: "1", name: "林夏" }, { id: "2", name: "周衡" }], csv: DEFAULT_CSV,
});

describe("native CSV export", () => {
  it("asks for a save location before starting and suppresses duplicate clicks", async () => {
    let resolve!: (value: string) => void;
    save.mockImplementation(() => new Promise<string>(done => { resolve = done; }));
    const wrapper = dialog();
    await wrapper.get(".btn.primary").trigger("click");
    await flushPromises();
    expect(save).toHaveBeenCalledWith(expect.objectContaining({ defaultPath: expect.stringMatching(/^customers-.*\.csv$/) }));
    expect(wrapper.get(".btn.primary").attributes("disabled")).toBeDefined();
    await wrapper.get(".btn.primary").trigger("click");
    expect(save).toHaveBeenCalledTimes(1);
    expect(wrapper.emitted("start")).toBeUndefined();
    resolve(path);
    await flushPromises();
    expect(wrapper.emitted("start")?.[0]?.[0]).toMatchObject({ path, scope: "loaded" });
    wrapper.unmount();
  });

  it("offers the markdown filter when exporting as markdown", async () => {
    save.mockResolvedValue(null);
    const wrapper = dialog();
    await wrapper.get("input[value='markdown']").setValue();
    expect((wrapper.get("input[aria-label='导出路径']").element as HTMLInputElement).value).toMatch(/\.md$/);
    await wrapper.get(".btn.primary").trigger("click");
    await flushPromises();
    expect(save).toHaveBeenCalledWith(expect.objectContaining({
      title: "导出为 Markdown",
      defaultPath: expect.stringMatching(/\.md$/),
      filters: [{ name: "Markdown 文件", extensions: ["md"] }],
    }));
    expect(wrapper.emitted("start")).toBeUndefined();
    wrapper.unmount();
  });

  it("keeps the dialog open on cancellation and displays picker errors", async () => {
    const wrapper = dialog();
    save.mockResolvedValueOnce(null).mockRejectedValueOnce(new Error("保存对话框不可用"));
    await wrapper.get(".btn.primary").trigger("click");
    await flushPromises();
    expect(wrapper.emitted("start")).toBeUndefined();
    expect(wrapper.emitted("close")).toBeUndefined();
    await wrapper.get(".btn.primary").trigger("click");
    await flushPromises();
    expect(wrapper.get("[role='alert']").text()).toContain("保存对话框不可用");
    expect(invoke).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("writes loaded rows through native IPC and only reports success after the write completes", async () => {
    let resolve!: () => void;
    invoke.mockImplementation(() => new Promise<void>(done => { resolve = done; }));
    const wrapper = mount(TaskCenter);
    const input = options();
    const pending = runExportTask(input);
    await wrapper.get("[aria-label='后台任务']").trigger("click");
    expect(wrapper.find("[data-status='running']").exists()).toBe(true);
    expect(invoke).toHaveBeenCalledExactlyOnceWith("export_write_loaded", { path, contents: renderCsv(input.columns, input.rows, input.csv) });
    resolve();
    const { result } = await pending;
    await flushPromises();
    expect(result.success).toBe(true);
    expect(result.message).toContain(path);
    expect(wrapper.find("[data-status='done']").exists()).toBe(true);
    expect(wrapper.find("[data-status='running']").exists()).toBe(false);
    wrapper.unmount();
  });

  it("shows a failed task when the native write fails, without claiming a download", async () => {
    invoke.mockRejectedValue("文件被占用");
    const { result } = await runExportTask(options());
    expect(result.success).toBe(false);
    expect(result.message).toContain("文件被占用");
    expect(result.message).not.toContain("开始下载");
    expect(exportTasks[0]?.status).toBe("failed");
  });
});
