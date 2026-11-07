//! 外壳状态。命令、菜单、快捷键和页面动作都从 `dispatch` 进入。

use std::collections::{BTreeSet, VecDeque};
use std::path::Path;

use crate::app::views::connections::{blank_draft, ConnectionPage, Secrets};
use crate::app::views::history::{HistoryPage, HistorySection, LoadPlan};
use crate::app::views::settings::SettingsForm;
use crate::app::views::welcome::WelcomePage;
use crate::app::{AppPage, CommandSpec, Density, Effect, FocusTarget, Locale, Shell, ThemePreference, COMMANDS};
use crate::application::tasks::TaskToken;
use crate::application::{AppServices, TaskBoard};
use crate::connections::model::{CaMode, Engine, SshAuthKind};
use crate::i18n;
use crate::state::{AppState, Platform, SessionState, SessionTab, ShellSettings, StateError};

use super::close::{draft_pending, evaluate, message_key, update_block_key, CloseFacts, CloseKind, CloseScope};
use super::layout::{layout_for, painted_frame, unbreakable_px, ChromeLayout, PaintedFrame};
use super::menus::{MenuPlace, MenuTarget, NavKey, OpenMenu};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkTool {
    Query,
    Table,
    Structure,
    Admin,
    Redis,
    Mongo,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryTab {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub draft: String,
    pub last_executed_sql: Option<String>,
    pub uncommitted_cells: bool,
    pub cell_note: String,
    pub running: bool,
    pub connection_id: Option<String>,
    pub connection_name: Option<String>,
    pub database: Option<String>,
    pub engine: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftField {
    Name,
    Host,
    Port,
    Database,
    Username,
    File,
    Url,
    CaFile,
    ClientCert,
    ClientKey,
    SshHost,
    SshPort,
    SshUser,
    SshKey,
    Password,
    SshPassword,
    Passphrase,
    Replica,
    Filter,
    HistorySearch,
    SaveName,
    PaletteQuery,
    BundlePassphrase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingJob {
    RefreshConnections,
    SaveConnection,
    DeleteConnection,
    TestConnection,
    OpenConnection,
    Disconnect,
    ExportConnections,
    ImportConnections,
    RefreshHistory,
    SaveNamed,
    ClearHistory,
    DeleteRun,
    DeleteSaved,
}

#[derive(Clone, Debug)]
pub struct ClosePrompt {
    pub scope: CloseScope,
    pub tab_id: Option<String>,
    pub kind: CloseKind,
}

#[derive(Clone)]
pub struct WorkbenchModel {
    pub shell: Shell,
    pub settings: SettingsForm,
    pub tabs: Vec<QueryTab>,
    pub active_tab: Option<String>,
    pub connections: ConnectionPage,
    pub history: HistoryPage,
    pub welcome: WelcomePage,
    pub menu: Option<OpenMenu>,
    pub menu_return: FocusTarget,
    pub close_prompt: Option<ClosePrompt>,
    pub release_close: bool,
    pub running_jobs: u32,
    pub viewport_width: i32,
    pub palette_query: String,
    pub palette_index: usize,
    pub notice_key: Option<&'static str>,
    pub notice_detail: String,
    pub copied: String,
    pub executions: u32,
    pub loads: u32,
    pub commands: Vec<String>,
    pub pending: VecDeque<PendingJob>,
    pub focused_field: Option<DraftField>,
    pub url_buffer: String,
    pub export_path: Option<String>,
    pub import_path: Option<String>,
    pub bundle_passphrase: String,
    pub import_notes: Vec<String>,
    pub grid_writable: bool,
    /// 网格暂存不进 app-state.json，但窗口关闭仍要看见它。
    pub grid_dirty: bool,
    pub startup_warning: Option<String>,
    pub tool: WorkTool,
    pub table_spec: Option<String>,
    /// 对象树当前选中的 `id|name|database|schema|table|engine|env`。
    pub object_spec: Option<String>,
    pub exchange_open: bool,
    /// 导入、导出、dump 或 migrate 仍在跑。关闭守卫把它和网格暂存、查询草稿分开。
    pub exchange_running: bool,
    /// 结构或数据比较仍在跑。关闭守卫把它和网格暂存、查询草稿分开。
    pub compare_running: bool,
    /// `connection|database|schema|label`。关系图只读打开这一范围。
    pub diagram_spec: Option<String>,
    /// `connection|name|database`。键浏览只对已打开的 Redis 会话发 SCAN。
    pub redis_spec: Option<String>,
    /// `connection|name|database|collection`。文档浏览只对已打开的 MongoDB 会话发命令。
    pub mongo_spec: Option<String>,
    /// Redis 键浏览或命令还在跑。关闭守卫把它和网格暂存、查询草稿分开。
    pub redis_running: bool,
    /// MongoDB 文档读取或写入还在跑。关闭守卫把它和网格暂存、查询草稿分开。
    pub mongo_running: bool,
}

impl WorkbenchModel {
    pub fn new(shell: Shell) -> Self {
        let mut settings = SettingsForm::default();
        settings.capture_shell(&shell);
        Self {
            shell,
            settings,
            tabs: Vec::new(),
            active_tab: None,
            connections: ConnectionPage::default(),
            history: HistoryPage::default(),
            welcome: WelcomePage { has_connections: false },
            menu: None,
            menu_return: FocusTarget::Editor,
            close_prompt: None,
            release_close: false,
            running_jobs: 0,
            viewport_width: 1360,
            palette_query: String::new(),
            palette_index: 0,
            notice_key: None,
            notice_detail: String::new(),
            copied: String::new(),
            executions: 0,
            loads: 0,
            commands: Vec::new(),
            pending: VecDeque::new(),
            focused_field: None,
            url_buffer: String::new(),
            export_path: None,
            import_path: None,
            bundle_passphrase: String::new(),
            import_notes: Vec::new(),
            grid_writable: false,
            grid_dirty: false,
            startup_warning: None,
            tool: WorkTool::Query,
            table_spec: None,
            object_spec: None,
            exchange_open: false,
            exchange_running: false,
            compare_running: false,
            diagram_spec: None,
            redis_spec: None,
            mongo_spec: None,
            redis_running: false,
            mongo_running: false,
        }
    }

    pub fn apply_settings(&mut self, settings: &ShellSettings) {
        self.settings = SettingsForm::from_settings(settings);
        self.settings.apply_shell(&mut self.shell);
    }

    pub fn locale(&self) -> i18n::Locale {
        match self.shell.locale {
            Locale::En => i18n::Locale::En,
            Locale::ZhCn => i18n::Locale::ZhCn,
        }
    }

    pub fn text(&self, key: &str) -> String {
        i18n::text(self.locale(), key)
    }

    pub fn layout(&self) -> ChromeLayout {
        layout_for(self.viewport_width, self.shell.sidebar_open)
    }

    pub fn painted(&self) -> PaintedFrame {
        painted_frame(self.viewport_width, self.shell.sidebar_open, self.shell.density.control_padding())
    }

    pub fn set_width(&mut self, width: i32) {
        self.viewport_width = width;
        if width <= 960 && self.shell.sidebar_open && width <= 640 {
            // 640 以下侧栏改为覆盖，仍允许打开。
        }
    }

    pub fn dispatch(&mut self, id: &str) -> Option<Effect> {
        self.commands.push(id.to_owned());
        if self.close_prompt.is_some() && !matches!(id, "close-stay" | "close-leave") {
            return Some(Effect::None);
        }
        if let Some(effect) = self.shell.dispatch(id) {
            self.after_shell(id);
            return Some(effect);
        }
        if self.local(id) {
            return Some(Effect::None);
        }
        self.commands.pop();
        None
    }

    fn after_shell(&mut self, id: &str) {
        match id {
            "new-query" => self.open_blank_query(),
            "new-connection" => {
                self.connections.start_new(Engine::Postgres);
                self.focused_field = Some(DraftField::Name);
            }
            "connections" => self.welcome.has_connections = !self.connections.rows.is_empty(),
            "history" => self.history.section = HistorySection::Runs,
            "saved-queries" => {
                self.history.section = HistorySection::Saved;
                self.shell.page = AppPage::History;
            }
            "close-tab" => self.request_close_active(),
            "execute-query" | "run-current" | "run-selection" => self.executions += 1,
            "command-palette" | "quick-open" => {
                self.palette_query.clear();
                self.palette_index = 0;
                self.menu = None;
            }
            "help" | "shortcut-help" => self.menu = None,
            "theme-light" | "theme-dark" | "theme-system" => self.settings.capture_shell(&self.shell),
            "open-structure" => {
                self.tool = WorkTool::Structure;
                if let Some(spec) = self.object_spec.clone() {
                    self.table_spec = Some(spec);
                }
            }
            _ => {}
        }
    }

    fn local(&mut self, id: &str) -> bool {
        if let Some(spec) = id.strip_prefix("open-table:") {
            self.tool = WorkTool::Table;
            self.shell.page = AppPage::Workspace;
            self.shell.focus = FocusTarget::Grid;
            self.table_spec = Some(spec.to_owned());
            return true;
        }
        if let Some(spec) = id.strip_prefix("open-structure-for:") {
            self.tool = WorkTool::Structure;
            self.shell.page = AppPage::Structure;
            self.table_spec = Some(spec.to_owned());
            return true;
        }
        if let Some(spec) = id.strip_prefix("open-redis:") {
            self.tool = WorkTool::Redis;
            self.shell.page = AppPage::Workspace;
            self.shell.focus = FocusTarget::Sidebar;
            self.redis_spec = Some(spec.to_owned());
            return true;
        }
        if let Some(spec) = id.strip_prefix("open-mongo:") {
            self.tool = WorkTool::Mongo;
            self.shell.page = AppPage::Workspace;
            self.shell.focus = FocusTarget::Sidebar;
            self.mongo_spec = Some(spec.to_owned());
            return true;
        }
        if let Some(connection_id) = id.strip_prefix("connection:") {
            self.connections.selected = Some(connection_id.to_owned());
            self.shell.page = AppPage::Connections;
            return true;
        }
        if id.starts_with("exchange-") {
            return true;
        }
        if id.starts_with("compare-") {
            self.shell.page = AppPage::Compare;
            return true;
        }
        if id == "open-diagram" {
            self.open_diagram_from_object();
            return true;
        }
        if let Some(spec) = id.strip_prefix("open-diagram:") {
            self.diagram_spec = Some(spec.to_owned());
            self.shell.page = AppPage::Diagram;
            return true;
        }
        if id.starts_with("diagram-") {
            self.shell.page = AppPage::Diagram;
            return true;
        }
        if let Some(engine) = id.strip_prefix("conn-engine:") {
            if let Ok(engine) = Engine::parse(engine) {
                self.connections.select_engine(engine);
                self.shell.page = AppPage::Connections;
                self.focused_field = Some(DraftField::Name);
            }
            return true;
        }
        match id {
            "activity-workspace" => {
                if self.shell.page == AppPage::Workspace {
                    self.shell.sidebar_open = !self.shell.sidebar_open;
                    self.shell.focus = FocusTarget::Sidebar;
                } else {
                    self.shell.page = AppPage::Workspace;
                    self.shell.focus = FocusTarget::Editor;
                }
            }
            "activity-connections" => {
                let _ = self.shell.dispatch("connections");
            }
            "activity-history" => {
                let _ = self.shell.dispatch("history");
            }
            "activity-settings" => {
                let _ = self.shell.dispatch("settings");
            }
            "conn-edit" => self.connections.edit_selected(),
            "conn-duplicate" => self.connections.duplicate_selected(),
            "conn-delete" => {
                if self.connections.arm_delete() {
                    self.notice_key = Some("connections.deleteDialog.title");
                    self.notice_detail.clear();
                }
            }
            "conn-delete-confirm" => {
                self.connections.confirm_delete = false;
                if self.connections.selected.is_some() {
                    self.enqueue(PendingJob::DeleteConnection);
                }
            }
            "conn-delete-cancel" => {
                self.connections.confirm_delete = false;
                self.notice_key = Some("common.cancel");
                self.notice_detail.clear();
            }
            "conn-test" => {
                if self.connections.selected.is_some() {
                    self.enqueue(PendingJob::TestConnection);
                }
            }
            "conn-connect" | "conn-open" => {
                if self.connections.selected.is_some() {
                    self.enqueue(PendingJob::OpenConnection);
                }
            }
            "conn-disconnect" => {
                if self.connections.selected.is_some() {
                    self.enqueue(PendingJob::Disconnect);
                }
            }
            "conn-save" => {
                self.connections.issues = self.connections.validate();
                if self.connections.editor.is_some() && self.connections.issues.is_empty() {
                    self.enqueue(PendingJob::SaveConnection);
                } else if let Some(issue) = self.connections.issues.first() {
                    self.notice_key = Some(issue.key);
                    self.notice_detail = issue.field.clone();
                } else {
                    self.notice_key = Some("connections.editor.newTitle");
                }
            }
            "conn-refresh" => self.enqueue(PendingJob::RefreshConnections),
            "conn-export" => {
                if self.export_path.is_some() {
                    self.enqueue(PendingJob::ExportConnections);
                } else {
                    self.notice_key = Some("connections.page.exportButton");
                }
            }
            "conn-import" => {
                if self.import_path.is_some() {
                    self.enqueue(PendingJob::ImportConnections);
                } else {
                    self.notice_key = Some("connections.page.importRillSql");
                }
            }
            "conn-apply-url" => {
                if let Err(key) = self.connections.apply_url(&self.url_buffer) {
                    self.notice_key = Some(key);
                }
            }
            "conn-filter-engine" => self.connections.cycle_engine_filter(),
            "conn-tls-cycle" => self.connections.cycle_tls(),
            "conn-ssh-toggle" => self.connections.toggle_ssh(),
            "conn-ssh-auth" => self.connections.cycle_ssh_auth(),
            "conn-ca-mode" => self.connections.cycle_ca_mode(),
            "conn-env-cycle" => self.connections.cycle_env(),
            "menu-object" => self.open_tree_menu(false),
            "menu-object-item" => self.open_tree_menu(true),
            "menu-grid" => self.open_grid_menu(),
            "menu-tab" => self.open_tab_menu(),
            "menu-editor" => self.open_editor_menu(),
            "menu-connection" => self.open_connection_menu(),
            "menu-history" => self.open_history_menu(),
            "update-install" => {
                let _ = self.request_update_install();
            }
            "null-next" => {
                self.settings.null_copy = match self.settings.null_copy.as_str() {
                    "NULL" => "empty",
                    "empty" => "N",
                    _ => "NULL",
                }
                .to_owned();
            }
            "timeout-cycle" => {
                self.settings.query_timeout_sec = match self.settings.query_timeout_sec {
                    0 => 15,
                    15 => 60,
                    _ => 0,
                };
            }
            "page-size-up" => self.settings.page_size = (self.settings.page_size + 50).clamp(1, 10_000),
            "preview-up" => self.settings.preview_kb = (self.settings.preview_kb + 1).clamp(1, 64),
            "tree-open-data" | "tree-copy-name" | "tree-select-sql" => self.tree_action(id),
            "open-exchange" => {
                self.exchange_open = !self.exchange_open;
            }
            "open-admin" => {
                self.tool = WorkTool::Admin;
                self.shell.page = AppPage::Structure;
            }
            "grid-copy-row" | "grid-copy-column" | "grid-filter-value" => {
                self.copied = id.to_owned();
                self.shell.focus = FocusTarget::Grid;
            }
            "tab-close-others" => self.close_others(),
            "tab-close-right" => self.close_right(),
            "tab-close-all" => self.request_close_all(),
            "editor-clear" => self.clear_editor(),
            "history-load" => self.load_selected_history(),
            "history-copy" => self.copy_selected_sql(),
            "history-detail" => self.notice_key = Some("history.page.sqlPreview"),
            "history-filter-connection" => self.history.filter_selected_connection(),
            "history-refresh" => self.enqueue(PendingJob::RefreshHistory),
            "history-save-named" => self.enqueue(PendingJob::SaveNamed),
            "history-clear" => {
                if !self.history.confirm_clear {
                    self.history.confirm_clear = true;
                    self.notice_key = Some("history.page.clearConfirm");
                } else {
                    self.history.confirm_clear = false;
                    self.enqueue(PendingJob::ClearHistory);
                }
            }
            "history-delete" => self.enqueue(PendingJob::DeleteRun),
            "saved-delete" => self.enqueue(PendingJob::DeleteSaved),
            "density-compact" | "density-standard" | "density-relaxed" => {
                self.settings.density = id.trim_start_matches("density-").to_owned();
                self.settings.apply_shell(&mut self.shell);
            }
            "editor-theme-next" => {
                self.settings.cycle_editor_theme();
            }
            "locale-zh" => {
                self.settings.locale = "zh-CN".into();
                self.settings.apply_shell(&mut self.shell);
            }
            "locale-en" => {
                self.settings.locale = "en".into();
                self.settings.apply_shell(&mut self.shell);
            }
            "font-up" => self.settings.set_editor_font(self.settings.editor_font + 1),
            "font-down" => self.settings.set_editor_font(self.settings.editor_font - 1),
            "toggle-updates" => self.settings.auto_check_updates = !self.settings.auto_check_updates,
            "close-stay" => self.choose_stay(),
            "close-leave" => self.choose_leave(),
            "welcome-connections" => {
                let _ = self.shell.dispatch("connections");
            }
            "welcome-sqlite" => {
                let _ = self.shell.dispatch("open-sqlite");
            }
            "welcome-compare" => {
                let _ = self.shell.dispatch("compare");
            }
            "welcome-history" => {
                let _ = self.shell.dispatch("history");
            }
            "welcome-saved" => {
                let _ = self.shell.dispatch("saved-queries");
                self.history.section = HistorySection::Saved;
            }
            _ => return false,
        }
        true
    }

    fn tree_action(&mut self, id: &str) {
        match id {
            "tree-copy-name" => self.copied = self.connections.selected.clone().unwrap_or_default(),
            "tree-select-sql" => {
                let sql = "select * from dual".to_owned();
                self.open_query_draft(sql, false);
            }
            "tree-open-data" => {
                self.shell.page = AppPage::Workspace;
                self.shell.focus = FocusTarget::Grid;
                self.notice_key = Some("catalog.tree.openData");
                if let Some(spec) = self.object_spec.clone() {
                    self.tool = WorkTool::Table;
                    self.table_spec = Some(spec);
                }
            }
            _ => {}
        }
    }

    fn open_diagram_from_object(&mut self) {
        let Some(spec) = self.object_spec.clone() else {
            self.shell.page = AppPage::Diagram;
            self.diagram_spec = None;
            return;
        };
        let parts: Vec<&str> = spec.split('|').collect();
        if parts.len() >= 6 && (parts[5] == "redis" || parts[5] == "mongodb") {
            self.notice_key = Some("diagram.unsupported");
            self.notice_detail.clear();
            return;
        }
        if let Some(spec) = crate::app::views::diagram::spec_from_object(&spec) {
            self.diagram_spec = Some(spec);
            self.shell.page = AppPage::Diagram;
        }
    }

    fn enqueue(&mut self, job: PendingJob) {
        self.running_jobs = self.running_jobs.saturating_add(1);
        self.pending.push_back(job);
    }

    pub fn take_pending(&mut self) -> Option<PendingJob> {
        self.pending.pop_front()
    }

    pub fn drain_pending(&mut self) -> Vec<PendingJob> {
        self.pending.drain(..).collect()
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn finish_job(&mut self) {
        self.running_jobs = self.running_jobs.saturating_sub(1);
    }

    pub fn prepare_list_token(&mut self, tasks: &TaskBoard) -> TaskToken {
        self.connections.begin(tasks)
    }

    pub fn begin_owner(&self, tasks: &TaskBoard, owner: &str) -> TaskToken {
        tasks.supersede(owner);
        tasks.begin(owner)
    }

    pub fn open_menu(&mut self, place: MenuPlace, target: MenuTarget) {
        if self.shell.palette_open || self.shell.help_open {
            self.shell.close_overlay();
        }
        self.menu_return = self.shell.focus;
        self.menu = Some(OpenMenu::new(place, target));
        self.shell.focus = FocusTarget::Palette;
    }

    pub fn open_connection_menu(&mut self) {
        self.open_menu(MenuPlace::ConnectionRow, MenuTarget::Connection);
    }

    pub fn open_history_menu(&mut self) {
        let target = if self.history.section == HistorySection::Saved { MenuTarget::Saved } else { MenuTarget::Run };
        self.open_menu(MenuPlace::HistoryEntry, target);
    }

    pub fn open_editor_menu(&mut self) {
        self.open_menu(MenuPlace::Editor, MenuTarget::QueryTab);
    }

    pub fn open_tab_menu(&mut self) {
        self.open_menu(MenuPlace::TabBar, MenuTarget::QueryTab);
    }

    pub fn open_grid_menu(&mut self) {
        let target = if self.grid_writable { MenuTarget::WritableGrid } else { MenuTarget::ReadonlyGrid };
        self.open_menu(MenuPlace::Grid, target);
    }

    pub fn open_tree_menu(&mut self, object: bool) {
        let target = if object { MenuTarget::Object } else { MenuTarget::Connection };
        self.open_menu(MenuPlace::ObjectTree, target);
    }

    pub fn dismiss(&mut self) -> Effect {
        if self.menu.take().is_some() {
            self.shell.focus = self.menu_return;
            return Effect::None;
        }
        if self.close_prompt.is_some() {
            self.choose_stay();
            return Effect::None;
        }
        self.shell.close_overlay();
        Effect::None
    }

    pub fn key(&mut self, key: NavKey) -> Option<String> {
        if self.menu.is_some() {
            return self.menu_key(key);
        }
        if self.close_prompt.is_some() {
            return match key {
                NavKey::Escape => {
                    self.choose_stay();
                    None
                }
                NavKey::Enter => {
                    self.choose_leave();
                    Some("close-leave".into())
                }
                _ => None,
            };
        }
        if self.shell.palette_open {
            return self.palette_key(key);
        }
        None
    }

    fn menu_key(&mut self, key: NavKey) -> Option<String> {
        let menu = self.menu.as_mut()?;
        match key {
            NavKey::Escape => {
                let _ = menu;
                self.dismiss();
                None
            }
            NavKey::ArrowDown => {
                menu.move_by(1);
                None
            }
            NavKey::ArrowUp => {
                menu.move_by(-1);
                None
            }
            NavKey::Home => {
                menu.home();
                None
            }
            NavKey::End => {
                menu.end();
                None
            }
            NavKey::Enter => {
                let id = menu.current().map(|item| item.id.to_owned());
                self.menu = None;
                self.shell.focus = self.menu_return;
                if let Some(id) = id {
                    let _ = self.dispatch(&id);
                    return Some(id);
                }
                None
            }
        }
    }

    fn palette_key(&mut self, key: NavKey) -> Option<String> {
        let entries = self.palette_entries();
        match key {
            NavKey::Escape => {
                self.dismiss();
                None
            }
            NavKey::ArrowDown => {
                if !entries.is_empty() {
                    self.palette_index = (self.palette_index + 1) % entries.len();
                }
                None
            }
            NavKey::ArrowUp => {
                if !entries.is_empty() {
                    self.palette_index = (self.palette_index + entries.len() - 1) % entries.len();
                }
                None
            }
            NavKey::Home => {
                self.palette_index = 0;
                None
            }
            NavKey::End => {
                self.palette_index = entries.len().saturating_sub(1);
                None
            }
            NavKey::Enter => {
                let id = entries.get(self.palette_index).map(|entry| entry.id.clone())?;
                self.shell.close_overlay();
                let _ = self.dispatch(&id);
                Some(id)
            }
        }
    }

    pub fn palette_entries(&self) -> Vec<PaletteEntry> {
        let mut entries = Vec::new();
        for command in COMMANDS {
            entries.push(PaletteEntry {
                id: command.id.to_owned(),
                group: "command",
                label_key: command.label_key,
                label: self.text(command.label_key),
            });
        }
        for row in &self.connections.rows {
            entries.push(PaletteEntry {
                id: format!("connection:{}", row.saved.id),
                group: "connection",
                label_key: "shell.paletteGroup.connection",
                label: row.saved.name.clone(),
            });
        }
        filter_palette(&entries, &self.palette_query, self.shell.palette_tables_only)
    }

    pub fn facts(&self) -> CloseFacts {
        let running = self.running_jobs > 0 || self.exchange_running || self.compare_running || self.redis_running || self.mongo_running || self.tabs.iter().any(|tab| tab.running);
        let uncommitted = self.grid_dirty || self.tabs.iter().any(|tab| tab.uncommitted_cells);
        let draft = self.tabs.iter().any(|tab| draft_pending(&tab.kind, &tab.draft, tab.last_executed_sql.as_deref()));
        CloseFacts { running, uncommitted, draft }
    }

    pub fn tab_facts(&self, id: &str) -> CloseFacts {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) else {
            return CloseFacts { running: false, uncommitted: false, draft: false };
        };
        CloseFacts {
            running: tab.running,
            uncommitted: tab.uncommitted_cells,
            draft: draft_pending(&tab.kind, &tab.draft, tab.last_executed_sql.as_deref()),
        }
    }

    pub fn request_close_active(&mut self) {
        let Some(id) = self.active_tab.clone() else { return };
        self.request_close_tab(&id);
    }

    pub fn request_close_tab(&mut self, id: &str) {
        let facts = self.tab_facts(id);
        match evaluate(facts) {
            CloseKind::Allow => self.remove_tab(id),
            kind => {
                self.notice_key = message_key(CloseScope::Tab, kind);
                if !matches!(kind, CloseKind::BlockedRunning) {
                    self.close_prompt = Some(ClosePrompt { scope: CloseScope::Tab, tab_id: Some(id.to_owned()), kind });
                }
            }
        }
    }

    pub fn allow_system_close(&mut self) -> bool {
        if self.release_close {
            self.release_close = false;
            return true;
        }
        let facts = self.facts();
        match evaluate(facts) {
            CloseKind::Allow => true,
            kind => {
                self.notice_key = message_key(CloseScope::Window, kind);
                if let CloseKind::Confirm { .. } = kind {
                    self.close_prompt = Some(ClosePrompt { scope: CloseScope::Window, tab_id: None, kind });
                }
                false
            }
        }
    }

    pub fn request_update_install(&mut self) -> Result<(), &'static str> {
        match update_block_key(self.facts()) {
            None => Ok(()),
            Some(key) => {
                self.notice_key = Some(key);
                Err(key)
            }
        }
    }

    fn choose_stay(&mut self) {
        self.close_prompt = None;
    }

    fn choose_leave(&mut self) {
        let Some(prompt) = self.close_prompt.take() else { return };
        match prompt.scope {
            CloseScope::Tab => {
                if let Some(id) = prompt.tab_id {
                    self.remove_tab(&id);
                }
            }
            CloseScope::Window => self.release_close = true,
        }
    }

    fn remove_tab(&mut self, id: &str) {
        self.tabs.retain(|tab| tab.id != id);
        if self.active_tab.as_deref() == Some(id) {
            self.active_tab = self.tabs.last().map(|tab| tab.id.clone());
        }
    }

    fn close_others(&mut self) {
        let Some(id) = self.active_tab.clone() else { return };
        let others: Vec<String> = self.tabs.iter().filter(|tab| tab.id != id).map(|tab| tab.id.clone()).collect();
        for other in others {
            self.request_close_tab(&other);
            if self.close_prompt.is_some() {
                return;
            }
        }
    }

    fn close_right(&mut self) {
        let Some(id) = self.active_tab.clone() else { return };
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else { return };
        let right: Vec<String> = self.tabs.iter().skip(index + 1).map(|tab| tab.id.clone()).collect();
        for id in right {
            self.request_close_tab(&id);
            if self.close_prompt.is_some() {
                return;
            }
        }
    }

    fn request_close_all(&mut self) {
        let ids: Vec<String> = self.tabs.iter().map(|tab| tab.id.clone()).collect();
        for id in ids {
            self.request_close_tab(&id);
            if self.close_prompt.is_some() {
                return;
            }
        }
    }

    fn clear_editor(&mut self) {
        if let Some(tab) = self.active_mut() {
            if !tab.running {
                tab.draft.clear();
            }
        }
    }

    fn load_selected_history(&mut self) {
        let known = self.known_ids();
        let plan = if self.history.section == HistorySection::Saved {
            self.history.plan_saved(&known)
        } else {
            self.history.plan_run(&known)
        };
        if let Some(plan) = plan {
            self.apply_load(plan);
        }
    }

    pub fn apply_load(&mut self, plan: LoadPlan) {
        let _ignored = plan.execute;
        self.loads += 1;
        self.history.loads += 1;
        self.open_query_draft(plan.sql, false);
        if let Some(tab) = self.active_mut() {
            tab.connection_id = plan.connection_id;
            tab.connection_name = Some(plan.connection_name);
            tab.database = plan.database;
            tab.engine = Some(plan.engine);
            tab.last_executed_sql = None;
        }
        if let Some(key) = plan.notice_key {
            self.notice_key = Some(key);
        }
        self.shell.page = AppPage::Workspace;
        self.shell.focus = FocusTarget::Editor;
    }

    fn copy_selected_sql(&mut self) {
        self.copied = if self.history.section == HistorySection::Saved {
            self.history.plan_saved(&self.known_ids()).map(|plan| plan.sql).unwrap_or_default()
        } else if self.shell.page == AppPage::Workspace {
            self.active_tab().map(|tab| tab.draft.clone()).unwrap_or_default()
        } else {
            self.history.plan_run(&self.known_ids()).map(|plan| plan.sql).unwrap_or_default()
        };
    }

    fn open_blank_query(&mut self) {
        self.open_query_draft(String::new(), true);
    }

    /// 对象树或文件打开查询：写入草稿和目标，不执行。
    pub fn open_bound_query(&mut self, draft: String, connection_id: Option<String>, connection_name: Option<String>, database: Option<String>, engine: Option<String>) {
        self.open_query_draft(draft, false);
        if let Some(tab) = self.active_mut() {
            tab.connection_id = connection_id;
            tab.connection_name = connection_name;
            tab.database = database;
            tab.engine = engine;
            tab.last_executed_sql = None;
            tab.running = false;
        }
        self.shell.focus = FocusTarget::Editor;
    }

    pub fn set_query_target(&mut self, id: &str, connection_id: Option<String>, connection_name: Option<String>, database: Option<String>, engine: Option<String>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.connection_id = connection_id;
            tab.connection_name = connection_name;
            tab.database = database;
            tab.engine = engine;
        }
    }

    fn open_query_draft(&mut self, draft: String, keep_page: bool) {
        let id = format!("query-{}", self.tabs.len() + 1);
        let title_n = self.tabs.iter().filter(|tab| tab.kind == "query").count() + 1;
        self.tabs.push(QueryTab {
            id: id.clone(),
            title: format!("query {title_n}"),
            kind: "query".into(),
            draft,
            last_executed_sql: None,
            uncommitted_cells: false,
            cell_note: String::new(),
            running: false,
            connection_id: None,
            connection_name: None,
            database: None,
            engine: None,
        });
        self.active_tab = Some(id);
        if !keep_page {
            self.shell.page = AppPage::Workspace;
        }
    }

    pub fn active_tab(&self) -> Option<&QueryTab> {
        let id = self.active_tab.as_deref()?;
        self.tabs.iter().find(|tab| tab.id == id)
    }

    fn active_mut(&mut self) -> Option<&mut QueryTab> {
        let id = self.active_tab.clone()?;
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    pub fn known_ids(&self) -> BTreeSet<String> {
        self.connections.rows.iter().map(|row| row.saved.id.clone()).collect()
    }

    pub fn show_welcome(&self) -> bool {
        self.shell.page == AppPage::Workspace && self.tabs.is_empty()
    }

    pub fn set_field(&mut self, field: DraftField, value: String) {
        self.focused_field = Some(field);
        match field {
            DraftField::Password => self.connections.secrets.password = value,
            DraftField::SshPassword => {
                self.connections.secrets.ssh_password = value.clone();
                if !value.trim().is_empty() {
                    if let Some(draft) = self.connections.editor.as_mut() {
                        draft.ssh_auth = SshAuthKind::Password;
                    }
                }
            }
            DraftField::Passphrase => self.connections.secrets.ssh_passphrase = value,
            DraftField::BundlePassphrase => self.bundle_passphrase = value,
            DraftField::Url => self.url_buffer = value,
            DraftField::Filter => self.connections.filter_text = value,
            DraftField::HistorySearch => self.history.filter_text = value,
            DraftField::SaveName => self.history.save_name = value,
            DraftField::PaletteQuery => self.palette_query = value,
            other => {
                if let Some(draft) = self.connections.editor.as_mut() {
                    *field_slot(draft, other) = value;
                    if other == DraftField::CaFile && !draft.ca_file.trim().is_empty() {
                        draft.ca_mode = CaMode::File;
                    }
                }
            }
        }
    }

    pub fn type_into(&mut self, text: &str) {
        let Some(field) = self.focused_field else { return };
        if text == "backspace" {
            let current = self.field_value(field);
            let mut chars: Vec<char> = current.chars().collect();
            chars.pop();
            self.set_field(field, chars.into_iter().collect());
            return;
        }
        let mut current = self.field_value(field);
        current.push_str(text);
        self.set_field(field, current);
    }

    fn field_value(&self, field: DraftField) -> String {
        match field {
            DraftField::Password => self.connections.secrets.password.clone(),
            DraftField::SshPassword => self.connections.secrets.ssh_password.clone(),
            DraftField::Passphrase => self.connections.secrets.ssh_passphrase.clone(),
            DraftField::BundlePassphrase => self.bundle_passphrase.clone(),
            DraftField::Url => self.url_buffer.clone(),
            DraftField::Filter => self.connections.filter_text.clone(),
            DraftField::HistorySearch => self.history.filter_text.clone(),
            DraftField::SaveName => self.history.save_name.clone(),
            DraftField::PaletteQuery => self.palette_query.clone(),
            other => self.connections.editor.as_ref().map(|draft| field_slot_ref(draft, other).to_owned()).unwrap_or_default(),
        }
    }

    pub fn access_nodes(&self) -> Vec<A11yNode> {
        let mut nodes = vec![
            node("menu-bar", "MenuBar", "shell.menu.file", "expanded", "menu"),
            node("toolbar", "Toolbar", "shell.command.newQuery", "enabled", "toolbar"),
            node("activity", "Navigation", "shell.nav.main", "current", "navigate"),
            node("open-exchange", "Button", "export.open", if self.exchange_open { "expanded" } else { "enabled" }, "open-exchange"),
            node("sidebar", "Navigation", "shell.sidebar.explorer", if self.shell.sidebar_open { "expanded" } else { "collapsed" }, "navigate"),
            node("tabs", "TabList", "shell.tabs.tablist", "selected", "select"),
            node("status", "Status", "shell.statusbar.noTarget", "live", "status"),
            node("palette", "Dialog", "shell.palette.title", if self.shell.palette_open { "open" } else { "closed" }, "dialog"),
            node("density-compact", "Button", "shell.settings.densityCompact", if self.settings.density == "compact" { "selected" } else { "enabled" }, "set-density"),
            node("density-standard", "Button", "shell.settings.densityStandard", if self.settings.density == "standard" { "selected" } else { "enabled" }, "set-density"),
            node("density-relaxed", "Button", "shell.settings.densityRelaxed", if self.settings.density == "relaxed" { "selected" } else { "enabled" }, "set-density"),
            node("editor", "MultilineTextInput", "query.editor.area", "editable", "edit"),
            node("grid", "Grid", "shell.shortcuts.groupGrid", if self.grid_writable { "editable" } else { "readonly" }, "grid"),
        ];
        if let Some(menu) = &self.menu {
            nodes.push(node("context-menu", "Menu", menu.place.label_key(), "open", "menu"));
            for item in &menu.items {
                nodes.push(A11yNode {
                    id: item.id.to_owned(),
                    role: "MenuItem",
                    label_key: item.label_key,
                    state: if item.enabled { "enabled" } else { "disabled" },
                    action: item.id,
                });
            }
        }
        nodes
    }

    pub fn visible_keys(&self) -> Vec<&'static str> {
        let mut keys = vec![
            "shell.menu.file",
            "shell.menu.edit",
            "shell.menu.view",
            "shell.menu.query",
            "shell.menu.tools",
            "shell.menu.window",
            "shell.menu.help",
            "shell.nav.workspace",
            "shell.nav.connections",
            "shell.nav.history",
            "shell.nav.settings",
            "export.open",
            "shell.sidebar.explorer",
            "shell.command.newQuery",
            "shell.command.execute",
            "shell.command.openSqlite",
            "shell.command.saveQuery",
            "shell.command.history",
            "shell.palette.title",
            "shell.settings.title",
            "shell.settings.density",
            "shell.settings.densityCompact",
            "shell.settings.densityStandard",
            "shell.settings.densityRelaxed",
            "shell.settings.editorFont",
            "shell.settings.editorTheme",
            "shell.settings.language",
            "shell.welcome.title",
            "shell.welcome.factEngines",
            "connections.page.title",
            "connections.page.emptyFirstHint",
            "connections.page.note",
            "history.page.titleRuns",
            "history.page.loadOnlySql",
            "history.savedPanel.openNote",
            "shell.closeGuard.windowDraft",
            "shell.closeGuard.windowCells",
            "shell.closeGuard.windowBoth",
            "shell.update.busy",
            "shell.update.pending",
        ];
        keys.extend(self.page_keys());
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    fn page_keys(&self) -> Vec<&'static str> {
        match self.shell.page {
            AppPage::Connections => vec![
                "connections.editor.newTitle",
                "connections.editor.tls.title",
                "connections.editor.ssh.title",
                "connections.editor.test.button",
                "connections.page.importRillSql",
                "connections.page.exportButton",
                "connections.editor.ssh.authLabel",
                "connections.editor.ssh.authKey",
                "connections.editor.ssh.authPassword",
                "connections.editor.ssh.keyFile",
                "connections.editor.ssh.port",
                "connections.editor.tls.caLabel",
                "connections.editor.tls.caSystem",
                "connections.editor.tls.caFile",
                "connections.editor.tls.clientCert",
                "connections.editor.tls.clientKey",
                "connections.editor.replicaSet",
                "connections.exportDialog.passphrase",
                "connections.importReport.skippedTitle",
                "connections.deleteDialog.title",
                "connections.deleteDialog.confirm",
                "connections.deleteDialog.willKeepDb",
            ],
            AppPage::History => vec!["history.page.runs", "history.page.saved", "history.page.loadToEditor", "history.savedPanel.save"],
            AppPage::Compare => vec![
                "compare.dialog.title",
                "compare.dialog.subtitle",
                "compare.dialog.compareStructure",
                "compare.dialog.compareData",
                "common.cancel",
            ],
            AppPage::Diagram => vec!["diagram.readonly", "diagram.foreignKeys", "catalog.tree.diagram", "diagram.loading"],
            AppPage::Settings => vec![
                "shell.settings.theme",
                "shell.settings.editorThemeAuto",
                "shell.settings.editorThemeWarm",
                "shell.theme.light",
                "shell.theme.dark",
                "shell.theme.system",
                "common.localeZh",
                "common.localeEn",
                "shell.settings.rowsPerPage",
                "shell.settings.nullCopy",
                "shell.settings.timeout",
                "shell.update.autoCheck",
            ],
            _ if self.show_welcome() => vec!["shell.welcome.resumeTitle", "shell.welcome.recent", "shell.welcome.saved", "common.newConnection", "common.openSqlite"],
            _ => vec!["shell.statusbar.noTarget", "query.editor.area"],
        }
    }

    pub fn missing_copy(&self, locale: i18n::Locale) -> Vec<&'static str> {
        self.visible_keys().into_iter().filter(|key| i18n::text(locale, key) == *key || i18n::text(locale, key).is_empty()).collect()
    }

    pub fn copy_overflow(&self, locale: i18n::Locale) -> i32 {
        let layout = self.layout();
        let mut overflow = layout.overflow;
        let limit = layout.width.max(1);
        for key in self.visible_keys() {
            let text = i18n::text(locale, key);
            let width = unbreakable_px(&text);
            if width > limit {
                overflow += width - limit;
            }
        }
        overflow
    }

    pub fn snapshot(&self) -> AppState {
        let session = SessionState {
            active_id: self.active_tab.clone(),
            tabs: self.tabs.iter().map(tab_to_session).collect(),
        };
        AppState::new(self.settings.to_settings(), session)
    }

    pub fn restore_session(&mut self, session: &SessionState) {
        self.tabs = session.tabs.iter().map(session_to_tab).collect();
        self.active_tab = session.active_id.clone().or_else(|| self.tabs.first().map(|tab| tab.id.clone()));
    }

    pub fn secrets(&self) -> &Secrets {
        &self.connections.secrets
    }
}

