export type ShortcutGroup = "全局" | "SQL 编辑器" | "表格";

export interface ShortcutDef {
  group: ShortcutGroup;
  chord: string;
  label: string;
}

export const SHORTCUTS: readonly ShortcutDef[] = [
  { group: "全局", chord: "Mod+K", label: "命令面板（表、连接、命令）" },
  { group: "全局", chord: "Mod+P", label: "快速打开表" },
  { group: "全局", chord: "Mod+T", label: "新建查询" },
  { group: "全局", chord: "Mod+W", label: "关闭当前标签" },
  { group: "全局", chord: "Mod+B", label: "显示 / 隐藏资源管理器" },
  { group: "全局", chord: "Alt+1…9", label: "切换到第 N 个标签" },
  { group: "全局", chord: "Mod+,", label: "设置" },
  { group: "全局", chord: "?", label: "快捷键帮助" },
  { group: "SQL 编辑器", chord: "Mod+Enter", label: "执行选中内容；无选中时执行全部" },
  { group: "SQL 编辑器", chord: "Mod+Shift+Enter", label: "只执行光标所在语句" },
  { group: "SQL 编辑器", chord: "Mod+.", label: "请求取消执行" },
  { group: "SQL 编辑器", chord: "Ctrl+Space", label: "补全" },
  { group: "SQL 编辑器", chord: "Mod+/", label: "注释 / 取消注释" },
  { group: "SQL 编辑器", chord: "Mod+Shift+F", label: "格式化选中的 SQL；无选中时格式化全部" },
  { group: "SQL 编辑器", chord: "Mod+Shift+E", label: "查看当前语句的执行计划" },
  { group: "SQL 编辑器", chord: "Esc 然后 Tab", label: "离开编辑器（Tab 在编辑器中用于缩进）" },
  { group: "表格", chord: "↑ ↓ ← →", label: "移动单元格" },
  { group: "表格", chord: "Enter / F2", label: "编辑单元格" },
  { group: "表格", chord: "Esc", label: "取消编辑" },
  { group: "表格", chord: "Delete", label: "标记删除行（暂存）" },
  { group: "表格", chord: "Mod+Z", label: "撤销最近一项暂存变更" },
  { group: "表格", chord: "Mod+S", label: "预览并提交变更" },
  { group: "表格", chord: "Mod+C", label: "复制值" },
  { group: "表格", chord: "Mod+Shift+C", label: "复制为 SQL 值（区分 NULL）" },
];

export function isMacPlatform(platform = "", userAgent = ""): boolean {
  return /Mac|iPhone|iPad/i.test(platform) || /Mac OS/i.test(userAgent);
}

export function detectMac(nav: Navigator | undefined = globalThis.navigator): boolean {
  if (!nav) return false;
  return isMacPlatform(nav.platform, nav.userAgent);
}

export function formatChord(chord: string, mac: boolean): string {
  return chord.replaceAll("Mod", mac ? "⌘" : "Ctrl");
}

export function chordParts(chord: string, mac: boolean): string[] {
  if (/[…\s]/.test(chord) && !chord.includes("+")) return [chord];
  return formatChord(chord, mac).split("+");
}

export function shortcutPlatformNote(mac: boolean): string {
  return mac
    ? "macOS 按键 · 关闭标签为 ⌘W，新建查询为 ⌘T"
    : "Windows / Linux 按键 · 关闭标签为 Ctrl+W，新建查询为 Ctrl+T";
}

export function globalChord(label: string): string | undefined {
  return SHORTCUTS.find((item) => item.group === "全局" && item.label === label)?.chord;
}
