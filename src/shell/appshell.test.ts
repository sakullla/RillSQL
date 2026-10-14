import { mount } from "@vue/test-utils";
import { afterEach, describe, expect, it } from "vitest";
import AppShell from "./AppShell.vue";

afterEach(() => {
  location.hash = "";
});

describe("AppShell", () => {
  it("opens the welcome page with two primary paths and no login gate", () => {
    const wrapper = mount(AppShell);
    const text = wrapper.text();
    expect(text).toContain("澄库");
    expect(text).toContain("RillSQL");
    expect(wrapper.find(".welcome").exists()).toBe(true);
    expect(wrapper.find(".titlebar").exists()).toBe(true);
    expect(wrapper.find(".rail").exists()).toBe(true);
    expect(wrapper.find(".statusbar").exists()).toBe(true);

    // R2：首次启动直接呈现两条主路径，无需文档或快捷键
    const paths = wrapper.findAll(".welcome-path");
    expect(paths).toHaveLength(2);
    expect(paths[0]!.text()).toContain("新建连接");
    expect(paths[1]!.text()).toContain("打开 SQLite 文件");
    expect(text).not.toMatch(/云登录|请登录/);
    wrapper.unmount();
  });

  it("keeps secondary entries out of the status bar by default", () => {
    const wrapper = mount(AppShell);
    const status = wrapper.get(".statusbar").text();
    expect(status).not.toContain("快捷键");
    expect(status).not.toContain("仅存本机");
    wrapper.unmount();
  });

  it("navigates to the connections page from the new-connection path", async () => {
    const wrapper = mount(AppShell);
    await wrapper.findAll(".welcome-path")[0]!.trigger("click");
    expect(location.hash).toBe("#/connections");
    expect(wrapper.find(".welcome").exists()).toBe(false);
    wrapper.unmount();
  });
});