fn tab_to_session(tab: &QueryTab) -> SessionTab {
    SessionTab {
        id: tab.id.clone(),
        title: tab.title.clone(),
        kind: tab.kind.clone(),
        draft: tab.draft.clone(),
        connection_id: tab.connection_id.clone(),
        connection_name: tab.connection_name.clone(),
        database: tab.database.clone(),
        engine: tab.engine.clone(),
        connection_status: None,
        target_text: None,
        target_notice: None,
        path: None,
        last_executed_sql: tab.last_executed_sql.clone(),
        table_pane: None,
        structure_section: None,
        definition: None,
        diagram_schema: None,
        redis_key: None,
        doc_database: None,
        doc_collection: None,
        admin_schema: None,
        title_key: None,
    }
}

fn session_to_tab(tab: &SessionTab) -> QueryTab {
    QueryTab {
        id: tab.id.clone(),
        title: tab.title.clone(),
        kind: tab.kind.clone(),
        draft: tab.draft.clone(),
        last_executed_sql: tab.last_executed_sql.clone(),
        uncommitted_cells: false,
        cell_note: String::new(),
        running: false,
        connection_id: tab.connection_id.clone(),
        connection_name: tab.connection_name.clone(),
        database: tab.database.clone(),
        engine: tab.engine.clone(),
    }
}

