//! 外壳状态。命令、菜单、快捷键和页面动作都从 `dispatch` 进入。

use std::collections::{BTreeSet, VecDeque};
use std::path::Path;

use crate::app::views::connections::{blank_draft, ConnectionPage, Secrets};
use crate::app::views::history::{HistoryPage, HistorySection, LoadPlan};
use crate::app::views::settings::SettingsForm;
use crate::app::views::welcome::WelcomePage;
use crate::app::model::ObjectTarget;
use crate::app::{AppPage, CommandSpec, Density, Effect, FocusTarget, Locale, Shell, ThemePreference, COMMANDS};
use crate::application::tasks::TaskToken;
use crate::application::{AppServices, TaskBoard};
use crate::connections::model::{CaMode, Engine, SshAuthKind};
use crate::app::views::connections::LinkStatus;
use crate::app::views::query::object_spec;
use crate::catalog::hierarchy::{NavKind, NavNode, NavRow, NavTree, ObjectKey};
use crate::catalog::{NavAction, NavConnection};
use crate::i18n;

use super::nav::{self, NavRequest};
use crate::query::assist::TreeObject;
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

/// 一个可切换、可关闭、可恢复的工作项类型。持久化字符串沿用旧标签 kind，
/// 保证已有 app-state.json 仍可读取。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkItemKind {
    Query,
    Table,
    Structure,
    Admin,
    Redis,
    Mongo,
}

