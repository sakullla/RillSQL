import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import TitleBar from "./TitleBar.vue";
import type { MenuDefinition } from "./menu-model";

const MENUS: MenuDefinition[] = [{ id: "file", label: "文件", items: [{ kind: "item", id: "new-query", label: "新建查询" }] }];

describe("TitleBar", () => {
  it("renders the menu row and the tool row", () => {
    const wrapper = mount(TitleBar, {
      props: { theme: "light", menus: MENUS, hasQueryTab: true },
      global: { stubs: { teleport: true } },
      attachTo: document.body,
    });
    expect(wrapper.find(".tb-menu-row").exists()).toBe(true);
    expect(wrapper.find(".tb-tool-row").exists()).toBe(true);
    expect(wrapper.get('[role="menubar"]').text()).toContain("文件");
    wrapper.unmount();
  });

  it("forwards menu selection", async () => {
    const wrapper = mount(TitleBar, {
      props: { theme: "light", menus: MENUS, hasQueryTab: true },
      global: { stubs: { teleport: true } },
      attachTo: document.body,
    });
    await wrapper.findAll('[role="menubar"] button')[0].trigger("click");
    await wrapper.vm.$nextTick();
    await wrapper.vm.$nextTick();
    await wrapper.get('[data-id="new-query"]').trigger("click");
    expect(wrapper.emitted("menu-select")?.[0]).toEqual(["file", "new-query"]);
    wrapper.unmount();
  });

  it("forwards toolbar actions", async () => {
    const wrapper = mount(TitleBar, {
      props: { theme: "light", menus: MENUS, hasQueryTab: true },
      global: { stubs: { teleport: true } },
      attachTo: document.body,
    });
    await wrapper.get('[data-action="new-query"]').trigger("click");
    expect(wrapper.emitted("action")?.[0]).toEqual(["new-query"]);
    wrapper.unmount();
  });
});
