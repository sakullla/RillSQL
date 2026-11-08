//! Shell state shared by the GPUI window and headless tests.
//! Command ids and dictionary keys follow the existing Vue shell.

#![cfg_attr(not(test), allow(dead_code))]

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locale {
    ZhCn,
    En,
}

impl Locale {
    pub fn code(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::En => "en",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemePreference {
    Light,
    Dark,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedTheme {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Density {
    Compact,
    Standard,
    Relaxed,
}

impl Density {
    pub fn row_height(self) -> i32 {
        match self {
            Self::Compact => 24,
            Self::Standard => 32,
            Self::Relaxed => 40,
        }
    }

    pub fn control_padding(self) -> i32 {
        match self {
            Self::Compact => 4,
            Self::Standard => 8,
            Self::Relaxed => 12,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusTarget {
    Editor,
    Grid,
    Palette,
    Sidebar,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppPage {
    Workspace,
    Connections,
    History,
    Settings,
    Structure,
    Compare,
    Diagram,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilePrompt {
    Open,
    Save { suggested_name: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    ApplyTheme,
    RefreshMenus,
    PromptOpen,
    PromptSave,
    FocusEditor,
    ShowCompletions,
    ToggleComment,
    FormatSql,
    ExplainSql,
    ExecuteSelection,
    ExecuteCurrent,
    CopyRegion,
    CopySqlValue,
    EditCell,
    CancelEdit,
    CommitEdit,
    MarkDelete,
    SelectAllCells,
}

pub struct CommandSpec {
    pub id: &'static str,
    pub label_key: &'static str,
    pub icon: &'static str,
    pub chord: Option<&'static str>,
}

pub struct Shortcut {
    pub id: &'static str,
    pub group: &'static str,
    pub chord: &'static str,
    pub label_key: &'static str,
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec { id: "new-query", label_key: "shell.command.newQuery", icon: "code", chord: Some("Mod+T") },
    CommandSpec { id: "execute-query", label_key: "shell.command.execute", icon: "play", chord: Some("Mod+Enter") },
    CommandSpec { id: "open-structure", label_key: "shell.command.newStructure", icon: "table", chord: None },
    CommandSpec { id: "compare", label_key: "shell.command.compare", icon: "table", chord: None },
    CommandSpec { id: "new-connection", label_key: "shell.command.newConnection", icon: "plus", chord: None },
    CommandSpec { id: "open-sqlite", label_key: "shell.command.openSqlite", icon: "file", chord: None },
    CommandSpec { id: "history", label_key: "shell.command.history", icon: "history", chord: None },
    CommandSpec { id: "save-query", label_key: "shell.command.saveQuery", icon: "file", chord: None },
    CommandSpec { id: "saved-queries", label_key: "shell.command.savedQueries", icon: "code", chord: None },
    CommandSpec { id: "connections", label_key: "shell.command.connections", icon: "database", chord: None },
    CommandSpec { id: "settings", label_key: "shell.command.settings", icon: "sliders", chord: Some("Mod+,") },
    CommandSpec { id: "sidebar", label_key: "shell.command.toggleSidebar", icon: "sidebar", chord: Some("Mod+B") },
    CommandSpec { id: "theme-light", label_key: "shell.command.themeLight", icon: "sun", chord: None },
    CommandSpec { id: "theme-dark", label_key: "shell.command.themeDark", icon: "moon", chord: None },
    CommandSpec { id: "theme-system", label_key: "shell.command.themeSystem", icon: "monitor", chord: None },
    CommandSpec { id: "help", label_key: "shell.command.help", icon: "keyboard", chord: Some("?") },
];

pub const SHORTCUTS: &[Shortcut] = &[
    Shortcut { id: "command-palette", group: "global", chord: "Mod+K", label_key: "shell.shortcuts.commandPalette" },
    Shortcut { id: "quick-open", group: "global", chord: "Mod+P", label_key: "shell.shortcuts.quickOpen" },
    Shortcut { id: "new-query", group: "global", chord: "Mod+T", label_key: "shell.shortcuts.newQuery" },
    Shortcut { id: "close-tab", group: "global", chord: "Mod+W", label_key: "shell.shortcuts.closeTab" },
    Shortcut { id: "toggle-sidebar", group: "global", chord: "Mod+B", label_key: "shell.shortcuts.toggleSidebar" },
    Shortcut { id: "next-tab", group: "global", chord: "Ctrl+Tab", label_key: "shell.shortcuts.nextTab" },
    Shortcut { id: "nth-tab", group: "global", chord: "Alt+1…9", label_key: "shell.shortcuts.nthTab" },
    Shortcut { id: "open-settings", group: "global", chord: "Mod+,", label_key: "shell.shortcuts.openSettings" },
    Shortcut { id: "shortcut-help", group: "global", chord: "?", label_key: "shell.shortcuts.shortcutHelp" },
    Shortcut { id: "run-selection", group: "editor", chord: "Mod+Enter", label_key: "shell.shortcuts.runSelection" },
    Shortcut { id: "run-current", group: "editor", chord: "Mod+Shift+Enter", label_key: "shell.shortcuts.runCurrent" },
    Shortcut { id: "request-cancel", group: "editor", chord: "Mod+.", label_key: "shell.shortcuts.requestCancel" },
    Shortcut { id: "undo", group: "editor", chord: "Mod+Z", label_key: "shell.shortcuts.undo" },
    Shortcut { id: "redo", group: "editor", chord: if cfg!(target_os = "macos") { "Mod+Shift+Z" } else { "Ctrl+Y / Ctrl+Shift+Z" }, label_key: "shell.shortcuts.redo" },
    Shortcut { id: "find", group: "editor", chord: "Mod+F", label_key: "shell.shortcuts.find" },
    Shortcut { id: "autocomplete", group: "editor", chord: "Ctrl+Space", label_key: "shell.shortcuts.autocomplete" },
    Shortcut { id: "toggle-comment", group: "editor", chord: "Mod+/", label_key: "shell.shortcuts.toggleComment" },
    Shortcut { id: "toggle-results", group: "editor", chord: "Mod+J", label_key: "shell.shortcuts.toggleResults" },
    Shortcut { id: "format", group: "editor", chord: "Mod+Shift+F", label_key: "shell.shortcuts.format" },
    Shortcut { id: "explain", group: "editor", chord: "Mod+Shift+E", label_key: "shell.shortcuts.explain" },
    Shortcut { id: "leave-editor", group: "editor", chord: "Tab", label_key: "shell.shortcuts.leaveEditor" },
    Shortcut { id: "move-cells", group: "table", chord: "↑ ↓ ← →", label_key: "shell.shortcuts.moveCells" },
    Shortcut { id: "select-all-cells", group: "table", chord: "Mod+A", label_key: "shell.shortcuts.selectAllCells" },
    Shortcut { id: "edit-cell", group: "table", chord: "Enter / F2", label_key: "shell.shortcuts.editCell" },
    Shortcut { id: "cancel-edit", group: "table", chord: "Esc", label_key: "shell.shortcuts.cancelEdit" },
    Shortcut { id: "mark-delete", group: "table", chord: "Delete", label_key: "shell.shortcuts.markDelete" },
    Shortcut { id: "commit", group: "table", chord: "Mod+S", label_key: "shell.shortcuts.commit" },
    Shortcut { id: "copy-region", group: "table", chord: "Mod+C", label_key: "shell.shortcuts.copyRegion" },
    Shortcut { id: "copy-sql-value", group: "table", chord: "Mod+Shift+C", label_key: "shell.shortcuts.copySqlValue" },
    Shortcut { id: "grid-undo", group: "table", chord: "Mod+Z", label_key: "shell.shortcuts.undoStage" },
];

const ICONS: &[&str] = &[
    "code", "play", "table", "plus", "file", "history", "database", "sliders", "sidebar", "sun",
    "moon", "monitor", "keyboard",
];

pub struct ControlSpec {
    pub id: &'static str,
    pub role: &'static str,
    pub label_key: &'static str,
}

pub fn shared_controls() -> Vec<ControlSpec> {
    let mut controls: Vec<_> = COMMANDS
        .iter()
        .map(|command| ControlSpec {
            id: command.id,
            role: "Button",
            label_key: command.label_key,
        })
        .collect();
    controls.extend([
        ControlSpec { id: "editor", role: "TextArea", label_key: "shell.command.newQuery" },
        ControlSpec { id: "grid", role: "Grid", label_key: "shell.shortcuts.groupGrid" },
        ControlSpec { id: "palette", role: "Dialog", label_key: "shell.shortcuts.commandPalette" },
        ControlSpec { id: "density-compact", role: "Button", label_key: "shell.settings.densityCompact" },
        ControlSpec { id: "density-standard", role: "Button", label_key: "shell.settings.densityStandard" },
        ControlSpec { id: "density-relaxed", role: "Button", label_key: "shell.settings.densityRelaxed" },
    ]);
    controls
}

#[derive(Clone)]
pub struct Shell {
    pub locale: Locale,
    pub theme: ThemePreference,
    pub prefers_dark: bool,
    pub density: Density,
    pub focus: FocusTarget,
    focus_before_overlay: Option<FocusTarget>,
    pub palette_open: bool,
    pub palette_tables_only: bool,
    pub sidebar_open: bool,
    pub help_open: bool,
    pub results_open: bool,
    pub page: AppPage,
    pub tab_index: usize,
    pub close_requests: u32,
    pub cancel_requests: u32,
    pub marked_delete: u32,
    pub pending_file: Option<FilePrompt>,
    pub opened_path: Option<PathBuf>,
    pub saved_path: Option<PathBuf>,
    pub dropped_paths: Vec<PathBuf>,
    pub rejected_drops: Vec<String>,
    pub last_command: Option<&'static str>,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            locale: Locale::ZhCn,
            theme: ThemePreference::System,
            prefers_dark: false,
            density: Density::Standard,
            focus: FocusTarget::Editor,
            focus_before_overlay: None,
            palette_open: false,
            palette_tables_only: false,
            sidebar_open: true,
            help_open: false,
            results_open: true,
            page: AppPage::Workspace,
            tab_index: 0,
            close_requests: 0,
            cancel_requests: 0,
            marked_delete: 0,
            pending_file: None,
            opened_path: None,
            saved_path: None,
            dropped_paths: Vec::new(),
            rejected_drops: Vec::new(),
            last_command: None,
        }
    }
}

impl Shell {
    pub fn resolved_theme(&self) -> ResolvedTheme {
        match self.theme {
            ThemePreference::Light => ResolvedTheme::Light,
            ThemePreference::Dark => ResolvedTheme::Dark,
            ThemePreference::System => {
                if self.prefers_dark {
                    ResolvedTheme::Dark
                } else {
                    ResolvedTheme::Light
                }
            }
        }
    }

    pub fn text(&self, key: &'static str) -> &'static str {
        translate(self.locale, key)
    }

    pub fn set_locale(&mut self, locale: Locale) -> Effect {
        self.locale = locale;
        Effect::RefreshMenus
    }

    pub fn set_density(&mut self, density: Density) {
        self.density = density;
    }

    pub fn set_prefers_dark(&mut self, prefers_dark: bool) -> Effect {
        self.prefers_dark = prefers_dark;
        Effect::ApplyTheme
    }

    pub fn close_overlay(&mut self) {
        if self.palette_open || self.help_open {
            self.palette_open = false;
            self.palette_tables_only = false;
            self.help_open = false;
            if let Some(previous) = self.focus_before_overlay.take() {
                self.focus = previous;
            }
        }
    }

    pub fn command(id: &str) -> Option<&'static CommandSpec> {
        COMMANDS.iter().find(|command| command.id == id)
    }

    pub fn shortcut(id: &str) -> Option<&'static Shortcut> {
        SHORTCUTS.iter().find(|shortcut| shortcut.id == id)
    }

    pub fn dispatch(&mut self, id: &str) -> Option<Effect> {
        if let Some(command) = Self::command(id) {
            self.last_command = Some(command.id);
        } else if SHORTCUTS.iter().any(|shortcut| shortcut.id == id) {
            self.last_command = Some(shortcut_static_id(id)?);
        } else if id == "run-current" {
            self.last_command = Some("run-current");
        } else {
            return None;
        }
        Some(self.apply(id))
    }

    fn apply(&mut self, id: &str) -> Effect {
        match id {
            "command-palette" => self.open_palette(false),
            "quick-open" => self.open_palette(true),
            "new-query" => {
                self.page = AppPage::Workspace;
                self.focus = FocusTarget::Editor;
                Effect::FocusEditor
            }
            "execute-query" | "run-selection" => Effect::ExecuteSelection,
            "run-current" => Effect::ExecuteCurrent,
            "open-structure" => {
                self.page = AppPage::Structure;
                Effect::None
            }
            "compare" => {
                self.page = AppPage::Compare;
                Effect::None
            }
            "new-connection" | "connections" => {
                self.page = AppPage::Connections;
                Effect::None
            }
            "open-sqlite" => {
                self.pending_file = Some(FilePrompt::Open);
                Effect::PromptOpen
            }
            "history" => {
                self.page = AppPage::History;
                Effect::None
            }
            "save-query" => {
                self.pending_file = Some(FilePrompt::Save {
                    suggested_name: "query.sql".into(),
                });
                Effect::PromptSave
            }
            "saved-queries" => {
                self.page = AppPage::History;
                Effect::None
            }
            "settings" | "open-settings" => {
                self.page = AppPage::Settings;
                self.focus = FocusTarget::Settings;
                Effect::None
            }
            "sidebar" | "toggle-sidebar" => {
                self.sidebar_open = !self.sidebar_open;
                self.focus = FocusTarget::Sidebar;
                Effect::None
            }
            "theme-light" => {
                self.theme = ThemePreference::Light;
                Effect::ApplyTheme
            }
            "theme-dark" => {
                self.theme = ThemePreference::Dark;
                Effect::ApplyTheme
            }
            "theme-system" => {
                self.theme = ThemePreference::System;
                Effect::ApplyTheme
            }
            "help" | "shortcut-help" => {
                self.open_help();
                Effect::None
            }
            "close-tab" => {
                self.close_requests += 1;
                Effect::None
            }
            "next-tab" => {
                self.tab_index = self.tab_index.wrapping_add(1);
                Effect::None
            }
            "nth-tab" => Effect::None,
            "request-cancel" => {
                self.cancel_requests += 1;
                Effect::None
            }
            "autocomplete" => Effect::ShowCompletions,
            "toggle-comment" => Effect::ToggleComment,
            "toggle-results" => {
                self.results_open = !self.results_open;
                Effect::None
            }
            "format" => Effect::FormatSql,
            "explain" => Effect::ExplainSql,
            "leave-editor" => {
                self.focus = FocusTarget::Grid;
                Effect::None
            }
            "move-cells" => {
                self.focus = FocusTarget::Grid;
                Effect::None
            }
            "select-all-cells" => Effect::SelectAllCells,
            "edit-cell" => Effect::EditCell,
            "cancel-edit" => Effect::CancelEdit,
            "mark-delete" => {
                self.marked_delete += 1;
                Effect::MarkDelete
            }
            "commit" => Effect::CommitEdit,
            "copy-region" => Effect::CopyRegion,
            "copy-sql-value" => Effect::CopySqlValue,
            _ => Effect::None,
        }
    }

    fn open_palette(&mut self, tables_only: bool) -> Effect {
        if !self.palette_open && !self.help_open {
            self.focus_before_overlay = Some(self.focus);
        }
        self.help_open = false;
        self.palette_open = true;
        self.palette_tables_only = tables_only;
        self.focus = FocusTarget::Palette;
        Effect::None
    }

    fn open_help(&mut self) {
        if !self.palette_open && !self.help_open {
            self.focus_before_overlay = Some(self.focus);
        }
        self.palette_open = false;
        self.help_open = true;
        self.focus = FocusTarget::Palette;
    }

    pub fn note_opened(&mut self, path: PathBuf) {
        self.pending_file = None;
        self.opened_path = Some(path);
    }

    pub fn note_saved(&mut self, path: PathBuf) {
        self.pending_file = None;
        self.saved_path = Some(path);
    }

    pub fn note_drop(&mut self, path: PathBuf) {
        self.dropped_paths.push(path);
    }

    pub fn note_rejected_drop(&mut self, reason: String) {
        self.rejected_drops.push(reason);
    }
}

fn shortcut_static_id(id: &str) -> Option<&'static str> {
    SHORTCUTS.iter().find(|shortcut| shortcut.id == id).map(|shortcut| shortcut.id)
}

pub fn translate(locale: Locale, key: &'static str) -> &'static str {
    match locale {
        Locale::ZhCn => zh(key),
        Locale::En => en(key),
    }
    .unwrap_or(key)
}

fn zh(key: &str) -> Option<&'static str> {
    Some(match key {
        "shell.command.newQuery" => "新建查询",
        "shell.command.execute" => "执行",
        "shell.command.newStructure" => "新建表结构",
        "shell.command.compare" => "对比库",
        "shell.command.newConnection" => "新建连接",
        "shell.command.openSqlite" => "打开 SQLite 文件",
        "shell.command.history" => "查询历史",
        "shell.command.saveQuery" => "保存当前查询",
        "shell.command.savedQueries" => "保存的查询",
        "shell.command.connections" => "连接管理",
        "shell.command.settings" => "设置",
        "shell.command.toggleSidebar" => "显示 / 隐藏资源管理器",
        "shell.command.themeLight" => "切换到浅色主题",
        "shell.command.themeDark" => "切换到深色主题",
        "shell.command.themeSystem" => "主题跟随系统",
        "shell.command.help" => "键盘快捷键",
        "shell.command.quit" => "退出",
        "shell.menu.file" => "文件",
        "shell.menu.edit" => "编辑",
        "shell.menu.view" => "视图",
        "shell.menu.query" => "查询",
        "shell.theme.light" => "浅色",
        "shell.theme.dark" => "深色",
        "shell.theme.system" => "跟随系统",
        "shell.settings.densityCompact" => "紧凑",
        "shell.settings.densityStandard" => "标准",
        "shell.settings.densityRelaxed" => "宽松",
        "shell.shortcuts.groupGrid" => "表格",
        "shell.shortcuts.commandPalette" => "命令面板（表、连接、命令）",
        "shell.shortcuts.quickOpen" => "快速打开表",
        "shell.shortcuts.newQuery" => "新建查询",
        "shell.shortcuts.closeTab" => "关闭当前标签",
        "shell.shortcuts.toggleSidebar" => "显示 / 隐藏资源管理器",
        "shell.shortcuts.nextTab" => "切换到下一个标签",
        "shell.shortcuts.nthTab" => "切换到第 N 个标签",
        "shell.shortcuts.openSettings" => "设置",
        "shell.shortcuts.shortcutHelp" => "快捷键帮助",
        "shell.shortcuts.runSelection" => "执行选中内容；无选中时执行光标所在语句",
        "shell.shortcuts.runCurrent" => "只执行光标所在语句",
        "shell.shortcuts.requestCancel" => "请求取消执行",
        "shell.shortcuts.autocomplete" => "补全",
        "shell.shortcuts.toggleComment" => "注释 / 取消注释",
        "shell.shortcuts.toggleResults" => "显示 / 隐藏查询结果区",
        "shell.shortcuts.format" => "格式化选中的 SQL；无选中时格式化全部",
        "shell.shortcuts.explain" => "查看当前语句的执行计划",
        "shell.shortcuts.leaveEditor" => "离开编辑器（不会插入缩进）",
        "shell.shortcuts.moveCells" => "移动单元格；按住 Shift 扩展选择",
        "shell.shortcuts.selectAllCells" => "选中已读取的全部单元格",
        "shell.shortcuts.editCell" => "编辑单元格",
        "shell.shortcuts.cancelEdit" => "取消编辑；未编辑时离开表格",
        "shell.shortcuts.markDelete" => "标记删除选中的行（暂存）",
        "shell.shortcuts.commit" => "预览并提交变更",
        "shell.shortcuts.copyRegion" => "复制选中区域（TSV）",
        "shell.shortcuts.copySqlValue" => "复制为 SQL 值（区分 NULL）",
        "shell.shortcuts.undo" => "撤销",
        "shell.shortcuts.redo" => "重做",
        "shell.shortcuts.find" => "查找",
        "shell.shortcuts.replace" => "替换",
        "shell.shortcuts.undoStage" => "撤销最近一次暂存的修改",
        "shell.command.cut" => "剪切",
        "shell.command.copy" => "复制",
        "shell.command.paste" => "粘贴",
        "shell.command.selectAll" => "全选",
        "common.save" => "保存",
        "common.cancel" => "取消",
        _ => return None,
    })
}

fn en(key: &str) -> Option<&'static str> {
    Some(match key {
        "shell.command.newQuery" => "New query",
        "shell.command.execute" => "Execute",
        "shell.command.newStructure" => "New table structure",
        "shell.command.compare" => "Compare databases",
        "shell.command.newConnection" => "New connection",
        "shell.command.openSqlite" => "Open SQLite file",
        "shell.command.history" => "Query history",
        "shell.command.saveQuery" => "Save current query",
        "shell.command.savedQueries" => "Saved queries",
        "shell.command.connections" => "Manage connections",
        "shell.command.settings" => "Settings",
        "shell.command.toggleSidebar" => "Show / hide the explorer",
        "shell.command.themeLight" => "Switch to light theme",
        "shell.command.themeDark" => "Switch to dark theme",
        "shell.command.themeSystem" => "Theme follows system",
        "shell.command.help" => "Keyboard shortcuts",
        "shell.command.quit" => "Quit",
        "shell.menu.file" => "File",
        "shell.menu.edit" => "Edit",
        "shell.menu.view" => "View",
        "shell.menu.query" => "Query",
        "shell.theme.light" => "Light",
        "shell.theme.dark" => "Dark",
        "shell.theme.system" => "Follow system",
        "shell.settings.densityCompact" => "Compact",
        "shell.settings.densityStandard" => "Standard",
        "shell.settings.densityRelaxed" => "Relaxed",
        "shell.shortcuts.groupGrid" => "Grid",
        "shell.shortcuts.commandPalette" => "Command palette (tables, connections, commands)",
        "shell.shortcuts.quickOpen" => "Quick open table",
        "shell.shortcuts.newQuery" => "New query",
        "shell.shortcuts.closeTab" => "Close current tab",
        "shell.shortcuts.toggleSidebar" => "Show / hide the explorer",
        "shell.shortcuts.nextTab" => "Switch to next tab",
        "shell.shortcuts.nthTab" => "Switch to tab N",
        "shell.shortcuts.openSettings" => "Settings",
        "shell.shortcuts.shortcutHelp" => "Shortcut help",
        "shell.shortcuts.runSelection" => "Run selection; with nothing selected, run the statement at the cursor",
        "shell.shortcuts.runCurrent" => "Run only the statement at the cursor",
        "shell.shortcuts.requestCancel" => "Request cancellation",
        "shell.shortcuts.autocomplete" => "Autocomplete",
        "shell.shortcuts.toggleComment" => "Comment / uncomment",
        "shell.shortcuts.toggleResults" => "Show / hide the query results pane",
        "shell.shortcuts.format" => "Format selected SQL; with nothing selected, format everything",
        "shell.shortcuts.explain" => "Show the execution plan of the current statement",
        "shell.shortcuts.leaveEditor" => "Leave the editor (no indentation inserted)",
        "shell.shortcuts.moveCells" => "Move between cells; hold Shift to extend the selection",
        "shell.shortcuts.selectAllCells" => "Select every loaded cell",
        "shell.shortcuts.editCell" => "Edit cell",
        "shell.shortcuts.cancelEdit" => "Cancel editing; leave the grid when not editing",
        "shell.shortcuts.markDelete" => "Mark selected rows for deletion (staged)",
        "shell.shortcuts.commit" => "Preview and commit changes",
        "shell.shortcuts.copyRegion" => "Copy the selection (TSV)",
        "shell.shortcuts.copySqlValue" => "Copy as SQL values (NULL-aware)",
        "shell.shortcuts.undo" => "Undo",
        "shell.shortcuts.redo" => "Redo",
        "shell.shortcuts.find" => "Find",
        "shell.shortcuts.replace" => "Replace",
        "shell.shortcuts.undoStage" => "Undo the last staged change",
        "shell.command.cut" => "Cut",
        "shell.command.copy" => "Copy",
        "shell.command.paste" => "Paste",
        "shell.command.selectAll" => "Select all",
        "common.save" => "Save",
        "common.cancel" => "Cancel",
        _ => return None,
    })
}

pub fn dictionary_keys() -> Vec<&'static str> {
    let mut keys = Vec::new();
    for command in COMMANDS {
        keys.push(command.label_key);
    }
    for shortcut in SHORTCUTS {
        keys.push(shortcut.label_key);
    }
    keys.extend([
        "shell.command.quit",
        "shell.menu.file",
        "shell.menu.edit",
        "shell.shortcuts.replace",
        "shell.command.cut",
        "shell.command.copy",
        "shell.command.paste",
        "shell.command.selectAll",
        "shell.menu.view",
        "shell.menu.query",
        "shell.theme.light",
        "shell.theme.dark",
        "shell.theme.system",
        "shell.settings.densityCompact",
        "shell.settings.densityStandard",
        "shell.settings.densityRelaxed",
        "common.save",
        "common.cancel",
    ]);
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// A late completion or lint applies only when it still names the current document.
pub fn accept_versioned(current: u64, response: u64) -> bool {
    current == response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales_share_every_foundation_key_and_do_not_echo_the_key() {
        for key in dictionary_keys() {
            let zh = translate(Locale::ZhCn, key);
            let en = translate(Locale::En, key);
            assert_ne!(zh, key, "{key}");
            assert_ne!(en, key, "{key}");
            assert_ne!(zh, en, "{key}");
        }
    }

    #[test]
    fn command_icons_and_ids_match_the_existing_shell() {
        let ids: Vec<_> = COMMANDS.iter().map(|command| command.id).collect();
        assert_eq!(
            ids,
            [
                "new-query",
                "execute-query",
                "open-structure",
                "compare",
                "new-connection",
                "open-sqlite",
                "history",
                "save-query",
                "saved-queries",
                "connections",
                "settings",
                "sidebar",
                "theme-light",
                "theme-dark",
                "theme-system",
                "help",
            ]
        );
        for command in COMMANDS {
            assert!(ICONS.contains(&command.icon), "{}", command.icon);
        }
    }

    #[test]
    fn theme_density_focus_and_commands_are_headless() {
        let mut shell = Shell::default();
        assert_eq!(shell.resolved_theme(), ResolvedTheme::Light);
        shell.prefers_dark = true;
        assert_eq!(shell.resolved_theme(), ResolvedTheme::Dark);
        assert_eq!(shell.dispatch("theme-light"), Some(Effect::ApplyTheme));
        assert_eq!(shell.resolved_theme(), ResolvedTheme::Light);
        shell.set_density(Density::Compact);
        assert_eq!(shell.density.row_height(), 24);
        shell.set_density(Density::Relaxed);
        assert_eq!(shell.density.control_padding(), 12);
        assert_eq!(shell.dispatch("command-palette"), Some(Effect::None));
        assert_eq!(shell.focus, FocusTarget::Palette);
        shell.close_overlay();
        assert_eq!(shell.focus, FocusTarget::Editor);
        assert!(!shell.palette_open);
        assert_eq!(shell.dispatch("sidebar"), Some(Effect::None));
        assert!(!shell.sidebar_open);
        assert_eq!(shell.focus, FocusTarget::Sidebar);
        assert_eq!(shell.dispatch("new-query"), Some(Effect::FocusEditor));
        assert_eq!(shell.focus, FocusTarget::Editor);
        assert_eq!(shell.dispatch("open-sqlite"), Some(Effect::PromptOpen));
        assert_eq!(shell.pending_file, Some(FilePrompt::Open));
        assert_eq!(shell.dispatch("not-a-command"), None);
        for command in COMMANDS {
            assert!(shell.dispatch(command.id).is_some(), "{}", command.id);
        }
        for shortcut in SHORTCUTS {
            assert!(shell.dispatch(shortcut.id).is_some(), "{}", shortcut.id);
        }
        for control in shared_controls() {
            assert!(!control.role.is_empty());
            assert_ne!(translate(Locale::En, control.label_key), control.label_key);
        }
    }

    #[test]
    fn stale_document_responses_are_rejected() {
        assert!(accept_versioned(3, 3));
        assert!(!accept_versioned(4, 3));
    }
}