impl WorkItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Table => "table",
            Self::Structure => "structure",
            Self::Admin => "database",
            Self::Redis => "keys",
            Self::Mongo => "docs",
        }
    }

    pub fn stored(text: &str) -> Option<Self> {
        match text {
            "query" => Some(Self::Query),
            "table" => Some(Self::Table),
            "structure" | "definition" => Some(Self::Structure),
            "database" => Some(Self::Admin),
            "keys" | "redis" => Some(Self::Redis),
            "docs" | "mongo" => Some(Self::Mongo),
            _ => None,
        }
    }

    pub fn tool(self) -> WorkTool {
        match self {
            Self::Query => WorkTool::Query,
            Self::Table => WorkTool::Table,
            Self::Structure => WorkTool::Structure,
            Self::Admin => WorkTool::Admin,
            Self::Redis => WorkTool::Redis,
            Self::Mongo => WorkTool::Mongo,
        }
    }

    pub fn page(self) -> AppPage {
        match self {
            Self::Structure | Self::Admin => AppPage::Structure,
            Self::Query | Self::Table | Self::Redis | Self::Mongo => AppPage::Workspace,
        }
    }

    pub fn is_query(self) -> bool {
        matches!(self, Self::Query)
    }

    /// 每个工作项恢复自身时的焦点位置；返回工作项和关闭浮层都用这一份，
    /// 不再按页面猜焦点。
    pub fn focus(self) -> FocusTarget {
        match self {
            Self::Query => FocusTarget::Editor,
            Self::Table => FocusTarget::Grid,
            Self::Structure | Self::Admin => FocusTarget::Editor,
            Self::Redis | Self::Mongo => FocusTarget::Sidebar,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkItem {
    pub id: String,
    /// 单调递增的工作项实例编号；标签 id 可回收，但它不会。
    pub epoch: u64,
    pub title: String,
    pub kind: WorkItemKind,
    /// 表 / 结构 / Redis / MongoDB 的目标规格；查询项为空。
    pub spec: Option<String>,
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

impl WorkItem {
    fn query(id: String, title: String, draft: String) -> Self {
        Self {
            id,
            epoch: next_epoch(),
            title,
            kind: WorkItemKind::Query,
            spec: None,
            draft,
            last_executed_sql: None,
            uncommitted_cells: false,
            cell_note: String::new(),
            running: false,
            connection_id: None,
            connection_name: None,
            database: None,
            engine: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    /// 资源管理器里的对象名筛选；与连接列表的 `Filter` 分开，互不覆盖。
    ObjectFilter,
    HistorySearch,
    SaveName,
    RenameName,
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
    RenameSaved,
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
    pub tabs: Vec<WorkItem>,
    pub active_tab: Option<String>,
    pub connections: ConnectionPage,
    pub history: HistoryPage,
    pub welcome: WelcomePage,
    pub menu: Option<OpenMenu>,
    pub menu_return: FocusTarget,
    pub close_prompt: Option<ClosePrompt>,
    /// 批量关闭（其他/右侧/全部）待处理的工作项；确认后继续，取消即停止。
    pub close_batch: Vec<String>,
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
    /// 资源管理器对象区的筛选词。只匹配对象名，不动连接列表的 `Filter`。
    pub object_filter: String,
    /// 最近一次从查询页同步的对象索引；资源管理器、对象筛选和快速打开共用这一份。
    pub catalog: Vec<TreeObject>,
    /// 最近一次对象读取的错误；与空目录、筛选无命中分开呈现。
    pub catalog_error: String,
    /// 真实层级树：库/模式/对象类型/对象/列与索引，带局部失败与续读状态。
    pub nav: NavTree,
    /// 整个目录读取失败的原因；单节点失败留在节点自己身上。
    pub nav_error: String,
    /// 模型只排队异步读取，由视图发起并回填。
    pub nav_requests: VecDeque<NavRequest>,
    /// 正在读取的目录任务数；空目录与“还在读”必须能分辨。
    pub nav_loading: u32,
    /// 最近一次完整读取结果；单条连接重试时用它保住其它连接。
    pub nav_source: Vec<NavConnection>,
    /// 对象树菜单右键的那一行；菜单动作只认它，绝不回落到别的对象。
    pub nav_menu_node: Option<String>,
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
    /// 已经发起的更新检查次数；关闭自动检查时不增加，也不启动任何网络任务。
    pub update_checks: u32,
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
            close_batch: Vec::new(),
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
            object_filter: String::new(),
            catalog: Vec::new(),
            catalog_error: String::new(),
            nav: NavTree::new(),
            nav_error: String::new(),
            nav_requests: VecDeque::new(),
            nav_loading: 0,
            nav_source: Vec::new(),
            nav_menu_node: None,
            exchange_open: false,
            exchange_running: false,
            compare_running: false,
            diagram_spec: None,
            redis_spec: None,
            mongo_spec: None,
            redis_running: false,
            mongo_running: false,
            update_checks: 0,
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
        let page_before = self.shell.page;
        if let Some(effect) = self.shell.dispatch(id) {
            self.after_shell(id, page_before);
            return Some(effect);
        }
        if self.local(id) {
            return Some(Effect::None);
        }
        self.commands.pop();
        None
    }

    fn after_shell(&mut self, id: &str, page_before: AppPage) {
        match id {
            "new-query" => self.open_blank_query(),
            "new-connection" => {
                self.connections.start_new(Engine::Postgres);
                self.focused_field = Some(DraftField::Name);
            }
            "connections" => self.welcome.has_connections = !self.connections.rows.is_empty(),
            "history" => {
                self.history.section = HistorySection::Runs;
                self.history.cancel_rename();
                self.history.confirm_delete = false;
            }
            "saved-queries" => {
                self.history.section = HistorySection::Saved;
                self.history.filter_status.clear();
                self.history.cancel_rename();
                self.history.confirm_delete = false;
                self.shell.page = AppPage::History;
            }
            "open-exchange" => {
                self.exchange_open = !self.exchange_open;
            }
            "close-tab" => self.request_close_active(),
            "next-tab" => self.cycle_item(1),
            "prev-tab" => self.cycle_item(-1),
            "execute-query" | "run-current" | "run-selection" => self.executions += 1,
            "command-palette" | "quick-open" => {
                self.palette_query.clear();
                self.palette_index = 0;
                self.menu = None;
            }
            "help" | "shortcut-help" => self.menu = None,
            "theme-light" | "theme-dark" | "theme-system" => self.settings.capture_shell(&self.shell),
            "open-structure" => {
                if let Some(spec) = self.object_spec.clone() {
                    self.open_item(WorkItemKind::Structure, spec);
                } else {
                    self.notice_key = Some("shell.tabs.needObject");
                    self.notice_detail.clear();
                    self.shell.page = page_before;
                }
            }
            _ => {}
        }
    }

    fn local(&mut self, id: &str) -> bool {
        if (id == "nav-refresh" || id.starts_with("nav-")) && self.nav_command(id) {
            return true;
        }
        if let Some(spec) = id.strip_prefix("open-table:") {
            self.open_item(WorkItemKind::Table, spec.to_owned());
            return true;
        }
        if let Some(spec) = id.strip_prefix("open-structure-for:") {
            self.open_item(WorkItemKind::Structure, spec.to_owned());
            return true;
        }
        if let Some(spec) = id.strip_prefix("open-redis:") {
            self.open_item(WorkItemKind::Redis, spec.to_owned());
            return true;
        }
        if let Some(spec) = id.strip_prefix("open-mongo:") {
            self.open_item(WorkItemKind::Mongo, spec.to_owned());
            return true;
        }
        if let Some(connection_id) = id.strip_prefix("connection:") {
            self.connections.selected = Some(connection_id.to_owned());
            self.shell.page = AppPage::Connections;
            return true;
        }
        if id == "exchange-object" {
            // 对象菜单里的导入导出以被选中的对象为准：先把这个对象打开成表数据，
            // 抽屉只在实际来源与所选对象一致时打开，避免导出别一张表。
            let Some(target) = self.object_target() else {
                self.notice_key = Some("shell.tabs.needObject");
                self.notice_detail.clear();
                return true;
            };
            if !target.is_relational() {
                // 文档集合没有表格导入导出；说明原因，不改当前工作项。
                self.notice_key = Some("catalog.tree.relationalOnly");
                self.notice_detail = target.name.clone();
                return true;
            }
            let spec = target.spec();
            if self.active_tab().and_then(|tab| tab.spec.clone()).as_deref() != Some(spec.as_str()) {
                self.open_item(WorkItemKind::Table, spec.clone());
            }
            if self.active_tab().and_then(|tab| tab.spec.as_deref()) == Some(spec.as_str()) {
                self.exchange_open = true;
            }
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
                    // 回到工作台就是回到当前工作项：页面、工具与焦点都还原，
                    // 不重建草稿、暂存、选择与滚动。
                    self.return_to_work();
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
            "null-next" => self.settings.cycle_null(),
            "null-prev" => self.settings.cycle_null_step(-1),
            "timeout-cycle" => self.settings.cycle_timeout(),
            "timeout-prev" => self.settings.cycle_timeout_step(-1),
            "timeout-0" => self.settings.query_timeout_sec = 0,
            "timeout-15" => self.settings.query_timeout_sec = 15,
            "timeout-60" => self.settings.query_timeout_sec = 60,
            "null-NULL" | "null-empty" | "null-N" => self.settings.null_copy = id.trim_start_matches("null-").to_owned(),
            "page-size-up" => self.settings.step_page_size(50),
            "page-size-down" => self.settings.step_page_size(-50),
            "preview-up" => self.settings.step_preview_kb(1),
            "preview-down" => self.settings.step_preview_kb(-1),
            "object-page-up" => self.settings.step_object_page(50),
            "object-page-down" => self.settings.step_object_page(-50),
            "tree-open-data"
            | "tree-copy-name"
            | "tree-select-sql"
            | "tree-insert-object"
            | "tree-insert-select"
            | "tree-insert-column"
            | "tree-copy-column"
            | "tree-new-query" => self.tree_action(id),
            "open-admin" => {
                if let Some(spec) = self.table_spec.clone().or_else(|| self.object_spec.clone()) {
                    self.open_item(WorkItemKind::Admin, spec);
                } else {
                    self.notice_key = Some("shell.tabs.needObject");
                    self.notice_detail.clear();
                }
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
            "history-detail" => self.history.preview_open = true,
            "history-preview-close" => self.history.preview_open = false,
            "history-filter-connection" => self.history.filter_selected_connection(),
            "history-filter-reset" => self.history.reset_filters(),
            "history-status-all" => self.history.set_status_filter("all"),
            "history-status-ok" => self.history.set_status_filter("ok"),
            "history-status-failed" => self.history.set_status_filter("failed"),
            "history-refresh" => self.enqueue(PendingJob::RefreshHistory),
            "history-save-named" => {
                if self.history.save_name.trim().is_empty() {
                    self.notice_key = Some("history.page.needName");
                    self.notice_detail.clear();
                } else if self.active_tab().is_some_and(|tab| tab.kind.is_query()) {
                    self.enqueue(PendingJob::SaveNamed);
                } else {
                    self.notice_key = Some("history.savedPanel.needTab");
                    self.notice_detail.clear();
                }
            }
            "history-clear" => {
                if self.history.runs.is_empty() {
                    self.history.confirm_clear = false;
                    self.notice_key = Some("ui.history.empty");
                    self.notice_detail.clear();
                } else if !self.history.confirm_clear {
                    self.history.confirm_clear = true;
                    self.notice_key = Some("history.page.clearConfirm");
                    self.notice_detail.clear();
                } else {
                    self.history.confirm_clear = false;
                    self.enqueue(PendingJob::ClearHistory);
                }
            }
            "history-delete" => {
                if self.history.selected_run.is_some() {
                    self.enqueue(PendingJob::DeleteRun);
                } else {
                    self.notice_key = Some("ui.history.delete");
                    self.notice_detail.clear();
                }
            }
            "saved-delete" => {
                if self.history.selected_saved.is_none() {
                    self.notice_key = Some("ui.history.delete");
                    self.notice_detail.clear();
                } else if !self.history.confirm_delete {
                    self.history.confirm_delete = true;
                    self.notice_key = Some("history.page.deleteSavedConfirm");
                    self.notice_detail.clear();
                } else {
                    self.history.confirm_delete = false;
                    self.enqueue(PendingJob::DeleteSaved);
                }
            }
            "history-clear-cancel" => {
                self.history.confirm_clear = false;
                self.notice_key = None;
                self.notice_detail.clear();
            }
            "saved-delete-cancel" => {
                self.history.confirm_delete = false;
                self.notice_key = None;
                self.notice_detail.clear();
            }
            "history-rename" => {
                if self.history.begin_rename() {
                    self.focused_field = Some(DraftField::RenameName);
                    self.notice_key = None;
                    self.notice_detail.clear();
                } else {
                    self.notice_key = Some("history.savedPanel.openNote");
                    self.notice_detail.clear();
                }
            }
            "history-rename-confirm" => {
                if self.history.rename_request().is_some() {
                    self.enqueue(PendingJob::RenameSaved);
                } else {
                    self.notice_key = Some("history.page.needName");
                    self.notice_detail.clear();
                }
            }
            "history-rename-cancel" => {
                self.history.cancel_rename();
                self.notice_key = None;
                self.notice_detail.clear();
            }
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
                self.relabel_query_tabs();
            }
            "locale-en" => {
                self.settings.locale = "en".into();
                self.settings.apply_shell(&mut self.shell);
                self.relabel_query_tabs();
            }
            "font-up" => self.settings.step_editor_font(1),
            "font-down" => self.settings.step_editor_font(-1),
            "editor-theme-prev" => self.settings.cycle_editor_theme_step(-1),
            "toggle-updates" => self.settings.toggle_updates(),
            "check-updates" => {
                self.begin_update_check();
            }
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
        // 对象动作只认完整目标：连接、库、模式、对象名一起来自同一行。
        // 右键菜单记住被点的那一行，键盘用焦点行；都不借上一张表。
        let menu_node = self.nav_menu_node.clone();
        let candidate = menu_node.clone().or_else(|| self.nav.focus().map(str::to_owned));
        if let Some(node_id) = candidate.as_deref().and_then(|node| self.tree_action_node(id, node)) {
            if let Some(command) = self.tree_action_command(id, &node_id) {
                let _ = self.nav_command(&command);
            }
            self.nav_menu_node = None;
            return;
        }
        if let Some(node_id) = menu_node {
            // 右键的那一行不支持这个动作：说明原因，绝不作用到别的对象。
            let name = self.nav.node(&node_id).map(|node| node.name.clone()).unwrap_or_default();
            self.nav_menu_node = None;
            self.notice_key = Some("catalog.tree.actionUnavailable");
            self.notice_detail = name;
            return;
        }
        match id {
            "tree-copy-name" => match self.object_target() {
                Some(target) => self.copied = Self::qualified_name(&target),
                None => {
                    self.copied.clear();
                    self.notice_key = Some("shell.tabs.needObject");
                    self.notice_detail.clear();
                }
            },
            "tree-insert-object" | "tree-insert-select" | "tree-insert-column" | "tree-copy-column" => {
                self.notice_key = Some("shell.tabs.needObject");
                self.notice_detail.clear();
            }
            "tree-select-sql" => match self.object_target() {
                Some(target) => {
                    // 只写草稿并带上该对象的连接/库，不执行任何语句。
                    let sql = crate::query::assist::browse_sql(&target.engine, &target.schema, &target.name);
                    self.open_bound_query(
                        sql,
                        Some(target.connection_id.clone()),
                        Some(target.connection_name.clone()),
                        Some(target.database.clone()),
                        Some(target.engine.clone()),
                    );
                }
                None => {
                    self.notice_key = Some("shell.tabs.needObject");
                    self.notice_detail.clear();
                }
            },
            "tree-open-data" => {
                self.notice_key = Some("catalog.tree.openData");
                if let Some(spec) = self.object_spec.clone() {
                    self.open_item(WorkItemKind::Table, spec);
                }
            }
            _ => {}
        }
    }

    /// 这一行是否支持这个对象动作；不支持就返回 None，绝不借别的目标。
    fn tree_action_node(&self, id: &str, node_id: &str) -> Option<String> {
        let node = self.nav.node(node_id)?;
        let supported = match id {
            "tree-open-data" | "tree-insert-object" | "tree-insert-select" | "tree-select-sql" => {
                node.kind.is_object() && !node.stale
            }
            "tree-copy-column" | "tree-insert-column" => matches!(node.kind, NavKind::Column | NavKind::Index),
            "tree-copy-name" => {
                !node.stale && matches!(node.kind, NavKind::Object | NavKind::Database | NavKind::Schema)
            }
            "tree-new-query" => !node.connection_id.is_empty() && !node.stale,
            _ => false,
        };
        supported.then(|| node_id.to_owned())
    }

    /// 把菜单动作换成绑定这一行的导航命令。
    fn tree_action_command(&mut self, id: &str, node_id: &str) -> Option<String> {
        match id {
            "tree-open-data" => Some(format!("nav-open-data:{node_id}")),
            "tree-copy-name" => Some(format!("nav-copy-name:{node_id}")),
            "tree-copy-column" => Some(format!("nav-copy-column:{node_id}")),
            "tree-insert-object" => Some(format!("nav-insert-object:{node_id}")),
            "tree-insert-select" => Some(format!("nav-insert-select:{node_id}")),
            "tree-insert-column" => {
                let column = self.nav.node(node_id).map(|node| node.name.clone()).unwrap_or_default();
                let object = self
                    .nav
                    .node(node_id)
                    .and_then(|node| node.parent.as_deref())
                    .and_then(|group| self.nav.node(group))
                    .and_then(|group| group.parent.clone())
                    .unwrap_or_default();
                Some(format!("nav-insert-column:{object}\u{1}{column}"))
            }
            "tree-select-sql" => Some(format!("nav-select-sql:{node_id}")),
            "tree-new-query" => Some(format!("nav-new-query:{node_id}")),
            _ => None,
        }
    }

    /// 复制用的限定名：模式与对象名，忽略展示用的连接名。
    fn qualified_name(target: &ObjectTarget) -> String {
        if target.schema.is_empty() {
            target.name.clone()
        } else {
            format!("{}.{}", target.schema, target.name)
        }
    }

    fn open_diagram_from_object(&mut self) {
        let Some(target) = self.object_target() else {
            self.shell.page = AppPage::Diagram;
            self.diagram_spec = None;
            return;
        };
        if !target.is_relational() {
            self.notice_key = Some("diagram.unsupported");
            self.notice_detail.clear();
            return;
        }
        if let Some(spec) = crate::app::views::diagram::spec_from_object(&target.spec()) {
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
        // 只有对象树右键会设置 nav_menu_node；其它菜单先清掉上一轮的行。
        self.nav_menu_node = None;
        self.menu_return = self.shell.focus;
        self.menu = Some(OpenMenu::new(place, target));
        self.shell.focus = FocusTarget::Palette;
    }

    /// 对象树右键：菜单只属于被点的那一行，动作绑定这一行的身份。
    pub fn open_nav_menu(&mut self, node_id: &str) -> bool {
        let Some(node) = self.nav.node(node_id).cloned() else { return false };
        self.nav.set_focus(node_id);
        self.sync_object_spec_from_focus();
        let target = tree_menu_target(&node);
        self.open_menu(MenuPlace::ObjectTree, target);
        self.nav_menu_node = Some(node_id.to_owned());
        true
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

    /// 标签右键：先激活目标项，再打开菜单；视图、目标与菜单基准保持一致。
    pub fn tab_menu_for(&mut self, id: &str) -> bool {
        if !self.activate_item(id) {
            return false;
        }
        self.open_tab_menu();
        true
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
        // 菜单关掉就不再持着右键的那一行，之后的动作回到键盘焦点或当前工作项。
        self.nav_menu_node = None;
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
                detail: self.command_detail(command.id),
            });
        }
        for row in &self.connections.rows {
            entries.push(PaletteEntry {
                id: format!("connection:{}", row.saved.id),
                group: "connection",
                label_key: "shell.paletteGroup.connection",
                label: row.saved.name.clone(),
                detail: String::new(),
            });
        }
        entries.extend(self.catalog_entries());
        filter_palette(&entries, &self.palette_query, self.shell.palette_tables_only)
    }

    /// 命令面板里“对比”“导入导出”要带上当前对象或结果；没有上下文时这一行说明缺什么。
    fn command_detail(&self, id: &str) -> String {
        match id {
            "compare" => self.compare_detail(),
            "open-exchange" => self.exchange_detail(),
            _ => String::new(),
        }
    }

    /// 记录一次对象读取结果。内容没变就不重新分配，渲染每帧同步也不会抖动。
    /// 返回 true 表示索引确实变了。
    pub fn set_catalog(&mut self, objects: &[TreeObject], error: &str) -> bool {
        if self.catalog.as_slice() == objects && self.catalog_error == error {
            return false;
        }
        self.catalog = objects.to_vec();
        self.catalog_error = error.to_owned();
        true
    }


    /// 把一次目录读取结果并进层级树；展开、焦点和已经读到的列/索引保留。
    /// partial=true 时只替换同 id 的那条连接，其它连接的已读内容不动。
    pub fn apply_navigation(&mut self, connections: &[NavConnection], partial: bool) -> bool {
        self.nav_loading = self.nav_loading.saturating_sub(1);
        self.nav_error.clear();
        if partial {
            for connection in connections {
                match self.nav_source.iter_mut().find(|item| item.id == connection.id) {
                    Some(slot) => *slot = connection.clone(),
                    None => self.nav_source.push(connection.clone()),
                }
            }
        } else {
            self.nav_source = connections.to_vec();
        }
        let snapshot = self.nav_source.clone();
        let changed = self.nav.apply(&snapshot);
        self.sync_object_spec_from_focus();
        if self.nav.focus().is_none() {
            self.nav.focus_first();
            self.sync_object_spec_from_focus();
        }
        changed
    }

    /// 整目录读取失败：保留上一份树，只把原因放在顶部提示条。
    pub fn nav_failed(&mut self, message: impl Into<String>) {
        self.nav_loading = self.nav_loading.saturating_sub(1);
        self.nav_error = message.into();
    }

    pub fn request_nav_refresh(&mut self, connection: Option<String>) {
        self.nav_loading = self.nav_loading.saturating_add(1);
        self.nav_requests.push_back(NavRequest::Refresh { connection });
    }

    pub fn request_nav_details(&mut self, node_id: &str) {
        if self.nav.begin_details(node_id) {
            self.nav_requests.push_back(NavRequest::Details(node_id.to_owned()));
        }
    }

    pub fn take_nav_request(&mut self) -> Option<NavRequest> {
        self.nav_requests.pop_front()
    }

    pub fn nav_node(&self, id: &str) -> Option<&NavNode> {
        self.nav.node(id)
    }

    pub fn nav_rows(&self) -> Vec<NavRow> {
        self.nav.visible()
    }

    /// 一条连接下面的树行，深度从连接下一层开始，供资源管理器缩进使用。
    pub fn nav_rows_for(&self, connection_id: &str) -> Vec<NavRow> {
        let root = crate::catalog::hierarchy::connection_node_id(connection_id);
        self.nav
            .visible()
            .into_iter()
            .filter(|row| row.depth > 0)
            .filter(|row| {
                // 只保留真正挂在这条连接下的行：父链必须经过连接节点。
                self.nav
                    .node(&row.id)
                    .and_then(|node| node.parent.as_deref())
                    .map(|parent| self.nav_within(parent, &root))
                    .unwrap_or(false)
            })
            .map(|row| NavRow {
                depth: row.depth.saturating_sub(1),
                ..row
            })
            .collect()
    }

    /// 资源管理器连接行的展开状态就存在树上：鼠标、键盘和渲染读同一份。
    pub fn nav_connection_expanded(&self, connection_id: &str) -> bool {
        self.nav
            .is_expanded(&crate::catalog::hierarchy::connection_node_id(connection_id))
    }

    /// 记下展开意图：节点还没读进树时也保留，下一次读取后直接展开。
    pub fn expand_nav_connection(&mut self, connection_id: &str) {
        let id = crate::catalog::hierarchy::connection_node_id(connection_id);
        self.nav.expand_reserved(&id);
    }

    /// 鼠标 chevron 或双击连接行：展开与折叠都落在树上。
    pub fn toggle_nav_connection(&mut self, connection_id: &str) -> bool {
        let id = crate::catalog::hierarchy::connection_node_id(connection_id);
        if self.nav.is_expanded(&id) {
            self.nav.collapse(&id);
        } else {
            self.nav.expand_reserved(&id);
        }
        self.nav.is_expanded(&id)
    }

    /// 双击树行：容器展开/折叠，对象打开自己的数据或文档页。
    pub fn nav_toggle_command(&mut self, node_id: &str) -> Option<String> {
        let action = self.nav.activate_or_toggle(node_id);
        let command = self.nav_activation_command(action);
        self.sync_object_spec_from_focus();
        command
    }

    fn nav_within(&self, parent: &str, root: &str) -> bool {
        let mut current = parent.to_owned();
        loop {
            if current == root {
                return true;
            }
            let Some(node) = self.nav.node(&current) else { return false };
            match node.parent.clone() {
                Some(next) => current = next,
                None => return false,
            }
        }
    }

    /// 搜索：完整路径 + 高亮由树给出；清除后恢复原来的展开与选择。
    pub fn nav_search(&mut self, text: &str) -> bool {
        let changed = self.nav.search(text);
        if !text.trim().is_empty() {
            self.sync_object_spec_from_focus();
        }
        changed
    }

    pub fn nav_clear_search(&mut self) -> bool {
        self.nav.clear_search()
    }

    fn sync_object_spec_from_focus(&mut self) {
        let Some(id) = self.nav.focus().map(str::to_owned) else { return };
        let Some(node) = self.nav.node(&id).cloned() else { return };
        match node.kind {
            NavKind::Object | NavKind::Column | NavKind::Index => {
                let object_node = if node.kind == NavKind::Object {
                    Some(node.clone())
                } else {
                    node.parent
                        .as_deref()
                        .and_then(|parent| self.nav.node(parent))
                        .filter(|parent| parent.kind == NavKind::Object)
                        .or_else(|| {
                            node.parent
                                .as_deref()
                                .and_then(|group| self.nav.node(group))
                                .and_then(|group| group.parent.as_deref())
                                .and_then(|object| self.nav.node(object))
                        })
                        .cloned()
                };
                if let Some(object_node) = object_node {
                    let environment = self.nav_environment(&object_node.connection_id);
                    if let Some(spec) = nav::spec_for(&object_node, &environment) {
                        self.object_spec = Some(spec);
                    }
                }
            }
            _ => {}
        }
    }

    /// 单击/回车：树自己处理展开，其余转成外壳命令。
    pub fn nav_activate_command(&mut self, node_id: &str) -> Option<String> {
        let action = self.nav.activate(node_id);
        let command = self.nav_activation_command(action);
        self.sync_object_spec_from_focus();
        command
    }

    /// 资源管理器对象区的空态：读取失败、还在读、目录为空、搜索无命中必须能分辨。
    pub fn nav_empty_key(&self, connection_id: &str) -> Option<&'static str> {
        if self.nav.node(&crate::catalog::hierarchy::connection_node_id(connection_id))
            .and_then(|node| node.error.as_ref())
            .is_some()
            || (!self.nav_error.is_empty() && self.nav.roots().is_empty())
        {
            return Some("ui.explorer.error");
        }
        if self.nav_loading > 0 {
            return Some("catalog.tree.loading");
        }
        if !self.object_filter.trim().is_empty() {
            return Some("ui.explorer.noMatch");
        }
        Some("ui.explorer.noObjects")
    }

    /// 搜索命中行（完整路径）与普通树行分开渲染。
    pub fn nav_searching(&self) -> bool {
        !self.object_filter.trim().is_empty()
    }

    fn nav_environment(&self, connection_id: &str) -> String {
        self.connections
            .rows
            .iter()
            .find(|row| row.saved.id == connection_id)
            .map(|row| row.saved.env.clone())
            .unwrap_or_default()
    }

    fn nav_target(&self, node_id: &str) -> Option<ObjectKey> {
        let node = self.nav.node(node_id)?;
        if node.stale {
            return None;
        }
        match node.kind {
            NavKind::Column | NavKind::Index => node
                .parent
                .as_deref()
                .and_then(|parent| self.nav.node(parent))
                .and_then(|group| group.parent.as_deref())
                .and_then(|object| self.nav.node(object))
                .and_then(|object| object.object()),
            _ => node.object(),
        }
    }

    /// 键盘在树上移动与展开；返回需要外壳继续执行的命令。
    pub fn nav_key(&mut self, key: NavKey) -> Option<String> {
        let rows = self.nav.visible();
        if rows.is_empty() {
            return None;
        }
        let action = match key {
            NavKey::ArrowDown => {
                self.nav.move_focus(1);
                None
            }
            NavKey::ArrowUp => {
                self.nav.move_focus(-1);
                None
            }
            NavKey::Home => {
                self.nav.focus_first();
                None
            }
            NavKey::End => {
                self.nav.focus_last();
                None
            }
            NavKey::Enter => {
                let id = self.nav.focus()?.to_owned();
                let action = self.nav.activate(&id);
                Some(self.nav_activation_command(action).unwrap_or_else(|| format!("nav-focus:{id}")))
            }
            NavKey::Escape => None,
        };
        self.sync_object_spec_from_focus();
        action
    }

    /// 右方向键展开/打开；左方向键折叠/回到父节点。
    pub fn nav_step(&mut self, forward: bool) -> Option<String> {
        let id = self.nav.focus()?.to_owned();
        let action = if forward { self.nav.activate(&id) } else { self.nav.retreat(&id) };
        let command = self.nav_activation_command(action);
        self.sync_object_spec_from_focus();
        command
    }

    fn nav_activation_command(&mut self, action: NavAction) -> Option<String> {
        match action {
            NavAction::None | NavAction::FocusParent => None,
            NavAction::Load(id) => {
                // 展开已经由树自己完成；对象节点这时按需读列/索引，别的层级不额外发命令。
                if self.nav.node(&id).map(|node| node.kind) == Some(NavKind::Object) {
                    self.request_nav_details(&id);
                }
                None
            }
            NavAction::OpenData(id) => Some(format!("nav-open-data:{id}")),
            NavAction::OpenStructure(id) => Some(format!("nav-open-structure:{id}")),
            NavAction::OpenKeyspace { connection_id, keyspace } => {
                Some(format!("nav-open-keyspace:{connection_id}\u{1}{keyspace}"))
            }
            NavAction::OpenDocuments(id) => Some(format!("nav-open-docs:{id}")),
            NavAction::InsertColumn { object_id, column } => {
                Some(format!("nav-insert-column:{object_id}\u{1}{column}"))
            }
        }
    }

    /// 对象/列/索引动作的规格。菜单、键盘和右键都从这里取同一份身份。
    fn nav_spec(&self, node_id: &str) -> Option<String> {
        let node = self.nav.node(node_id)?;
        let object = match node.kind {
            NavKind::Column | NavKind::Index => {
                let group = node.parent.as_deref().and_then(|parent| self.nav.node(parent))?;
                let object = group.parent.as_deref().and_then(|object| self.nav.node(object))?;
                object.clone()
            }
            _ => node.clone(),
        };
        let environment = self.nav_environment(&object.connection_id);
        nav::spec_for(&object, &environment)
    }

    /// 把文本并入当前查询草稿。没有查询项时新建一条草稿，照旧不执行。
    fn insert_into_draft(&mut self, text: String) -> bool {
        if text.is_empty() {
            return false;
        }
        let Some(tab_id) = self
            .active_tab()
            .filter(|tab| tab.kind.is_query())
            .map(|tab| tab.id.clone())
        else {
            // 没有活动查询项就开一条带这段文本的草稿；照旧不执行。
            self.open_query_draft(text, false);
            self.shell.page = AppPage::Workspace;
            self.shell.focus = FocusTarget::Editor;
            return true;
        };
        if let Some(tab) = self.tabs.iter_mut().find(|item| item.id == tab_id) {
            tab.draft = nav::append_text(&tab.draft, &text);
            self.shell.page = AppPage::Workspace;
            self.shell.focus = FocusTarget::Editor;
            return true;
        }
        false
    }

    fn nav_command(&mut self, id: &str) -> bool {
        if id == "nav-refresh" {
            self.request_nav_refresh(None);
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-retry:") {
            let Some(node) = self.nav.node(rest) else { return true };
            let kind = node.kind;
            let connection_id = node.connection_id.clone();
            let has_kind = node.object_kind.is_some();
            match kind {
                NavKind::Connection => self.request_nav_refresh(Some(connection_id)),
                NavKind::Object => self.request_nav_details(rest),
                // 种类分组只在失败的那一类上重试；加载态也只留在这个分组。
                NavKind::Group if has_kind => {
                    if self.nav.begin_group(rest) {
                        self.nav_requests.push_back(NavRequest::Group(rest.to_owned()));
                    }
                }
                // 库/模式整段重试仍按范围续读，别把下一页错挂到分组下面。
                _ => {
                    let scope = self.nav_storage_scope(rest);
                    self.nav.begin_scope(&scope);
                    self.nav_requests.push_back(NavRequest::More(scope));
                }
            }
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-more:") {
            let scope = self.nav_storage_scope(rest);
            if self.nav.node(&scope).is_some_and(|node| node.loading) {
                return true;
            }
            self.nav.begin_scope(&scope);
            self.nav_requests.push_back(NavRequest::More(scope));
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-toggle:") {
            if self.nav.toggle(rest) && self.nav.needs_details(rest) {
                self.request_nav_details(rest);
            }
            self.nav.set_focus(rest);
            self.sync_object_spec_from_focus();
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-focus:") {
            self.nav.set_focus(rest);
            self.sync_object_spec_from_focus();
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-open-data:") {
            let Some(node) = self.nav.node(rest).cloned() else {
                // 节点已经不在树上（刷新后删除）：保留选择并说明原因。
                self.notice_key = Some("catalog.tree.staleObject");
                self.notice_detail.clear();
                return true;
            };
            let Some(object) = self.nav_target(rest) else {
                self.notice_key = Some("catalog.tree.staleObject");
                self.notice_detail.clear();
                return true;
            };
            if node.engine == Engine::Mongodb {
                // 集合是文档：走集合浏览器，绝不开关系型表格页。
                let connection_name = self.nav_connection_name(&node.connection_id);
                let spec = nav::mongo_spec(&node.connection_id, &connection_name, &node.database, &node.name);
                self.open_item(WorkItemKind::Mongo, spec);
                return true;
            }
            if !object.opens_data() {
                // 例程、触发器、序列等没有行数据，不能误开数据页。
                self.notice_key = Some("catalog.tree.noData");
                self.notice_detail = node.name.clone();
                return true;
            }
            if let Some(spec) = self.nav_spec(rest) {
                self.open_item(WorkItemKind::Table, spec);
            }
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-open-structure:") {
            if self.nav.node(rest).is_some_and(|node| node.engine == Engine::Mongodb) {
                // 集合没有 SQL 结构；文档在集合浏览器里看。
                self.notice_key = Some("catalog.tree.noStructure");
                self.notice_detail = self.nav.node(rest).map(|node| node.name.clone()).unwrap_or_default();
                return true;
            }
            if let Some(spec) = self.nav_spec(rest) {
                self.open_item(WorkItemKind::Structure, spec);
            } else {
                self.notice_key = Some("catalog.tree.staleObject");
                self.notice_detail.clear();
            }
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-open-diagram:") {
            self.nav.set_focus(rest);
            self.sync_object_spec_from_focus();
            self.open_diagram_from_object();
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-copy-name:") {
            // 库/模式行复制自己的名字；对象行仍按引擎层级写限定名。
            let text = match self.nav.node(rest).map(|node| (node.kind, node.name.clone(), node.stale)) {
                Some((NavKind::Database | NavKind::Schema, name, false)) => Some(name),
                Some((NavKind::Object, _, false)) => self.nav_target(rest).map(|object| nav::qualified_name(&object)),
                _ => None,
            };
            match text {
                Some(text) => self.copied = text,
                None => {
                    self.copied.clear();
                    self.notice_key = Some("catalog.tree.staleObject");
                    self.notice_detail.clear();
                }
            }
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-copy-column:") {
            match self.nav.node(rest) {
                Some(node) if !node.stale => self.copied = node.name.clone(),
                _ => {
                    self.copied.clear();
                    self.notice_key = Some("catalog.tree.staleObject");
                    self.notice_detail.clear();
                }
            }
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-insert-object:") {
            match self.nav_target(rest) {
                Some(object) => {
                    self.insert_into_draft(nav::insert_object_text(&object));
                }
                None => {
                    self.notice_key = Some("catalog.tree.staleObject");
                    self.notice_detail.clear();
                }
            }
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-insert-select:") {
            match self.nav_target(rest) {
                Some(object) => {
                    self.insert_into_draft(nav::select_text(&object));
                }
                None => {
                    self.notice_key = Some("catalog.tree.staleObject");
                    self.notice_detail.clear();
                }
            }
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-insert-column:") {
            // 对象 id 本身含分隔符，列名取最后一段。
            let (object_id, column) = rest.rsplit_once('\u{1}').unwrap_or((rest, ""));
            let column = if column.is_empty() {
                self.nav.node(object_id).map(|node| node.name.clone()).unwrap_or_default()
            } else {
                column.to_owned()
            };
            if self.nav.node(object_id).is_none() || column.is_empty() {
                self.notice_key = Some("catalog.tree.staleObject");
                self.notice_detail.clear();
                return true;
            }
            self.insert_into_draft(nav::insert_column_text(&column));
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-open-keyspace:") {
            let (connection_id, keyspace) = rest.rsplit_once('\u{1}').unwrap_or((rest, "0"));
            let name = self.nav_connection_name(connection_id);
            self.open_item(WorkItemKind::Redis, nav::redis_spec(connection_id, &name, keyspace));
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-open-docs:") {
            let Some(node) = self.nav.node(rest) else { return true };
            let connection_name = self.nav_connection_name(&node.connection_id);
            let database = node.database.clone();
            let collection = node.name.clone();
            self.open_item(
                WorkItemKind::Mongo,
                nav::mongo_spec(&node.connection_id, &connection_name, &database, &collection),
            );
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-new-query:") {
            // 空白查询绑定这一行的连接/库；库、模式、分组行也能用，不写 SQL、不执行。
            let node = self.nav.node(rest).filter(|node| !node.stale).map(|node| {
                (node.connection_id.clone(), node.connection_name.clone(), node.database.clone(), node.engine)
            });
            match node.filter(|(connection_id, ..)| !connection_id.is_empty()) {
                Some((connection_id, connection_name, database, engine)) => {
                    self.open_bound_query(
                        String::new(),
                        Some(connection_id),
                        Some(connection_name),
                        Some(database),
                        Some(engine.as_str().to_owned()),
                    );
                }
                None => {
                    self.notice_key = Some("catalog.tree.staleObject");
                    self.notice_detail.clear();
                }
            }
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-select-sql:") {
            match self.nav_target(rest) {
                Some(object) => {
                    let sql = nav::select_text(&object);
                    let target = self.nav.node(rest).and_then(|node| node.object());
                    let (database, schema) = target
                        .map(|object| (object.database, object.schema))
                        .unwrap_or_default();
                    self.open_bound_query(
                        sql,
                        Some(object.connection_id.clone()),
                        Some(object.connection_name.clone()),
                        Some(database),
                        Some(object.engine.as_str().to_owned()),
                    );
                    let _ = schema;
                }
                None => {
                    self.notice_key = Some("catalog.tree.staleObject");
                    self.notice_detail.clear();
                }
            }
            return true;
        }
        false
    }

    /// 对象列表真正按页读取的范围：往上找到库/模式/连接节点。
    fn nav_storage_scope(&self, node_id: &str) -> String {
        let mut current = node_id.to_owned();
        loop {
            let Some(node) = self.nav.node(&current) else { return node_id.to_owned() };
            match node.kind {
                NavKind::Group | NavKind::Column | NavKind::ColumnGroup | NavKind::Index | NavKind::IndexGroup => {
                    match node.parent.clone() {
                        Some(parent) => current = parent,
                        None => return node_id.to_owned(),
                    }
                }
                _ => return current,
            }
        }
    }

    fn nav_connection_name(&self, connection_id: &str) -> String {
        self.connections
            .rows
            .iter()
            .find(|row| row.saved.id == connection_id)
            .map(|row| row.saved.name.clone())
            .or_else(|| {
                // 连接列表还没同步时，用目录快照里这条连接的显示名。
                self.nav_source
                    .iter()
                    .find(|item| item.id == connection_id)
                    .map(|item| item.name.clone())
            })
            .unwrap_or_else(|| connection_id.to_owned())
    }

    /// 快速打开里的真实表 / 视图。label 用对象名，保证搜索匹配的是对象；
    /// 库、模式与连接放在 detail 里，不干扰排序。
    pub fn catalog_entries(&self) -> Vec<PaletteEntry> {
        self.catalog
            .iter()
            // 断开、删除或连接失败的旧对象不再冒充“当前可打开”；
            // 状态未知时保留（离线读取与测试），状态为已连接时仍要求连接记录存在。
            .filter(|object| {
                let known = self
                    .connections
                    .rows
                    .iter()
                    .any(|row| row.saved.id == object.connection_id);
                match self.connections.status.get(&object.connection_id).copied() {
                    Some(LinkStatus::Connected) => known,
                    Some(_) => false,
                    None => true,
                }
            })
            .filter(|object| is_table_object(object))
            .map(|object| {
                let environment = self
                    .connections
                    .rows
                    .iter()
                    .find(|row| row.saved.id == object.connection_id)
                    .map(|row| row.saved.env.clone())
                    .unwrap_or_default();
                PaletteEntry {
                    id: format!("open-table:{}", object_spec(object, &environment)),
                    group: "table",
                    label_key: "shell.paletteGroup.table",
                    label: object.name.clone(),
                    detail: object_detail(object),
                }
            })
            .collect()
    }

    /// 某一连接下已读取的对象，按对象名做不区分大小写的包含匹配。
    /// 对象树展示索引里的全部关系型对象；当前目录读取只产出表与视图，索引层
    /// 不丢弃其他 kind，留给后续结构工具接入。快速打开再收窄到表与视图。
    pub fn catalog_for(&self, connection_id: &str, filter: &str) -> Vec<&TreeObject> {
        let needle = filter.trim().to_lowercase();
        self.catalog
            .iter()
            .filter(|object| object.connection_id == connection_id)
            .filter(|object| relational_engine(&object.engine))
            .filter(|object| needle.is_empty() || object.name.to_lowercase().contains(&needle))
            .collect()
    }

    /// 连接是否有任何已读取的对象（不受对象筛选影响），用来区分空目录与筛选无命中。
    pub fn has_objects(&self, connection_id: &str) -> bool {
        !self.catalog_for(connection_id, "").is_empty()
    }

    /// 资源管理器按真实元数据分组；筛选词来自独立的 object_filter。
    pub fn object_groups(&self, connection_id: &str) -> Vec<ObjectGroup> {
        let objects = self.catalog_for(connection_id, &self.object_filter);
        group_objects(&objects)
    }

    /// 对象区空态文案：读取失败、目录为空、筛选无命中必须能分辨。
    pub fn object_notice(&self, connection_id: &str) -> Option<&'static str> {
        if !self.object_groups(connection_id).is_empty() {
            return None;
        }
        if !self.catalog_error.is_empty() {
            return Some("ui.explorer.error");
        }
        if !self.has_objects(connection_id) {
            return Some("ui.explorer.noObjects");
        }
        Some("ui.explorer.noMatch")
    }

    /// 快速打开的空态文案；只在筛选结果为空时调用。
    pub fn palette_empty_key(&self) -> &'static str {
        if !self.catalog_error.is_empty() {
            return "ui.palette.error";
        }
        if self.shell.palette_tables_only {
            if self.catalog_entries().is_empty() {
                return "ui.palette.noObjects";
            }
            return "ui.explorer.noMatch";
        }
        "ui.palette.empty"
    }

    pub fn facts(&self) -> CloseFacts {
        let running = self.running_jobs > 0 || self.exchange_running || self.compare_running || self.redis_running || self.mongo_running || self.tabs.iter().any(|tab| tab.running);
        let uncommitted = self.grid_dirty || self.tabs.iter().any(|tab| tab.uncommitted_cells);
        let draft = self.tabs.iter().any(|tab| draft_pending(tab.kind.as_str(), &tab.draft, tab.last_executed_sql.as_deref()));
        CloseFacts { running, uncommitted, draft }
    }

    pub fn tab_facts(&self, id: &str) -> CloseFacts {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) else {
            return CloseFacts { running: false, uncommitted: false, draft: false };
        };
        CloseFacts {
            running: tab.running,
            uncommitted: tab.uncommitted_cells,
            draft: draft_pending(tab.kind.as_str(), &tab.draft, tab.last_executed_sql.as_deref()),
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

    /// 更新检查的唯一入口。关闭自动检查时直接返回 false，不发起网络检查；
    /// 开启时只在这里记账，真正的检查任务由调用方在返回 true 后启动。
    pub fn begin_update_check(&mut self) -> bool {
        if !self.settings.auto_check_updates {
            return false;
        }
        self.update_checks = self.update_checks.saturating_add(1);
        true
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
        self.close_batch.clear();
    }

    fn choose_leave(&mut self) {
        let Some(prompt) = self.close_prompt.take() else { return };
        match prompt.scope {
            CloseScope::Tab => {
                if let Some(id) = prompt.tab_id {
                    self.remove_tab(&id);
                    self.close_batch.retain(|queued| queued != &id);
                    self.close_next_queued();
                }
            }
            CloseScope::Window => self.release_close = true,
        }
    }

    fn remove_tab(&mut self, id: &str) {
        let index = self.tabs.iter().position(|tab| tab.id == id);
        let removed = self.tabs.iter().find(|tab| tab.id == id).cloned();
        self.tabs.retain(|tab| tab.id != id);
        self.relabel_query_tabs();
        if self.active_tab.as_deref() != Some(id) {
            // 关闭的不是当前项：只有它正好被当前工具显示时才重新绑定视图。
            if let Some(removed) = removed {
                if self.view_shows(&removed) {
                    let page = self.shell.page;
                    self.rebind_to_active();
                    if !matches!(page, AppPage::Workspace | AppPage::Structure) {
                        self.shell.page = page;
                    }
                }
            }
            return;
        }
        // 桌面习惯：优先激活右侧邻居，其次左侧；都带未提交暂存时先不切换。
        let next = self.pick_neighbour(index);
        self.active_tab = None;
        let page = self.shell.page;
        match next {
            Some((next, kind, spec)) => {
                self.show_item(&next, kind, spec);
                // 用户可能正停在设置/历史/对比页，只在工作页上跟随标签切换。
                if !matches!(page, AppPage::Workspace | AppPage::Structure) {
                    self.shell.page = page;
                }
            }
            None => {
                self.reset_work_view();
                if page == AppPage::Structure {
                    self.shell.page = AppPage::Workspace;
                }
                if !self.tabs.is_empty() {
                    self.notice_key = Some("shell.tabs.pendingCells");
                }
            }
        }
    }

    /// 关闭当前项后选一个不会清空暂存网格的邻居；优先右邻居，其次左邻居，
    /// 都不安全时退回仍持有暂存的那张表，避免用一次关闭操作丢掉未提交修改。
    fn pick_neighbour(&self, index: Option<usize>) -> Option<(String, WorkItemKind, Option<String>)> {
        let index = index?;
        let candidate = |index: usize| {
            self.tabs
                .get(index)
                .map(|tab| (tab.id.clone(), tab.kind, tab.spec.clone()))
        };
        let mut candidates = Vec::new();
        for offset in [index, index.saturating_sub(1)] {
            if let Some(item) = candidate(offset) {
                candidates.push(item);
            }
        }
        for (id, kind, spec) in candidates {
            if !self.switch_blocked(kind, spec.as_deref()) {
                return Some((id, kind, spec));
            }
        }
        self.tabs
            .iter()
            .find(|tab| tab.kind == WorkItemKind::Table && tab.uncommitted_cells)
            .map(|tab| (tab.id.clone(), tab.kind, tab.spec.clone()))
    }

    fn view_shows(&self, item: &WorkItem) -> bool {
        if self.tool != item.kind.tool() {
            return false;
        }
        match item.kind {
            WorkItemKind::Table | WorkItemKind::Structure | WorkItemKind::Admin => {
                self.table_spec.is_some() && self.table_spec == item.spec
            }
            WorkItemKind::Redis => self.redis_spec.is_some() && self.redis_spec == item.spec,
            WorkItemKind::Mongo => self.mongo_spec.is_some() && self.mongo_spec == item.spec,
            WorkItemKind::Query => false,
        }
    }

    fn rebind_to_active(&mut self) {
        let active = self.active_tab.clone();
        match active {
            Some(id) => {
                let target = self
                    .tabs
                    .iter()
                    .find(|tab| tab.id == id)
                    .map(|tab| (tab.id.clone(), tab.kind, tab.spec.clone()));
                if let Some((id, kind, spec)) = target {
                    self.show_item(&id, kind, spec);
                }
            }
            None => self.reset_work_view(),
        }
    }

    fn reset_work_view(&mut self) {
        self.tool = WorkTool::Query;
        self.table_spec = None;
        self.redis_spec = None;
        self.mongo_spec = None;
        self.shell.focus = FocusTarget::Editor;
    }

    fn close_others(&mut self) {
        let Some(id) = self.active_tab.clone() else { return };
        let others: Vec<String> = self.tabs.iter().filter(|tab| tab.id != id).map(|tab| tab.id.clone()).collect();
        self.close_sequence(others);
    }

    fn close_right(&mut self) {
        let Some(id) = self.active_tab.clone() else { return };
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else { return };
        let right: Vec<String> = self.tabs.iter().skip(index + 1).map(|tab| tab.id.clone()).collect();
        self.close_sequence(right);
    }

    fn request_close_all(&mut self) {
        let ids: Vec<String> = self.tabs.iter().map(|tab| tab.id.clone()).collect();
        self.close_sequence(ids);
    }

    /// 逐项关闭：需要确认时停下；确认后从断点继续，运行中的项保留在原位。
    fn close_sequence(&mut self, ids: Vec<String>) {
        self.close_batch = ids;
        self.close_next_queued();
    }

    fn close_next_queued(&mut self) {
        while let Some(id) = self.close_batch.first().cloned() {
            match evaluate(self.tab_facts(&id)) {
                CloseKind::Allow => {
                    self.close_batch.remove(0);
                    self.remove_tab(&id);
                }
                CloseKind::BlockedRunning => {
                    self.close_batch.remove(0);
                    self.notice_key = message_key(CloseScope::Tab, CloseKind::BlockedRunning);
                }
                kind => {
                    self.notice_key = message_key(CloseScope::Tab, kind);
                    self.close_prompt = Some(ClosePrompt { scope: CloseScope::Tab, tab_id: Some(id), kind });
                    return;
                }
            }
        }
    }

    /// 打开或聚焦一个异构工作项；同一目标重复打开只激活已有项，不复制状态。
    pub fn open_item(&mut self, kind: WorkItemKind, spec: String) {
        if let Some(existing) = self
            .tabs
            .iter()
            .find(|item| item.kind == kind && item.spec.as_deref() == Some(spec.as_str()))
        {
            let id = existing.id.clone();
            let _ = self.activate_item(&id);
            return;
        }
        if self.switch_blocked(kind, Some(spec.as_str())) {
            self.notice_key = Some("shell.tabs.pendingCells");
            self.notice_detail.clear();
            return;
        }
        let title = self.item_title(kind, &spec);
        let id = self.next_item_id(kind);
        let epoch = next_epoch();
        // 目标规格开头几段就是连接、库和引擎；把它一并带进工作项，状态栏与关闭保护看到同一目标。
        let parts: Vec<&str> = spec.split('|').collect();
        let part = |index: usize| {
            parts
                .get(index)
                .map(|text| text.trim().to_owned())
                .filter(|text| !text.is_empty())
        };
        let (connection_id, connection_name, database, engine) = (part(0), part(1), part(2), part(5));
        self.tabs.push(WorkItem {
            id: id.clone(),
            epoch,
            title,
            kind,
            spec: Some(spec),
            draft: String::new(),
            last_executed_sql: None,
            uncommitted_cells: false,
            cell_note: String::new(),
            running: false,
            connection_id,
            connection_name,
            database,
            engine,
        });
        let _ = self.activate_item(&id);
    }

    /// 当前表格尚有暂存单元格时，切到另一个表格类工作项会清空共享网格，
    /// 因此先要求提交或撤销，避免静默丢修改。
    fn switch_blocked(&self, kind: WorkItemKind, spec: Option<&str>) -> bool {
        let Some(staged) = self
            .tabs
            .iter()
            .find(|item| item.kind == WorkItemKind::Table && item.uncommitted_cells)
            .and_then(|item| item.spec.clone())
        else {
            return false;
        };
        // 只有打开另一张表会重新加载共享网格；结构/管理页不碰网格数据。
        kind == WorkItemKind::Table && spec != Some(staged.as_str())
    }

    /// 激活工作项并把视图、工具与目标一起切到该工作项。
    /// 返回 false 表示被未提交的表格暂存拦下，调用方不应再改变标签选择。
    pub fn activate_item(&mut self, id: &str) -> bool {
        let Some((kind, spec)) = self
            .tabs
            .iter()
            .find(|item| item.id == id)
            .map(|item| (item.kind, item.spec.clone()))
        else {
            return false;
        };
        if self.active_tab.as_deref() != Some(id) && self.switch_blocked(kind, spec.as_deref()) {
            self.notice_key = Some("shell.tabs.pendingCells");
            self.notice_detail.clear();
            return false;
        }
        self.show_item(id, kind, spec);
        true
    }

    fn show_item(&mut self, id: &str, kind: WorkItemKind, spec: Option<String>) {
        self.active_tab = Some(id.to_owned());
        self.tool = kind.tool();
        self.shell.page = kind.page();
        self.shell.focus = kind.focus();
        match kind {
            WorkItemKind::Table | WorkItemKind::Structure | WorkItemKind::Admin => self.table_spec = spec,
            WorkItemKind::Redis => self.redis_spec = spec,
            WorkItemKind::Mongo => self.mongo_spec = spec,
            WorkItemKind::Query => {
                // 查询项不继承上一个表/专用页面的目标，避免 open-admin、导出等消费者取到旧目标。
                self.table_spec = None;
                self.redis_spec = None;
                self.mongo_spec = None;
            }
        }
    }

    fn cycle_item(&mut self, delta: isize) {
        if self.tabs.is_empty() {
            return;
        }
        let index = self
            .active_tab
            .as_deref()
            .and_then(|id| self.tabs.iter().position(|tab| tab.id == id))
            .unwrap_or(0);
        let len = self.tabs.len() as isize;
        let next = (index as isize + delta).rem_euclid(len) as usize;
        let id = self.tabs[next].id.clone();
        let _ = self.activate_item(&id);
    }

    fn next_item_id(&self, kind: WorkItemKind) -> String {
        let mut serial = self.tabs.len() + 1;
        loop {
            let id = format!("{}-{serial}", kind.as_str());
            if !self.tabs.iter().any(|item| item.id == id) {
                return id;
            }
            serial += 1;
        }
    }

    fn item_title(&self, kind: WorkItemKind, spec: &str) -> String {
        let field = |index: usize| spec.split('|').nth(index).unwrap_or("").trim().to_owned();
        match kind {
            WorkItemKind::Query => String::new(),
            WorkItemKind::Table | WorkItemKind::Structure => {
                let name = field(4);
                if name.is_empty() {
                    field(1)
                } else {
                    name
                }
            }
            WorkItemKind::Admin => self.text("ui.tool.admin"),
            WorkItemKind::Redis | WorkItemKind::Mongo => field(1),
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
        let id = self.next_item_id(WorkItemKind::Query);
        // 标签标题是界面文案，不是用户数据；按当前语言生成并在语言切换时重排。
        self.tabs.push(WorkItem::query(id.clone(), String::new(), draft));
        self.relabel_query_tabs();
        self.activate_item(&id);
        if !keep_page {
            self.shell.page = AppPage::Workspace;
        }
    }

    /// 查询标签按当前语言的“查询 N / Query N”重新编号；表、结构等项保留对象名。
    fn relabel_query_tabs(&mut self) {
        let template = i18n::text(self.locale(), "ui.query.tabTitle");
        let mut ordinal = 0;
        for tab in &mut self.tabs {
            if tab.kind.is_query() {
                ordinal += 1;
                tab.title = i18n::fill(&template, &[("n", &ordinal.to_string())]);
            }
        }
    }

    pub fn active_tab(&self) -> Option<&WorkItem> {
        let id = self.active_tab.as_deref()?;
        self.tabs.iter().find(|tab| tab.id == id)
    }

    /// 对象树里选中的对象目标。菜单动作以它为准，避免被活动工作项的旧目标顶掉。
    pub fn object_target(&self) -> Option<ObjectTarget> {
        self.object_spec.as_deref().and_then(ObjectTarget::parse)
    }

    /// 当前对象：优先活动工作项自己的目标，其次对象树里的选择。
    pub fn current_object(&self) -> Option<ObjectTarget> {
        self.active_tab()
            .and_then(|tab| tab.spec.as_deref())
            .and_then(ObjectTarget::parse)
            .or_else(|| self.object_target())
    }

    /// 只有连接/库、没有具体对象时的范围（查询项就是这样）。
    pub fn active_scope(&self) -> Option<ObjectTarget> {
        let tab = self.active_tab()?;
        let connection_id = tab.connection_id.clone()?;
        if connection_id.is_empty() {
            return None;
        }
        Some(ObjectTarget::new(
            connection_id,
            tab.connection_name.clone().unwrap_or_default(),
            tab.database.clone().unwrap_or_default(),
            String::new(),
            String::new(),
            tab.engine.clone().unwrap_or_default(),
            String::new(),
        ))
    }

    /// 对比入口带入的目标：优先当前对象，其次当前连接/库范围。
    pub fn compare_target(&self) -> Option<ObjectTarget> {
        self.current_object().or_else(|| self.active_scope())
    }

    /// 命令面板“对比”条目要说明的上下文；缺失条件也要说清楚。
    pub fn compare_detail(&self) -> String {
        match self.compare_target() {
            Some(target) if !target.name.is_empty() => {
                i18n::fill(&self.text("compare.context.carried"), &[("path", &target.path())])
            }
            Some(target) => target.path(),
            None => self.text("compare.context.none"),
        }
    }

    /// 交换/导入导出条目的来源说明：查询项是结果，表格/结构等项用它的对象；
    /// 抽屉实际按活动工作项导出，所以这里也只看活动工作项，并说明缺什么。
    pub fn exchange_detail(&self) -> String {
        let Some(tab) = self.active_tab() else {
            return self.text("shell.palette.exchange.none");
        };
        if tab.kind.is_query() {
            return self.text("shell.paletteGroup.queryResult");
        }
        match tab.spec.as_deref().and_then(ObjectTarget::parse) {
            Some(target) => target.path(),
            None => self.text("shell.palette.exchange.none"),
        }
    }

    /// 从对比、设置等页面回到当前工作项：页面、工具与焦点跟着工作项走，
    /// 草稿、暂存、选择与滚动都不重建。
    pub fn return_to_work(&mut self) -> bool {
        let Some(id) = self.active_tab.clone() else {
            self.shell.page = AppPage::Workspace;
            self.shell.focus = FocusTarget::Editor;
            return false;
        };
        self.activate_item(&id)
    }

    fn active_mut(&mut self) -> Option<&mut WorkItem> {
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
            DraftField::ObjectFilter => {
                // 资源管理器搜索与对象索引是同一份：显示完整路径并高亮，清除后恢复树。
                self.object_filter = value;
                let query = self.object_filter.clone();
                self.nav_search(&query);
            }
            DraftField::HistorySearch => self.history.filter_text = value,
            DraftField::SaveName => self.history.save_name = value,
            DraftField::RenameName => self.history.rename_text = value,
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

    pub(crate) fn field_value(&self, field: DraftField) -> String {
        match field {
            DraftField::Password => self.connections.secrets.password.clone(),
            DraftField::SshPassword => self.connections.secrets.ssh_password.clone(),
            DraftField::Passphrase => self.connections.secrets.ssh_passphrase.clone(),
            DraftField::BundlePassphrase => self.bundle_passphrase.clone(),
            DraftField::Url => self.url_buffer.clone(),
            DraftField::Filter => self.connections.filter_text.clone(),
            DraftField::ObjectFilter => self.object_filter.clone(),
            DraftField::HistorySearch => self.history.filter_text.clone(),
            DraftField::SaveName => self.history.save_name.clone(),
            DraftField::RenameName => self.history.rename_text.clone(),
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
            "ui.query.tabTitle",
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
            AppPage::History => vec![
                "history.page.runs",
                "history.page.saved",
                "history.page.loadToEditor",
                "history.savedPanel.save",
                "history.page.noMatch",
                "history.page.noMatchSaved",
                "history.page.rename",
                "history.page.needName",
                "history.status.ok",
                "history.status.failed",
                "history.filter.all",
                "history.filter.reset",
            ],
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
                "shell.settings.objectPage",
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

    /// 文案按实际内容宽度衡量：侧栏停靠时页面只有 `content_width` 可用，
    /// 不能拿整窗宽度代替内容区，否则窄窗口里的溢出会被放过。
    pub fn copy_overflow(&self, locale: i18n::Locale) -> i32 {
        let layout = self.layout();
        let mut overflow = layout.overflow;
        let limit = layout.content_width.max(160);
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
        self.tabs = session.tabs.iter().filter_map(session_to_item).collect();
        self.relabel_query_tabs();
        self.tool = WorkTool::Query;
        self.table_spec = None;
        self.redis_spec = None;
        self.mongo_spec = None;
        let active = session
            .active_id
            .clone()
            .filter(|id| self.tabs.iter().any(|tab| &tab.id == id))
            .or_else(|| self.tabs.first().map(|tab| tab.id.clone()));
        self.active_tab = None;
        if let Some(id) = active {
            self.activate_item(&id);
        }
    }

    pub fn secrets(&self) -> &Secrets {
        &self.connections.secrets
    }
}

fn tab_to_session(tab: &WorkItem) -> SessionTab {
    SessionTab {
        id: tab.id.clone(),
        title: tab.title.clone(),
        kind: tab.kind.as_str().to_owned(),
        spec: tab.spec.clone(),
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

fn session_to_item(tab: &SessionTab) -> Option<WorkItem> {
    let kind = WorkItemKind::stored(&tab.kind)?;
    let spec = tab.spec.clone().filter(|spec| !spec.trim().is_empty());
    // 没有目标规格的表、结构或专用页面无法安全恢复；宁可少恢复，也不显示空壳工作项。
    if !kind.is_query() && spec.is_none() {
        return None;
    }
    Some(WorkItem {
        id: tab.id.clone(),
        epoch: next_epoch(),
        title: tab.title.clone(),
        kind,
        spec,
        draft: tab.draft.clone(),
        last_executed_sql: tab.last_executed_sql.clone(),
        uncommitted_cells: false,
        cell_note: String::new(),
        running: false,
        connection_id: tab.connection_id.clone(),
        connection_name: tab.connection_name.clone(),
        database: tab.database.clone(),
        engine: tab.engine.clone(),
    })
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
    /// 搜索与显示都用它；对象条目是对象名，命令和连接是各自的文案。
    pub label: String,
    /// 补充说明，例如对象的“库.模式 · 连接”；没有时为空。
    pub detail: String,
}

/// 对象树一行的菜单范围：对象按能否开数据页分，列/索引只给自己的动作，
/// 库/模式/种类分组是范围行，不提供对象动作。
fn tree_menu_target(node: &NavNode) -> MenuTarget {
    match node.kind {
        NavKind::Column | NavKind::Index => MenuTarget::Column,
        NavKind::Object => match node.object() {
            Some(object) if object.engine == Engine::Mongodb => MenuTarget::DocumentObject,
            Some(object) if object.opens_data() => MenuTarget::Object,
            Some(_) => MenuTarget::NonDataObject,
            None => MenuTarget::Scope,
        },
        _ => MenuTarget::Scope,
    }
}

/// 资源管理器里的一个层级组：同一连接下相同的 (database, schema)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectGroup {
    pub database: String,
    pub schema: String,
    pub objects: Vec<TreeObject>,
}

/// Redis / MongoDB 的对象不是关系型表，不能参与表层级或“打开表”。
pub fn relational_engine(engine: &str) -> bool {
    !matches!(engine.to_ascii_lowercase().as_str(), "redis" | "mongodb" | "mongo")
}

/// 只有真实表 / 视图才进入对象索引。
pub fn is_table_object(object: &TreeObject) -> bool {
    relational_engine(&object.engine) && matches!(object.kind.as_str(), "table" | "view")
}

/// 按 (database, schema) 分组，保持对象原有顺序；非关系型对象不产生层级。
/// 渲染时同一 database 只显示一次标题，分组本身仍按模式拆分。
pub fn group_objects(objects: &[&TreeObject]) -> Vec<ObjectGroup> {
    let mut groups: Vec<ObjectGroup> = Vec::new();
    for object in objects {
        // 资源管理器保留全部关系型对象；表 / 视图的收窄只发生在快速打开。
        if !relational_engine(&object.engine) {
            continue;
        }
        match groups
            .iter_mut()
            .find(|group| group.database == object.database && group.schema == object.schema)
        {
            Some(group) => group.objects.push((*object).clone()),
            None => groups.push(ObjectGroup {
                database: object.database.clone(),
                schema: object.schema.clone(),
                objects: vec![(*object).clone()],
            }),
        }
    }
    groups
}

/// 对象条目的补充说明：“库.模式 · 连接”；缺哪段就少哪段。
pub fn object_detail(object: &TreeObject) -> String {
    let mut scope = object.database.clone();
    if !object.schema.is_empty() {
        if !scope.is_empty() {
            scope.push('.');
        }
        scope.push_str(&object.schema);
    }
    match (scope.is_empty(), object.connection_name.is_empty()) {
        (true, _) => object.connection_name.clone(),
        (false, true) => scope,
        (false, false) => format!("{scope} · {}", object.connection_name),
    }
}

pub fn next_epoch() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn filter_palette(entries: &[PaletteEntry], query: &str, tables_only: bool) -> Vec<PaletteEntry> {
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
