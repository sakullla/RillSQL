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

export function filterPalette(items: readonly PaletteItem[], query: string, mode: "all" | "tables"): PaletteItem[] {
  const trimmed = query.trim().toLowerCase();
  if (mode === "tables") {
    const q = trimmed;
    return items.filter((item) => item.group === "table" && (!q || item.label.toLowerCase().includes(q)));
  }
  if (trimmed.startsWith(">")) {
    const q = trimmed.slice(1).trim();
    return items.filter((item) => item.group === "command" && item.label.toLowerCase().includes(q));
  }
  return items.filter((item) => {
    if (!trimmed) return true;
    return item.label.toLowerCase().includes(trimmed) || (item.path ?? "").toLowerCase().includes(trimmed);
  });
}
