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
        expect(entry.menus?.some((m) => menuSet.has(m)), `${chord} 的 menus 与菜单归属不一致`).toBe(true);
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

  it("lists editor shortcuts that are not in a menu under SQL 编辑器", () => {
    const wrapper = mount(ShortcutHelp, { props: { open: true } });
    expect(wrapper.text()).toContain("补全"); // Ctrl+Space，不在任何菜单里
    expect(wrapper.text()).toContain("SQL 编辑器");
    wrapper.unmount();
  });
});
