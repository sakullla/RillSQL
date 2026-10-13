import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import AppShell from "./AppShell.vue";

describe("AppShell", () => {
  it("opens the welcome page without asking for a cloud login", () => {
    const wrapper = mount(AppShell);
    const text = wrapper.text();
    expect(text).toContain("澄库");
    expect(text).toContain("RillSQL");
    expect(wrapper.find(".welcome").exists()).toBe(true);
    expect(wrapper.find(".titlebar").exists()).toBe(true);
    expect(wrapper.find(".rail").exists()).toBe(true);
    expect(wrapper.find(".statusbar").exists()).toBe(true);
    expect(text).not.toMatch(/云登录|请登录/);
  });
});