fn field_slot(draft: &mut crate::connections::model::ConnectionDraft, field: DraftField) -> &mut String {
    match field {
        DraftField::Name => &mut draft.name,
        DraftField::Host => &mut draft.host,
        DraftField::Port => &mut draft.port,
        DraftField::Database => &mut draft.database,
        DraftField::Username => &mut draft.username,
        DraftField::File => &mut draft.file_path,
        DraftField::CaFile => &mut draft.ca_file,
        DraftField::ClientCert => &mut draft.client_cert,
        DraftField::ClientKey => &mut draft.client_key,
        DraftField::SshHost => &mut draft.ssh_host,
        DraftField::SshPort => &mut draft.ssh_port,
        DraftField::SshUser => &mut draft.ssh_user,
        DraftField::SshKey => &mut draft.ssh_key,
        DraftField::Replica => &mut draft.replica_set,
        _ => &mut draft.name,
    }
}

fn field_slot_ref(draft: &crate::connections::model::ConnectionDraft, field: DraftField) -> &str {
    match field {
        DraftField::Name => &draft.name,
        DraftField::Host => &draft.host,
        DraftField::Port => &draft.port,
        DraftField::Database => &draft.database,
        DraftField::Username => &draft.username,
        DraftField::File => &draft.file_path,
        DraftField::CaFile => &draft.ca_file,
        DraftField::ClientCert => &draft.client_cert,
        DraftField::ClientKey => &draft.client_key,
        DraftField::SshHost => &draft.ssh_host,
        DraftField::SshPort => &draft.ssh_port,
        DraftField::SshUser => &draft.ssh_user,
        DraftField::SshKey => &draft.ssh_key,
        DraftField::Replica => &draft.replica_set,
        _ => &draft.name,
    }
}

