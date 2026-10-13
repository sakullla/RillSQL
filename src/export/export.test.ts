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
import { resetExportTasks } from "./tasks";
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

  it("shows export tasks in the titlebar control", () => {
    const wrapper = mount(TaskCenter);
    expect(wrapper.attributes("data-module")).toBe("export");
    expect(wrapper.get("[aria-label='后台任务']").text()).toContain("任务");
    wrapper.unmount();
  });
});
