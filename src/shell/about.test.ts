import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import { ABOUT_ABSENT } from "./product";
import { DEFAULT_SETTINGS } from "./settings";
import SettingsView from "./SettingsView.vue";
import WelcomeView from "./WelcomeView.vue";

describe("product identity", () => {
  it("shows both names on the welcome page without a cloud login", () => {
    const wrapper = mount(WelcomeView);
    const text = wrapper.text();
    expect(text).toContain("澄库");
    expect(text).toContain("RillSQL");
    expect(text).toContain("无需账号");
    expect(text).not.toMatch(/登录|云账号|注册/);
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
  });
});
