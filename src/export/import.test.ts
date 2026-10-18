import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import ImportDialog from "./ImportDialog.vue";
import type { TableTarget } from "../grid/target";
import type { TableSchema } from "../grid/types";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const target: TableTarget = { connectionId: "shop", database: "app", schema: "public", table: "customers", engine: "postgres" };
const schema: TableSchema = {
  engine: "postgres",
  schema: "public",
  name: "customers",
  kind: "table",
  key: ["id"],
  columns: [
    { name: "id", typeName: "integer", nullable: false, primary: true, unique: true, auto: true },
    { name: "name", typeName: "text", nullable: false, primary: false, unique: false, auto: false },
    { name: "email", typeName: "text", nullable: false, primary: false, unique: true, auto: false },
    { name: "note", typeName: "text", nullable: true, primary: false, unique: false, auto: false },
  ],
};

function dialog() {
  return mount(ImportDialog, { props: { open: true, target, schema, tables: [{ schema: "public", table: "customers" }] } });
}

async function chooseFile(wrapper: ReturnType<typeof dialog>, name: string, contents: string) {
  const input = wrapper.get("input[type='file']");
  const file = new File([contents], name, { type: "text/plain" });
  Object.defineProperty(input.element, "files", { value: { 0: file, length: 1, item: () => file } });
  await input.trigger("change");
  await flushPromises();
}

