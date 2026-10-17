import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import Icon from "./Icon.vue";
import { ICON_PATHS, type IconName } from "./icons";

const REQUIRED = [
  "play", "stop", "refresh", "format", "plan", "filter", "sort", "copy", "trash", "pencil",
  "eye", "link", "upload", "folder", "save", "chevron-down", "chevron-right", "chevron-left",
  "chevron-up", "arrow-right", "arrow-up", "arrow-down", "external", "columns", "grid",
  "info", "alert", "pin", "clock", "plug", "power", "clipboard", "list", "maximize",
  "minimize", "gear", "key", "shield", "user", "text", "more",
] as const;

describe("icon library", () => {
  it("ships at least 50 icons", () => {
    expect(Object.keys(ICON_PATHS).length).toBeGreaterThanOrEqual(50);
  });

  it("covers every icon the desktop chrome and context menus need", () => {
    for (const name of REQUIRED) {
      expect(Object.keys(ICON_PATHS), `missing ${name}`).toContain(name);
    }
  });

  it("only stores single-stroke path data", () => {
    for (const name of Object.keys(ICON_PATHS) as IconName[]) {
      // 既有 code / check 用小写 m 起笔（相对 moveto），渲染等价，不强制改名。
      expect(/^[Mm]/.test(ICON_PATHS[name].trim()), `${name} must start with a moveto`).toBe(true);
    }
  });

  it("renders the path through Icon", () => {
    const wrapper = mount(Icon, { props: { name: "play" } });
    expect(wrapper.get("path").attributes("d")).toBe(ICON_PATHS.play);
  });
});
