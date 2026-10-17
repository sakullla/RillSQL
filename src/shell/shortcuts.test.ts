import { describe, expect, it } from "vitest";
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
