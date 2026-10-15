import type { AppPage } from "../router";
import type { ThemePreference } from "./theme";

export type PaletteGroup = "命令" | "表与视图" | "连接";

export interface PaletteItem {
  id: string;
  group: PaletteGroup;
  label: string;
  icon: "code" | "plus" | "file" | "history" | "database" | "sliders" | "sidebar" | "sun" | "moon" | "monitor" | "keyboard" | "table";
  chord?: string;
  path?: string;
  run: () => void;
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
}

export function shellCommands(actions: ShellActions): PaletteItem[] {
  return [
    { id: "new-query", group: "命令", label: "新建查询", icon: "code", chord: "Mod+T", run: actions.openQuery },
    { id: "open-structure", group: "命令", label: "新建表结构", icon: "table", run: actions.openStructure },
    { id: "new-connection", group: "命令", label: "新建连接", icon: "plus", run: () => actions.showPage("connections") },
    { id: "open-sqlite", group: "命令", label: "打开 SQLite 文件", icon: "file", run: actions.requestSqlite },
    { id: "history", group: "命令", label: "查询历史", icon: "history", run: () => actions.showPage("history") },
    { id: "save-query", group: "命令", label: "保存当前查询", icon: "file", run: actions.saveQuery },
    { id: "saved-queries", group: "命令", label: "保存的查询", icon: "code", run: actions.showSavedQueries },
    { id: "connections", group: "命令", label: "连接管理", icon: "database", run: () => actions.showPage("connections") },
    { id: "settings", group: "命令", label: "设置", icon: "sliders", chord: "Mod+,", run: () => actions.showPage("settings") },
    { id: "sidebar", group: "命令", label: "显示 / 隐藏资源管理器", icon: "sidebar", chord: "Mod+B", run: actions.toggleSidebar },
    { id: "theme-light", group: "命令", label: "切换到浅色主题", icon: "sun", run: () => actions.setTheme("light") },
    { id: "theme-dark", group: "命令", label: "切换到深色主题", icon: "moon", run: () => actions.setTheme("dark") },
    { id: "theme-system", group: "命令", label: "主题跟随系统", icon: "monitor", run: () => actions.setTheme("system") },
    { id: "help", group: "命令", label: "键盘快捷键", icon: "keyboard", chord: "?", run: actions.openHelp },
  ];
}

export function filterPalette(items: readonly PaletteItem[], query: string, mode: "all" | "tables"): PaletteItem[] {
  const trimmed = query.trim().toLowerCase();
  if (mode === "tables") {
    const q = trimmed;
    return items.filter((item) => item.group === "表与视图" && (!q || item.label.toLowerCase().includes(q)));
  }
  if (trimmed.startsWith(">")) {
    const q = trimmed.slice(1).trim();
    return items.filter((item) => item.group === "命令" && item.label.toLowerCase().includes(q));
  }
  return items.filter((item) => {
    if (!trimmed) return true;
    return item.label.toLowerCase().includes(trimmed) || (item.path ?? "").toLowerCase().includes(trimmed);
  });
}
