import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TableTarget } from "../grid/target";
import { canDropIndex, statusLabel } from "./logic";

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
  rowCount: 1,
  columns: [
    { name: "id", typeName: "INTEGER", nullable: false, defaultSql: "", primary: true, auto: false, generated: false },
    { name: "note", typeName: "TEXT", nullable: true, defaultSql: "", primary: false, auto: false, generated: false },
  ],
  indexes: [
    { name: "sqlite_autoindex_notes_1", unique: true, columns: [{ name: "id", descending: false }], protected: true, reason: "由主键约束维护" },
    { name: "idx_note", unique: false, columns: [{ name: "note", descending: false }], protected: false, reason: "" },
  ],
  foreignKeyList: [],
  incoming: [],
  displaySql: "CREATE TABLE \"notes\" (id INTEGER PRIMARY KEY, note TEXT)",
};

function buttonNamed(wrapper: ReturnType<typeof mount>, text: string) {
  const found = wrapper.findAll("button").find((node) => node.text() === text);
  expect(found, text).toBeTruthy();
  return found!;
}

describe("structure editors", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(async (command: string) => {
      if (command === "ddl_describe") return described;
      return {};
    });
  });

  it("keeps a protected index from being deleted as a plain index", async () => {
    expect(canDropIndex(described.indexes[0]!)).toBe(false);
    expect(canDropIndex(described.indexes[1]!)).toBe(true);
    const wrapper = mount(StructurePane, { props: { tabId: "table:c1", draft: "", purpose: "edit", target } });
    await flushPromises();
    await buttonNamed(wrapper, "索引").trigger("click");
    expect(wrapper.text()).toContain("不能单独删除");
    expect(wrapper.findAll("button").filter((node) => node.text() === "删除")).toHaveLength(1);
    wrapper.unmount();
  });

  it("does not execute when the preview is cancelled", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "ddl_describe") return described;
      if (command === "ddl_preview") {
        return {
          blocked: [],
          risks: [{ level: "danger", text: "将永久删除 sqlite / notes。" }],
          steps: [{ sql: 'DROP TABLE "notes";', label: "删除 notes" }],
          rule: "这些语句逐条自动提交，失败即停。",
          rebuild: false,
          target: "sqlite / notes",
        };
      }
      throw new Error(`unexpected ${command}`);
    });
    const wrapper = mount(StructurePane, { props: { tabId: "table:c1", draft: "", purpose: "edit", target } });
    await flushPromises();
    await buttonNamed(wrapper, "删除表").trigger("click");
    await buttonNamed(wrapper, "预览 SQL").trigger("click");
    await flushPromises();
    expect(wrapper.get("[aria-label='变更预览']").text()).toContain('DROP TABLE "notes";');
    await buttonNamed(wrapper, "取消").trigger("click");
    expect(invoke.mock.calls.some((call) => call[0] === "ddl_execute")).toBe(false);
    expect(wrapper.find("[aria-label='变更预览']").exists()).toBe(false);
    wrapper.unmount();
  });

  it("disables confirm when the plan is blocked and shows failed and skipped receipts", async () => {
    expect(statusLabel("failed")).toBe("失败");
    expect(statusLabel("skipped")).toBe("未执行");
    invoke.mockImplementation(async (command: string) => {
      if (command === "ddl_describe") return described;
      if (command === "ddl_preview") {
        return { blocked: ["不能这样改。"], risks: [], steps: [], rule: "失败即停。", rebuild: false, target: "sqlite / notes" };
      }
      throw new Error(`unexpected ${command}`);
    });
    const blocked = mount(StructurePane, { props: { tabId: "table:c1", draft: "", purpose: "edit", target } });
    await flushPromises();
    await buttonNamed(blocked, "预览 SQL").trigger("click");
    await flushPromises();
    expect(buttonNamed(blocked, "执行").attributes("disabled")).toBeDefined();
    expect(blocked.text()).toContain("不能这样改。");
    blocked.unmount();

    invoke.mockImplementation(async (command: string) => {
      if (command === "ddl_describe") return described;
      if (command === "ddl_preview") {
        return {
          blocked: [],
          risks: [],
          steps: [
            { sql: "ALTER TABLE \"notes\" ADD COLUMN a TEXT;", label: "新增 a" },
            { sql: "ALTER TABLE \"notes\" ADD COLUMN b TEXT;", label: "新增 b" },
          ],
          rule: "已经成功的语句不撤销。",
          rebuild: false,
          target: "sqlite / notes",
        };
      }
      if (command === "ddl_execute") {
        return {
          blocked: [],
          rule: "已经成功的语句不撤销。",
          target: "sqlite / notes",
          rebuild: false,
          steps: [
            { sql: "ALTER TABLE \"notes\" ADD COLUMN a TEXT;", label: "新增 a", status: "failed", message: "重复列" },
            { sql: "ALTER TABLE \"notes\" ADD COLUMN b TEXT;", label: "新增 b", status: "skipped", message: "前面的语句没有成功，这条没有执行。" },
          ],
        };
      }
      return {};
    });
    const wrapper = mount(StructurePane, { props: { tabId: "table:c1", draft: "kept", purpose: "edit", target } });
    await flushPromises();
    expect(wrapper.attributes("data-draft")).toBe("kept");
    await buttonNamed(wrapper, "预览 SQL").trigger("click");
    await flushPromises();
    await buttonNamed(wrapper, "执行").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("失败");
    expect(wrapper.text()).toContain("未执行");
    const executeCall = invoke.mock.calls.find((call) => call[0] === "ddl_execute");
    expect(executeCall?.[1]).toMatchObject({ request: { confirmedSql: ["ALTER TABLE \"notes\" ADD COLUMN a TEXT;", "ALTER TABLE \"notes\" ADD COLUMN b TEXT;"] } });
    wrapper.unmount();
  });
});
