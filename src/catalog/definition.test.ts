import { mount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";
import { fetchDefinition } from "./backend";
import DefinitionView from "./DefinitionView.vue";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke,
}));

const object = {
  connectionId: "shop",
  database: "shop",
  schema: "",
  name: "add_customer",
  kind: "function" as const,
  signature: "",
  relation: "",
};

describe("definition view", () => {
  it("shows the server error instead of a blank success and has no save action", async () => {
    invoke.mockRejectedValueOnce("读取定义失败：permission denied");
    const failed = mount(DefinitionView, { props: { object, label: "add_customer" } });
    await vi.waitFor(() => {
      expect(failed.find("[data-definition-error]").exists()).toBe(true);
    });
    expect(failed.get("[data-definition-error]").text()).toContain("permission denied");
    expect(failed.find("[data-definition-sql]").exists()).toBe(false);
    expect(failed.text()).not.toContain("保存");
    failed.unmount();

    invoke.mockResolvedValueOnce("CREATE FUNCTION add_customer() RETURNS INT\nBEGIN\n  RETURN 1;\nEND");
    const wrapper = mount(DefinitionView, { props: { object, label: "add_customer" } });
    await vi.waitFor(() => {
      expect(wrapper.find("[data-definition-sql]").exists()).toBe(true);
    });
    expect(wrapper.get("[data-definition-sql]").text()).toContain("CREATE FUNCTION add_customer");
    expect(wrapper.get("[data-definition-sql]").attributes("data-readonly")).toBe("true");
    expect(wrapper.find("[data-action=save]").exists()).toBe(false);
    await wrapper.get("[data-action=copy-definition]").trigger("click");
    expect(wrapper.emitted("copy-sql")?.[0]?.[0]).toContain("CREATE FUNCTION add_customer");
    expect(invoke).toHaveBeenCalledWith("catalog_definition", expect.objectContaining({ name: "add_customer", kind: "function" }));
    wrapper.unmount();
  });
});

describe("sequence definition", () => {
  it("shows the read-only sequence sql and does not offer save", async () => {
    const sequence = { ...object, schema: "public", name: "orders_id_seq", kind: "sequence" as const };
    const sql = 'CREATE SEQUENCE "public"."orders_id_seq" AS bigint INCREMENT BY 1 MINVALUE 1 MAXVALUE 9223372036854775807 START WITH 1 CACHE 1 NO CYCLE;';
    invoke.mockResolvedValueOnce(sql);
    const wrapper = mount(DefinitionView, { props: { object: sequence, label: "orders_id_seq" } });
    await vi.waitFor(() => {
      expect(wrapper.find("[data-definition-sql]").exists()).toBe(true);
    });
    expect(wrapper.get("[data-definition]").attributes("data-kind")).toBe("sequence");
    expect(wrapper.get("[data-definition-sql]").text()).toBe(sql);
    expect(wrapper.get("[data-definition-sql]").attributes("data-readonly")).toBe("true");
    expect(wrapper.find("[data-action=save]").exists()).toBe(false);
    expect(wrapper.text()).not.toContain("保存");
    await wrapper.get("[data-action=copy-definition]").trigger("click");
    expect(wrapper.emitted("copy-sql")?.[0]?.[0]).toBe(sql);
    expect(invoke).toHaveBeenCalledWith("catalog_definition", expect.objectContaining({ name: "orders_id_seq", kind: "sequence" }));
    wrapper.unmount();
  });
});

describe("definition command client", () => {
  it("rejects an empty payload instead of treating it as success", async () => {
    invoke.mockResolvedValueOnce("  ");
    await expect(fetchDefinition(object)).rejects.toThrow("没有读到定义");
  });
});