#[derive(Clone, Debug)]
pub struct PaletteEntry {
    pub id: String,
    pub group: &'static str,
    pub label_key: &'static str,
    pub label: String,
}

pub fn filter_palette(entries: &[PaletteEntry], query: &str, tables_only: bool) -> Vec<PaletteEntry> {
    let trimmed = query.trim().to_lowercase();
    if tables_only {
        let tables: Vec<_> = entries.iter().filter(|entry| entry.group == "table").cloned().collect();
        return if trimmed.is_empty() { tables } else { rank(&tables, &trimmed) };
    }
    if let Some(rest) = trimmed.strip_prefix('>') {
        let commands: Vec<_> = entries.iter().filter(|entry| entry.group == "command").cloned().collect();
        let rest = rest.trim();
        return if rest.is_empty() { commands } else { rank(&commands, rest) };
    }
    if trimmed.is_empty() {
        let mut ordered: Vec<_> = entries.iter().filter(|entry| entry.group == "command").cloned().collect();
        ordered.extend(entries.iter().filter(|entry| entry.group != "command").cloned());
        return ordered;
    }
    rank(entries, &trimmed)
}

fn rank(entries: &[PaletteEntry], query: &str) -> Vec<PaletteEntry> {
    let scored: Vec<(u8, usize, PaletteEntry)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let score = match_score(&entry.label, query);
            (score > 0).then_some((score, index, entry.clone()))
        })
        .collect();
    let groups: Vec<&str> = entries.iter().map(|entry| entry.group).collect();
    let mut ordered = Vec::new();
    let mut seen = Vec::new();
    for group in groups {
        if seen.contains(&group) {
            continue;
        }
        seen.push(group);
        let mut group_items: Vec<_> = scored.iter().filter(|(_, _, entry)| entry.group == group).collect();
        group_items.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        for item in group_items {
            ordered.push(item.2.clone());
        }
    }
    let _ = scored.len();
    ordered
}

