import { chordByLabel } from "./shortcuts";
import type { MenuDefinition } from "./menu-model";
import type { ThemePreference } from "./theme";

export interface MenuContext {
  hasQueryTab: boolean;
}

export function shellMenus(ctx: MenuContext, theme: ThemePreference): MenuDefinition[] {
  const q = ctx.hasQueryTab;
  return [
    {
      id: "file",
      label: "文件",
      items: [
        { kind: "item", id: "new-query", label: "新建查询", shortcut: chordByLabel("新建查询") },
        { kind: "item", id: "open-sqlite", label: "打开 SQLite 文件…" },
        { kind: "sep" },
        { kind: "item", id: "save-draft", label: "保存当前查询到「保存的查询」" },
        { kind: "sep" },
        { kind: "item", id: "settings", label: "设置", shortcut: chordByLabel("设置") },
      ],
    },
    {
      id: "edit",
      label: "编辑",
      items: [
        { kind: "item", id: "palette", label: "命令面板", shortcut: chordByLabel("命令面板（表、连接、命令）") },
        { kind: "item", id: "quick-open", label: "快速打开表", shortcut: chordByLabel("快速打开表") },
      ],
    },
    {
      id: "view",
      label: "视图",
      items: [
        { kind: "item", id: "toggle-sidebar", label: "资源管理器", shortcut: chordByLabel("显示 / 隐藏资源管理器") },
        { kind: "sep" },
        { kind: "item", id: "theme-light", label: "浅色", checked: theme === "light" },
        { kind: "item", id: "theme-dark", label: "深色", checked: theme === "dark" },
        { kind: "item", id: "theme-system", label: "跟随系统", checked: theme === "system" },
        { kind: "sep" },
        { kind: "item", id: "shortcuts", label: "快捷键与帮助", shortcut: chordByLabel("快捷键帮助") },
      ],
    },
    {
      id: "query",
      label: "查询",
      items: [
        { kind: "item", id: "run", label: "执行选中内容；无选中时执行光标所在语句", shortcut: chordByLabel("执行选中内容；无选中时执行光标所在语句"), disabled: !q },
        { kind: "item", id: "run-statement", label: "只执行光标所在语句", shortcut: chordByLabel("只执行光标所在语句"), disabled: !q },
        { kind: "item", id: "cancel", label: "请求取消执行", shortcut: chordByLabel("请求取消执行"), disabled: !q },
        { kind: "sep" },
        { kind: "item", id: "format", label: "格式化 SQL", shortcut: chordByLabel("格式化选中的 SQL；无选中时格式化全部"), disabled: !q },
        { kind: "item", id: "plan", label: "执行计划", shortcut: chordByLabel("查看当前语句的执行计划"), disabled: !q },
      ],
    },
    {
      id: "tools",
      label: "工具",
      items: [
        { kind: "item", id: "tasks", label: "任务中心（导入 / 导出）" },
        { kind: "sep" },
        { kind: "item", id: "settings", label: "设置", shortcut: chordByLabel("设置") },
      ],
    },
    {
      id: "window",
      label: "窗口",
      items: [
        { kind: "item", id: "close-tab", label: "关闭当前标签", shortcut: chordByLabel("关闭当前标签") },
        { kind: "item", id: "close-others", label: "关闭其他标签" },
        { kind: "item", id: "close-right", label: "关闭右侧标签" },
        { kind: "sep" },
        { kind: "item", id: "goto-tab", label: "切换到第 N 个标签", shortcut: chordByLabel("切换到第 N 个标签"), disabled: true },
      ],
    },
    {
      id: "help",
      label: "帮助",
      items: [
        { kind: "item", id: "shortcuts", label: "快捷键与帮助", shortcut: chordByLabel("快捷键帮助") },
        { kind: "item", id: "about", label: "关于 RillSQL" },
      ],
    },
  ];
}
