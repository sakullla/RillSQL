import { mount } from "@vue/test-utils";
import { afterEach, describe, expect, it } from "vitest";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import type { CatalogConnection } from "../catalog/types";
import { ABOUT_ABSENT } from "./product";
import { DEFAULT_SETTINGS } from "./settings";
import SettingsView from "./SettingsView.vue";
import WelcomeView from "./WelcomeView.vue";

afterEach(() => {
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

describe("product identity", () => {
  it("shows both names and the two primary paths on the welcome page", () => {
    const wrapper = mount(WelcomeView);
    const text = wrapper.text();
    expect(text).toContain("澄库");
    expect(text).toContain("RillSQL");
    expect(text).toContain("无需账号");
    expect(text).toContain("配置留在本机");
    expect(text).toContain("密码进入系统凭据库");
    expect(text).toContain("新建连接");
    expect(text).toContain("打开 SQLite 文件");
    expect(text).not.toContain("从数据，到答案。");
    expect(text).not.toContain("SQL FIRST");
    expect(text).not.toContain("WORKSPACE");
    expect(text).not.toMatch(/登录|云账号|注册/);
    expect(wrapper.find(".desk-db").exists()).toBe(false);
  });

  it("does not list saved connections when the workspace has no tab", () => {
    replaceCatalog([savedConnection()]);
    const wrapper = mount(WelcomeView);
    expect(wrapper.find(".desk-db").exists()).toBe(false);
    expect(wrapper.findAll(".desk-entry")).toHaveLength(0);
    expect(wrapper.text()).toContain("新建查询");
    expect(wrapper.text()).not.toContain("书店");
    expect(wrapper.text()).not.toContain("从数据，到答案。");
    expect(wrapper.text()).not.toContain("SQL FIRST");
    expect(wrapper.text()).not.toContain("WORKSPACE");
  });

  it("emits the two primary paths as distinct actions", async () => {
    const wrapper = mount(WelcomeView);
    const paths = wrapper.findAll(".desk-entry");
    await paths[0]!.trigger("click");
    await paths[1]!.trigger("click");
    expect(wrapper.emitted("connections")).toHaveLength(1);
    expect(wrapper.emitted("sqlite")).toHaveLength(1);
  });

  it("states the first release exclusions on the about section", () => {
    const wrapper = mount(SettingsView, { props: { settings: { ...DEFAULT_SETTINGS }, dataDir: null } });
    const text = wrapper.text();
    expect(text).toContain("澄库 · RillSQL");
    expect(text).toContain(ABOUT_ABSENT);
    for (const item of ["AI", "云同步", "NoSQL", "关系图", "备份恢复", "运维监控"]) {
      expect(text).toContain(item);
    }
    expect(text).toContain("颜色从不作为唯一的信息来源");
    expect(text).not.toContain("PREFERENCES");
  });

  it("keeps the shortcut help reachable from settings instead of the status bar", async () => {
    const wrapper = mount(SettingsView, { props: { settings: { ...DEFAULT_SETTINGS }, dataDir: null } });
    const help = wrapper.findAll(".btn").find((item) => item.text().includes("查看快捷键"));
    expect(help).toBeTruthy();
    await help!.trigger("click");
    expect(wrapper.emitted("help")).toHaveLength(1);
  });
});
