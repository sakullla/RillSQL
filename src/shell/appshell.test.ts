import { mount } from "@vue/test-utils";
import { afterEach, describe, expect, it } from "vitest";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import type { CatalogConnection } from "../catalog/types";
import AppShell from "./AppShell.vue";

afterEach(() => {
  location.hash = "";
  resetCatalog();
});

function savedConnection(): CatalogConnection {
  return {
    id: "pg",
    name: "书店",
    engine: "postgres",
    env: "dev",
    envLabel: "开发",
    envTone: "dev",
    status: "disconnected",
    defaultDatabase: "bookstore",
    foreignKeys: null,
    error: null,
    databases: [],
  };
}

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
    const welcome = wrapper.get(".welcome");
    const paths = welcome.findAll(".desk-action");
    expect(paths).toHaveLength(2);
    expect(paths[0]!.text()).toContain("新建连接");
    expect(paths[1]!.text()).toContain("打开 SQLite 文件");
    const solid = welcome.findAll(".btn.primary");
    expect(solid).toHaveLength(1);
    expect(solid[0]!.text()).toContain("新建连接");
    expect(paths[1]!.classes()).not.toContain("primary");
    expect(text).toContain("配置留在本机");
    expect(text).toContain("密码进入系统凭据库");
    expect(text).not.toContain("从数据，到答案。");
    expect(text).not.toContain("SQL FIRST");
    expect(text).not.toContain("WORKSPACE");
    expect(wrapper.find(".desk-db").exists()).toBe(false);
    expect(text).not.toMatch(/云登录|请登录/);
    wrapper.unmount();
  });

  it("keeps new query as the only solid welcome action when a connection exists", () => {
    replaceCatalog([savedConnection()]);
    const wrapper = mount(AppShell);
    const welcome = wrapper.get(".welcome");
    const solid = welcome.findAll(".btn.primary");
    expect(solid).toHaveLength(1);
    expect(solid[0]!.text()).toContain("新建查询");
    expect(welcome.findAll(".desk-action")).toHaveLength(0);
    expect(welcome.text()).not.toContain("新建连接");
    expect(welcome.text()).not.toContain("打开 SQLite");
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
    await wrapper.findAll(".desk-action")[0]!.trigger("click");
    expect(location.hash).toBe("#/connections");
    expect(wrapper.find(".workspace-view").attributes("style")).toContain("display: none");
    wrapper.unmount();
  });
});
