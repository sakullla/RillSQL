import { describe, expect, it } from "vitest";
import { mount } from "@vue/test-utils";
import { shellMenus } from "./menus";
import ShortcutHelp from "./ShortcutHelp.vue";
import { chordByLabel, chordParts, formatChord, globalChord, isMacPlatform, SHORTCUTS, shortcutPlatformNote } from "./shortcuts";

describe("desktop shortcuts", () => {
  it("uses platform tab shortcuts instead of the prototype browser keys", () => {
    expect(globalChord("新建查询")).toBe("Mod+T");
    expect(globalChord("关闭当前标签")).toBe("Mod+W");
    const chords = SHORTCUTS.map((item) => item.chord);
    expect(chords).not.toContain("Alt+W");
    expect(chords).not.toContain("Mod+Alt+N");
    expect(formatChord("Mod+W", false)).toBe("Ctrl+W");
    expect(formatChord("Mod+T", true)).toBe("⌘+T");
    expect(chordParts("Mod+T", true)).toEqual(["⌘", "T"]);
    expect(shortcutPlatformNote(false)).toContain("Ctrl+W");
    expect(shortcutPlatformNote(true)).toContain("⌘T");
  });

  it("detects macOS without treating Linux as Mac", () => {
    expect(isMacPlatform("MacIntel", "")).toBe(true);
    expect(isMacPlatform("Win32", "Windows NT 10")).toBe(false);
    expect(chordParts("Alt+1…9", false)).toEqual(["Alt", "1…9"]);
  });

  it("chordByLabel finds chords in every group", () => {
    expect(chordByLabel("新建查询")).toBe("Mod+T");
    expect(chordByLabel("执行选中内容；无选中时执行光标所在语句")).toBe("Mod+Enter");
    expect(chordByLabel("编辑单元格")).toBe("Enter / F2");
    expect(chordByLabel("不存在的动作")).toBeUndefined();
  });
});

describe("shortcut ↔ menu alignment", () => {
  it("declares menus for every shortcut that appears in a menu", () => {
    // 菜单里带 shortcut 的项，其 chord 必须能在 SHORTCUTS 里找到，
    // 且该条 SHORTCUTS 必须标注了对应的 menu 名。
    // 同一 chord 可能出现在多个菜单（? / Mod+,），用 chord → 菜单名集合 断言。
    const menuLabels = new Map<string, Set<string>>();
    for (const menu of shellMenus({ hasQueryTab: true }, "system")) {
      for (const item of menu.items) {
        if (item.kind !== "item" || !item.shortcut) continue;
        const set = menuLabels.get(item.shortcut) ?? new Set<string>();
        set.add(menu.label);
        menuLabels.set(item.shortcut, set);
      }
    }
    expect(menuLabels.size).toBeGreaterThan(0);
    for (const [chord, menuSet] of menuLabels) {
      const entry = SHORTCUTS.find((item) => item.chord === chord);
      expect(entry, `chord ${chord} 不在快捷键表里`).toBeTruthy();
      if (entry) {
        expect(entry.menus, `${chord} 缺 menus 标注`).toBeTruthy();
        // 双向集合相等：少标（Mod+, 只写 文件）或多标（多写 查询）都算错
        expect(new Set(entry.menus), `${chord} 的 menus 标注与菜单实际归属不一致`).toEqual(menuSet);
      }
    }
  });
});

describe("ShortcutHelp grouping", () => {
  it("renders one section per menu that has shortcuts, in menu-bar order", () => {
    const wrapper = mount(ShortcutHelp, { props: { open: true } });
    const titles = wrapper.findAll("h4").map((n) => n.text());
    expect(titles).toEqual(expect.arrayContaining(["文件", "查询", "窗口"]));
    // 顺序断言：文件 必须在 查询 之前
    expect(titles.indexOf("文件")).toBeLessThan(titles.indexOf("查询"));
    expect(titles.indexOf("查询")).toBeLessThan(titles.indexOf("窗口"));
    wrapper.unmount();
  });

  it("files 补全 under the SQL 编辑器 section, not elsewhere", () => {
    const wrapper = mount(ShortcutHelp, { props: { open: true } });
    const sections = wrapper.findAll("section");
    // DOMWrapper 不保证同一节点引用相等，用 element 做身份判断
    const section = sections.find((s) => s.find("h4")?.text() === "SQL 编辑器");
    expect(section, "缺少 SQL 编辑器 节").toBeTruthy();
    expect(section!.text()).toContain("补全"); // Ctrl+Space，不在任何菜单里
    for (const other of sections) {
      if (other.element === section!.element) continue;
      expect(other.text(), "补全 不应出现在别的节里").not.toContain("补全");
    }
    wrapper.unmount();
  });

  it("matches the menu bar order", () => {
    const labels = shellMenus({ hasQueryTab: true }, "system").map((m) => m.label);
    const wrapper = mount(ShortcutHelp, { props: { open: true } });
    const titles = wrapper.findAll("h4").map((n) => n.text());
    // 只比较同时出现在两者里的标题（编辑器/表格节不在菜单里）
    const menuTitles = titles.filter((t) => labels.includes(t));
    expect(menuTitles).toEqual(labels.filter((l) => menuTitles.includes(l)));
    wrapper.unmount();
  });
});
