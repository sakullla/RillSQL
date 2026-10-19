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

describe("definition command client", () => {
  it("rejects an empty payload instead of treating it as success", async () => {
    invoke.mockResolvedValueOnce("  ");
    await expect(fetchDefinition(object)).rejects.toThrow("没有读到定义");
  });
});