describe("import into an existing table", () => {
  beforeEach(() => {
    invoke.mockReset();
    resetCatalog();
  });

  it("writes two mapped csv rows and reports the count", async () => {
    invoke.mockResolvedValue({
      applied: [
        { id: "import-1", sql: "INSERT", label: "新增 1 行" },
        { id: "import-2", sql: "INSERT", label: "新增 1 行" },
      ],
      failed: null,
      pendingIds: [],
      rolledBack: false,
      rule: "按顺序逐条提交，遇到错误即停止；已成功的语句不撤销。",
    });
    const wrapper = dialog();
    await chooseFile(wrapper, "rows.csv", "name,email,note\r\n甲,a@example.com,\r\n乙,b@example.com,备注");
    expect(wrapper.text()).toContain("将写入 2 行");
    await wrapper.get(".btn.primary").trigger("click");
    await flushPromises();
    const call = invoke.mock.calls.find((item) => item[0] === "grid_commit");
    const changes = call?.[1].request.changes as { values: { column: string; value: string | null }[] }[];
    expect(changes).toHaveLength(2);
    expect(changes[0]?.values).toEqual(expect.arrayContaining([{ column: "name", value: "甲" }, { column: "email", value: "a@example.com" }]));
    expect(JSON.stringify(changes)).not.toContain("未提交");
    expect(wrapper.get("[data-import-result]").text()).toContain("已写入 2 行");
    wrapper.unmount();
  });

  it("does not write when import is cancelled or a required column is missing", async () => {
    const cancelled = dialog();
    await chooseFile(cancelled, "rows.csv", "name,email\r\n甲,a@example.com");
    await cancelled.get(".btn").trigger("click");
    expect(cancelled.emitted("close")).toHaveLength(1);
    expect(invoke).not.toHaveBeenCalled();
    cancelled.unmount();

    const missing = dialog();
    await chooseFile(missing, "rows.csv", "name\r\n甲");
    expect(missing.text()).toContain("必填字段 email 没有对应列");
    expect(missing.get(".btn.primary").attributes("disabled")).toBeDefined();
    await missing.get(".btn.primary").trigger("click");
    expect(invoke).not.toHaveBeenCalled();
    missing.unmount();
  });

  it("rejects a json value that is not an object array before writing", async () => {
    const wrapper = dialog();
    await wrapper.get("input[value='json']").setValue(true);
    await chooseFile(wrapper, "rows.json", "{\"name\":\"甲\"}");
    expect(wrapper.text()).toContain("JSON 必须是由对象组成的数组");
    expect(invoke).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("shows the failed json row and does not call the import a full success", async () => {
    invoke.mockResolvedValue({
      applied: [{ id: "import-1", sql: "INSERT", label: "新增 1 行" }],
      failed: { id: "import-2", sql: "INSERT", label: "新增 1 行", message: "invalid input syntax for type integer" },
      pendingIds: ["import-2"],
      rolledBack: false,
      rule: "按顺序逐条提交，遇到错误即停止；已成功的语句不撤销。",
    });
    const wrapper = dialog();
    await wrapper.get("input[value='json']").setValue(true);
    await chooseFile(wrapper, "rows.json", "[{\"name\":\"甲\",\"email\":\"a@example.com\"},{\"name\":\"乙\",\"email\":\"bad\"}]");
    await wrapper.get(".btn.primary").trigger("click");
    await flushPromises();
    const text = wrapper.get("[data-import-result]").text();
    expect(text).toContain("第 2 行失败");
    expect(text).toContain("invalid input syntax for type integer");
    expect(text).toContain("不是全部成功");
    expect(text).not.toContain("已写入 2 行");
    wrapper.unmount();
  });

  it("shows the catalog connection, table and environment before writing, and cancel writes nothing", async () => {
    replaceCatalog([{
      id: "shop",
      name: "Shop",
      engine: "postgres",
      env: "prod",
      envLabel: "生产",
      envTone: "prod",
      status: "connected",
      defaultDatabase: "app",
      foreignKeys: null,
      error: null,
      databases: [],
    }]);
    const wrapper = dialog();
    const targetText = wrapper.get("[data-import-target]").text();
    expect(targetText).toContain("Shop");
    expect(targetText).toContain("customers");
    expect(targetText).toContain("生产");
    await chooseFile(wrapper, "rows.csv", "name,email,note\r\n甲,a@example.com,\r\n乙,b@example.com,备注");
    expect(wrapper.text()).toContain("将写入 2 行");
    expect(wrapper.get("[data-import-target]").text()).toContain("生产");
    expect(invoke).not.toHaveBeenCalled();
    await wrapper.findAll("button").find((button) => button.text() === "取消")!.trigger("click");
    expect(wrapper.emitted("close")).toHaveLength(1);
    expect(invoke).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("uses Listbox for the table and mapping selects and keeps the commit payload", async () => {
    invoke.mockResolvedValue({
      applied: [
        { id: "import-1", sql: "INSERT", label: "新增 1 行" },
        { id: "import-2", sql: "INSERT", label: "新增 1 行" },
      ],
      failed: null,
      pendingIds: [],
      rolledBack: false,
      rule: "按顺序逐条提交，遇到错误即停止；已成功的语句不撤销。",
    });
    const wrapper = dialog();
    expect(wrapper.find("select").exists()).toBe(false);
    expect(wrapper.get("button[role='combobox'][aria-label='导入的表']").exists()).toBe(true);
    await chooseFile(wrapper, "rows.csv", "name,email,note\r\n甲,a@example.com,\r\n乙,b@example.com,备注");
    expect(wrapper.find("select").exists()).toBe(false);
    const mappingLabels = wrapper
      .findAll("button[role='combobox']")
      .map((item) => item.attributes("aria-label") ?? "")
      .filter((label) => label.endsWith(" 对应的列"));
    expect(mappingLabels).toHaveLength(4);
    expect(mappingLabels).toEqual(expect.arrayContaining(["id 对应的列", "name 对应的列", "email 对应的列", "note 对应的列"]));

    await wrapper.get("button[aria-label='note 对应的列']").trigger("click");
    const option = wrapper.findAll("[role='option']").find((item) => item.text().trim() === "不导入");
    expect(option).toBeDefined();
    await option!.trigger("click");
    expect(wrapper.get("button[aria-label='note 对应的列']").text()).toContain("不导入");

    await wrapper.get(".btn.primary").trigger("click");
    await flushPromises();
    const call = invoke.mock.calls.find((item) => item[0] === "grid_commit");
    expect(call).toBeDefined();
    const changes = call![1].request.changes as { id: string; kind: string; values: { column: string; value: string | null }[] }[];
    expect(changes).toHaveLength(2);
    expect(changes[0]).toMatchObject({
      id: "import-1",
      kind: "insert",
      values: [
        { column: "name", value: "甲" },
        { column: "email", value: "a@example.com" },
      ],
    });
    expect(changes[1]?.values).toEqual([
      { column: "name", value: "乙" },
      { column: "email", value: "b@example.com" },
    ]);
    expect(JSON.stringify(changes)).not.toContain("note");
    wrapper.unmount();
  });
});
