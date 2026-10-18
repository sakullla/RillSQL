import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import AppShell from "./AppShell.vue";

async function openMenu(wrapper: ReturnType<typeof mount>, label: string) {
  const button = wrapper.findAll('[role="menubar"] button').find((item) => item.text() === label);
  expect(button, `menubar button ${label}`).toBeTruthy();
  await button!.trigger("click");
  await wrapper.vm.$nextTick();
  await wrapper.vm.$nextTick();
}

afterEach(() => {
  location.hash = "";
  delete document.documentElement.dataset.theme;
});

describe("AppShell menu and toolbar mapping", () => {
  it("applies the dark theme from the view menu", async () => {
    const wrapper = mount(AppShell, {
      global: { stubs: { teleport: true } },
      attachTo: document.body,
    });
    await flushPromises();
    await openMenu(wrapper, "视图");
    await wrapper.get('[data-id="theme-dark"]').trigger("click");
    await flushPromises();
    expect(document.documentElement.dataset.theme).toBe("dark");
    wrapper.unmount();
  });

  it("opens the save flow from the file menu", async () => {
    const wrapper = mount(AppShell, {
      global: { stubs: { teleport: true } },
      attachTo: document.body,
    });
    await flushPromises();
    await openMenu(wrapper, "文件");
    await wrapper.get('[data-id="save-draft"]').trigger("click");
    // HistoryPage 是异步组件，等待其挂载后再断言
    await vi.waitFor(() => {
      expect(wrapper.find(".history-page").exists()).toBe(true);
    });
    expect(wrapper.text()).toContain("保存的查询");
    expect(wrapper.get('[data-action="show-saved"]').attributes("aria-selected")).toBe("true");
    wrapper.unmount();
  });

  it("opens the same save flow from the toolbar", async () => {
    const wrapper = mount(AppShell, {
      global: { stubs: { teleport: true } },
      attachTo: document.body,
    });
    await flushPromises();
    await wrapper.get('[data-action="save-draft"]').trigger("click");
    await vi.waitFor(() => {
      expect(wrapper.find(".history-page").exists()).toBe(true);
    });
    expect(wrapper.text()).toContain("保存的查询");
    expect(wrapper.get('[data-action="show-saved"]').attributes("aria-selected")).toBe("true");
    wrapper.unmount();
  });

  it("opens the command palette from the toolbar", async () => {
    const wrapper = mount(AppShell, {
      global: { stubs: { teleport: true } },
      attachTo: document.body,
    });
    await flushPromises();
    await wrapper.get('[data-action="palette"]').trigger("click");
    await flushPromises();
    expect(wrapper.find("#palette").exists()).toBe(true);
    wrapper.unmount();
  });
});
