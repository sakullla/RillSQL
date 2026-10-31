import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import type { CatalogConnection } from "../catalog/types";
import type { Column } from "../grid/types";
import MigrateDialog from "./MigrateDialog.vue";
import { migrationHint, suggestMigrationMapping } from "./migrate";
import TaskCenter from "./TaskCenter.vue";
import { applyMigrateProgress, cancelTask, exportTasks, resetExportTasks, runMigrateTask } from "./tasks";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

afterEach(() => {
  resetExportTasks();
  resetCatalog();
});

const sourceColumns: Column[] = [
  { name: "id", typeName: "integer", nullable: false, primary: true, unique: true, auto: false },
  { name: "name", typeName: "text", nullable: true, primary: false, unique: false, auto: false },
  { name: "amount", typeName: "numeric", nullable: true, primary: false, unique: false, auto: false },
  { name: "legacy", typeName: "text", nullable: true, primary: false, unique: false, auto: false },
];
const targetColumns: Column[] = [
  { name: "id", typeName: "bigint", nullable: false, primary: true, unique: true, auto: false },
  { name: "Name", typeName: "varchar(40)", nullable: true, primary: false, unique: false, auto: false },
  { name: "amount", typeName: "integer", nullable: true, primary: false, unique: false, auto: false },
];

function migrationCatalog(): CatalogConnection[] {
  const connection = (id: string, name: string, engine: CatalogConnection["engine"], table: string): CatalogConnection => ({
    id,
    name,
    engine,
    env: "none",
    envLabel: "",
    envTone: "none",
    status: "connected",
    defaultDatabase: "app",
    foreignKeys: null,
    error: null,
    databases: [
      {
        name: "app",
        error: null,
        schemas: [{ name: "public", objects: [{ name: table, kind: "table", estimatedRows: 3, stale: false }], total: 1, error: null }],
      },
    ],
  });
  return [connection("pg", "旧库", "postgres", "orders"), connection("my", "新库", "mysql", "t_orders")];
}

/** 取最小结构而不是具体 Wrapper 类型：getComponent 返回的包装也能传进来。 */
interface PickWrapper {
  get: (selector: string) => { element: Element };
  findAll: (selector: string) => { element: Element; text: () => string }[];
}

async function pick(wrapper: PickWrapper, label: string, option: string) {
  const trigger = wrapper.get(`[role="combobox"][aria-label="${label}"]`);
  (trigger.element as HTMLElement).click();
  await nextTick();
  const match = wrapper.findAll('[role="option"]').find((node) => node.text() === option);
  expect(match, option).toBeTruthy();
  (match!.element as HTMLElement).click();
  await nextTick();
}

