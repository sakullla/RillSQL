import { tr } from "../i18n";

/** 快捷键分组标识；展示文字走字典 shell.shortcuts.group*。 */
export type ShortcutGroup = "global" | "editor" | "table";

export interface ShortcutDef {
  id: string;
  group: ShortcutGroup;
  chord: string;
  /** 字典 key（shell.shortcuts.*），渲染处用 tr 翻译。 */
  label: string;
}

export const SHORTCUTS: readonly ShortcutDef[] = [
  { id: "command-palette", group: "global", chord: "Mod+K", label: "shell.shortcuts.commandPalette" },
  { id: "quick-open", group: "global", chord: "Mod+P", label: "shell.shortcuts.quickOpen" },
  { id: "new-query", group: "global", chord: "Mod+T", label: "shell.shortcuts.newQuery" },
  { id: "close-tab", group: "global", chord: "Mod+W", label: "shell.shortcuts.closeTab" },
  { id: "toggle-sidebar", group: "global", chord: "Mod+B", label: "shell.shortcuts.toggleSidebar" },
  { id: "next-tab", group: "global", chord: "Ctrl+Tab", label: "shell.shortcuts.nextTab" },
  { id: "nth-tab", group: "global", chord: "Alt+1…9", label: "shell.shortcuts.nthTab" },
  { id: "open-settings", group: "global", chord: "Mod+,", label: "shell.shortcuts.openSettings" },
  { id: "shortcut-help", group: "global", chord: "?", label: "shell.shortcuts.shortcutHelp" },
  { id: "run-selection", group: "editor", chord: "Mod+Enter", label: "shell.shortcuts.runSelection" },
  { id: "run-current", group: "editor", chord: "Mod+Shift+Enter", label: "shell.shortcuts.runCurrent" },
  { id: "request-cancel", group: "editor", chord: "Mod+.", label: "shell.shortcuts.requestCancel" },
  { id: "autocomplete", group: "editor", chord: "Ctrl+Space", label: "shell.shortcuts.autocomplete" },
  { id: "toggle-comment", group: "editor", chord: "Mod+/", label: "shell.shortcuts.toggleComment" },
  { id: "format", group: "editor", chord: "Mod+Shift+F", label: "shell.shortcuts.format" },
  { id: "explain", group: "editor", chord: "Mod+Shift+E", label: "shell.shortcuts.explain" },
  { id: "leave-editor", group: "editor", chord: "Tab", label: "shell.shortcuts.leaveEditor" },
  { id: "move-cells", group: "table", chord: "↑ ↓ ← →", label: "shell.shortcuts.moveCells" },
  { id: "select-all-cells", group: "table", chord: "Mod+A", label: "shell.shortcuts.selectAllCells" },
  { id: "edit-cell", group: "table", chord: "Enter / F2", label: "shell.shortcuts.editCell" },
  { id: "cancel-edit", group: "table", chord: "Esc", label: "shell.shortcuts.cancelEdit" },
  { id: "mark-delete", group: "table", chord: "Delete", label: "shell.shortcuts.markDelete" },
  { id: "commit", group: "table", chord: "Mod+S", label: "shell.shortcuts.commit" },
  { id: "copy-region", group: "table", chord: "Mod+C", label: "shell.shortcuts.copyRegion" },
  { id: "copy-sql-value", group: "table", chord: "Mod+Shift+C", label: "shell.shortcuts.copySqlValue" },
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
  return tr(mac ? "shell.shortcuts.noteMac" : "shell.shortcuts.noteWin");
}

/** 按快捷键条目 id 查全局快捷键（原按中文 label 查找，i18n 迁移后改为稳定 id）。 */
export function globalChord(id: string): string | undefined {
  return SHORTCUTS.find((item) => item.group === "global" && item.id === id)?.chord;
}