fn match_score(text: &str, query: &str) -> u8 {
    let haystack = text.to_lowercase();
    if haystack.starts_with(query) {
        return 3;
    }
    if haystack.contains(query) {
        return 2;
    }
    let mut at = 0;
    for ch in query.chars() {
        let Some(found) = haystack[at..].find(ch) else { return 0 };
        at += found + ch.len_utf8();
    }
    1
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct A11yNode {
    pub id: String,
    pub role: &'static str,
    pub label_key: &'static str,
    pub state: &'static str,
    pub action: &'static str,
}

fn node(id: &str, role: &'static str, label_key: &'static str, state: &'static str, action: &'static str) -> A11yNode {
    A11yNode { id: id.to_owned(), role, label_key, state, action }
}

pub struct Startup {
    pub state: AppState,
    pub warning_key: Option<&'static str>,
    pub warning_detail: String,
}

pub async fn load_startup(services: &AppServices, legacy: Option<&Path>, platform: Platform) -> Startup {
    let path = services.state.path();
    if path.exists() {
        return match services.load_state().await {
            Ok(state) => Startup { state, warning_key: None, warning_detail: String::new() },
            Err(err) => Startup {
                state: AppState::new(ShellSettings::default(), SessionState::default()),
                warning_key: Some(state_warning_key(&err)),
                warning_detail: err.to_string(),
            },
        };
    }
    if let Some(root) = legacy {
        let report = services.import_legacy(root, platform).await;
        if report.wrote_state {
            if let Ok(state) = services.load_state().await {
                return Startup { state, warning_key: None, warning_detail: String::new() };
            }
        }
    }
    Startup { state: AppState::new(ShellSettings::default(), SessionState::default()), warning_key: None, warning_detail: String::new() }
}

fn state_warning_key(err: &StateError) -> &'static str {
    match err {
        StateError::UnknownVersion(_) => "shell.state.unknown",
        StateError::Forbidden(_) => "shell.state.forbidden",
        StateError::Malformed => "shell.state.malformed",
    }
}

pub async fn persist_state(services: &AppServices, state: &AppState) -> Result<(), String> {
    if services.state.path().exists() {
        services.load_state().await.map_err(|err| err.to_string())?;
    }
    let blob = serde_json::to_string(state).unwrap_or_default();
    if blob.contains("sshPassphrase") || blob.contains("\"password\"") {
        return Err("shell.state.forbidden".into());
    }
    services.save_state(state).await
}

#[allow(dead_code)]
pub fn blank_engine(engine: Engine) -> crate::connections::model::ConnectionDraft {
    blank_draft(engine)
}

#[allow(dead_code)]
fn _command_spec(command: &CommandSpec) -> &str {
    command.id
}

#[allow(dead_code)]
fn _density(density: Density) -> i32 {
    density.row_height()
}

#[allow(dead_code)]
fn _theme(theme: ThemePreference) -> &'static str {
    match theme {
        ThemePreference::Light => "light",
        ThemePreference::Dark => "dark",
        ThemePreference::System => "system",
    }
}