describe("data migration", () => {
  it("auto-maps same-name columns exactly first, case-insensitively next, and leaves the rest empty", () => {
    const mapping = suggestMigrationMapping(sourceColumns, targetColumns);
    expect(mapping).toEqual({ id: "id", name: "Name", amount: "amount", legacy: "" });
  });

  it("hints lossy families without blocking and passes safe pairs through", () => {
    expect(migrationHint(sourceColumns[0], targetColumns[0])).toBeNull();
    expect(migrationHint(sourceColumns[1], { ...targetColumns[1], typeName: "text" })).toBeNull();
    const limited = migrationHint(sourceColumns[1], targetColumns[1]);
    expect(limited).toContain("不会被截断");
    const truncating = migrationHint(sourceColumns[2], targetColumns[2]);
    expect(truncating).toContain("截断");
    expect(migrationHint(sourceColumns[1], targetColumns[0])).toContain("数字");
    expect(migrationHint(sourceColumns[1], { ...targetColumns[0], typeName: "blob" })).toContain("十六进制");
    expect(migrationHint({ ...sourceColumns[1], typeName: "geometry" }, targetColumns[1])).toContain("几何");
    expect(migrationHint({ ...sourceColumns[1], typeName: "bytea" }, targetColumns[1])).toContain("十六进制文本");
  });

  it("runs a migrate task, applies progress events, keeps the report and routes cancel", async () => {
    invoke.mockReset();
    invoke.mockImplementation(async (command: string) => {
      if (command === "migrate_data") {
        return {
          status: "done",
          success: true,
          message: "迁移完成：orders → t_orders。已插入 7 行，跳过 2 行（主键或唯一键冲突），失败 1 行。失败样本：9。",
          inserted: 7,
          skipped: 2,
          failed: 1,
          failedIds: ["9"],
          firstErrors: ["9：Data too long"],
          warnings: [],
        };
      }
      if (command === "migrate_cancel") return undefined;
      throw new Error(`unexpected ${command}`);
    });
    const { task, done } = runMigrateTask({
      id: "migrate-1",
      source: { connectionId: "pg", engine: "postgres", database: "app", schema: "public", table: "orders" },
      target: { connectionId: "my", engine: "mysql", database: "app", schema: "", table: "t_orders" },
      mapping: [{ source: "id", target: "id" }],
      options: null,
    });
    applyMigrateProgress({ id: "migrate-1", rows: 4, inserted: 3, skipped: 1, failed: 0 });
    expect(task.status).toBe("running");
    expect(task.doneRows).toBe(4);
    expect(task.message).toBe("正在迁移：已处理 4 行（插入 3、跳过 1、失败 0）");
    const finished = await done;
    expect(finished.status).toBe("done");
    expect(finished.doneRows).toBe(10);
    expect(finished.message).toContain("已插入 7 行");
    expect(finished.message).toContain("失败 1 行");
    expect(finished.report?.failedIds).toEqual(["9"]);
    expect(invoke).toHaveBeenCalledWith("migrate_data", {
      request: expect.objectContaining({
        source: expect.objectContaining({ connectionId: "pg", table: "orders" }),
        target: expect.objectContaining({ connectionId: "my", table: "t_orders" }),
        mapping: [{ source: "id", target: "id" }],
        options: null,
      }),
    });
    await cancelTask(finished);
    expect(invoke).toHaveBeenCalledWith("migrate_cancel", { id: "migrate-1" });
    invoke.mockReset();
  });

  it("starts a migration from the dialog with the auto mapping, type hints and explicit confirm", async () => {
    invoke.mockReset();
    invoke.mockImplementation(async (command: string, payload?: { request?: { table?: string } }) => {
      if (command === "grid_describe") {
        const table = payload?.request?.table ?? "";
        if (table === "orders") return { engine: "postgres", schema: "public", name: "orders", kind: "table", columns: sourceColumns, key: ["id"] };
        if (table === "t_orders") return { engine: "mysql", schema: "app", name: "t_orders", kind: "table", columns: targetColumns, key: ["id"] };
        throw new Error(`unexpected describe ${table}`);
      }
      if (command === "migrate_data") {
        return { status: "cancelled", success: false, message: "已取消迁移", inserted: 0, skipped: 0, failed: 0, failedIds: [], firstErrors: [], warnings: [] };
      }
      throw new Error(`unexpected ${command}`);
    });
    replaceCatalog(migrationCatalog());
    const wrapper = mount(TaskCenter);
    await wrapper.get("[aria-label='后台任务']").trigger("click");
    await wrapper.get("[data-action='migrate-open']").trigger("click");
    const dialog = wrapper.getComponent(MigrateDialog);
    expect(dialog.get("h2").text()).toBe("跨库数据迁移");
    await pick(dialog, "源表", "orders");
    await pick(dialog, "目标表", "t_orders");
    await flushPromises();
    // 同名自动映射：id/name（忽略大小写）/amount 已连上，legacy 留空。
    expect(dialog.get('[role="combobox"][aria-label="源列 id 映射到哪一列"]').text()).toContain("id");
    expect(dialog.get('[role="combobox"][aria-label="源列 legacy 映射到哪一列"]').text()).toContain("不迁移");
    // 类型提示：varchar 限长与 numeric→integer 都给警告，不阻断。
    expect(dialog.text()).toContain("不会被截断");
    expect(dialog.text()).toContain("截断");
    const start = dialog.get(".btn.primary");
    expect(start.attributes("disabled")).toBeDefined();
    const checks = dialog.findAll("input[type='checkbox']");
    for (const box of checks) await box.setValue(true);
    expect(start.attributes("disabled")).toBeUndefined();
    await start.trigger("click");
    await flushPromises();
    expect(dialog.emitted("close")).toHaveLength(1);
    expect(invoke).toHaveBeenCalledWith("migrate_data", {
      request: expect.objectContaining({
        source: expect.objectContaining({ connectionId: "pg", engine: "postgres", database: "app", schema: "public", table: "orders" }),
        target: expect.objectContaining({ connectionId: "my", engine: "mysql", database: "app", table: "t_orders" }),
        mapping: [{ source: "id", target: "id" }, { source: "name", target: "Name" }, { source: "amount", target: "amount" }],
        options: null,
      }),
    });
    const task = exportTasks.find((item) => item.kind === "migrate");
    expect(task?.fileName).toBe("迁移 orders → t_orders");
    wrapper.unmount();
  });

  it("shows the failed identities of a finished migration in the task row", async () => {
    exportTasks.push({
      id: "migrate-9",
      kind: "migrate",
      fileName: "迁移 orders → t_orders",
      path: "pg/orders → my/t_orders",
      status: "done",
      doneRows: 10,
      totalRows: 0,
      partial: false,
      message: "迁移完成：已插入 7 行，跳过 2 行（主键或唯一键冲突），失败 1 行。失败样本：9。",
      residualPath: null,
      report: { failedIds: ["9", "行#12"], firstErrors: ["9：Data too long", "行#12：cannot be null"] },
    });
    const wrapper = mount(TaskCenter);
    await wrapper.get("[aria-label='后台任务']").trigger("click");
    const row = wrapper.get('[data-status="done"]');
    expect(row.text()).toContain("已插入 7 行");
    await row.get(".tc-report summary").trigger("click");
    expect(row.text()).toContain("行#12");
    expect(row.text()).toContain("9：Data too long");
    wrapper.unmount();
  });

  it("marks an empty catalog with a hint and keeps start disabled", async () => {
    const wrapper = mount(MigrateDialog, { props: { open: true } });
    expect(wrapper.text()).toContain("还没有已打开的连接");
    expect(wrapper.get(".btn.primary").attributes("disabled")).toBeDefined();
    wrapper.unmount();
  });
});
