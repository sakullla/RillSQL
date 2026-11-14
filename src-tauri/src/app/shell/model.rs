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
use crate::connections::groups::{layout_groups, validate_group_name, ConnectionGroup, GroupLayout};
use crate::connections::model::{CaMode, Engine, EnvTag, SshAuthKind};
use crate::app::views::connections::LinkStatus;
use crate::app::views::query::object_spec;
use crate::catalog::hierarchy::{NavKind, NavNode, NavRow, NavTree, ObjectKey};
use crate::catalog::{NavAction, NavConnection};
use crate::i18n;

use super::danger::{DangerKind, DangerPrompt, DangerStep};
use super::nav::{self, NavRequest, TemplateKind};
use crate::query::assist::TreeObject;
use crate::state::{AppState, Platform, SessionState, SessionTab, ShellSettings, StateError};

use super::close::{draft_pending, evaluate, message_key, update_block_key, CloseFacts, CloseKind, CloseScope};
use super::conn_modal::{ConnectAction, ConnectIntent, ConnectionModal, ModalControl};
use super::explorer::{focus_rows, ExplorerItem, ExplorerRow};
use super::layout::{
    clamp_history_preview, clamp_sidebar, layout_with_sidebar, painted_frame_with_sidebar, unbreakable_px, ChromeLayout, PaintedFrame, HISTORY_PREVIEW_DEFAULT,
    SIDEBAR_DEFAULT,
};
use super::menus::{flatten_items, tree_menu_items, MenuPlace, MenuTarget, NavKey, ObjectFacts, OpenMenu};

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
    /// 危险操作确认层：生产环境要求输入的对象名。
    DangerConfirm,
    /// 危险操作确认层：重命名的新名字。
    DangerNewName,
    /// 分组名输入面板：新建分组与重命名共用。
    GroupName,
}

/// 对象树“查看 / 复制 DDL”的读取状态。`copy_only` 时不显示查看器，读到就写剪贴板。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DdlView {
    pub epoch: u64,
    pub node_id: String,
    pub object: ObjectKey,
    pub signature: String,
    pub relation: String,
    pub copy_only: bool,
    pub loading: bool,
    pub text: Option<String>,
    pub rebuilt: bool,
    pub missing: Vec<String>,
    pub error: Option<String>,
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
    /// 只重读分组与成员关系（启动恢复、导入之后）。
    RefreshGroups,
    /// 一次分组命令：新建/重命名、删除、归组、折叠。
    GroupOp,
}

/// 一次分组变更。拖拽、右键和键盘都排同一条命令，落库只走一条路径。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupOp {
    /// id 为空表示新建；名称已在这里校验过。
    Save { id: Option<String>, name: String },
    Delete { id: String },
    /// group 为空表示移回顶层。
    Assign { connection: String, group: Option<String> },
    Collapsed { id: String, collapsed: bool },
}

