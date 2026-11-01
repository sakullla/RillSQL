import type { AppPage } from "../router";
import { tr } from "../i18n";
import type { ThemePreference } from "./theme";

/** 命令面板分组标识；展示文字走字典 shell.paletteGroup.*。 */
export type PaletteGroup = "command" | "table" | "connection";

export interface PaletteItem {
  id: string;
  group: PaletteGroup;
  label: string;
  icon: "code" | "plus" | "file" | "history" | "database" | "sliders" | "sidebar" | "sun" | "moon" | "monitor" | "keyboard" | "table" | "play";
  chord?: string;
  path?: string;
  run: () => void;
}

/** 分组展示名：命令面板头部与分组标题渲染处使用。 */
export function paletteGroupLabel(group: PaletteGroup): string {
  return tr(`shell.paletteGroup.${group}`);
}

export interface ShellActions {
  openQuery: () => void;
  openStructure: () => void;
  showPage: (page: AppPage) => void;
  toggleSidebar: () => void;
  setTheme: (theme: ThemePreference) => void;
  openHelp: () => void;
  requestSqlite: () => void;
  saveQuery: () => void;
  showSavedQueries: () => void;
  executeQuery?: () => void;
  openCompare?: () => void;
}

export function shellCommands(actions: ShellActions): PaletteItem[] {
  return [
    { id: "new-query", group: "command", label: tr("shell.command.newQuery"), icon: "code", chord: "Mod+T", run: actions.openQuery },
    { id: "execute-query", group: "command", label: tr("shell.command.execute"), icon: "play", chord: "Mod+Enter", run: () => actions.executeQuery?.() },
    { id: "open-structure", group: "command", label: tr("shell.command.newStructure"), icon: "table", run: actions.openStructure },
    { id: "compare", group: "command", label: tr("shell.command.compare"), icon: "table", run: () => actions.openCompare?.() },
    { id: "new-connection", group: "command", label: tr("shell.command.newConnection"), icon: "plus", run: () => actions.showPage("connections") },
    { id: "open-sqlite", group: "command", label: tr("shell.command.openSqlite"), icon: "file", run: actions.requestSqlite },
    { id: "history", group: "command", label: tr("shell.command.history"), icon: "history", run: () => actions.showPage("history") },
    { id: "save-query", group: "command", label: tr("shell.command.saveQuery"), icon: "file", run: actions.saveQuery },
    { id: "saved-queries", group: "command", label: tr("shell.command.savedQueries"), icon: "code", run: actions.showSavedQueries },
    { id: "connections", group: "command", label: tr("shell.command.connections"), icon: "database", run: () => actions.showPage("connections") },
    { id: "settings", group: "command", label: tr("shell.command.settings"), icon: "sliders", chord: "Mod+,", run: () => actions.showPage("settings") },
    { id: "sidebar", group: "command", label: tr("shell.command.toggleSidebar"), icon: "sidebar", chord: "Mod+B", run: actions.toggleSidebar },
    { id: "theme-light", group: "command", label: tr("shell.command.themeLight"), icon: "sun", run: () => actions.setTheme("light") },
    { id: "theme-dark", group: "command", label: tr("shell.command.themeDark"), icon: "moon", run: () => actions.setTheme("dark") },
    { id: "theme-system", group: "command", label: tr("shell.command.themeSystem"), icon: "monitor", run: () => actions.setTheme("system") },
    { id: "help", group: "command", label: tr("shell.command.help"), icon: "keyboard", chord: "?", run: actions.openHelp },
  ];
}

/** 相关度：前缀 > 包含 > 子序列（odrs 能找到 orders）；0 表示不匹配。 */
function matchScore(text: string, query: string): number {
  const haystack = text.toLowerCase();
  if (haystack.startsWith(query)) return 3;
  if (haystack.includes(query)) return 2;
  let at = 0;
  for (const char of query) {
    at = haystack.indexOf(char, at);
    if (at < 0) return 0;
    at += 1;
  }
  return 1;
}

function ranked(items: readonly PaletteItem[], query: string): PaletteItem[] {
  const scored = items
    .map((item) => ({ item, score: Math.max(matchScore(item.label, query), (item.path ?? "").toLowerCase().includes(query) ? 1 : 0) }))
    .filter((entry) => entry.score > 0);
  // 分组标题按相邻项渲染：只在组内排序，组序沿用输入顺序。
  const groups = [...new Set(scored.map((entry) => entry.item.group))];
  return groups.flatMap((group) => scored.filter((entry) => entry.item.group === group).sort((a, b) => b.score - a.score).map((entry) => entry.item));
}

export function filterPalette(items: readonly PaletteItem[], query: string, mode: "all" | "tables"): PaletteItem[] {
  const trimmed = query.trim().toLowerCase();
  if (mode === "tables") {
    const tables = items.filter((item) => item.group === "table");
    return trimmed ? ranked(tables, trimmed) : tables;
  }
  if (trimmed.startsWith(">")) {
    const q = trimmed.slice(1).trim();
    const commands = items.filter((item) => item.group === "command");
    return q ? ranked(commands, q) : commands;
  }
  // 空输入先给命令：库里表再多，常用动作也不会被挤到列表底部。
  if (!trimmed) return [...items.filter((item) => item.group === "command"), ...items.filter((item) => item.group !== "command")];
  return ranked(items, trimmed);
}
