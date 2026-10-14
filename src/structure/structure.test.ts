import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TableTarget } from "../grid/target";
import {
  canDropIndex,
  columnImpact,
  draftsFromColumns,
  impactLevel,
  pendingCount,
  riskLevelLabel,
  statusLabel,
  transactionSummary,
} from "./logic";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke,
}));

import StructurePane from "./StructurePane.vue";

const target: TableTarget = { connectionId: "c1", database: "main", schema: "", table: "notes", engine: "sqlite" };

const described = {
  engine: "sqlite",
  database: "main",
  schema: "",
  name: "notes",
  kind: "table",
  storage: null,
  sqliteVersion: "3.51.3",
  foreignKeys: false,
  recordedForeignKeys: false,
  rowCount: 3,
  columns: [
    { name: "id", typeName: "INTEGER", nullable: false, defaultSql: "", primary: true, auto: false, generated: false },
    { name: "note", typeName: "TEXT", nullable: true, defaultSql: "", primary: false, auto: false, generated: false },
  ],
  indexes: [
    { name: "sqlite_autoindex_notes_1", unique: true, columns: [{ name: "id", descending: false }], protected: true, reason: "由主键约束维护" },
    { name: "idx_note", unique: false, columns: [{ name: "note", descending: false }], protected: false, reason: "" },
  ],
  foreignKeyList: [],
  incoming: [{ name: "note_fkey", table: "logs", schema: "", columns: ["note"], refColumns: ["note"] }],
  displaySql: 'CREATE TABLE "notes" (id INTEGER PRIMARY KEY, note TEXT)',
};

function buttonNamed(wrapper: ReturnType<typeof mount>, text: string) {
  const found = wrapper.findAll("button").find((node) => node.text() === text);
  expect(found, `button "${text}"`).toBeTruthy();
  return found!;
}

function planOf(overrides: Record<string, unknown>) {
  return {
    blocked: [] as string[],
    risks: [] as { level: string; text: string }[],
    steps: [] as { sql: string; label: string }[],
    rule: "这些语句逐条执行，失败即停。已经成功的语句不撤销，后面的语句不执行。",
    rebuild: false,
    target: "sqlite / notes",
    ...overrides,
  };
}

describe("structure presentation logic", () => {
  it("labels receipt statuses in plain words", () => {
    expect(statusLabel("ok")).toBe("已完成");
    expect(statusLabel("failed")).toBe("失败");
    expect(statusLabel("skipped")).toBe("未执行");
    expect(statusLabel("unknown")).toBe("结果未知");
    expect(statusLabel("rolled_back")).toBe("已回滚");
  });

  it("summarizes transaction semantics per engine without promising rollback the app does not do", () => {
    expect(transactionSummary("sqlite", true)).toContain("重建整张表");
    expect(transactionSummary("sqlite", true)).toContain("整体回滚");
    expect(transactionSummary("postgres", false)).toContain("PostgreSQL");
    expect(transactionSummary("postgres", false)).toContain("已经成功的语句不会撤销");
    expect(transactionSummary("mysql", false)).toContain("不能整体回滚");
    expect(transactionSummary("mariadb", false)).toContain("不能整体回滚");
    expect(transactionSummary("sqlite", false)).toContain("失败即停");
  });

  it("describes each row change in plain words and levels the risky ones", () => {
    const baseline = described.columns;
    const added = { original: null, name: "email", typeName: "TEXT", nullable: true, defaultSql: "", primary: false, auto: false, dropped: false };
    expect(columnImpact(added, undefined)).toBe("新增字段 email，类型 TEXT");
    expect(impactLevel(added, undefined)).toBe("info");
    const retyped = { ...draftsFromColumns(baseline)[1]!, typeName: "INTEGER" };
    expect(columnImpact(retyped, baseline[1])).toContain("类型由 TEXT 改为 INTEGER");
    expect(columnImpact(retyped, baseline[1])).toContain("无法转换的值会使执行失败");
    expect(impactLevel(retyped, baseline[1])).toBe("warn");
    const dropped = { ...draftsFromColumns(baseline)[1]!, dropped: true };
    expect(columnImpact(dropped, baseline[1])).toBe("删除字段 note：该列的数据会永久丢掉");
    expect(impactLevel(dropped, baseline[1])).toBe("danger");
    expect(pendingCount(draftsFromColumns(baseline), baseline)).toBe(0);
    expect(pendingCount([...draftsFromColumns(baseline), added], baseline)).toBe(1);
    expect(riskLevelLabel("danger")).toBe("危险");
    expect(riskLevelLabel("warn")).toBe("注意");
    expect(riskLevelLabel("info")).toBe("说明");
    expect(canDropIndex(described.indexes[0]!)).toBe(false);
    expect(canDropIndex(described.indexes[1]!)).toBe(true);
  });
});