/// 分组名输入框：新建与重命名共用一个小面板，不另开模态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupPrompt {
    /// 被重命名的分组；新建时为空。
    pub id: Option<String>,
    pub text: String,
    /// 校验失败的提示（空名、超长名）。
    pub error: Option<String>,
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
    /// 跳转定义选中的树节点：下一帧对象树把它滚进可见区，滚过就清掉。
    pub reveal_node: Option<String>,
    /// 键盘移动到的资源管理器行（group:/conn:/nav: 前缀）：下一帧滚进视口一次。
    pub reveal_row: Option<String>,
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
    /// “刷新此节点”按整条连接重读时：连接 id → 被点的节点。
    /// 读取失败时保留原子树并把原因挂在这个节点上，而不是用空结果重建。
    pub nav_node_refresh: std::collections::BTreeMap<String, String>,
    /// 最近一次从编辑器收回的光标/选区：(查询项, 起, 止)，字节偏移。SQL 模板按它插入。
    pub draft_cursor: Option<(String, usize, usize)>,
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
    /// 对象定义查看器（或一次“复制 DDL”的读取）。
    pub ddl_view: Option<DdlView>,
    /// 对象级危险操作的确认层。
    pub danger: Option<DangerPrompt>,
    /// 等列读完再插入的 SQL 模板：(对象节点, 种类)。
    pub pending_template: Option<(String, TemplateKind)>,
    /// 要写进系统剪贴板的文本；视图取走后写入。
    pub clipboard_out: Option<String>,
    /// 对象树侧栏宽度，默认 234，范围 200–480。
    pub sidebar_width: f32,
    pub sidebar_dragging: bool,
    /// 历史预览宽度，默认 360，范围 260–520，并排时列表至少留 240。
    pub history_preview_width: f32,
    pub history_dragging: bool,
    pub viewport_height: i32,
    /// 自定义环境标签。列表、欢迎页、状态栏和表单按 id 取名称与色调。
    pub env_tags: Vec<EnvTag>,
    /// 用户自定义分组（名称、顺序、折叠），来自配置库而不是会话快照。
    pub groups: Vec<ConnectionGroup>,
    /// 连接 id → 分组 id；没有条目就是未分组，不写哨兵值。
    pub group_members: std::collections::BTreeMap<String, String>,
    /// 分组名输入面板：新建与重命名共用。
    pub group_prompt: Option<GroupPrompt>,
    /// 右键的那一个分组；菜单动作只认它。
    pub group_menu: Option<String>,
    /// 等确认删除的分组：只删分组记录，连接回顶层。
    pub group_delete: Option<String>,
    /// 键盘当前停在资源管理器哪一行（分组 / 连接 / 树节点），虚拟化后按稳定 id 走。
    pub explorer_current: Option<ExplorerRow>,
    /// 排队的一次分组命令，随任务交给后台。
    pub pending_group_op: Option<GroupOp>,
    /// 对象树“新建表”：下一帧交给结构页 `begin_create`。
    pub pending_create: Option<String>,
    /// 从历史预览保存时钉住这一条，避免误存当前查询草稿。
    pub preview_save: Option<crate::query::SaveRequest>,
    /// 连接页引擎/环境筛选下拉。点按钮展开，点选项才改筛选值。
    pub conn_filter_menu: Option<ConnFilterMenu>,
    /// 新建/编辑连接的全局模态：启动台、命令搜索、对象树空态与连接行操作共用这一个生命周期。
    pub conn_modal: ConnectionModal,
    /// 等待连接成功的续接动作。连接只在真正需要时才建立，连上后按顺序重放一次。
    pub connect_intents: Vec<ConnectIntent>,
    /// 上一次连接失败时还挂着的动作：重试按钮重放它，不会丢掉用户想打开的对象。
    pub connect_retry: Option<ConnectIntent>,
    /// “保存并连接”刚存下的那次输入机密（连接 id + 机密）：紧接着的 OpenConnection
    /// 用它连接，而不是用被 close_connection_modal 清空后的空机密。连上或重新打开表单即清。
    pub pending_open_secrets: Option<(String, Secrets)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnFilterMenu {
    Engine,
    Env,
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
            reveal_node: None,
            reveal_row: None,
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
            nav_node_refresh: std::collections::BTreeMap::new(),
            draft_cursor: None,
            exchange_open: false,
            exchange_running: false,
            compare_running: false,
            diagram_spec: None,
            redis_spec: None,
            mongo_spec: None,
            redis_running: false,
            mongo_running: false,
            update_checks: 0,
            ddl_view: None,
            danger: None,
            pending_template: None,
            clipboard_out: None,
            sidebar_width: SIDEBAR_DEFAULT,
            sidebar_dragging: false,
            history_preview_width: HISTORY_PREVIEW_DEFAULT,
            history_dragging: false,
            viewport_height: 900,
            env_tags: Vec::new(),
            groups: Vec::new(),
            group_members: std::collections::BTreeMap::new(),
            group_prompt: None,
            group_menu: None,
            group_delete: None,
            explorer_current: None,
            pending_group_op: None,
            pending_create: None,
            preview_save: None,
            conn_filter_menu: None,
            conn_modal: ConnectionModal::default(),
            connect_intents: Vec::new(),
            connect_retry: None,
            pending_open_secrets: None,
        }
    }

    pub fn set_sidebar_width(&mut self, width: f32) {
        self.sidebar_width = clamp_sidebar(width);
    }

    pub fn reset_sidebar_width(&mut self) {
        self.sidebar_width = SIDEBAR_DEFAULT;
        self.sidebar_dragging = false;
    }

    pub fn set_history_preview_width(&mut self, width: f32) {
        let content = self.painted().content_width as f32;
        self.history_preview_width = clamp_history_preview(width, content);
    }

    pub fn reset_history_preview_width(&mut self) {
        self.history_preview_width = HISTORY_PREVIEW_DEFAULT;
        self.history_dragging = false;
    }

    pub fn history_preview_px(&self) -> f32 {
        let content = self.painted().content_width as f32;
        clamp_history_preview(self.history_preview_width, content)
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
        layout_with_sidebar(self.viewport_width, self.shell.sidebar_open, self.sidebar_width as i32)
    }

    pub fn painted(&self) -> PaintedFrame {
        painted_frame_with_sidebar(self.viewport_width, self.shell.sidebar_open, self.shell.density.control_padding(), self.sidebar_width as i32)
    }

    pub fn set_width(&mut self, width: i32) {
        self.viewport_width = width;
        if width <= 960 && self.shell.sidebar_open && width <= 640 {
            // 640 以下侧栏改为覆盖，仍允许打开。
        }
    }

    pub fn set_viewport(&mut self, width: i32, height: i32) {
        self.set_width(width);
        self.viewport_height = height.max(0);
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
                // 命令搜索、菜单和快捷键都到这里：打开同一个全局模态，不导航到另一个编辑页。
                self.open_connection_modal(Some(Engine::Postgres), FocusTarget::Connections, None, "new-connection");
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
            "open-admin" => self.open_admin(),
            "open-structure" => {
                let non_table = self.nav.focus_node().and_then(|node| node.object()).is_some_and(|object| !object.opens_data());
                if non_table {
                    self.notice_key = Some("catalog.tree.noStructure");
                    self.notice_detail = self.nav.focus_node().map(|node| node.name.clone()).unwrap_or_default();
                    self.shell.page = page_before;
                } else if let Some(spec) = self.object_spec.clone() {
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
        // 分组命令与连接菜单里的“移动到分组”共用同一批 id。
        if (id.starts_with("group-") || id.starts_with("conn-move-group")) && self.group_command(id) {
            return true;
        }
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
        if let Some(engine) = id.strip_prefix("conn-filter-engine:") {
            self.connections.filter_engine = if engine.is_empty() { None } else { crate::connections::model::Engine::parse(engine).ok() };
            self.conn_filter_menu = None;
            return true;
        }
        if let Some(env) = id.strip_prefix("conn-filter-env:") {
            self.connections.filter_env = env.to_owned();
            self.conn_filter_menu = None;
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
                self.shell.page = AppPage::Connections;
                self.open_connection_modal(Some(engine), FocusTarget::Connections, None, id);
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
            "conn-edit" | "conn-duplicate" | "conn-new-launcher" | "conn-new-sidebar" | "conn-modal-close" | "conn-cancel"
            | "conn-retry" | "conn-new-query" => {
                self.conn_command(id);
            },
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
                // 测试屏幕上的草稿：新建未保存的表单也能测，编辑已有连接时测的是改动后的字段。
                // 右键行"测试连接"时先把这一行载入同一个模态，绝不测配置库里的旧值。
                if self.connections.editor.is_none() || !self.conn_modal.open {
                    if let Some(selected) = self.connections.selected.clone() {
                        self.shell.page = AppPage::Connections;
                        self.edit_connection(&selected, false, FocusTarget::Connections, None, "conn-test");
                    }
                }
                if self.connections.editor.is_some() {
                    self.shell.page = AppPage::Connections;
                    self.connections.message_detail.clear();
                    self.notice_detail.clear();
                    // 字段本身不合法就不发请求：错误落在字段旁，不必等一次必然失败的探测。
                    self.connections.issues = self.connections.validate();
                    match self.connections.issues.first() {
                        Some(issue) => {
                            self.connections.message_key = None;
                            self.notice_key = Some(issue.key);
                        }
                        None => {
                            // 每次都拿到新代号：只有这一代的结果能收回指示，晚到的旧结果不行。
                            let generation = self.conn_modal.begin_test();
                            self.connections.expect_test(generation);
                            self.notice_key = self.connections.message_key;
                            self.enqueue(PendingJob::TestConnection);
                        }
                    }
                } else {
                    self.notice_key = Some("connections.editor.test.button");
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
            "conn-save" | "conn-save-connect" => {
                // 普通保存只保存；"保存并连接"保存成功后再走一次按需连接。
                self.conn_modal.save_and_connect = id == "conn-save-connect";
                self.connections.issues = self.connections.validate();
                if self.connections.editor.is_some() && self.connections.issues.is_empty() {
                    self.notice_key = None;
                    self.notice_detail.clear();
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
            "conn-filter-engine" => {
                self.conn_filter_menu = if self.conn_filter_menu == Some(ConnFilterMenu::Engine) { None } else { Some(ConnFilterMenu::Engine) };
            }
            "conn-filter-env" => {
                self.conn_filter_menu = if self.conn_filter_menu == Some(ConnFilterMenu::Env) { None } else { Some(ConnFilterMenu::Env) };
            }
            "conn-filter-clear" => {
                self.connections.clear_list_filters();
                self.conn_filter_menu = None;
            }
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
            | "tree-new-query"
            | "tree-view-ddl"
            | "tree-copy-ddl"
            | "tree-refresh-node"
            | "tree-template-insert"
            | "tree-template-update"
            | "tree-template-delete"
            | "tree-drop-object"
            | "tree-truncate-table"
            | "tree-rename-table" => self.tree_action(id),
            "ddl-close" => self.ddl_view = None,
            "ddl-copy" => {
                let text = self.ddl_view.as_ref().and_then(|view| view.text.clone());
                match text {
                    Some(text) => {
                        self.copied = text.clone();
                        self.clipboard_out = Some(text);
                        self.notice_key = Some("catalog.ddl.copied");
                        self.notice_detail.clear();
                    }
                    None => {
                        self.notice_key = Some("catalog.ddl.failed");
                        self.notice_detail.clear();
                    }
                }
            }
            "danger-cancel" => {
                // 取消或关闭只收起确认层，不发送任何语句。
                self.danger = None;
                if matches!(self.focused_field, Some(DraftField::DangerConfirm | DraftField::DangerNewName)) {
                    self.focused_field = None;
                }
            }
            "danger-preview" => {
                if let Some(prompt) = self.danger.as_mut().filter(|prompt| !prompt.running) {
                    prompt.request_preview();
                }
            }
            "danger-execute" => {
                if let Some(prompt) = self.danger.as_mut() {
                    prompt.click_execute();
                }
            }
            "status-target" => self.focus_status_target(),
            "status-jobs" => {
                self.exchange_open = true;
            }
            "tree-create-table" => self.begin_create_table(),
            "history-save-preview" => self.save_history_preview(),
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
                } else if self.active_tab().is_some_and(|tab| tab.kind.is_query() && !tab.draft.trim().is_empty())
                    || self.history.preview_plan(&self.known_ids()).is_some_and(|plan| !plan.sql.trim().is_empty())
                {
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
            "tree-view-ddl" | "tree-copy-ddl" | "tree-refresh-node" | "tree-template-insert" | "tree-template-update"
            | "tree-template-delete" | "tree-drop-object" | "tree-truncate-table" | "tree-rename-table" => {
                // 这些动作只认树上的一行；没有合适的行就说明，不借当前工作项。
                self.notice_key = Some("catalog.tree.actionUnavailable");
                self.notice_detail = self.nav.focus_node().map(|node| node.name.clone()).unwrap_or_default();
            }
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
            "tree-view-ddl" | "tree-copy-ddl" => node.object().is_some_and(|object| {
                node.kind == NavKind::Object
                    && !node.stale
                    && object.engine.is_relational()
                    && crate::catalog::definition::ddl_available(object.engine, object.kind)
            }),
            "tree-refresh-node" => matches!(
                node.kind,
                NavKind::Connection | NavKind::Database | NavKind::Schema | NavKind::Group | NavKind::Keyspace
            ),
            "tree-template-insert" | "tree-template-update" | "tree-template-delete" => node.object().is_some_and(|object| {
                node.kind == NavKind::Object && !node.stale && object.engine.is_relational() && object.opens_data()
            }),
            "tree-drop-object" | "tree-truncate-table" | "tree-rename-table" => node.object().is_some_and(|object| {
                node.kind == NavKind::Object
                    && !node.stale
                    && object.engine.is_relational()
                    && DangerKind::from_menu(id, object.kind).is_some()
            }),
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
            "tree-view-ddl" => Some(format!("nav-view-ddl:{node_id}")),
            "tree-copy-ddl" => Some(format!("nav-copy-ddl:{node_id}")),
            "tree-refresh-node" => Some(format!("nav-refresh-node:{node_id}")),
            "tree-template-insert" => Some(format!("nav-insert-template:insert:{node_id}")),
            "tree-template-update" => Some(format!("nav-insert-template:update:{node_id}")),
            "tree-template-delete" => Some(format!("nav-insert-template:delete:{node_id}")),
            "tree-drop-object" | "tree-truncate-table" | "tree-rename-table" => Some(format!("nav-danger:{id}:{node_id}")),
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

    // ------------------------------------------------------------ 连接模态与按需连接

    pub fn connection_modal_open(&self) -> bool {
        self.conn_modal.open
    }

    /// 连接模态打开时，全局快捷键不能在背景里改工作区：只放行关闭窗口。
    /// 模态自己的 Esc/Tab/回车由外壳的按键处理与 focus trap 负责，不经过这里。
    pub fn action_blocked_by_modal(&self, id: &str) -> bool {
        self.conn_modal.open && id != "quit-window"
    }

    /// 打开新建连接模态。启动台、命令搜索、对象树空态和快捷键都到这一处，
    /// 因此"新建连接"只有一份草稿与一个编辑面。
    pub fn open_connection_modal(&mut self, engine: Option<Engine>, origin: FocusTarget, control: Option<ModalControl>, trigger: &str) {
        // 重新开表单就是新一次输入：上一次“保存并连接”留下的机密不再复用。
        self.pending_open_secrets = None;
        self.connections.start_new(engine.unwrap_or(Engine::Postgres));
        self.connections.issues.clear();
        self.focused_field = Some(DraftField::Name);
        self.conn_modal.open_from(origin, control, trigger);
    }

    /// 编辑（或复制）一行配置：草稿来自这一行，屏幕上的改动不影响已保存配置直到保存成功。
    pub fn edit_connection(&mut self, id: &str, duplicate: bool, origin: FocusTarget, control: Option<ModalControl>, trigger: &str) -> bool {
        self.pending_open_secrets = None;
        self.connections.selected = Some(id.to_owned());
        if duplicate {
            self.connections.duplicate_selected();
        } else {
            self.connections.edit_selected();
        }
        if self.connections.editor.is_none() {
            return false;
        }
        self.connections.issues = self.connections.validate();
        self.connections.message_key = None;
        self.connections.message_detail.clear();
        self.focused_field = Some(DraftField::Name);
        self.conn_modal.open_from(origin, control, trigger);
        true
    }

    /// 关闭模态：不保存、不改已保存配置，草稿与本次机密一起丢掉。返回还焦目标。
    pub fn close_connection_modal(&mut self) -> Option<(FocusTarget, Option<ModalControl>)> {
        let restored = self.conn_modal.close()?;
        self.connections.editor = None;
        self.connections.secrets = Secrets::default();
        self.connections.issues.clear();
        self.connections.message_key = None;
        self.connections.message_detail.clear();
        self.connections.confirm_delete = false;
        self.shell.focus = restored.0;
        Some(restored)
    }

    fn conn_command(&mut self, id: &str) {
        match id {
            "conn-edit" | "conn-duplicate" => {
                let duplicate = id == "conn-duplicate";
                let origin = if self.shell.page == AppPage::Connections { FocusTarget::Connections } else { FocusTarget::Sidebar };
                if let Some(selected) = self.connections.selected.clone() {
                    self.shell.page = AppPage::Connections;
                    self.edit_connection(&selected, duplicate, origin, None, id);
                }
            }
            "conn-new-launcher" => {
                self.shell.page = AppPage::Connections;
                self.open_connection_modal(Some(Engine::Postgres), FocusTarget::Connections, Some(ModalControl::LauncherNew), id);
            }
            "conn-new-sidebar" => {
                self.open_connection_modal(Some(Engine::Postgres), FocusTarget::Sidebar, Some(ModalControl::SidebarNew), id);
            }
            "conn-cancel" | "conn-modal-close" => {
                let _ = self.close_connection_modal();
            }
            "conn-retry" => {
                let _ = self.retry_pending_connect();
            }
            "conn-new-query" => {
                let Some(id) = self.connections.selected.clone() else { return };
                if let Some(row) = self.connections.rows.iter().find(|row| row.saved.id == id) {
                    let name = row.saved.name.clone();
                    let engine = row.saved.engine.as_str().to_owned();
                    let database = row.saved.database.clone();
                    self.open_bound_query(String::new(), Some(id.clone()), Some(name), database, Some(engine));
                }
                // 查询草稿马上就能写；连接在后台建立，失败时重试仍回到这条连接。
                self.queue_connect(ConnectIntent::new(id, ConnectAction::Expand, None));
            }
            _ => {}
        }
    }

    /// 左侧对象的焦点/选中换成规格，供连接成功后重放动作。
    fn intent_spec(&self) -> Option<String> {
        self.object_spec.clone().or_else(|| self.active_scope().map(|target| target.spec()))
    }

    /// 这条命令是否要"先有连接"。返回待续接动作；None 表示照常执行。
    pub fn on_demand_intent(&self, id: &str) -> Option<ConnectIntent> {
        if let Some(rest) = id.strip_prefix("connection:") {
            let connection = rest.split('|').next().unwrap_or(rest);
            if self.connections.rows.iter().any(|row| row.saved.id == connection) {
                // 连接行双击/命令搜索选中：连上后进入工作区，而不是停在启动台。
                return Some(ConnectIntent::new(connection, ConnectAction::OpenWorkspace, None));
            }
            return None;
        }
        // 执行查询：活动查询项绑定的连接还没打开时，先连上再重放这次执行，
        // 而不是让查询页回答“这条连接还没有打开”（run-current/选区同一条路）。
        if crate::app::views::query::runs_query(id) {
            let tab = self.active_tab().filter(|tab| tab.kind.is_query())?;
            let connection = tab.connection_id.clone()?;
            if !self.connections.rows.iter().any(|row| row.saved.id == connection) {
                return None;
            }
            return Some(ConnectIntent::new(connection, ConnectAction::Run, None).with_replay(id));
        }
        let action = match id {
            "tree-open-data" => ConnectAction::OpenData,
            "open-structure" => ConnectAction::OpenStructure,
            "tree-new-query" => ConnectAction::NewQuery,
            _ => return None,
        };
        let spec = self.intent_spec()?;
        let target = ObjectTarget::parse(&spec)?;
        Some(ConnectIntent::new(target.connection_id, action, Some(spec)))
    }

    /// 按需连接的统一入口：已连接返回 false（调用方直接执行），否则排队并返回 true。
    pub fn gate_on_demand(&mut self, id: &str) -> bool {
        let Some(intent) = self.on_demand_intent(id) else { return false };
        if self.connections.row_status(&intent.connection_id) == LinkStatus::Connected {
            return false;
        }
        // 没有载入这条连接时不知道它的真实状态：按原样执行，不凭空发一次连接。
        if !self.connections.rows.iter().any(|row| row.saved.id == intent.connection_id) {
            return false;
        }
        self.queue_connect(intent);
        true
    }

    /// 确保这条连接可用；连上后执行一次排队的动作。重复点击复用在途连接，不重复打开。
    pub fn queue_connect(&mut self, intent: ConnectIntent) {
        let id = intent.connection_id.clone();
        if !self.connect_intents.iter().any(|queued| queued == &intent) {
            self.connect_intents.push(intent);
        }
        self.connect_retry = None;
        self.connections.selected = Some(id.clone());
        let name = self
            .connections
            .rows
            .iter()
            .find(|row| row.saved.id == id)
            .map(|row| row.saved.name.clone())
            .unwrap_or_default();
        match self.connections.row_status(&id) {
            LinkStatus::Connected => return,
            LinkStatus::Connecting => {}
            _ => {
                self.connections.status.insert(id.clone(), LinkStatus::Connecting);
                if !self.pending.iter().any(|job| matches!(job, PendingJob::OpenConnection)) {
                    self.enqueue(PendingJob::OpenConnection);
                }
            }
        }
        // 状态文字与指示点同时出现：颜色不是唯一的信息载体。
        self.notice_key = Some("ui.conn.connecting");
        self.notice_detail = name;
    }

    /// 已连上的那些动作：调用方按顺序执行一次，并从队列里移除。
    pub fn take_ready_intents(&mut self) -> Vec<ConnectIntent> {
        let ready: Vec<ConnectIntent> = self
            .connect_intents
            .iter()
            .filter(|intent| self.connections.row_status(&intent.connection_id) == LinkStatus::Connected)
            .cloned()
            .collect();
        self.connect_intents.retain(|intent| self.connections.row_status(&intent.connection_id) != LinkStatus::Connected);
        ready
    }

    /// 连接失败：动作留到重试里，不丢用户想打开的对象。
    pub fn fail_pending_intents(&mut self) {
        let failed: Vec<ConnectIntent> = self
            .connect_intents
            .iter()
            .filter(|intent| self.connections.row_status(&intent.connection_id) == LinkStatus::Failed)
            .cloned()
            .collect();
        self.connect_intents.retain(|intent| self.connections.row_status(&intent.connection_id) != LinkStatus::Failed);
        if let Some(intent) = failed.into_iter().next() {
            self.notice_key = Some("connections.status.error");
            self.notice_detail = self
                .connections
                .rows
                .iter()
                .find(|row| row.saved.id == intent.connection_id)
                .map(|row| row.saved.name.clone())
                .unwrap_or_default();
            self.connect_retry = Some(intent);
        }
    }

    /// 重试上一次失败的续接动作。
    pub fn retry_pending_connect(&mut self) -> bool {
        let Some(intent) = self.connect_retry.take() else { return false };
        self.queue_connect(intent);
        true
    }

    /// 模态里这一帧应该存在的全部 Tab 停靠点（真实 Tab 由 GPUI 的 focus trap 在容器内成环）。
    pub fn modal_stops(&self) -> Vec<super::conn_modal::ModalStop> {
        ConnectionModal::stops(&self.connections, &super::conn_modal::env_ids(&self.env_tags))
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
        let facts = Some(ObjectFacts {
            engine: node.engine,
            kind: node.object_kind.unwrap_or(crate::catalog::model::ObjectKind::Table),
        });
        self.open_menu(MenuPlace::ObjectTree, target);
        self.menu = Some(OpenMenu::with_items(MenuPlace::ObjectTree, tree_menu_items(target, facts)));
        self.nav_menu_node = Some(node_id.to_owned());
        true
    }

    pub fn open_connection_menu(&mut self) {
        self.open_menu(MenuPlace::ConnectionRow, MenuTarget::Connection);
        self.append_move_to_group();
    }

    /// 分组标题右键：菜单只属于被点的这一个分组。
    pub fn open_group_menu(&mut self, group_id: &str) -> bool {
        let Some(group) = self.group(group_id).cloned() else { return false };
        self.group_menu = Some(group.id.clone());
        self.open_menu(MenuPlace::ExplorerGroup, MenuTarget::Connection);
        self.menu = Some(OpenMenu::with_items(
            MenuPlace::ExplorerGroup,
            super::menus::group_menu_items(&group.id, group.collapsed),
        ));
        true
    }

    /// 连接菜单补一条“移动到分组”：目标写在命令里，条目 id 仍是稳定的 conn-move-group。
    fn append_move_to_group(&mut self) {
        let Some(connection) = self.connections.selected.clone() else { return };
        if !self.connections.rows.iter().any(|row| row.saved.id == connection) {
            return;
        }
        let current = self.group_of(&connection).map(str::to_owned);
        let entry = super::menus::move_to_group_branch(&connection, &self.groups, current.as_deref());
        if let Some(menu) = self.menu.as_mut() {
            menu.items.push(entry);
        }
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
        // 筛选浮层也归外点/Esc 关闭，不留一个悬着的下拉。
        self.conn_filter_menu = None;
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
        // 确认层与定义查看器：关掉就是取消，不发送任何语句。
        if self.danger.take().is_some() {
            return Effect::None;
        }
        if self.ddl_view.as_ref().is_some_and(|view| !view.copy_only) {
            self.ddl_view = None;
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
        if self.menu.is_none() {
            return None;
        }
        match key {
            NavKey::Escape | NavKey::ArrowLeft => {
                let open = self.menu.as_ref().is_some_and(|menu| menu.submenu.is_some());
                if open {
                    if let Some(menu) = self.menu.as_mut() {
                        menu.close_submenu();
                    }
                    return None;
                }
                if key == NavKey::Escape {
                    self.dismiss();
                }
                None
            }
            NavKey::ArrowRight => {
                if let Some(menu) = self.menu.as_mut() {
                    let _ = menu.open_submenu();
                }
                None
            }
            NavKey::ArrowDown => {
                if let Some(menu) = self.menu.as_mut() {
                    menu.move_by(1);
                }
                None
            }
            NavKey::ArrowUp => {
                if let Some(menu) = self.menu.as_mut() {
                    menu.move_by(-1);
                }
                None
            }
            NavKey::Home => {
                if let Some(menu) = self.menu.as_mut() {
                    menu.home();
                }
                None
            }
            NavKey::End => {
                if let Some(menu) = self.menu.as_mut() {
                    menu.end();
                }
                None
            }
            NavKey::Enter => {
                let opens = self.menu.as_ref().is_some_and(|menu| {
                    menu.submenu.is_none() && menu.active_parent().is_some_and(|item| !item.children.is_empty() && item.enabled)
                });
                if opens {
                    if let Some(menu) = self.menu.as_mut() {
                        let _ = menu.open_submenu();
                    }
                    return None;
                }
                let id = self.menu.as_ref().and_then(|menu| menu.current().map(|item| item.id.to_owned()));
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
            NavKey::ArrowLeft | NavKey::ArrowRight => None,
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
        // “刷新此节点”触发的整条连接重读如果失败了：不重建这条连接，
        // 原子树（包括续读、局部刷新补上的对象）原样留着，原因挂在被点的节点上。
        let mut kept: Vec<(String, String)> = Vec::new();
        let mut updated = !partial;
        if partial {
            for connection in connections {
                if let Some(node_id) = self.nav_node_refresh.remove(&connection.id) {
                    if let Some(message) = self.node_refresh_failure(&node_id, connection) {
                        kept.push((node_id, message));
                        continue;
                    }
                }
                updated = true;
                match self.nav_source.iter_mut().find(|item| item.id == connection.id) {
                    Some(slot) => *slot = connection.clone(),
                    None => self.nav_source.push(connection.clone()),
                }
            }
        } else {
            self.nav_source = connections.to_vec();
        }
        let changed = if updated {
            let snapshot = self.nav_source.clone();
            let changed = connections.iter().map(|connection| connection.id.clone()).collect();
            self.nav.apply_changed(&snapshot, &changed)
        } else {
            false
        };
        for (node_id, message) in kept {
            self.nav.set_error(&node_id, message);
        }
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
        // 等着整条连接重读的“刷新此节点”：树没动，原因也挂到被点的节点上。
        let pending = std::mem::take(&mut self.nav_node_refresh);
        for node_id in pending.into_values() {
            self.nav.set_error(&node_id, self.nav_error.clone());
        }
    }

    /// “刷新此节点”的整条连接重读是否算失败：连接级读取报错；或者被点节点范围内
    /// 原本有子项、没有错误的库/模式这次读取报错。一直就报错的库（例如 PostgreSQL
    /// 不在当前会话里的库）不算，否则这条连接永远刷新不了。返回要挂在节点上的原因。
    fn node_refresh_failure(&self, node_id: &str, incoming: &NavConnection) -> Option<String> {
        use crate::catalog::hierarchy::{connection_node_id, database_node_id, schema_node_id};
        if let Some(message) = &incoming.error {
            return Some(message.clone());
        }
        let node = self.nav.node(node_id)?;
        let scope = match node.kind {
            NavKind::Connection | NavKind::Keyspace => None,
            _ => Some(node.database.clone()),
        };
        // 只看有没有子项：读取报错的库/模式在树上本来就没有子项；上一次刷新失败而保留下来的
        // 子树虽然带着错误，再失败一次也必须继续保留。
        // 容器节点不存在（新出现的库/模式，或 SQLite 不显示的附加库）同样不算失败。
        let had_children = |id: &str| self.nav.node(id).is_some_and(|node| !node.children.is_empty());
        let engine = incoming.engine;
        // 没有库层的引擎（SQLite）只把第一个库的第一个模式挂在连接下面，其余库在树上不存在。
        let first_database = incoming.databases.first().map(|database| database.name.as_str());
        for database in &incoming.databases {
            if scope.as_deref().is_some_and(|name| name != database.name) {
                continue;
            }
            let shown_without_database = !engine.shows_databases() && first_database == Some(database.name.as_str());
            if let Some(message) = &database.error {
                let container = if engine.shows_databases() {
                    Some(database_node_id(&incoming.id, &database.name))
                } else if shown_without_database {
                    Some(connection_node_id(&incoming.id))
                } else {
                    None
                };
                if container.is_some_and(|id| had_children(&id)) {
                    return Some(message.clone());
                }
                continue;
            }
            // 没有模式层的引擎（MySQL/MongoDB/SQLite）只显示一个模式（空名优先，否则第一个），
            // 对象直接挂在库或连接下面。
            let shown_schema = database
                .schemas
                .iter()
                .find(|schema| schema.name.is_empty())
                .or_else(|| database.schemas.first())
                .map(|schema| schema.name.as_str());
            for schema in &database.schemas {
                let Some(message) = &schema.error else { continue };
                let container = if engine.shows_schemas() {
                    Some(schema_node_id(&incoming.id, &database.name, &schema.name))
                } else if engine.shows_databases() {
                    (shown_schema == Some(schema.name.as_str()))
                        .then(|| database_node_id(&incoming.id, &database.name))
                } else if shown_without_database
                    && database.schemas.first().map(|first| first.name.as_str()) == Some(schema.name.as_str())
                {
                    Some(connection_node_id(&incoming.id))
                } else {
                    None
                };
                if container.is_some_and(|id| had_children(&id)) {
                    return Some(message.clone());
                }
            }
        }
        None
    }

    pub fn request_nav_refresh(&mut self, connection: Option<String>) {
        self.nav_loading = self.nav_loading.saturating_add(1);
        self.nav_requests.push_back(NavRequest::Refresh { connection });
    }

    pub fn request_lazy_group(&mut self, node_id: &str) {
        if self.nav.needs_group(node_id) && self.nav.begin_group(node_id) {
            self.nav_requests.push_back(NavRequest::Group(node_id.to_owned()));
        }
    }

    /// 库节点按库读取自己的 schema 清单：别的库不随整条连接一次读出。
    pub fn request_database(&mut self, node_id: &str) {
        if self.nav.needs_database(node_id) && self.nav.begin_database(node_id) {
            self.nav_requests.push_back(NavRequest::Database(node_id.to_owned()));
        }
    }

    /// 展开一个容器节点后要发的按需读取：库读自己的 schema，分组读自己的对象。
    pub fn request_lazy_read(&mut self, node_id: &str) {
        if self.nav.node(node_id).map(|node| node.kind) == Some(NavKind::Database) {
            self.request_database(node_id);
        } else {
            self.request_lazy_group(node_id);
        }
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
        self.nav.visible_under(&crate::catalog::hierarchy::connection_node_id(connection_id))
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

    /// 搜索：完整路径与高亮由树提供；清除后恢复展开和选择。
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

    /// 会话连着的库（连接默认库）。读别的库要按库另读，不能借用这条会话。
    pub fn nav_connection_database(&self, connection_id: &str) -> Option<String> {
        self.connections
            .rows
            .iter()
            .find(|row| row.saved.id == connection_id)
            .and_then(|row| row.saved.database.clone())
            .filter(|database| !database.is_empty())
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
        // 分组标题、连接行与树行是同一条可见序列：上下、Home/End 与回车先走资源管理器。
        match key {
            NavKey::ArrowDown => return self.explorer_move(1),
            NavKey::ArrowUp => return self.explorer_move(-1),
            NavKey::Home => return self.explorer_edge(true),
            NavKey::End => return self.explorer_edge(false),
            NavKey::Enter => return self.explorer_enter(),
            NavKey::Escape | NavKey::ArrowLeft | NavKey::ArrowRight => {}
        }
        self.sync_object_spec_from_focus();
        None
    }
    // ------------------------------------------------------------ 连接分组

    /// 分组与成员关系落库后的唯一出口。成员表里指向已删除分组的条目按未分组读，
    /// 不隐藏连接、也不报错：删除分组只删分组记录。
    pub fn apply_groups(&mut self, groups: Vec<ConnectionGroup>, members: std::collections::BTreeMap<String, String>) {
        let known: std::collections::BTreeSet<String> = groups.iter().map(|group| group.id.clone()).collect();
        self.group_members = members.into_iter().filter(|(_, group)| known.contains(group)).collect();
        self.groups = groups;
        if self.group_menu.as_deref().is_some_and(|id| self.group(id).is_none()) {
            self.group_menu = None;
        }
        if self.group_delete.as_deref().is_some_and(|id| self.group(id).is_none()) {
            self.group_delete = None;
        }
        if self
            .group_prompt
            .as_ref()
            .and_then(|prompt| prompt.id.as_deref())
            .is_some_and(|id| self.group(id).is_none())
        {
            self.group_prompt = None;
        }
    }

    pub fn group(&self, id: &str) -> Option<&ConnectionGroup> {
        self.groups.iter().find(|group| group.id == id)
    }

    /// 资源管理器要渲染的分组结构；成员顺序沿用配置库的名称排序。
    pub fn group_layout(&self) -> GroupLayout {
        let ids: Vec<String> = self.connections.rows.iter().map(|row| row.saved.id.clone()).collect();
        layout_groups(&self.groups, &self.group_members, &ids)
    }

    pub fn group_of(&self, connection_id: &str) -> Option<&str> {
        self.group_members.get(connection_id).map(String::as_str)
    }

    /// 打开分组名输入面板：id 为空是新建，否则是重命名（预填原名）。
    pub fn open_group_prompt(&mut self, id: Option<String>) {
        let text = id.as_deref().and_then(|id| self.group(id)).map(|group| group.name.clone()).unwrap_or_default();
        self.group_menu = None;
        self.group_prompt = Some(GroupPrompt { id, text, error: None });
    }

    /// 确认分组名：先本地校验（空名、超长名当场拒绝），再排进后台落库。
    pub fn confirm_group_prompt(&mut self) -> bool {
        let Some(prompt) = self.group_prompt.clone() else { return false };
        match validate_group_name(&prompt.text) {
            Ok(name) => {
                self.pending_group_op = Some(GroupOp::Save { id: prompt.id.clone(), name });
                self.group_prompt = None;
                self.enqueue(PendingJob::GroupOp);
                true
            }
            Err(message) => {
                if let Some(prompt) = self.group_prompt.as_mut() {
                    prompt.error = Some(message);
                }
                false
            }
        }
    }

    /// 折叠 / 展开一个分组：先改本地状态让界面立刻响应，再排进后台持久化。
    pub fn toggle_group(&mut self, id: &str) -> bool {
        let Some(group) = self.group(id).cloned() else { return false };
        let collapsed = !group.collapsed;
        if let Some(entry) = self.groups.iter_mut().find(|entry| entry.id == id) {
            entry.collapsed = collapsed;
        }
        self.pending_group_op = Some(GroupOp::Collapsed { id: id.to_owned(), collapsed });
        self.enqueue(PendingJob::GroupOp);
        true
    }

    /// 归组（group 为空即移回顶层）。拖拽和右键都到这里，落库只有后台一条路径。
    pub fn assign_connection_group(&mut self, connection_id: &str, group: Option<&str>) -> bool {
        if !self.connections.rows.iter().any(|row| row.saved.id == connection_id) {
            return false;
        }
        if let Some(group) = group {
            if self.group(group).is_none() {
                return false;
            }
        }
        self.pending_group_op = Some(GroupOp::Assign { connection: connection_id.to_owned(), group: group.map(str::to_owned) });
        self.enqueue(PendingJob::GroupOp);
        true
    }

    /// 删除分组：连接回到顶层，连接配置与机密一个都不动。
    pub fn take_pending_group_op(&mut self) -> Option<GroupOp> {
        self.pending_group_op.take()
    }

    /// 分组相关的命令入口。菜单、键盘与拖拽都只发这些 id。
    fn group_command(&mut self, id: &str) -> bool {
        match id {
            "group-new" => self.open_group_prompt(None),
            "group-save" => {
                let _ = self.confirm_group_prompt();
            }
            "group-prompt-cancel" => {
                self.group_prompt = None;
            }
            "group-refresh" => self.enqueue(PendingJob::RefreshGroups),
            "group-delete-confirm" => {
                if let Some(group) = self.group_delete.take() {
                    self.pending_group_op = Some(GroupOp::Delete { id: group });
                    self.enqueue(PendingJob::GroupOp);
                }
            }
            "group-delete-cancel" => self.group_delete = None,
            _ => {
                if let Some(rest) = id.strip_prefix("group-toggle:") {
                    return self.toggle_group(rest);
                }
                if let Some(rest) = id.strip_prefix("group-rename:") {
                    self.open_group_prompt(Some(rest.to_owned()));
                } else if let Some(rest) = id.strip_prefix("group-delete:") {
                    if self.group(rest).is_some() {
                        // 先说明影响，再等第二次确认：连接不会跟着消失。
                        self.group_menu = None;
                        self.group_delete = Some(rest.to_owned());
                    }
                } else if let Some(rest) = id.strip_prefix("group-assign:") {
                    let (connection, group) = rest.split_once('').unwrap_or((rest, ""));
                    let group = if group.is_empty() { None } else { Some(group) };
                    return self.assign_connection_group(connection, group);
                } else {
                    return false;
                }
            }
        }
        true
    }

    // ------------------------------------------------------------ 资源管理器的扁平行

    /// 资源管理器的一条可见行：分组标题 → 组内连接 → 未分组连接，
    /// 每条展开的连接下面接它的树行。渲染只取这个序列的一个窗口。
    pub fn explorer_rows(&self) -> Vec<ExplorerItem> {
        if self.connections.rows.is_empty() {
            // 配置列表还没到手（启动第一帧、单测直接喂目录）时，只画树上已有的行：
            // 键盘与渲染读同一份序列，不会因为缺连接记录就整片空掉。
            if self.nav.empty() {
                return Vec::new();
            }
            return self.nav.visible().into_iter().map(ExplorerItem::Nav).collect();
        }
        let layout = self.group_layout();
        let searching = self.nav_searching();
        // 每条连接的成本只算一次：树行、命中与“被筛选隐藏”一起决定分组标题去留。
        let chunks: Vec<(String, Vec<ExplorerItem>, bool, bool)> = self
            .connections
            .rows
            .iter()
            .map(|row| {
                let id = row.saved.id.clone();
                let (items, hits, hidden) = self.connection_rows(&id);
                (id, items, hits, hidden)
            })
            .collect();
        let chunk_of = |id: &str| chunks.iter().find(|(chunk_id, _, _, _)| chunk_id == id);
        // 分组标题下只放“这一帧确实有行”的连接：被筛选隐藏的整条不出。
        let mut items = Vec::new();
        let error = if self.nav_error.is_empty() { &self.catalog_error } else { &self.nav_error };
        if !error.is_empty() {
            items.push(ExplorerItem::CatalogError { message: error.clone() });
        }
        for (group, members) in &layout.groups {
            let visible: Vec<&(String, Vec<ExplorerItem>, bool, bool)> = members.iter().filter_map(|id| chunk_of(id)).collect();
            let hits = visible.iter().filter(|(_, _, hits, _)| *hits).count();
            // 筛选时组里没有命中就整组隐藏；不筛选时连空组也要留着。
            if searching && hits == 0 {
                continue;
            }
            let collapsed = group.collapsed && !searching;
            items.push(ExplorerItem::Group { id: group.id.clone(), collapsed, members: members.len() });
            if collapsed {
                continue;
            }
            for (_, rows, _, hidden) in visible {
                if *hidden {
                    continue;
                }
                items.extend(rows.iter().cloned());
            }
        }
        for id in &layout.ungrouped {
            if let Some((_, rows, _, hidden)) = chunk_of(id) {
                if !*hidden {
                    items.extend(rows.iter().cloned());
                }
            }
        }
        if searching && !chunks.iter().any(|(_, _, hits, _)| *hits) {
            items.push(ExplorerItem::NoMatch);
        }
        items
    }

    /// 一条连接在资源管理器里的行（连接行 + 可选错误行 + 树行）。
    /// 返回（行, 是否有筛选命中, 是否被筛选整条隐藏）。
    fn connection_rows(&self, id: &str) -> (Vec<ExplorerItem>, bool, bool) {
        let searching = self.nav_searching();
        let nav_rows = self.nav_rows_for(id);
        let hits = nav_rows.iter().any(|row| row.highlight.is_some());
        if searching && !hits {
            return (Vec::new(), false, true);
        }
        let mut items = vec![ExplorerItem::Connection { id: id.to_owned() }];
        if let Some(error) = self
            .nav
            .node(&crate::catalog::hierarchy::connection_node_id(id))
            .and_then(|node| node.error.clone())
        {
            items.push(ExplorerItem::ConnectionError { connection_id: id.to_owned(), error });
        }
        let expanded = (searching || self.nav_connection_expanded(id))
            && self.connections.status.get(id).copied().unwrap_or(LinkStatus::Disconnected) == LinkStatus::Connected;
        if !expanded {
            return (items, hits, false);
        }
        if nav_rows.is_empty() {
            if let Some(key) = self.nav_empty_key(id) {
                if key != "ui.explorer.error" {
                    items.push(ExplorerItem::Note { connection_id: id.to_owned(), key });
                }
            }
            return (items, hits, false);
        }
        items.extend(nav_rows.into_iter().map(ExplorerItem::Nav));
        (items, hits, false)
    }

    /// 每行的真实高度，和渲染阶段读同一份 Metrics。
    pub fn explorer_heights(&self, items: &[ExplorerItem]) -> Vec<f32> {
        let metrics = self.shell.density.metrics();
        items.iter().map(|item| item.height(&metrics)).collect()
    }

    /// 键盘可停留的行，按渲染顺序。
    pub fn explorer_focus_rows(&self) -> Vec<ExplorerRow> {
        focus_rows(&self.explorer_rows())
    }

    /// 键盘当前行：显式记录优先，否则落在树焦点上（鼠标点树行的老路径）。
    pub fn explorer_current_row(&self) -> Option<ExplorerRow> {
        let rows = self.explorer_focus_rows();
        if let Some(current) = self.explorer_current.clone() {
            // 树焦点被别的入口挪走（跳转定义、鼠标点行）时，以树为准：
            // 资源管理器的当前行不能和屏幕上的选中行说两套话。
            let consistent = match &current {
                ExplorerRow::Node(id) => self.nav.focus() == Some(id.as_str()),
                _ => true,
            };
            if consistent && rows.contains(&current) {
                return Some(current);
            }
        }
        self.nav.focus().map(|id| ExplorerRow::Node(id.to_owned())).filter(|row| rows.contains(row))
    }

    /// 把键盘当前行写进三份状态：分组焦点、连接选择、树焦点各归其位。
    pub fn set_explorer_current(&mut self, row: ExplorerRow) {
        match &row {
            // 分组行只记焦点：菜单与折叠状态各自独立。
            ExplorerRow::Group(_) => {}
            ExplorerRow::Connection(id) => {
                self.connections.selected = Some(id.clone());
            }
            ExplorerRow::Node(id) => {
                self.nav.set_focus(id);
                self.sync_object_spec_from_focus();
            }
        }
        self.explorer_current = Some(row);
    }

    /// 上下移动：分组标题、连接行与树行在同一条序列里走，不需要先猜层级。
    pub fn explorer_move(&mut self, step: isize) -> Option<String> {
        let rows = self.explorer_focus_rows();
        if rows.is_empty() {
            return None;
        }
        let current = self.explorer_current_row();
        let index = current
            .as_ref()
            .and_then(|row| rows.iter().position(|item| item == row))
            .map(|index| index as isize)
            .unwrap_or(if step > 0 { -1 } else { rows.len() as isize });
        let next = (index + step).clamp(0, rows.len() as isize - 1) as usize;
        let row = rows[next].clone();
        self.reveal_row = Some(row.item_id());
        self.set_explorer_current(row);
        None
    }

    /// Home / End：跳到序列两端。
    pub fn explorer_edge(&mut self, first: bool) -> Option<String> {
        let rows = self.explorer_focus_rows();
        let row = (if first { rows.first() } else { rows.last() }).cloned()?;
        self.reveal_row = Some(row.item_id());
        self.set_explorer_current(row);
        None
    }

    /// 回车：分组折叠/展开，连接按需连接并展开，树节点照原规则打开。
    pub fn explorer_enter(&mut self) -> Option<String> {
        let Some(row) = self.explorer_current_row() else { return self.nav_key(NavKey::Enter) };
        match row {
            ExplorerRow::Group(id) => {
                self.toggle_group(&id);
                None
            }
            ExplorerRow::Connection(id) => {
                if self.connections.row_status(&id) == LinkStatus::Connected {
                    self.toggle_nav_connection(&id);
                } else {
                    self.expand_nav_connection(&id);
                    self.queue_connect(ConnectIntent::new(id, ConnectAction::Expand, None));
                }
                None
            }
            ExplorerRow::Node(id) => {
                let action = self.nav.activate(&id);
                let command = self.nav_activation_command(action);
                self.sync_object_spec_from_focus();
                command.or_else(|| Some(format!("nav-focus:{id}")))
            }
        }
    }

    /// 左右键：分组折叠/展开，连接展开/收起，树节点沿用展开与回到父节点。
    pub fn explorer_step(&mut self, forward: bool) -> Option<String> {
        let Some(row) = self.explorer_current_row() else {
            // 资源管理器还没有当前行（启动第一帧）：左右键直接落到树上。
            return self.nav_step_direct(forward);
        };
        match row {
            ExplorerRow::Group(id) => {
                let collapsed = self.group(&id).map(|group| group.collapsed).unwrap_or(false);
                // 右方向键展开折叠组，左方向键折叠展开组；没有可折叠的就退回上一行。
                let toggles = if forward { collapsed } else { !collapsed };
                if toggles {
                    self.toggle_group(&id);
                } else if !forward {
                    self.explorer_move(-1);
                }
                None
            }
            ExplorerRow::Connection(id) => {
                let expanded = self.nav_connection_expanded(&id);
                if forward && !expanded {
                    if self.connections.row_status(&id) == LinkStatus::Connected {
                        self.toggle_nav_connection(&id);
                    } else {
                        self.expand_nav_connection(&id);
                        self.queue_connect(ConnectIntent::new(id, ConnectAction::Expand, None));
                    }
                    None
                } else if !forward && expanded {
                    self.toggle_nav_connection(&id);
                    None
                } else if !forward {
                    self.explorer_move(-1);
                    None
                } else {
                    None
                }
            }
            ExplorerRow::Node(id) => {
                let action = if forward { self.nav.activate(&id) } else { self.nav.retreat(&id) };
                let command = self.nav_activation_command(action);
                self.sync_object_spec_from_focus();
                command
            }
        }
    }

    /// 某一行的下标：跳转定义与键盘定位把行滚进视口时用它算偏移。
    pub fn explorer_index(&self, items: &[ExplorerItem], target: &str) -> Option<usize> {
        items.iter().position(|item| item.id() == target)
    }

    /// 左右键：先看资源管理器当前行（分组、连接），再落到树节点上。
    pub fn nav_step(&mut self, forward: bool) -> Option<String> {
        self.explorer_step(forward)
    }

    fn nav_step_direct(&mut self, forward: bool) -> Option<String> {
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
                // 按节点种类分派：库读自己的 schema 清单，其余容器读自己的对象分组。
                self.request_lazy_read(&id);
                // 对象节点按需读列/索引。
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

    /// 编辑器里 Ctrl/⌘+点击对象：在对象树里展开到它并选中；表和视图同时打开结构页，
    /// 其它对象（例程、序列、集合……）只定位。树里还没读到这个对象时说明原因，
    /// 表/视图仍按查询页的目录身份（fallback_spec）打开结构页。返回是否在树上定位到了。
    pub fn go_to_definition(
        &mut self,
        connection_id: &str,
        database: &str,
        schema: &str,
        name: &str,
        kind: &str,
        fallback_spec: Option<String>,
    ) -> bool {
        let parsed = crate::catalog::model::parse_kind(kind);
        let found = self
            .nav
            .object_by_key(connection_id, database, schema, name, parsed)
            .or_else(|| {
                self.nav.nodes().find(|node| {
                    node.kind == NavKind::Object
                        && !node.stale
                        && node.connection_id == connection_id
                        && node.database == database
                        && node.schema.eq_ignore_ascii_case(schema)
                        && node.name.eq_ignore_ascii_case(name)
                })
            })
            .cloned();
        let Some(node) = found else {
            self.notice_key = Some("query.definition.notInTree");
            self.notice_detail = name.to_owned();
            if matches!(parsed, crate::catalog::model::ObjectKind::Table | crate::catalog::model::ObjectKind::View) {
                if let Some(spec) = fallback_spec {
                    self.open_item(WorkItemKind::Structure, spec);
                }
            }
            return false;
        };
        if let Some(parent) = node.parent.as_deref() {
            let _ = self.nav.expand(parent);
        }
        self.nav.set_focus(&node.id);
        self.sync_object_spec_from_focus();
        // 定位要看得见：侧栏收起时打开，并让对象树把这一行滚进可见区。
        self.shell.sidebar_open = true;
        self.reveal_node = Some(node.id.clone());
        let structure = node.engine != Engine::Mongodb && node.object().is_some_and(|object| object.opens_data());
        if structure {
            let _ = self.nav_command(&format!("nav-open-structure:{}", node.id));
        }
        true
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
                // 库读失败后重试：只重读这个库的 schema 清单，连接行和别的库不动。
                NavKind::Database => {
                    if self.nav.begin_database(rest) {
                        self.nav_requests.push_back(NavRequest::Database(rest.to_owned()));
                    }
                }
                NavKind::Object => self.request_nav_details(rest),
                // 种类分组只在失败的那一类上重试；加载态也只留在这个分组。
                NavKind::Group if has_kind => {
                    let more = node.paged && !node.unloaded && node.has_more;
                    if more && !node.loading {
                        self.nav.begin_scope(rest);
                        self.nav_requests.push_back(NavRequest::More(rest.to_owned()));
                    } else if self.nav.begin_group(rest) {
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
            if self.nav.toggle(rest) && self.nav.is_expanded(rest) {
                if self.nav.needs_details(rest) { self.request_nav_details(rest); }
                self.request_lazy_read(rest);
            }
            self.nav.set_focus(rest);
            self.sync_object_spec_from_focus();
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-focus:") {
            self.nav.set_focus(rest);
            self.explorer_current = Some(ExplorerRow::Node(rest.to_owned()));
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
            if self.nav.node(rest).is_some_and(|node| {
                node.engine == Engine::Mongodb || node.object().is_some_and(|object| !object.opens_data())
            }) {
                // 集合、触发器、例程、序列等没有表结构页；菜单和双击/Enter 走同一门控。
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
            if self.nav.node(rest).is_some_and(|node| node.object().is_some_and(|object| !object.opens_data())) {
                self.notice_key = Some("catalog.tree.noStructure");
                self.notice_detail = self.nav.node(rest).map(|node| node.name.clone()).unwrap_or_default();
                return true;
            }
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
        if let Some(rest) = id.strip_prefix("nav-view-ddl:") {
            self.open_definition(rest, false);
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-copy-ddl:") {
            self.open_definition(rest, true);
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-refresh-node:") {
            self.refresh_nav_node(rest);
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-insert-template:") {
            let Some((kind, node_id)) = rest.split_once(':').and_then(|(kind, node)| Some((TemplateKind::parse(kind)?, node))) else {
                return true;
            };
            self.begin_template(node_id, kind);
            return true;
        }
        if let Some(rest) = id.strip_prefix("nav-danger:") {
            let Some((menu_id, node_id)) = rest.split_once(':') else { return true };
            self.open_danger(menu_id, node_id);
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

    /// 查看 / 复制 DDL：只对读得到定义的关系型对象发起读取；读不到时说明原因，不打开空查看器。
    fn open_definition(&mut self, node_id: &str, copy_only: bool) {
        let Some(node) = self.nav.node(node_id).filter(|node| !node.stale && node.kind == NavKind::Object).cloned() else {
            self.notice_key = Some("catalog.tree.staleObject");
            self.notice_detail.clear();
            return;
        };
        let Some(object) = node.object() else { return };
        if !object.engine.is_relational() || !crate::catalog::definition::ddl_available(object.engine, object.kind) {
            self.notice_key = Some("catalog.tree.noDefinition");
            self.notice_detail = node.name.clone();
            return;
        }
        self.ddl_view = Some(DdlView {
            epoch: next_epoch(),
            node_id: node_id.to_owned(),
            object,
            signature: node.signature.clone(),
            relation: node.relation.clone(),
            copy_only,
            loading: true,
            text: None,
            rebuilt: false,
            missing: Vec::new(),
            error: None,
        });
        self.nav_requests.push_back(NavRequest::Definition(node_id.to_owned()));
    }

    /// 定义读完。只接受当前这一次读取；“复制 DDL”成功时把同一份文本交给剪贴板。
    pub fn definition_loaded(&mut self, epoch: u64, result: Result<crate::catalog::definition::ObjectDdl, String>) {
        let Some(view) = self.ddl_view.as_mut().filter(|view| view.epoch == epoch) else { return };
        view.loading = false;
        match result {
            Ok(ddl) => {
                view.text = Some(ddl.text.clone());
                view.rebuilt = ddl.rebuilt;
                view.missing = ddl.missing;
                view.error = None;
                if view.copy_only {
                    self.ddl_view = None;
                    self.copied = ddl.text.clone();
                    self.clipboard_out = Some(ddl.text);
                    self.notice_key = Some("catalog.ddl.copied");
                    self.notice_detail.clear();
                }
            }
            Err(message) => {
                // 失败只说明原因，不提供复制，也不动剪贴板。
                view.text = None;
                view.error = Some(message.clone());
                if view.copy_only {
                    self.ddl_view = None;
                    self.notice_key = Some("catalog.ddl.failed");
                    self.notice_detail = message;
                }
            }
        }
    }

    /// 视图把编辑器当前的光标/选区（字节偏移）交给模型；SQL 模板按它插入。
    pub fn set_draft_cursor(&mut self, tab_id: &str, from: usize, to: usize) {
        self.draft_cursor = Some((tab_id.to_owned(), from, to));
    }

    /// 视图取走要写进系统剪贴板的文本。
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard_out.take()
    }

    /// “刷新此节点”：只重读这一行的子项，展开状态由稳定的节点 id 保留。
    /// 连接/键空间以及没有对象分组的库（带模式层的引擎、MongoDB）按连接重读；
    /// 库/模式从第一页重读；种类分组只重读这一类。
    pub fn refresh_nav_node(&mut self, node_id: &str) {
        let Some(node) = self.nav.node(node_id).cloned() else {
            self.notice_key = Some("catalog.tree.staleObject");
            self.notice_detail.clear();
            return;
        };
        let whole_connection = match node.kind {
            NavKind::Connection | NavKind::Keyspace => true,
            NavKind::Database => node.engine.shows_schemas() || !node.engine.is_relational(),
            NavKind::Schema => !node.engine.is_relational(),
            _ => false,
        };
        if whole_connection {
            // 结果回来时按这个节点判断失败：失败就保留原子树，原因挂在这里。
            self.nav_node_refresh.insert(node.connection_id.clone(), node_id.to_owned());
            self.request_nav_refresh(Some(node.connection_id.clone()));
            return;
        }
        match node.kind {
            NavKind::Database | NavKind::Schema => {
                if node.loading {
                    return;
                }
                self.nav.begin_scope(node_id);
                self.nav_requests.push_back(NavRequest::Reload(node_id.to_owned()));
            }
            NavKind::Group if node.object_kind.is_some() => {
                if self.nav.begin_group(node_id) {
                    self.nav_requests.push_back(NavRequest::Group(node_id.to_owned()));
                }
            }
            _ => {
                self.notice_key = Some("catalog.tree.actionUnavailable");
                self.notice_detail = node.name.clone();
            }
        }
    }

    /// 生成 SQL 模板。列已经读过就直接插入；否则先按需读列，读完再插入。
    fn begin_template(&mut self, node_id: &str, kind: TemplateKind) {
        let Some(object) = self.nav_target(node_id).filter(|object| object.engine.is_relational() && object.opens_data()) else {
            self.notice_key = Some("catalog.tree.staleObject");
            self.notice_detail.clear();
            return;
        };
        match self.nav.object_columns(node_id) {
            Some(columns) if !columns.is_empty() => self.insert_template(kind, &object, &columns),
            _ => {
                self.pending_template = Some((node_id.to_owned(), kind));
                let loading = self.nav.node(node_id).is_some_and(|node| node.loading);
                if !loading {
                    self.request_nav_details(node_id);
                }
            }
        }
    }

    /// 这个节点有没有等列的模板；视图据此先把编辑器里的最新文本收回草稿。
    pub fn template_waits_for(&self, node_id: &str) -> bool {
        self.pending_template.as_ref().is_some_and(|(id, _)| id == node_id)
    }

    /// 列/索引读完：写进树里；若有等着这一行列的模板，用读到的列生成并插入。
    pub fn nav_details_loaded(
        &mut self,
        node_id: &str,
        columns: Result<Vec<crate::catalog::hierarchy::ColumnInfo>, String>,
        indexes: Result<Vec<crate::catalog::hierarchy::IndexInfo>, String>,
    ) {
        let column_error = columns.as_ref().err().cloned();
        self.nav.set_details(node_id, columns, indexes);
        if !self.template_waits_for(node_id) {
            return;
        }
        let Some((_, kind)) = self.pending_template.take() else { return };
        let columns = self.nav.object_columns(node_id).unwrap_or_default();
        match self.nav_target(node_id) {
            Some(object) if !columns.is_empty() => self.insert_template(kind, &object, &columns),
            _ => {
                self.notice_key = Some("catalog.tree.templateNoColumns");
                self.notice_detail = column_error.unwrap_or_default();
            }
        }
    }

    /// 模板只写草稿，不执行。当前不是查询项时，新开一个绑定这个对象连接与库的查询。
    fn insert_template(&mut self, kind: TemplateKind, object: &ObjectKey, columns: &[(String, bool)]) {
        let text = nav::template_text(kind, object, columns);
        let active = self.active_tab().filter(|tab| tab.kind.is_query()).map(|tab| tab.id.clone());
        if let Some(tab_id) = active {
            // 插在最近一次收回的光标处；有选区时插在选区开头，不替换、不删除选中的文本。
            // 还没有这一项的光标时（例如编辑器尚未创建）按末尾追加。
            let at = self
                .draft_cursor
                .as_ref()
                .filter(|(id, _, _)| *id == tab_id)
                .map(|(_, from, to)| (*from).min(*to));
            let Some(tab) = self.tabs.iter_mut().find(|item| item.id == tab_id) else { return };
            let at = at.unwrap_or(tab.draft.len());
            let (draft, end) = nav::insert_text_at(&tab.draft, at, &text);
            tab.draft = draft;
            // 连续插入时接在刚插入的文本后面。
            self.draft_cursor = Some((tab_id, end, end));
            self.shell.page = AppPage::Workspace;
            self.shell.focus = FocusTarget::Editor;
        } else {
            self.open_bound_query(
                text,
                Some(object.connection_id.clone()),
                Some(object.connection_name.clone()),
                Some(object.database.clone()),
                Some(object.engine.as_str().to_owned()),
            );
        }
    }

    /// 打开危险操作确认层。只接受关系型连接里的表/视图，种类不符时说明原因。
    fn open_danger(&mut self, menu_id: &str, node_id: &str) {
        let Some(object) = self.nav_target(node_id) else {
            self.notice_key = Some("catalog.tree.staleObject");
            self.notice_detail.clear();
            return;
        };
        if !object.engine.is_relational() {
            self.notice_key = Some("catalog.tree.relationalOnly");
            self.notice_detail = object.name.clone();
            return;
        }
        let Some(kind) = DangerKind::from_menu(menu_id, object.kind) else {
            self.notice_key = Some("catalog.tree.actionUnavailable");
            self.notice_detail = object.name.clone();
            return;
        };
        let env = self.nav_environment(&object.connection_id);
        let prompt = DangerPrompt::new(kind, node_id, object, &env);
        self.focused_field = if kind == DangerKind::Rename {
            Some(DraftField::DangerNewName)
        } else if prompt.prod {
            Some(DraftField::DangerConfirm)
        } else {
            None
        };
        self.danger = Some(prompt);
    }

    /// 视图取走确认层要做的一步异步工作。
    pub fn take_danger_request(&mut self) -> Option<(DangerStep, DangerPrompt)> {
        let prompt = self.danger.as_mut()?;
        let step = prompt.take_request()?;
        Some((step, prompt.clone()))
    }

    /// 计划与标签表读完。确认层已关闭或换了一次时丢弃。
    pub fn danger_planned(&mut self, epoch: u64, tags: Option<Vec<crate::connections::model::EnvTag>>, result: Result<crate::ddl::Plan, String>) {
        let Some(prompt) = self.danger.as_mut().filter(|prompt| prompt.epoch == epoch) else { return };
        prompt.apply_tags(tags.as_deref());
        prompt.apply_plan(result);
    }

    /// 执行结束。成功：收起确认层，把这个对象从树上拿掉并重读所属连接；
    /// 失败：数据库原因留在确认层里，树保持执行前的样子。
    pub fn danger_finished(&mut self, epoch: u64, result: Result<crate::ddl::Receipt, String>) -> bool {
        let Some(prompt) = self.danger.as_mut().filter(|prompt| prompt.epoch == epoch) else { return false };
        if !prompt.apply_receipt(result) {
            return false;
        }
        let prompt = self.danger.take().expect("prompt");
        if matches!(prompt.kind, DangerKind::DropTable | DangerKind::DropView | DangerKind::Rename) {
            self.forget_cached_object(&prompt.object.connection_id, &prompt.object.database, &prompt.object.schema, &prompt.object.name, prompt.object.kind);
            self.nav.forget(&prompt.node_id);
        }
        self.request_nav_refresh(Some(prompt.object.connection_id.clone()));
        self.notice_key = Some("catalog.danger.done");
        self.notice_detail = prompt.object.name.clone();
        if matches!(self.focused_field, Some(DraftField::DangerConfirm | DraftField::DangerNewName)) {
            self.focused_field = None;
        }
        true
    }

    /// 结构页删除表成功：这张表立即离开树，再重读所属连接。
    pub fn adopt_created_table(&mut self, spec: String) {
        let connection = spec.split('|').next().unwrap_or("").to_owned();
        if let Some(tab) = self.active_mut() {
            if tab.kind == WorkItemKind::Structure {
                tab.title = spec.split('|').nth(4).unwrap_or("").to_owned();
                tab.spec = Some(spec.clone());
            }
        }
        self.table_spec = Some(spec);
        if !connection.is_empty() {
            self.request_nav_refresh(Some(connection));
        }
    }

    fn forget_cached_object(&mut self, connection_id: &str, database: &str, schema: &str, name: &str, kind: crate::catalog::model::ObjectKind) {
        for connection in self.nav_source.iter_mut().filter(|connection| connection.id == connection_id) {
            for database in connection.databases.iter_mut().filter(|item| item.name == database) {
                for scope in database.schemas.iter_mut().filter(|item| item.name == schema) {
                    let before = scope.objects.len();
                    scope.objects.retain(|object| !(object.name == name && object.kind == kind));
                    let removed = before - scope.objects.len();
                    scope.total = scope.total.saturating_sub(removed);
                    for report in scope.kind_reports.iter_mut().filter(|report| report.kind == kind) {
                        report.total = report.total.saturating_sub(removed);
                    }
                }
            }
        }
    }

    pub fn table_dropped(&mut self, connection_id: &str, database: &str, schema: &str, table: &str) {
        self.forget_cached_object(connection_id, database, schema, table, crate::catalog::model::ObjectKind::Table);
        let id = self
            .nav
            .object_by_key(connection_id, database, schema, table, crate::catalog::model::ObjectKind::Table)
            .map(|node| node.id.clone());
        if let Some(id) = id {
            self.nav.forget(&id);
        }
        self.request_nav_refresh(Some(connection_id.to_owned()));
    }

    /// 对象列表真正按页读取的范围：往上找到库/模式/连接节点。
    fn nav_storage_scope(&self, node_id: &str) -> String {
        let mut current = node_id.to_owned();
        loop {
            let Some(node) = self.nav.node(&current) else { return node_id.to_owned() };
            if node.kind == NavKind::Group && node.paged { return current; }
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
        let sql = if self.history.section == HistorySection::Saved {
            self.history.plan_saved(&self.known_ids()).map(|plan| plan.sql).unwrap_or_default()
        } else if self.shell.page == AppPage::Workspace {
            self.active_tab().map(|tab| tab.draft.clone()).unwrap_or_default()
        } else {
            self.history.plan_run(&self.known_ids()).map(|plan| plan.sql).unwrap_or_default()
        };
        self.copied = sql.clone();
        if !sql.is_empty() {
            self.clipboard_out = Some(sql);
        }
    }

    fn save_history_preview(&mut self) {
        let Some(plan) = self.history.preview_plan(&self.known_ids()) else {
            self.notice_key = Some("history.page.noMatch");
            self.notice_detail.clear();
            return;
        };
        if plan.sql.trim().is_empty() {
            self.notice_key = Some("history.page.needName");
            self.notice_detail.clear();
            return;
        }
        if self.history.save_name.trim().is_empty() {
            let line = plan.sql.lines().find(|line| !line.trim().is_empty()).unwrap_or("query");
            self.history.save_name = line.chars().take(48).collect();
        }
        self.preview_save = Some(crate::query::SaveRequest {
            name: self.history.save_name.trim().to_owned(),
            sql: plan.sql,
            connection_id: plan.connection_id.unwrap_or_default(),
            connection_name: plan.connection_name,
            database_name: plan.database.unwrap_or_default(),
            engine: plan.engine,
        });
        self.enqueue(PendingJob::SaveNamed);
    }

    fn focus_status_target(&mut self) {
        let on_query = self.shell.page == AppPage::Workspace && self.tool == WorkTool::Query;
        if on_query && !self.connections.rows.is_empty() {
            self.shell.focus = FocusTarget::Editor;
            return;
        }
        if let Some(id) = self.active_tab().and_then(|tab| tab.connection_id.clone()) {
            self.connections.selected = Some(id);
        }
        self.shell.page = AppPage::Connections;
        self.shell.focus = FocusTarget::Settings;
    }

    fn open_admin(&mut self) {
        if let Some(spec) = self.admin_scope_spec().or_else(|| self.table_spec.clone()).or_else(|| self.object_spec.clone()) {
            self.nav_menu_node = None;
            self.open_item(WorkItemKind::Admin, spec);
        } else {
            self.notice_key = Some("shell.tabs.needObject");
            self.notice_detail.clear();
        }
    }

    /// 连接、库或模式节点打开管理页：带上这条连接和默认库，不借用上一张表。
    fn admin_scope_spec(&self) -> Option<String> {
        let id = self.nav_menu_node.clone().or_else(|| self.nav.focus().map(str::to_owned))?;
        let node = self.nav.node(&id)?;
        if !matches!(node.kind, NavKind::Connection | NavKind::Database | NavKind::Schema) {
            return None;
        }
        self.scope_spec(node)
    }

    fn begin_create_table(&mut self) {
        let id = self.nav_menu_node.clone().or_else(|| self.nav.focus().map(str::to_owned));
        let Some(node) = id.as_deref().and_then(|id| self.nav.node(id)).cloned() else {
            self.notice_key = Some("catalog.tree.actionUnavailable");
            self.notice_detail.clear();
            return;
        };
        if !matches!(node.kind, NavKind::Connection | NavKind::Database | NavKind::Schema | NavKind::Group) || !node.engine.is_relational() {
            self.notice_key = Some("catalog.tree.actionUnavailable");
            self.notice_detail = node.name.clone();
            return;
        }
        let Some(spec) = self.scope_spec(&node) else {
            self.notice_key = Some("shell.tabs.needObject");
            self.notice_detail.clear();
            return;
        };
        self.nav_menu_node = None;
        self.pending_create = Some(spec.clone());
        self.open_item(WorkItemKind::Structure, spec);
    }

    fn scope_spec(&self, node: &NavNode) -> Option<String> {
        if node.connection_id.is_empty() || !node.engine.is_relational() {
            return None;
        }
        let database = if !node.database.is_empty() {
            node.database.clone()
        } else {
            self.connections
                .rows
                .iter()
                .find(|row| row.saved.id == node.connection_id)
                .and_then(|row| row.saved.database.clone())
                .unwrap_or_default()
        };
        let name = if database.is_empty() { node.connection_name.clone() } else { database.clone() };
        if name.is_empty() {
            return None;
        }
        let env = self.nav_environment(&node.connection_id);
        Some(format!(
            "{}|{}|{}|{}|{}|{}|{}",
            node.connection_id,
            node.connection_name,
            database,
            node.schema,
            name,
            node.engine.as_str(),
            env
        ))
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
            DraftField::Password => {
                self.connections.secrets.password = value;
                self.connections.clear_issue("password");
            }
            DraftField::SshPassword => {
                self.connections.secrets.ssh_password = value.clone();
                self.connections.clear_issue("ssh_password");
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
            DraftField::GroupName => {
                if let Some(prompt) = self.group_prompt.as_mut() {
                    prompt.text = value;
                    prompt.error = None;
                }
            }
            DraftField::DangerConfirm => {
                if let Some(prompt) = self.danger.as_mut() {
                    prompt.set_typed(value);
                }
            }
            DraftField::DangerNewName => {
                if let Some(prompt) = self.danger.as_mut() {
                    prompt.set_new_name(value);
                }
            }
            other => {
                if let Some(draft) = self.connections.editor.as_mut() {
                    *field_slot(draft, other) = value;
                    if other == DraftField::CaFile && !draft.ca_file.trim().is_empty() {
                        draft.ca_mode = CaMode::File;
                    }
                }
                if let Some(name) = issue_field(other) {
                    self.connections.clear_issue(name);
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
            DraftField::GroupName => self.group_prompt.as_ref().map(|prompt| prompt.text.clone()).unwrap_or_default(),
            DraftField::DangerConfirm => self.danger.as_ref().map(|prompt| prompt.typed.clone()).unwrap_or_default(),
            DraftField::DangerNewName => self.danger.as_ref().map(|prompt| prompt.new_name.clone()).unwrap_or_default(),
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
            for item in flatten_items(&menu.items) {
                nodes.push(A11yNode {
                    id: item.command(),
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
        // 实际可写宽度是扣掉根 padding 之后的内容盒；用未减 padding 的宽度会漏掉窄窗口里的裁切。
        let limit = self.painted().content_width.max(160);
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

/// 草稿字段对应的校验字段名，与 connections::validate_draft 里的 field 字符串一致。
/// 没有校验规则的字段返回 None：改它不需要清错误。
fn issue_field(field: DraftField) -> Option<&'static str> {
    match field {
        DraftField::Host => Some("host"),
        DraftField::Port => Some("port"),
        DraftField::Database => Some("database"),
        DraftField::Username => Some("username"),
        DraftField::File => Some("file_path"),
        DraftField::CaFile => Some("ca_file"),
        DraftField::SshHost => Some("ssh_host"),
        DraftField::SshPort => Some("ssh_port"),
        DraftField::SshUser => Some("ssh_user"),
        _ => None,
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
