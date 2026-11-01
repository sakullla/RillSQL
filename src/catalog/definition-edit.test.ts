import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import { tr } from "../i18n";
import type { StructurePlan, StructureReceipt } from "../structure/types";
import DefinitionView from "./DefinitionView.vue";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

afterEach(() => {
  invoke.mockReset();
});

function plan(): StructurePlan {
  return {
    blocked: [],
    risks: [{ level: "info", text: "" }],
    steps: [{ sql: "ALTER TYPE public.order_status ADD VALUE 'shipped' BEFORE 'paid';", label: "追加枚举取值" }],
    rule: "逐条执行",
    rebuild: false,
    target: "postgres / shop / public",
  };
}

function receipt(): StructureReceipt {
  return {
    blocked: [],
    rule: "逐条执行",
    target: "postgres / shop / public",
    rebuild: false,
    steps: [{ sql: "ALTER TYPE public.order_status ADD VALUE 'shipped' BEFORE 'paid';", label: "追加枚举取值", status: "ok", message: "" }],
  };
}

function mockBackend() {
  invoke.mockImplementation((command: string) => {
    if (command === "catalog_definition") return Promise.resolve("CREATE TYPE public.order_status AS ENUM ('paid');");
    if (command === "ddl_preview") return Promise.resolve(plan());
    if (command === "ddl_execute") return Promise.resolve(receipt());
    return Promise.resolve(null);
  });
}

function calls(command: string): unknown[] {
  return invoke.mock.calls.filter(([name]) => name === command).map(([, args]) => args);
}

const enumObject = {
  connectionId: "shop",
  database: "shop",
  schema: "public",
  name: "order_status",
  kind: "enum" as const,
  signature: "",
  relation: "",
};

function confirmButton(wrapper: ReturnType<typeof mount>) {
  return wrapper.findAll("button").find((node) => node.text().trim() === tr("structure.change.confirmExecute"));
}

describe("definition editing entries", () => {
  it("previews addEnumValue before running it and reloads the definition", async () => {
    mockBackend();
    const wrapper = mount(DefinitionView, { props: { object: enumObject, label: "order_status", engine: "postgres" } });
    await flushPromises();

    const form = wrapper.get("[data-definition-edit]");
    expect(form.attributes("data-edit-kind")).toBe("enum");
    await wrapper.get("[data-field='enum-value']").setValue("shipped");
    await wrapper.get("[data-field='enum-before']").setValue("paid");
    await form.trigger("submit");
    await flushPromises();

    const preview = calls("ddl_preview")[0] as { request: { schema: string; action: unknown } };
    expect(preview.request.schema).toBe("public");
    expect(preview.request.action).toEqual({
      kind: "addEnumValue",
      enumName: "order_status",
      value: "shipped",
      before: "paid",
    });
    // 先预览，确认之前不发执行命令。
    expect(calls("ddl_execute")).toHaveLength(0);

    const confirm = confirmButton(wrapper);
    expect(confirm, "确认执行按钮").toBeTruthy();
    await confirm!.trigger("click");
    await flushPromises();

    const executed = calls("ddl_execute")[0] as { request: { confirmedSql: string[]; action: unknown } };
    expect(executed.request.confirmedSql).toEqual(["ALTER TYPE public.order_status ADD VALUE 'shipped' BEFORE 'paid';"]);
    expect(executed.request.action).toEqual({
      kind: "addEnumValue",
      enumName: "order_status",
      value: "shipped",
      before: "paid",
    });
    // 执行成功后重新读定义：目录与视图里都能看到新值。
    expect(calls("catalog_definition")).toHaveLength(2);
    wrapper.unmount();
  });

  it("previews alterDomain with only the parts that changed", async () => {
    mockBackend();
    const domain = { ...enumObject, name: "email_addr", kind: "domain" as const };
    const wrapper = mount(DefinitionView, { props: { object: domain, label: "email_addr", engine: "postgres" } });
    await flushPromises();

    expect(wrapper.get("[data-definition-edit]").attributes("data-edit-kind")).toBe("domain");
    await wrapper.get("[data-field='domain-default']").setValue("'noreply@example.com'");
    await wrapper.get("[data-definition-edit]").trigger("submit");
    await flushPromises();

    const preview = calls("ddl_preview")[0] as { request: { action: unknown } };
    expect(preview.request.action).toEqual({
      kind: "alterDomain",
      domain: "email_addr",
      defaultSql: "'noreply@example.com'",
      notNull: null,
    });
    wrapper.unmount();
  });

  it("does not offer editing on engines or kinds without alterable definitions", async () => {
    mockBackend();
    const wrapper = mount(DefinitionView, { props: { object: enumObject, label: "order_status", engine: "mysql" } });
    await flushPromises();
    expect(wrapper.find("[data-definition-edit]").exists()).toBe(false);
    wrapper.unmount();

    const sequence = { ...enumObject, name: "orders_seq", kind: "sequence" as const };
    const readOnly = mount(DefinitionView, { props: { object: sequence, label: "orders_seq", engine: "postgres" } });
    await flushPromises();
    expect(readOnly.find("[data-definition-edit]").exists()).toBe(false);
    expect(readOnly.get("[data-definition-sql]").attributes("data-readonly")).toBe("true");
    readOnly.unmount();
  });
});