describe("structure pane", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(async (command: string) => {
      if (command === "ddl_describe") return described;
      return {};
    });
  });

  it("keeps the current connection and database readable and marks staged field changes", async () => {
    const wrapper = mount(StructurePane, { props: { tabId: "table:c1", draft: "kept", purpose: "edit", target } });
    await flushPromises();
    expect(wrapper.attributes("data-draft")).toBe("kept");
    expect(wrapper.text()).toContain("正在修改表 notes");
    expect(wrapper.text()).toContain("来自连接「c1」的「main」");
    expect(wrapper.text()).toContain("直接改下面的字段");
    await buttonNamed(wrapper, "新增字段").trigger("click");
    await wrapper.get("[aria-label='字段名 3']").setValue("email");
    expect(wrapper.text()).toContain("新增字段 email，类型 TEXT");
    expect(wrapper.text()).toContain("1 处更改未执行");
    await wrapper.get("[aria-label='类型 2']").setValue("INTEGER");
    expect(wrapper.text()).toContain("类型由 TEXT 改为 INTEGER");
    expect(wrapper.text()).toContain("已修改");
    wrapper.unmount();
  });

  it("keeps a protected index from being deleted as a plain index", async () => {
    const wrapper = mount(StructurePane, { props: { tabId: "table:c1", draft: "", purpose: "edit", target } });
    await flushPromises();
    await buttonNamed(wrapper, "索引").trigger("click");
    expect(wrapper.text()).toContain("由主键约束维护，不能单独删除");
    expect(wrapper.findAll("button").filter((node) => node.text() === "删除")).toHaveLength(1);
    wrapper.unmount();
  });

  it("explains the drop in plain words and keeps the typed-name confirmation", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "ddl_describe") return described;
      if (command === "ddl_preview") {
        return planOf({
          risks: [{ level: "danger", text: "将永久删除 sqlite / notes 的结构和约 3 行数据。这不是备份。" }],
          steps: [{ sql: 'DROP TABLE "notes";', label: "删除 notes" }],
        });
      }
      throw new Error(`unexpected ${command}`);
    });
    const wrapper = mount(StructurePane, { props: { tabId: "table:c1", draft: "", purpose: "edit", target } });
    await flushPromises();
    await buttonNamed(wrapper, "删除表").trigger("click");
    /* 删除区直白说明：数据量可见，依赖列表收进折叠详情（R3）。 */
    expect(wrapper.text()).toContain("将永久删除这张表和里面约 3 行数据");
    expect(wrapper.text()).toContain("1 个外键引用这张表");
    const depDetails = wrapper.get("details.disclosure");
    expect(depDetails.attributes("open")).toBeUndefined();
    await depDetails.get("summary").trigger("click");
    expect(wrapper.text()).toContain("logs.note_fkey");

    await buttonNamed(wrapper, "查看将要发生什么").trigger("click");
    await flushPromises();
    const dialog = wrapper.get("[aria-label='变更预览']");
    expect(dialog.text()).toContain("目标：sqlite / notes");
    expect(dialog.text()).toContain("将要发生什么");
    expect(dialog.text()).toContain("将永久删除 sqlite / notes 的结构和约 3 行数据");
    expect(dialog.text()).toContain("这些语句逐条执行，失败即停");
    expect(dialog.text()).toContain("输入表名 notes 确认");
    expect(wrapper.get("[aria-label='确认表名']").attributes("disabled")).toBeUndefined();

    /* 分级确认：执行按钮在名字输入正确前不可用。 */
    const run = () => buttonNamed(wrapper, "确认执行");
    expect(run().attributes("disabled")).toBeDefined();
    await wrapper.get("[aria-label='确认表名']").setValue("other");
    expect(run().attributes("disabled")).toBeDefined();
    await wrapper.get("[aria-label='确认表名']").setValue("notes");
    expect(run().attributes("disabled")).toBeUndefined();

    /* 逐条 SQL 默认收进折叠详情。 */
    expect(dialog.find("details.disclosure").attributes("open")).toBeUndefined();
    await buttonNamed(wrapper, "取消").trigger("click");
    expect(invoke.mock.calls.some((call) => call[0] === "ddl_execute")).toBe(false);
    expect(wrapper.find("[aria-label='变更预览']").exists()).toBe(false);
    wrapper.unmount();
  });

  it("blocks execution with the precheck reason visible instead of hiding it", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "ddl_describe") return described;
      if (command === "ddl_preview") return planOf({ blocked: ["表含 CHECK 约束，重建不能可靠保留该约束，因此阻止执行。"] });
      throw new Error(`unexpected ${command}`);
    });
    const wrapper = mount(StructurePane, { props: { tabId: "table:c1", draft: "", purpose: "edit", target } });
    await flushPromises();
    await wrapper.get("[aria-label='类型 2']").setValue("INTEGER");
    await buttonNamed(wrapper, "查看将要发生什么").trigger("click");
    await flushPromises();
    const dialog = wrapper.get("[aria-label='变更预览']");
    expect(dialog.get("[role='alert']").text()).toContain("表含 CHECK 约束，重建不能可靠保留该约束，因此阻止执行。");
    expect(buttonNamed(wrapper, "确认执行").attributes("disabled")).toBeDefined();
    wrapper.unmount();
  });

  it("reports each step in plain words and expands the details when a step fails", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "ddl_describe") return described;
      if (command === "ddl_preview") {
        return planOf({
          steps: [
            { sql: 'ALTER TABLE "notes" ADD COLUMN a TEXT;', label: "新增字段 a" },
            { sql: 'ALTER TABLE "notes" ADD COLUMN b TEXT;', label: "新增字段 b" },
          ],
        });
      }
      if (command === "ddl_execute") {
        return {
          blocked: [],
          rule: "这些语句逐条执行，失败即停。已经成功的语句不撤销，后面的语句不执行。",
          target: "sqlite / notes",
          rebuild: false,
          steps: [
            { sql: 'ALTER TABLE "notes" ADD COLUMN a TEXT;', label: "新增字段 a", status: "ok", message: "" },
            { sql: 'ALTER TABLE "notes" ADD COLUMN b TEXT;', label: "新增字段 b", status: "failed", message: "重复列" },
            { sql: 'ALTER TABLE "notes" ADD COLUMN c TEXT;', label: "新增字段 c", status: "skipped", message: "前面的语句没有成功，这条没有执行。" },
          ],
        };
      }
      return {};
    });
    const wrapper = mount(StructurePane, { props: { tabId: "table:c1", draft: "", purpose: "edit", target } });
    await flushPromises();
    await wrapper.get("[aria-label='类型 2']").setValue("INTEGER");
    await buttonNamed(wrapper, "查看将要发生什么").trigger("click");
    await flushPromises();
    await buttonNamed(wrapper, "确认执行").trigger("click");
    await flushPromises();
    const dialog = wrapper.get("[aria-label='变更预览']");
    expect(dialog.text()).toContain("每一步的结果");
    expect(dialog.text()).toContain("已完成");
    expect(dialog.text()).toContain("失败");
    expect(dialog.text()).toContain("未执行");
    /* 失败场景自动展开详情，逐条 SQL 一步可达。 */
    expect(dialog.get("details.disclosure").attributes("open")).toBeDefined();
    expect(dialog.text()).toContain('ALTER TABLE "notes" ADD COLUMN b TEXT;');
    const executeCall = invoke.mock.calls.find((call) => call[0] === "ddl_execute");
    expect(executeCall?.[1]).toMatchObject({
      request: { confirmedSql: ['ALTER TABLE "notes" ADD COLUMN a TEXT;', 'ALTER TABLE "notes" ADD COLUMN b TEXT;'] },
    });
    wrapper.unmount();
  });
});
