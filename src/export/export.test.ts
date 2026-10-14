import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { mount } from "@vue/test-utils";
import { afterEach, describe, expect, it } from "vitest";
import { customersFixture } from "../grid/fixture";
import { queryPage } from "../grid/data";
import { selectExportGate } from "../grid/sql";
import { parseCsvDistinct, renderCsv, type CsvOptions } from "./csv";
import { planExport } from "./plan";
import TaskCenter from "./TaskCenter.vue";
import { exportTasks, resetExportTasks } from "./tasks";
import { startExport } from "./write";

const csv: CsvOptions = { delimiter: ",", nullToken: "", bom: false, header: true };

afterEach(() => {
  resetExportTasks();
});

function tempDir(): string {
  return mkdtempSync(join(tmpdir(), "rillsql-export-"));
}

describe("csv export", () => {
  it("round-trips quotes, commas, newlines, chinese, null and empty string", () => {
    const columns = ["id", "note"];
    const rows = [
      { id: "1", note: null },
      { id: "2", note: "" },
      { id: "3", note: '他说"你好"，\n下一行,还有逗号' },
    ];
    const text = renderCsv(columns, rows, csv);
    expect(text).toContain(',""');
    expect(text.split("\r\n")[1]).toBe('"1",');
    const parsed = parseCsvDistinct(text, ",");
    expect(parsed[1]?.[1]).toEqual({ value: null, quoted: false });
    expect(parsed[2]?.[1]).toEqual({ value: "", quoted: true });
    expect(parsed[3]?.[1]?.value).toBe('他说"你好"，\n下一行,还有逗号');
    const marked = renderCsv(columns, rows, { ...csv, nullToken: "NULL" });
    expect(marked).toContain(",NULL");
    expect(parseCsvDistinct(marked, ",")[1]?.[1]?.value).toBe("NULL");
    expect(parseCsvDistinct(marked, ",")[2]?.[1]?.value).toBe("");
  });

  it("marks a partial file and only re-executes a single select", () => {
    const partial = planExport({ origin: "table", scope: "loaded", truncated: true, fileName: "customers-2026-10-14.csv" });
    expect(partial.partial).toBe(true);
    expect(partial.complete).toBe(false);
    expect(partial.fileName).toBe("customers-2026-10-14（部分）.csv");
    expect(partial.notice).toContain("不是完整结果");
    expect(selectExportGate("/* c */ SELECT id FROM t").ok).toBe(true);
    expect(selectExportGate("-- note\nselect 1").ok).toBe(true);
    expect(selectExportGate("WITH c AS (SELECT 1) SELECT * FROM c").ok).toBe(false);
    expect(selectExportGate("SELECT 1; DELETE FROM t").ok).toBe(false);
    expect(selectExportGate("DELETE FROM t").reason).toContain("SELECT");
    const blocked = planExport({ origin: "query", scope: "full", statement: "UPDATE t SET a = 1", truncated: true, fileName: "q.csv" });
    expect(blocked.allowed).toBe(false);
    const allowed = planExport({ origin: "query", scope: "full", statement: "SELECT id, note FROM customers", truncated: true, fileName: "q.csv" });
    expect(allowed.reexecutes).toBe(true);
    expect(allowed.notice).toContain("重新执行这条 SELECT");
  });

  it("does not read again until the re-execution notice is accepted", () => {
    const plan = planExport({ origin: "query", scope: "full", statement: "SELECT 1", truncated: false, fileName: "q.csv" });
    let reads = 0;
    const blocked = startExport({
      plan,
      acknowledged: false,
      path: join(tempDir(), "q.csv"),
      columns: ["id"],
      rows: [{ id: "1" }],
      csv,
      readRows: () => {
        reads += 1;
        return [{ id: "9" }];
      },
    });
    expect(blocked.status).toBe("blocked");
    expect(blocked.success).toBe(false);
    expect(reads).toBe(0);
    const dir = tempDir();
    const path = join(dir, "q.csv");
    const done = startExport({
      plan,
      acknowledged: true,
      path,
      columns: ["id"],
      rows: [{ id: "1" }],
      csv,
      readRows: () => {
        reads += 1;
        return [{ id: "9" }];
      },
    });
    expect(done.success).toBe(true);
    expect(reads).toBe(1);
    expect(readFileSync(path, "utf8")).toContain('"9"');
    rmSync(dir, { recursive: true, force: true });
  });

  it("keeps a partial file on failure and deletes it on cancel", () => {
    const dir = tempDir();
    const path = join(dir, "customers（部分）.csv");
    const rows = [{ id: "1", note: "甲" }, { id: "2", note: "乙" }, { id: "3", note: "丙" }];
    const plan = planExport({ origin: "table", scope: "loaded", truncated: true, fileName: path });
    const failed = startExport({
      plan,
      acknowledged: false,
      path: plan.fileName,
      columns: ["id", "note"],
      rows,
      csv,
      control: { failAfterRows: 1, failMessage: "磁盘空间不足" },
    });
    expect(failed.success).toBe(false);
    expect(failed.status).toBe("failed");
    expect(failed.message).toContain("不会冒充完整结果");
    expect(existsSync(path)).toBe(false);
    expect(existsSync(`${path}.partial`)).toBe(true);
    expect(readFileSync(`${path}.partial`, "utf8")).toContain("甲");
    expect(readFileSync(`${path}.partial`, "utf8")).not.toContain("乙");

    const cancelled = startExport({
      plan,
      acknowledged: false,
      path: join(dir, "other.csv"),
      columns: ["id", "note"],
      rows,
      csv,
      control: { cancelAfterRows: 1 },
    });
    expect(cancelled.status).toBe("cancelled");
    expect(cancelled.success).toBe(false);
    expect(cancelled.message).toContain("未完成的文件已删除");
    expect(existsSync(join(dir, "other.csv"))).toBe(false);
    expect(existsSync(join(dir, "other.csv.partial"))).toBe(false);

    const missing = startExport({
      plan,
      acknowledged: false,
      path: join(dir, "missing", "nested", "out.csv"),
      columns: ["id"],
      rows: [{ id: "1" }],
      csv,
    });
    expect(missing.success).toBe(false);
    expect(missing.message).toContain("路径不可写");
    rmSync(dir, { recursive: true, force: true });
  });

  it("exports the server-side filtered rows rather than only the open page", () => {
    const store = customersFixture(130);
    const page = queryPage(store, { filters: [{ column: "id", op: "gt", value: "100" }], sort: null, offset: 0, limit: 10 });
    expect(page.ok).toBe(true);
    if (!page.ok) return;
    const plan = planExport({ origin: "table", scope: "full", truncated: page.truncated, fileName: "customers.csv" });
    const dir = tempDir();
    const path = join(dir, plan.fileName);
    const result = startExport({
      plan,
      acknowledged: false,
      path,
      columns: ["id"],
      rows: page.rows.map((row) => ({ id: row.cells.id })),
      csv,
      readRows: () => {
        const all = queryPage(store, { filters: [{ column: "id", op: "gt", value: "100" }], sort: null, offset: 0, limit: 1_000_000 });
        if (!all.ok) return [];
        return all.rows.map((row) => ({ id: row.cells.id }));
      },
    });
    expect(result.success).toBe(true);
    expect(result.writtenRows).toBe(30);
    expect(result.message).not.toContain("不是完整结果");
    rmSync(dir, { recursive: true, force: true });
  });

  it("shows running progress and plain statuses, and clears finished tasks", async () => {
    exportTasks.push(
      { id: "t-run", fileName: "orders.csv", path: "/tmp/orders.csv", status: "running", doneRows: 40, totalRows: 100, partial: false, message: "正在导出", residualPath: null },
      { id: "t-done", fileName: "customers（部分）.csv", path: "/tmp/customers（部分）.csv", status: "done", doneRows: 100, totalRows: 100, partial: true, message: "已导出 100 行。", residualPath: null },
      { id: "t-fail", fileName: "ledger.csv", path: "/tmp/ledger.csv", status: "failed", doneRows: 12, totalRows: 100, partial: false, message: "磁盘空间不足。已写入 12 行，不完整的文件保留为 ledger.csv.partial。", residualPath: "/tmp/ledger.csv.partial" },
      { id: "t-cancel", fileName: "notes.csv", path: "/tmp/notes.csv", status: "cancelled", doneRows: 5, totalRows: 100, partial: false, message: "已取消导出：写入 5 行后停止，未完成的文件已删除。", residualPath: null },
    );
    const wrapper = mount(TaskCenter);
    expect(wrapper.attributes("data-module")).toBe("export");
    const trigger = wrapper.get("[aria-label='后台任务']");
    expect(trigger.text()).toContain("导出中 40/100");
    await trigger.trigger("click");
    const text = wrapper.text();
    expect(text).toContain("后台任务");
    expect(text).toContain("进行中");
    expect(text).toContain("正在导出：已写入 40 / 100 行");
    expect(text).toContain("已完成：写入 100 行");
    expect(text).toContain("不是完整结果");
    expect(text).toContain("导出失败：磁盘空间不足");
    expect(text).toContain("已取消：未完成的文件已删除");
    const runningRow = wrapper.get("[data-status='running']");
    expect(runningRow.get("[role='progressbar']").attributes("aria-valuenow")).toBe("40");
    expect(runningRow.get("button").text()).toBe("取消");
    expect(wrapper.find("[data-status='done'] button").exists()).toBe(false);
    await wrapper.get("[data-action='clear-finished']").trigger("click");
    expect(exportTasks.map((task) => task.id)).toEqual(["t-run"]);
    wrapper.unmount();
  });

  it("shows an idle trigger and an empty list when nothing is running", async () => {
    const wrapper = mount(TaskCenter);
    const trigger = wrapper.get("[aria-label='后台任务']");
    expect(trigger.text()).toContain("任务");
    await trigger.trigger("click");
    expect(wrapper.text()).toContain("没有后台任务");
    wrapper.unmount();
  });
});
