//! Redis 工作台（R4）。顶栏 = 连接/逻辑库 + 页签（键 | 慢日志 | 信息 | 大键 | 客户端 | Pub/Sub | 节点[集群]）。
//! 键页：后台全量 SCAN（进度、可取消、上限提示）+ 虚拟列表，树/模板/平铺三种视图（分隔符按连接保存），
//! 模板批量删除/TTL（预览 → 确认 → 分批执行），右侧键详情（分页取值、集群槽位）；控制台为可折叠底部面板。
//! 所有写入先确认；连接挂生产标签时确认还要输入连接名。浏览只走 SCAN，禁 KEYS。

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::InputState;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use crate::ui::kit::{self, stack_frame, FieldSpec, Fields, Palette, Radius, Tip as _, TypeScale};
use crate::ui::{ApplyBlock, CommitResult, EditMode, FormatViewer, FormatViewerEvent, TruncationState, ViewerSpec};
use gpui_kit::{
    anchored, deferred, div, px, Anchor, AnyElement, AppContext as _, ClickEvent, ClipboardItem, Context, Entity, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels,
    Point, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window,
};

use crate::application::redis::{DeleteOutcome, ExecOutcome, KeyspaceDb, TtlOutcome};
use crate::application::AppServices;
use crate::i18n::{self, Locale};
use crate::adapters::redis::complete::{self, RedisCompletion};
use crate::adapters::redis::exec::{self, CommandRisk, RespReply};
use crate::adapters::redis::batch::{self as batch_ops, BatchOp, BatchPreview, BatchProgress};
use crate::adapters::redis::cluster::{self, KeyPlacement};
use crate::adapters::redis::conn::{ClusterFallback, Topology};
use crate::adapters::redis::diag::{BigKey, BigKeyEvent, ClientInfo, InfoSection, PubSubMessage, SlowEntry, SlowSort};
use crate::adapters::redis::keys::{self, RedisInitialValue, RedisKeyDetail, RedisKeyInfo, RedisMutation, RedisValue, ValuePage};
use crate::adapters::redis::keyspace::{self, KeyspacePrefs, TemplateGroup, TreeRow};
use crate::adapters::redis::scan::{ScanEvent, ScanOptions, ScanResume};

const SIDEBAR_DEFAULT_W: f32 = 304.;
const SIDEBAR_MIN_W: f32 = 220.;
const SIDEBAR_MAX_W: f32 = 560.;

#[allow(dead_code, clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Field {
    Pattern,
    Ttl,
    Rename,
    Command,
    NewKey,
    NewTtl,
    NewField,
    NewValue,
    NewScore,
    EntryField,
    EntryValue,
    EntryMember,
    EntryScore,
    EntryIndex,
    StreamId,
    StreamField,
    StreamValue,
    HashQuery,
    SetQuery,
    Separators,
    ScanCap,
    BatchTtl,
    ConfirmTyped,
    SubChannels,
    PubChannel,
    PubMessage,
    InfoQuery,
    ClientsQuery,
}

#[derive(Clone, Debug, PartialEq)]
enum Confirm {
    Delete { key: String },
    Rename { from: String, to: String },
    Exec { command: String, args: Vec<String>, line: String },
    Ttl { key: String, seconds: Option<i64> },
    /// guard：写回核对（读取时的值指纹）。确认之后、写入之前再核对一次，确认框开着时别人改了值也不会被覆盖。
    Mutation { mutation: RedisMutation, key: String, guard: Option<(keys::ValueProbe, u64)> },
    /// 模板批量操作：预览（数量与样例）就是确认框的内容。
    /// db：发起时的逻辑库，批量操作只在这个库上执行。
    Batch { template: String, keys: Vec<String>, op: BatchOp, preview: BatchPreview, db: u8 },
    SlowlogReset { node: String },
    ClientKill { node: String, id: String, addr: String },
    Publish { channel: String, message: String },
}

/// 顶栏页签。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Keys,
    Console,
    Slowlog,
    Info,
    BigKeys,
    Clients,
    PubSub,
    Nodes,
}

/// 服务端错误按 redis-cli 的样子显示：`(error) <原因>`，去掉驱动加的长前缀。
/// 本地拒绝（例如禁用 KEYS）等不是服务端错误的提示原样保留。
pub(crate) fn console_error_text(message: &str) -> String {
    let message = message.trim();
    let reason = ["ResponseError: ", "An error was signalled by the server - "]
        .iter()
        .find_map(|prefix| message.find(prefix).map(|at| &message[at + prefix.len()..]));
    match reason {
        Some(reason) => format!("(error) {}", reason.trim()),
        None => message.to_owned(),
    }
}

/// 控制台保留的执行记录条数。
const CONSOLE_LOG_MAX: usize = 300;

/// 控制台的一条执行记录。
#[derive(Clone, Debug, PartialEq)]
pub struct ConsoleEntry {
    pub db: String,
    pub line: String,
    pub output: String,
    pub ok: bool,
    pub elapsed: Option<std::time::Duration>,
}

impl ConsoleEntry {
    /// 复制全部记录时的文字：和 redis-cli 一样，提示符 + 命令，下面是输出。
    fn transcript(&self) -> String {
        format!("db{}> {}\n{}", self.db, self.line, self.output)
    }
}

impl Page {
    pub(crate) const ALL: [Page; 8] = [Page::Keys, Page::Console, Page::Slowlog, Page::Info, Page::BigKeys, Page::Clients, Page::PubSub, Page::Nodes];

    pub(crate) fn id(self) -> &'static str {
        match self {
            Page::Keys => "keys",
            Page::Console => "console",
            Page::Slowlog => "slowlog",
            Page::Info => "info",
            Page::BigKeys => "bigkeys",
            Page::Clients => "clients",
            Page::PubSub => "pubsub",
            Page::Nodes => "nodes",
        }
    }

    fn from_id(id: &str) -> Option<Page> {
        Page::ALL.into_iter().find(|page| page.id() == id)
    }

    pub(crate) fn label_key(self) -> &'static str {
        match self {
            Page::Keys => "redis.page.keys",
            Page::Console => "redis.page.console",
            Page::Slowlog => "redis.page.slowlog",
            Page::Info => "redis.page.info",
            Page::BigKeys => "redis.page.bigkeys",
            Page::Clients => "redis.page.clients",
            Page::PubSub => "redis.page.pubsub",
            Page::Nodes => "redis.page.nodes",
        }
    }
}

/// 键列表的视图：命名空间树 / 模板归并 / 平铺。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewMode {
    Tree,
    Template,
    Flat,
}

/// 后台扫描的状态：进度（已读取 / DBSIZE）、续扫位置、是否被取消或达到上限。
#[derive(Clone, Debug)]
struct ScanState {
    running: bool,
    cancel: Arc<AtomicBool>,
    resume: ScanResume,
    total: Option<u64>,
    capped: bool,
    cancelled: bool,
    epoch: u64,
}

impl Default for ScanState {
    fn default() -> Self {
        Self { running: false, cancel: Arc::new(AtomicBool::new(false)), resume: ScanResume::default(), total: None, capped: false, cancelled: false, epoch: 0 }
    }
}

/// 进行中的批量操作。
#[derive(Clone, Debug)]
struct BatchRun {
    template: String,
    op: BatchOp,
    progress: BatchProgress,
    cancel: Arc<AtomicBool>,
}

/// Pub/Sub 收到的单条消息（附带本地接收时间）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReceivedMessage {
    pub(super) channel: String,
    pub(super) payload: String,
    pub(super) at: String,
}

/// 诊断页的数据。每页第一次打开时读取，之后按刷新重读。
#[derive(Default)]
struct DiagState {
    node: String,
    busy: bool,
    error: String,
    slowlog: Vec<SlowEntry>,
    slow_by_time: bool,
    slow_asc: bool,
    slow_loaded: bool,
    info: Vec<InfoSection>,
    info_section: String,
    info_loaded: bool,
    clients: Vec<ClientInfo>,
    clients_query: String,
    clients_rows: Vec<usize>,
    clients_loaded: bool,
    big: Vec<BigKey>,
    big_running: bool,
    big_scanned: u64,
    big_total: Option<u64>,
    big_cancel: Option<Arc<AtomicBool>>,
    big_memory: bool,
    big_done: bool,
    sub_input: String,
    sub_running: bool,
    sub_ready: bool,
    sub_cancel: Option<Arc<AtomicBool>>,
    messages: VecDeque<ReceivedMessage>,
    pub_channel: String,
    pub_message: String,
    node_sizes: Vec<(String, u64)>,
    nodes_loaded: bool,
}

/// 订阅消息列表最多保留的条数。
const PUBSUB_KEEP: usize = 500;
/// 模板展开后列出的成员上限（更多的用“按模板过滤”服务端扫描）。
const TEMPLATE_MEMBERS_SHOWN: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Follow {
    None,
    Read,
}

/// 失败来源。错误块按它落在自己的上下文里，不再把一切错误都塞进底部一行红字。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ErrorSource {
    /// SCAN 读键列表（含未打开连接）。
    Scan,
    /// 读取选中键的内容。
    Detail,
    /// 命令行 / 写删命令。
    Command,
}

/// 键列表的呈现状态：加载 → 错误 → 无命中 → 无数据 → 有行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListState {
    Loading,
    Error,
    NoMatch,
    Empty,
    Rows,
}

fn list_state(busy: bool, scan_failed: bool, total: usize, visible: usize, pattern_active: bool, done: bool) -> ListState {
    if busy && total == 0 {
        return ListState::Loading;
    }
    if scan_failed && total == 0 {
        return ListState::Error;
    }
    if total == 0 {
        if !done {
            return ListState::Rows;
        }
        if pattern_active {
            return ListState::NoMatch;
        }
        return ListState::Empty;
    }
    if visible == 0 {
        return ListState::NoMatch;
    }
    ListState::Rows
}

/// 命名空间树与列表的统一渲染行：树模式下文件夹与叶子键按真实层级穿插排列，绝不重复堆叠扁平列表；
/// 模板模式下每个模板一行（键数、类型、内存估算），展开后列出成员键。
#[derive(Clone, Debug, PartialEq, Eq)]
enum KeyRow {
    Folder {
        prefix: String,
        segment: String,
        depth: usize,
        count: usize,
        open: bool,
    },
    Key {
        index: usize,
        display: String,
        depth: usize,
    },
    Template {
        group: usize,
        open: bool,
    },
    TemplateMore {
        template: String,
        shown: usize,
        total: usize,
    },
}

/// 结构化查看器里可以直接写回的条目：Hash 字段值、List 指定下标的元素。其余条目只读。
#[derive(Clone, Debug, PartialEq, Eq)]
enum EntryTarget {
    Hash(String),
    List(i64),
}

#[derive(Clone, Debug)]
struct ContextItem {
    id: String,
    icon: IconName,
    label: String,
    danger: bool,
}

#[derive(Clone, Debug)]
struct ContextMenuState {
    at: Point<Pixels>,
    items: Vec<ContextItem>,
}

pub struct RedisPane {
    services: Arc<AppServices>,
    locale: Locale,
    connection_id: String,
    connection_name: String,
    database: String,
    db_picker_open: bool,
    type_picker_open: bool,
    db_counts: Vec<KeyspaceDb>,
    sidebar_w: f32,
    sidebar_drag_start: Option<(f32, f32)>,
    pattern: String,
    scanned_pattern: String,
    keys: Vec<RedisKeyInfo>,
    key_pos: HashMap<String, usize>,
    done: bool,
    scan: ScanState,
    epoch: u64,
    page: Page,
    topology: Topology,
    prefs: KeyspacePrefs,
    prefs_open: bool,
    sep_input: String,
    cap_input: String,
    view_mode: ViewMode,
    rows_cache: Option<Rc<Vec<KeyRow>>>,
    templates_cache: Option<Rc<Vec<TemplateGroup>>>,
    open_templates: BTreeSet<String>,
    template_memory: HashMap<String, Option<u64>>,
    batch: Option<BatchRun>,
    batch_keys: Vec<String>,
    batch_ttl_for: Option<String>,
    batch_ttl_input: String,
    placement: Option<KeyPlacement>,
    value_loading: bool,
    prod_target: bool,
    confirm_typed: String,
    confirm_typed_error: bool,
    diag: DiagState,
    selected: String,
    detail: Option<Arc<RedisKeyDetail>>,
    ttl_input: String,
    ttl_open: bool,
    rename_to: String,
    rename_open: bool,
    command_input: String,
    reply: String,
    /// 控制台的执行记录（最新在最后），超过上限丢最早的。
    console_log: VecDeque<ConsoleEntry>,
    /// 正在执行的那条命令：结果回来时连同耗时写进记录。
    console_pending: Option<(String, String, std::time::Instant)>,
    console_scroll: gpui_kit::ScrollHandle,
    note: String,
    error: String,
    confirm: Option<Confirm>,
    confirm_prompt: String,
    busy: bool,
    generation: u64,
    copied: String,
    follow: Follow,
    pending_ttl: Option<i64>,
    ttl_was_persist: bool,
    focused: Option<Field>,
    fields: Fields<Field>,
    type_filter: String,
    nav_choice: Option<bool>,
    viewport_w: f32,
    /// 本面板实际占的宽度（上一帧量到的）。窗口宽度包含应用侧栏，不能直接当面板宽度用。
    pane_w: Rc<std::cell::Cell<Option<f32>>>,
    error_source: ErrorSource,
    expanded: BTreeSet<String>,
    complete_open: bool,
    complete_index: usize,
    history: Vec<String>,
    history_at: Option<usize>,
    history_draft: String,
    new_open: bool,
    new_type: String,
    new_key: String,
    new_ttl: String,
    new_field: String,
    new_value: String,
    new_score: String,
    new_overwrite: bool,
    entry_field: String,
    entry_value: String,
    entry_member: String,
    entry_score: String,
    entry_index: String,
    stream_id: String,
    stream_field: String,
    stream_value: String,
    hash_query: String,
    set_query: String,
    stream_focus: Option<usize>,
    string_viewer: Option<Entity<FormatViewer>>,
    string_sub: Option<Subscription>,
    viewer_key: String,
    reload_viewer: bool,
    /// 集合条目级格式化查看器（Hash / List / Set / ZSet / Stream 选中项自动识别 JSON/XML/Hex）。
    entry_focus: Option<usize>,
    entry_viewer: Option<Entity<FormatViewer>>,
    entry_viewer_tag: String,
    context_menu: Option<ContextMenuState>,
    /// 集合条目的添加 / 编辑表单。平时收起，不占值区高度。
    entry_form_open: bool,
    entry_target: Option<EntryTarget>,
    entry_viewer_sub: Option<Subscription>,
    /// 下一帧要聚焦的输入框（打开 TTL / 重命名 / 新建键 / 条目表单时把光标放进去）。
    focus_request: Option<Field>,
    /// 键列表（虚拟列表）的滚动句柄：叠在列表右侧的可见滚动条与它同步。
    key_scroll: gpui_kit::UniformListScrollHandle,
    /// 键列表的焦点（回车在新标签中打开选中的键）。
    key_focus: Option<gpui_kit::FocusHandle>,
    /// 正在读取选中的键（与写入的 busy 分开：读取中仍可切换到别的键）。
    reading: bool,
    /// 读取的代数：只有最后一次选键的结果落地，旧响应丢弃。
    read_generation: u64,
    /// `detail` 属于哪个键（换键时旧内容保留到新值到达）。
    detail_key: String,
    /// 读取时发现已不存在的键：列表里画删除线。
    gone: std::collections::HashSet<String>,
    /// 只读表格：条目表与诊断页。
    tables: HashMap<String, tables::TableSlot>,
    /// 条目表的行 → 条目下标（筛选、排序之后）。
    entry_rows: Vec<usize>,
    /// 条目表当前对应的详情：换了详情（换键或重读）时，表里的选中重新对齐到当前条目。
    entry_rows_detail: Option<Arc<RedisKeyDetail>>,
    /// 条目表当前对应的（键, 类型, 筛选, 排序）：同一视图下只多了一页时按追加处理，保持滚动与选中。
    entry_rows_view: Option<(String, String, String, u8)>,
    /// 条目表当前详情已读到的条目数：同一视图下条目数变多，即为“加载更多”的追加页。
    entry_rows_loaded: usize,
    /// 「加载更多」落地的追加页：(追加前的详情, 追加后的详情)。只有它能让条目表按追加处理；
    /// 其它任何换详情（换键、刷新、写后重读）都按重建处理，选中重新对齐到当前条目。
    entry_append: Option<(Arc<RedisKeyDetail>, Arc<RedisKeyDetail>)>,
    /// 条目表首列宽的测量缓存：(数据戳, 宽度)。
    entry_first_w: Option<(u64, f32)>,
    /// 「加载更多」失败的原因：页脚显示并给出重试，不把整个值区换成错误态。
    value_more_error: Option<String>,
    /// 条目表排序：0 原顺序，1 升序，2 降序（按主列）。
    entry_sort: u8,
    /// 值区分隔条拖动：(起点 x, 起始宽度)。
    value_drag_start: Option<(f32, f32)>,
    /// 写回前核对的代数：只认最后一次。
    guard_generation: u64,
    /// 上一次失败是「值已被别人修改」：错误条给出刷新。
    stale_value: bool,
    /// 键名截断缓存：(键名, 可用宽度) → 显示文本。
    fit_cache: HashMap<(String, u32), tables::FittedKey>,
    /// 生成 `fit_cache` 时的分隔符：分隔符一变（改偏好、换连接载入偏好），拆分结果全部作废。
    fit_cache_seps: Vec<String>,
    /// 诊断信息页收起的分组。
    info_query: String,
    /// 请求外壳在新标签中固定打开的键（规格 `连接|名称|库|键`）。
    open_requests: Vec<String>,
}

impl RedisPane {
    pub fn new(services: Arc<AppServices>) -> Self {
        Self {
            services,
            locale: Locale::ZhCn,
            connection_id: String::new(),
            connection_name: String::new(),
            database: "0".into(),
            db_picker_open: false,
            type_picker_open: false,
            db_counts: Vec::new(),
            sidebar_w: SIDEBAR_DEFAULT_W,
            sidebar_drag_start: None,
            pattern: String::new(),
            scanned_pattern: String::new(),
            keys: Vec::new(),
            key_pos: HashMap::new(),
            done: false,
            scan: ScanState::default(),
            epoch: 0,
            page: Page::Keys,
            topology: Topology::default(),
            prefs: KeyspacePrefs::default(),
            prefs_open: false,
            sep_input: String::new(),
            cap_input: String::new(),
            view_mode: ViewMode::Flat,
            rows_cache: None,
            templates_cache: None,
            open_templates: BTreeSet::new(),
            template_memory: HashMap::new(),
            batch: None,
            batch_keys: Vec::new(),
            batch_ttl_for: None,
            batch_ttl_input: String::new(),
            placement: None,
            value_loading: false,
            prod_target: false,
            confirm_typed: String::new(),
            confirm_typed_error: false,
            diag: DiagState::default(),
            selected: String::new(),
            detail: None,
            ttl_input: String::new(),
            ttl_open: false,
            rename_to: String::new(),
            rename_open: false,
            command_input: String::new(),
            reply: String::new(),
            console_log: VecDeque::new(),
            console_pending: None,
            console_scroll: gpui_kit::ScrollHandle::new(),
            note: String::new(),
            error: String::new(),
            confirm: None,
            confirm_prompt: String::new(),
            busy: false,
            generation: 0,
            copied: String::new(),
            follow: Follow::None,
            pending_ttl: None,
            ttl_was_persist: false,
            focused: None,
            fields: Fields::default(),
            type_filter: String::new(),
            nav_choice: None,
            viewport_w: 1200.,
            pane_w: Rc::new(std::cell::Cell::new(None)),
            error_source: ErrorSource::Command,
            expanded: BTreeSet::new(),
            complete_open: false,
            complete_index: 0,
            history: Vec::new(),
            history_at: None,
            history_draft: String::new(),
            new_open: false,
            new_type: "string".into(),
            new_key: String::new(),
            new_ttl: String::new(),
            new_field: String::new(),
            new_value: String::new(),
            new_score: String::new(),
            new_overwrite: false,
            entry_field: String::new(),
            entry_value: String::new(),
            entry_member: String::new(),
            entry_score: String::new(),
            entry_index: String::new(),
            stream_id: String::new(),
            stream_field: String::new(),
            stream_value: String::new(),
            hash_query: String::new(),
            set_query: String::new(),
            stream_focus: None,
            string_viewer: None,
            string_sub: None,
            viewer_key: String::new(),
            reload_viewer: false,
            entry_focus: None,
            entry_viewer: None,
            entry_viewer_tag: String::new(),
            context_menu: None,
            entry_form_open: false,
            entry_target: None,
            entry_viewer_sub: None,
            focus_request: None,
            key_scroll: gpui_kit::UniformListScrollHandle::new(),
            key_focus: None,
            reading: false,
            read_generation: 0,
            detail_key: String::new(),
            gone: std::collections::HashSet::new(),
            tables: HashMap::new(),
            entry_rows: Vec::new(),
            entry_rows_detail: None,
            entry_rows_view: None,
            entry_rows_loaded: 0,
            entry_append: None,
            entry_first_w: None,
            value_more_error: None,
            entry_sort: 0,
            value_drag_start: None,
            guard_generation: 0,
            stale_value: false,
            fit_cache: HashMap::new(),
            fit_cache_seps: Vec::new(),
            info_query: String::new(),
            open_requests: Vec::new(),
        }
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
    }

    /// 有前台操作或后台扫描在进行。
    pub fn running(&self) -> bool {
        self.busy || self.scan.running || self.reading
    }

    /// 正在读取选中的键。
    pub fn reading(&self) -> bool {
        self.reading
    }

    /// 读取时发现已不存在的键。
    pub fn key_gone(&self, name: &str) -> bool {
        self.gone.contains(name)
    }

    /// 外壳取走「在新标签中固定打开」的请求（规格 `连接|名称|库|键`）。
    pub fn take_open_requests(&mut self) -> Vec<String> {
        std::mem::take(&mut self.open_requests)
    }

    /// 外壳按环境标签告诉面板目标是不是生产库：生产库的每次写入确认都要输入连接名。
    pub fn set_prod_target(&mut self, prod: bool) {
        self.prod_target = prod;
    }

    pub fn prod_target(&self) -> bool {
        self.prod_target
    }

    pub fn page(&self) -> Page {
        self.page
    }

    pub fn scan_running(&self) -> bool {
        self.scan.running
    }

    pub fn scan_capped(&self) -> bool {
        self.scan.capped
    }

    pub fn topology(&self) -> &Topology {
        &self.topology
    }

    /// 确认框里要求输入的文本（生产库）：连接名，没有名字时用连接 id。
    fn confirm_expected(&self) -> String {
        if self.connection_name.trim().is_empty() { self.connection_id.clone() } else { self.connection_name.trim().to_owned() }
    }

    /// 确认按钮可用：非生产库直接可用；生产库要求输入与连接名一致。
    fn confirm_ready(&self) -> bool {
        confirm_gate(self.prod_target, &self.confirm_typed, &self.confirm_expected())
    }

    pub fn set_pattern(&mut self, value: &str) {
        self.pattern = value.to_owned();
    }

    pub fn set_ttl(&mut self, value: &str) {
        self.ttl_input = value.to_owned();
        self.ttl_open = true;
    }

    pub fn set_rename(&mut self, value: &str) {
        self.rename_to = value.to_owned();
        self.rename_open = true;
    }

    /// 填命令（测试与「填入命令」用）：命令只在控制台页里，所以顺带切过去。
    pub fn set_command(&mut self, value: &str) {
        self.command_input = value.to_owned();
        self.page = Page::Console;
    }

    /// 控制台最后一条记录是失败时的输出。
    pub fn console_error(&self) -> Option<&str> {
        self.console_log.back().filter(|entry| !entry.ok).map(|entry| entry.output.as_str())
    }

    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    pub fn command_input(&self) -> &str {
        &self.command_input
    }

    pub fn ttl_input(&self) -> &str {
        &self.ttl_input
    }

    pub fn rename_to(&self) -> &str {
        &self.rename_to
    }

    pub fn selected(&self) -> &str {
        &self.selected
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    pub fn keys(&self) -> &[RedisKeyInfo] {
        &self.keys
    }

    pub fn error(&self) -> &str {
        &self.error
    }

    pub fn note(&self) -> &str {
        &self.note
    }

    pub fn reply(&self) -> &str {
        &self.reply
    }

    pub fn scan_done(&self) -> bool {
        self.done
    }

    pub fn detail_ttl(&self) -> Option<i64> {
        self.detail.as_ref().and_then(|detail| detail.ttl_ms)
    }

    #[cfg(test)]
    pub fn plant_key(&mut self, name: &str) {
        self.connection_id = "missing".into();
        self.connection_name = "缓存".into();
        self.database = "0".into();
        self.set_keys(vec![RedisKeyInfo { name: name.into(), key_type: "string".into(), ttl_ms: Some(-1) }]);
        self.selected = name.into();
        self.detail_key = name.into();
        self.detail = Some(Arc::new(RedisKeyDetail {
            key_type: "string".into(),
            ttl_ms: Some(-1),
            value: RedisValue::String { text: "hello".into() },
            total: Some(5),
            next: None,
            memory: None,
            encoding: None,
        }));
    }

    pub fn copied(&self) -> &str {
        &self.copied
    }

    pub fn confirm_open(&self) -> bool {
        self.confirm.is_some()
    }

    pub fn confirm_prompt(&self) -> &str {
        &self.confirm_prompt
    }

    pub fn detail_text(&self) -> String {
        self.detail.as_ref().map(|detail| format!("{} {}", detail.key_type, format_value(&detail.value, self.locale))).unwrap_or_default()
    }

    fn reset_target_state(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.epoch = self.epoch.wrapping_add(1);
        self.stop_background();
        self.scan = ScanState::default();
        self.page = Page::Keys;
        self.topology = Topology::default();
        self.prefs = KeyspacePrefs::default();
        self.prefs_open = false;
        self.sep_input.clear();
        self.cap_input.clear();
        self.view_mode = ViewMode::Flat;
        self.open_templates.clear();
        self.template_memory.clear();
        self.batch = None;
        self.batch_keys.clear();
        self.batch_ttl_for = None;
        self.batch_ttl_input.clear();
        self.placement = None;
        self.value_loading = false;
        self.confirm_typed.clear();
        self.confirm_typed_error = false;
        self.diag = DiagState::default();
        self.busy = false;
        self.connection_id.clear();
        self.connection_name.clear();
        self.database = "0".to_owned();
        self.db_picker_open = false;
        self.db_counts.clear();
        self.pattern.clear();
        self.scanned_pattern.clear();
        self.set_keys(Vec::new());
        self.done = false;
        self.selected.clear();
        self.detail = None;
        self.ttl_input.clear();
        self.ttl_open = false;
        self.rename_to.clear();
        self.rename_open = false;
        self.command_input.clear();
        self.reply.clear();
        self.console_log.clear();
        self.console_pending = None;
        self.copied.clear();
        self.note.clear();
        self.error.clear();
        self.confirm = None;
        self.confirm_prompt.clear();
        self.pending_ttl = None;
        self.ttl_was_persist = false;
        self.follow = Follow::None;
        self.fields = Fields::default();
        self.type_filter.clear();
        self.nav_choice = None;
        self.error_source = ErrorSource::Command;
        self.expanded.clear();
        self.complete_open = false;
        self.complete_index = 0;
        self.history.clear();
        self.history_at = None;
        self.history_draft.clear();
        self.new_open = false;
        self.new_type = "string".into();
        self.new_key.clear();
        self.new_ttl.clear();
        self.new_field.clear();
        self.new_value.clear();
        self.new_score.clear();
        self.new_overwrite = false;
        self.clear_entry_drafts();
        self.hash_query.clear();
        self.set_query.clear();
        self.stream_focus = None;
        self.string_viewer = None;
        self.string_sub = None;
        self.viewer_key.clear();
        self.reload_viewer = false;
        self.entry_focus = None;
        self.entry_viewer = None;
        self.entry_viewer_tag.clear();
        self.context_menu = None;
        self.entry_form_open = false;
        self.entry_target = None;
        self.entry_viewer_sub = None;
        self.focus_request = None;
        self.reading = false;
        self.read_generation = self.read_generation.wrapping_add(1);
        self.detail_key.clear();
        self.gone.clear();
        self.tables.clear();
        self.entry_rows.clear();
        self.entry_rows_detail = None;
        self.entry_rows_view = None;
        self.entry_rows_loaded = 0;
        self.entry_append = None;
        self.entry_first_w = None;
        self.value_more_error = None;
        self.entry_sort = 0;
        self.value_drag_start = None;
        self.guard_generation = self.guard_generation.wrapping_add(1);
        self.stale_value = false;
        self.info_query.clear();
    }

    fn clear_entry_drafts(&mut self) {
        self.entry_field.clear();
        self.entry_value.clear();
        self.entry_member.clear();
        self.entry_score.clear();
        self.entry_index.clear();
        self.stream_id.clear();
        self.stream_field.clear();
        self.stream_value.clear();
    }

    pub fn open_spec(&mut self, spec: &str, cx: &mut Context<Self>) {
        self.reset_target_state();
        let Some((id, name, database)) = parse_spec(spec) else {
            self.show_error_from(ErrorSource::Scan, self.text("redis.tab.needConnection"));
            cx.notify();
            return;
        };
        self.connection_id = id;
        self.connection_name = name;
        self.database = database.trim_start_matches("db").to_owned();
        if self.database.is_empty() {
            self.database = "0".into();
        }
        self.load_context(cx);
        // 带键的规格（新标签中固定打开某个键）：直接读这个键，不等扫描。
        if let Some(key) = spec_key(spec) {
            self.select_key(key, cx);
        }
    }

    /// 双击、回车、「在新标签中固定打开」：先在当前视图里选中这个键，同时把固定标签的请求留给外壳
    /// （`take_open_requests`，格式 `连接|连接名|库|键`）。外壳观察到通知后取走请求，新建固定的 Redis 标签并直接读这个键。
    fn request_open_tab(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.connection_id.is_empty() || name.is_empty() || self.confirm.is_some() {
            return;
        }
        self.select_key(name.to_owned(), cx);
        self.open_requests.push(format!("{}|{}|{}|{}", self.connection_id, self.connection_name, self.database, name));
        cx.notify();
    }

    /// 打开目标时：先读本连接的键空间偏好与拓扑，再开始后台扫描（分隔符、上限都按偏好）。
    fn load_context(&mut self, cx: &mut Context<Self>) {
        if self.connection_id.is_empty() {
            return;
        }
        // 会话没打开：直接走扫描，由它就地报“还没有打开”，不去读偏好。
        let Some((handle, _)) = self.services.connections.redis_handle(&self.connection_id) else {
            self.scan_from_start(cx);
            return;
        };
        self.topology = handle.topology();
        if self.topology.cluster {
            self.database = "0".into();
        }
        if let Some(text) = self.fallback_text() {
            self.note = text;
        }
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.scan.running = true;
        self.run_bg(
            cx,
            async move { crate::application::redis::load_prefs(connections.as_ref(), &connection_id).await },
            |pane, prefs, cx| {
                pane.scan.running = false;
                pane.prefs = prefs;
                pane.invalidate_rows();
                pane.refresh_keyspace_counts(cx);
                pane.scan_from_start(cx);
            },
        );
    }

    /// 后台任务：结果只在目标没换（epoch 一致）时交回，不占用前台 busy。
    fn run_bg<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl std::future::Future<Output = T> + Send + 'static,
        finish: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    ) {
        let epoch = self.epoch;
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let _ = tx.send(work.await);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |pane, cx| {
                            if pane.epoch == epoch {
                                finish(pane, result, cx);
                                cx.notify();
                            }
                        });
                    }
                });
            }
        })
        .detach();
    }

    /// 后台流：工作端经 `send` 推事件，界面端把已到达的事件合并处理。目标换了或接收方不在时工作端停下。
    fn run_stream<E: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce(Box<dyn Fn(E) -> bool + Send + Sync>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + 'static,
        apply: fn(&mut Self, Vec<E>, &mut Context<Self>),
    ) {
        let epoch = self.epoch;
        let view = cx.entity().downgrade();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<E>();
        crate::app::views::grid::runtime_for_views().spawn(async move {
            work(Box::new(move |event| tx.send(event).is_ok())).await;
        });
        cx.spawn(async move |_, cx| {
            while let Some(first) = rx.recv().await {
                // 把已经到达的事件一起处理；界面按帧合并重绘，不必在这里等待。
                let mut events = vec![first];
                while let Ok(event) = rx.try_recv() {
                    events.push(event);
                }
                let alive = cx.update(|cx| {
                    let Some(view) = view.upgrade() else { return false };
                    view.update(cx, |pane, cx| {
                        if pane.epoch != epoch {
                            return false;
                        }
                        apply(pane, events, cx);
                        cx.notify();
                        true
                    })
                });
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// 停下后台扫描、大键扫描与订阅（换目标或关闭时）。
    fn stop_background(&mut self) {
        self.scan.cancel.store(true, Ordering::SeqCst);
        if let Some(cancel) = &self.diag.big_cancel {
            cancel.store(true, Ordering::SeqCst);
        }
        if let Some(cancel) = &self.diag.sub_cancel {
            cancel.store(true, Ordering::SeqCst);
        }
        if let Some(batch) = &self.batch {
            batch.cancel.store(true, Ordering::SeqCst);
        }
    }

    /// 集群退回单节点会话时的说明。
    pub(super) fn fallback_text(&self) -> Option<String> {
        match self.topology.fallback.as_ref()? {
            ClusterFallback::Tunnel => Some(self.text("redis.cluster.fallbackTunnel")),
            ClusterFallback::Unreachable(reason) => Some(i18n::fill(&self.text("redis.cluster.fallbackUnreachable"), &[("reason", reason)])),
        }
    }

    fn refresh_keyspace_counts(&mut self, cx: &mut Context<Self>) {
        if self.connection_id.is_empty() {
            return;
        }
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.run_bg(
            cx,
            async move { crate::application::redis::list_keyspaces(connections.as_ref(), &connection_id).await },
            |pane, result, _| {
                if let Ok(counts) = result {
                    pane.db_counts = counts;
                }
            },
        );
    }

    /// 集群（含退回单节点浏览的集群节点）只有 0 号库，切到其他库服务端会拒绝。
    fn db_zero_only(&self) -> bool {
        self.topology.cluster || self.topology.fallback.is_some()
    }

    fn switch_db(&mut self, db: u8, cx: &mut Context<Self>) {
        self.db_picker_open = false;
        self.context_menu = None;
        let target_db = db.to_string();
        if self.connection_id.is_empty() {
            self.database = target_db;
            return;
        }
        if self.db_zero_only() && db != 0 {
            self.show_error_from(ErrorSource::Scan, self.text("redis.cluster.dbOnlyZero"));
            return;
        }
        // 批量操作结束后要按发起库更新键索引；进行中不切库（可先取消）。
        if self.batch_running() {
            self.show_error_from(ErrorSource::Scan, self.text("redis.batch.switchBlocked"));
            return;
        }
        self.scan.cancel.store(true, Ordering::SeqCst);
        self.scan.running = false;
        self.database = target_db;
        self.set_keys(Vec::new());
        self.done = false;
        self.selected.clear();
        self.detail = None;
        self.detail_key.clear();
        self.reading = false;
        self.read_generation = self.read_generation.wrapping_add(1);
        self.placement = None;
        self.entry_focus = None;
        self.entry_viewer_tag.clear();
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.spawn(
            cx,
            true,
            async move { crate::application::redis::select_db(connections.as_ref(), &connection_id, db).await },
            Self::finish_switch_db,
        );
    }

    fn finish_switch_db(&mut self, result: Result<Vec<KeyspaceDb>, String>, cx: &mut Context<Self>) {
        match result {
            Ok(counts) => {
                self.db_counts = counts;
                self.scan_from_start(cx);
            }
            Err(message) => self.show_error_from(ErrorSource::Scan, message),
        }
    }

    /// 复制一段值。空字符串也是值（R20）：剪贴板得到空文本，提示说清复制的是空字符串。
    fn copy_text(&mut self, text: String, note_key: &str, cx: &mut Context<Self>) {
        if text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
            self.copied = String::new();
            self.note = self.text("redis.copied.empty");
            self.error.clear();
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.copied = text;
        self.note = self.text(note_key);
        self.error.clear();
    }

    fn current_completions(&self) -> (usize, usize, Vec<RedisCompletion>) {
        if self.page != Page::Console || !self.complete_open || self.command_input.trim().is_empty() {
            return (0, 0, Vec::new());
        }
        // 键名补全只取索引前一部分：全量索引可达数十万键，逐帧复制会拖慢输入。
        let known_keys: Vec<String> = self.keys.iter().take(5000).map(|k| k.name.clone()).collect();
        let (start, end, items) =
            complete::complete_command_line(&self.command_input, self.command_input.len(), &known_keys, self.locale);
        // 若唯一候选与当前输入完全一致，则无需再弹出补全浮层。
        if items.len() == 1 && self.command_input[start..end].eq_ignore_ascii_case(&items[0].insert) {
            return (start, end, Vec::new());
        }
        (start, end, items)
    }

    fn accept_completion(&mut self, index: usize) -> bool {
        let (start, end, items) = self.current_completions();
        let Some(chosen) = items.get(index) else {
            return false;
        };
        let mut next = String::with_capacity(self.command_input.len() + chosen.insert.len() + 1);
        next.push_str(&self.command_input[..start]);
        next.push_str(&chosen.insert);
        let rest = &self.command_input[end..];
        if rest.is_empty() {
            next.push(' ');
        } else {
            next.push_str(rest);
        }
        self.command_input = next;
        self.complete_index = 0;
        // 接受命令名后保留补全开关，方便紧接着补全键名。
        self.complete_open = chosen.kind == "command";
        true
    }

    pub fn command(&mut self, id: &str, cx: &mut Context<Self>) {
        self.context_menu = None;
        if self.command_extra(id, cx) {
            cx.notify();
            return;
        }
        let local = matches!(
            id,
            "redis-cancel"
                | "redis-console"
                | "redis-console-clear"
                | "redis-console-copy"
                | "redis-view-flat"
                | "redis-view-tree"
                | "redis-new-key"
                | "redis-new-key-value"
                | "redis-entry-filter"
                | "redis-nav-toggle"
                | "redis-error-dismiss"
                | "redis-db-toggle"
                | "redis-ttl-toggle"
                | "redis-ttl-close"
                | "redis-rename-toggle"
                | "redis-rename-close"
                | "redis-copy-key"
                | "redis-use-scan"
                | "redis-entry-form"
                | "redis-entry-form-close"
                | "redis-new-close"
                | "redis-db-close"
        ) || id.starts_with("redis-type-")
            || id.starts_with("redis-hash-edit:")
            || id.starts_with("redis-zset-edit:")
            || id.starts_with("redis-list-edit:")
            || id.starts_with("redis-history-")
            || id.starts_with("redis-folder:")
            || id.starts_with("redis-folder-filter:")
            || id.starts_with("redis-folder-copy:")
            || id.starts_with("redis-folder-new:")
            || id.starts_with("redis-key-copy-name:")
            || id.starts_with("redis-key-fill-cmd:")
            || id.starts_with("redis-hash-copy:")
            || id.starts_with("redis-hash-copy-field:")
            || id.starts_with("redis-list-copy:")
            || id.starts_with("redis-set-copy:")
            || id.starts_with("redis-zset-copy:")
            || id.starts_with("redis-stream-copy:")
            || id.starts_with("redis-stream-copy-id:")
            || id.starts_with("redis-entry-inspect:")
            || id.starts_with("redis-complete-pick:")
            || id.starts_with("redis-xopen:")
            // 读取中仍可切换到别的键：旧响应按读取代数丢弃。
            || id.starts_with("redis-select:")
            || id.starts_with("redis-open-tab:")
            || id == "redis-entry-sort"
            || id.starts_with("redis-console-copy:")
            || id.starts_with("redis-console-rerun:")
            || id == "redis-inspector-toggle"
            || id == "redis-open-tab"
            || id == "redis-detail-fill-cmd"
            || id == "redis-tree-expand-all"
            || id == "redis-tree-collapse-all";

        if self.busy && !local {
            self.show_error_from(ErrorSource::Command, self.text("redis.tab.busy"));
            cx.notify();
            return;
        }

        if id == "redis-filter" || id == "redis-refresh" || id == "redis-retry-scan" {
            self.scan_from_start(cx);
        } else if id == "redis-use-scan" {
            let (_, args) = split_commandline(&self.command_input);
            let pat = args.first().map(String::as_str).unwrap_or("*");
            self.pattern = if pat == "*" { String::new() } else { pat.to_owned() };
            self.error.clear();
            self.scan_from_start(cx);
        } else if id == "redis-db-toggle" {
            self.db_picker_open = !self.db_picker_open;
            if self.db_picker_open {
                self.refresh_keyspace_counts(cx);
            }
        } else if let Some(db_str) = id.strip_prefix("redis-db-pick:") {
            if let Ok(db) = db_str.parse::<u8>() {
                self.switch_db(db, cx);
            }
        } else if id == "redis-clear-filter" {
            self.type_filter.clear();
            self.pattern.clear();
            self.scan_from_start(cx);
        } else if id == "redis-retry-read" || id == "redis-refresh-key" {
            if !self.selected.is_empty() {
                let name = self.selected.clone();
                self.select_key(name, cx);
            }
        } else if id == "redis-error-dismiss" {
            self.error.clear();
            self.error_source = ErrorSource::Command;
        } else if id == "redis-nav-toggle" {
            self.nav_choice = Some(!self.keys_visible(self.viewport_w));
        } else if id == "redis-more" {
            if !self.done && !self.scan.running && !self.scan.capped {
                self.follow = Follow::None;
                self.load_keys(cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-select:") {
            if self.confirm.is_none() {
                if let Ok(index) = index.parse::<usize>() {
                    if let Some(key) = self.keys.get(index) {
                        let name = key.name.clone();
                        self.select_key(name, cx);
                    }
                }
            }
        } else if id == "redis-ttl-toggle" {
            self.ttl_open = !self.ttl_open;
            self.rename_open = false;
            if self.ttl_open {
                self.focus_request = Some(Field::Ttl);
            }
        } else if id == "redis-ttl-close" {
            self.ttl_open = false;
        } else if id == "redis-ttl" {
            // 头部按钮只负责打开编辑条；编辑条里的同名按钮才提交，避免误点就把剩余秒数原样写回去。
            if !self.ttl_open {
                self.ttl_open = true;
                self.rename_open = false;
                self.focus_request = Some(Field::Ttl);
            } else {
                self.save_ttl(cx);
            }
        } else if let Some(preset) = id.strip_prefix("redis-ttl-preset:") {
            self.ttl_input = if preset == "persist" { String::new() } else { preset.to_owned() };
            self.ttl_open = true;
            self.save_ttl(cx);
        } else if id == "redis-rename-toggle" {
            self.rename_open = !self.rename_open;
            self.ttl_open = false;
            if self.rename_open {
                if self.rename_to.is_empty() {
                    self.rename_to = self.selected.clone();
                }
                self.focus_request = Some(Field::Rename);
            }
        } else if id == "redis-rename-close" {
            self.rename_open = false;
        } else if id == "redis-rename" {
            if !self.rename_open {
                self.rename_open = true;
                self.ttl_open = false;
                if self.rename_to.trim().is_empty() {
                    self.rename_to = self.selected.clone();
                }
                self.focus_request = Some(Field::Rename);
            } else {
                self.ask_rename();
            }
        } else if id == "redis-delete" {
            self.ask_delete();
        } else if let Some(index) = id.strip_prefix("redis-key-delete:") {
            if let Ok(index) = index.parse::<usize>() {
                if let Some(key) = self.keys.get(index) {
                    let name = key.name.clone();
                    self.selected = name.clone();
                    self.confirm_prompt = i18n::fill(&self.text("redis.tab.deletePrompt"), &[("key", &name)]);
                    self.confirm = Some(Confirm::Delete { key: name });
                    self.note.clear();
                    self.error.clear();
                }
            }
        } else if let Some(index) = id.strip_prefix("redis-key-copy-name:") {
            if let Ok(index) = index.parse::<usize>() {
                if let Some(key) = self.keys.get(index) {
                    let name = key.name.clone();
                    self.copy_text(name, "redis.copied.keyName", cx);
                }
            }
        } else if let Some(index) = id.strip_prefix("redis-key-fill-cmd:") {
            if let Ok(index) = index.parse::<usize>() {
                if let Some(key) = self.keys.get(index).cloned() {
                    self.fill_command_for_key(&key.name, &key.key_type);
                }
            }
        } else if id == "redis-detail-fill-cmd" {
            if !self.selected.is_empty() {
                let name = self.selected.clone();
                let key_type = self
                    .detail
                    .as_deref()
                    .filter(|_| self.detail_key == name)
                    .map(|detail| detail.key_type.clone())
                    .or_else(|| self.key_pos.get(&name).and_then(|&ix| self.keys.get(ix)).map(|key| key.key_type.clone()))
                    .unwrap_or_default();
                self.fill_command_for_key(&name, &key_type);
            }
        } else if id == "redis-tree-expand-all" {
            self.expand_all_folders();
        } else if id == "redis-tree-collapse-all" {
            self.expanded.clear();
            self.invalidate_rows();
        } else if id == "redis-copy-key" {
            if !self.selected.is_empty() {
                let key = self.selected.clone();
                self.copy_text(key, "redis.copied.keyName", cx);
            }
        } else if id == "redis-exec" {
            self.complete_open = false;
            self.ask_exec(cx);
        } else if let Some(idx_str) = id.strip_prefix("redis-complete-pick:") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                let _ = self.accept_completion(idx);
            }
        } else if id == "redis-confirm" {
            self.run_confirm(cx);
        } else if id == "redis-cancel" {
            self.confirm = None;
            self.confirm_prompt.clear();
            self.confirm_typed.clear();
            self.confirm_typed_error = false;
        } else if id == "redis-entry-form" {
            self.entry_form_open = !self.entry_form_open;
            if self.entry_form_open {
                // 从「添加」进来是新条目：清掉上一次编辑留下的下标，提交时不会误写回旧位置。
                self.clear_entry_drafts();
                self.focus_request = Some(self.entry_first_field());
            }
        } else if id == "redis-entry-form-close" {
            self.entry_form_open = false;
            self.clear_entry_drafts();
        } else if id == "redis-new-close" {
            self.new_open = false;
            self.new_overwrite = false;
            if self.error_source == ErrorSource::Command {
                self.error.clear();
            }
        } else if id == "redis-db-close" {
            self.db_picker_open = false;
        } else if let Some(index) = id.strip_prefix("redis-list-edit:") {
            if let Some(val) = self.list_item(index) {
                if let Ok(ix) = index.parse::<usize>() {
                    self.entry_value = val;
                    self.entry_index = ix.to_string();
                    self.entry_focus = Some(ix);
                    self.entry_form_open = true;
                    self.focus_request = Some(Field::EntryValue);
                }
            }
        } else if let Some(index) = id.strip_prefix("redis-zset-edit:") {
            if let Ok(ix) = index.parse::<usize>() {
                if let Some(RedisKeyDetail { value: RedisValue::ZSet { entries }, .. }) = self.detail.as_deref() {
                    if let Some(entry) = entries.get(ix) {
                        self.entry_member = entry.member.clone();
                        self.entry_score = entry.score.clone();
                        self.entry_focus = Some(ix);
                        self.entry_form_open = true;
                        self.focus_request = Some(Field::EntryScore);
                    }
                }
            }
        } else if id == "redis-copy" {
            self.copy_value(cx);
        } else if id == "redis-console" {
            if self.page == Page::Console {
                self.page = Page::Keys;
            } else {
                self.show_console();
            }
        } else if id == "redis-console-clear" {
            self.console_log.clear();
            self.reply.clear();
            self.note.clear();
        } else if id == "redis-console-copy" {
            let out = self.console_log.iter().map(ConsoleEntry::transcript).collect::<Vec<_>>().join("\n\n");
            self.copy_text(out, "redis.copied.value", cx);
        } else if let Some(index) = id.strip_prefix("redis-console-copy:") {
            if let Some(entry) = index.parse::<usize>().ok().and_then(|index| self.console_log.get(index)) {
                let out = entry.output.clone();
                self.copy_text(out, "redis.copied.value", cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-console-rerun:") {
            if let Some(line) = index.parse::<usize>().ok().and_then(|index| self.console_log.get(index)).map(|entry| entry.line.clone()) {
                self.command_input = line;
                self.complete_open = false;
                self.history_at = None;
                self.focus_request = Some(Field::Command);
            }
        } else if let Some(cmd) = id.strip_prefix("redis-console-quick:") {
            self.command_input = cmd.to_owned();
            self.complete_open = false;
            self.history_at = None;
            self.ask_exec(cx);
        } else if id == "redis-view-flat" {
            self.view_mode = ViewMode::Flat;
            self.invalidate_rows();
        } else if id == "redis-view-template" {
            self.view_mode = ViewMode::Template;
            self.invalidate_rows();
        } else if id == "redis-view-tree" {
            self.view_mode = ViewMode::Tree;
            if self.expanded.is_empty() {
                self.expand_top_level();
            }
            self.invalidate_rows();
        } else if id == "redis-type-toggle" {
            self.type_picker_open = !self.type_picker_open;
        } else if let Some(kind) = id.strip_prefix("redis-type-") {
            self.type_filter = if kind == "all" { String::new() } else { kind.to_owned() };
            self.type_picker_open = false;
            self.invalidate_rows();
        } else if let Some(prefix) = id.strip_prefix("redis-folder:") {
            if !self.expanded.remove(prefix) {
                self.expanded.insert(prefix.to_owned());
            }
            self.invalidate_rows();
        } else if let Some(index) = id.strip_prefix("redis-open-tab:") {
            if let Some(name) = index.parse::<usize>().ok().and_then(|index| self.keys.get(index)).map(|key| key.name.clone()) {
                self.request_open_tab(&name, cx);
            }
        } else if id == "redis-open-tab" {
            let name = self.selected.clone();
            self.request_open_tab(&name, cx);
        } else if id == "redis-entry-sort" {
            self.entry_sort = (self.entry_sort + 1) % 3;
        } else if id == "redis-inspector-toggle" {
            self.prefs.inspector_hidden = !self.prefs.inspector_hidden;
            self.value_drag_start = None;
            self.persist_prefs(cx);
        } else if id == "redis-stale-refresh" {
            self.stale_value = false;
            self.error.clear();
            if !self.selected.is_empty() {
                let name = self.selected.clone();
                self.select_key(name, cx);
            }
        } else if let Some(prefix) = id.strip_prefix("redis-folder-filter:") {
            self.pattern = keyspace::folder_pattern(prefix, &self.prefs.separators);
            self.scan_from_start(cx);
        } else if let Some(prefix) = id.strip_prefix("redis-folder-copy:") {
            self.copy_text(prefix.to_owned(), "redis.copied.keyName", cx);
        } else if let Some(prefix) = id.strip_prefix("redis-folder-new:") {
            self.nav_choice = Some(true);
            self.new_open = true;
            self.new_key = format!("{prefix}{}", self.prefs.separators.first().map(String::as_str).unwrap_or(keyspace::DEFAULT_SEPARATOR));
            self.focus_request = Some(Field::NewKey);
        } else if id == "redis-history-older" {
            self.complete_open = false;
            self.history_step(true);
        } else if id == "redis-history-newer" {
            self.complete_open = false;
            self.history_step(false);
        } else if id == "redis-new-key" || id == "redis-new-key-value" {
            self.nav_choice = Some(true);
            self.new_open = !self.new_open;
            self.db_picker_open = false;
            if self.new_open {
                self.focus_request = Some(Field::NewKey);
            }
        } else if let Some(kind) = id.strip_prefix("redis-new-type-") {
            self.new_type = kind.to_owned();
            self.new_open = true;
            if self.error_source == ErrorSource::Command {
                self.error.clear();
            }
        } else if id == "redis-create" {
            self.ask_create();
        } else if id == "redis-overwrite" {
            self.new_overwrite = true;
            self.ask_create();
        } else if id == "redis-string-save" {
            self.ask_string(cx);
        } else if let Some(idx_str) = id.strip_prefix("redis-entry-inspect:") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                self.entry_focus = Some(idx);
            }
        } else if id == "redis-hash-add" {
            self.ask_hash_set();
        } else if let Some(index) = id.strip_prefix("redis-hash-edit:") {
            self.fill_hash(index);
        } else if let Some(index) = id.strip_prefix("redis-hash-copy:") {
            self.copy_hash(index, cx);
        } else if let Some(index) = id.strip_prefix("redis-hash-copy-field:") {
            if let Some(field) = self.hash_field(index) {
                self.copy_text(field, "redis.copied.keyName", cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-hash-del:") {
            self.ask_hash_del(index);
        } else if id == "redis-list-lpush" {
            self.ask_list_push(true);
        } else if id == "redis-list-rpush" {
            self.ask_list_push(false);
        } else if let Some(index) = id.strip_prefix("redis-list-copy:") {
            if let Some(val) = self.list_item(index) {
                self.copy_text(val, "redis.copied.value", cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-list-set:") {
            // 表单开着时这是“写回”：清空后提交就是写入空字符串（R20），不能当成“重新载入原值”。
            if list_set_fills(self.entry_form_open, &self.entry_value) {
                if let Some(val) = self.list_item(index) {
                    self.entry_value = val;
                    if let Ok(ix) = index.parse::<usize>() {
                        self.entry_focus = Some(ix);
                        self.entry_index = ix.to_string();
                    }
                    self.entry_form_open = true;
                    self.focus_request = Some(Field::EntryValue);
                }
            } else {
                self.ask_list_set(index);
            }
        } else if let Some(index) = id.strip_prefix("redis-list-rem:") {
            self.ask_list_rem(index);
        } else if id == "redis-set-add" {
            self.ask_set_add();
        } else if let Some(index) = id.strip_prefix("redis-set-copy:") {
            if let Some(member) = self.set_member(index) {
                self.copy_text(member, "redis.copied.value", cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-set-rem:") {
            self.ask_set_rem(index);
        } else if id == "redis-zadd" {
            self.ask_zadd();
        } else if let Some(index) = id.strip_prefix("redis-zset-copy:") {
            if let Some(member) = self.zset_member(index) {
                self.copy_text(member, "redis.copied.value", cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-zrem:") {
            self.ask_zrem(index);
        } else if id == "redis-xadd" {
            self.ask_xadd();
        } else if let Some(index) = id.strip_prefix("redis-stream-copy:") {
            if let Some(payload) = self.stream_payload_json(index) {
                self.copy_text(payload, "redis.copied.value", cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-stream-copy-id:") {
            if let Some(msg_id) = self.stream_message(index) {
                self.copy_text(msg_id, "redis.copied.keyName", cx);
            }
        } else if let Some(index) = id.strip_prefix("redis-xopen:") {
            self.open_stream(index);
        } else if let Some(index) = id.strip_prefix("redis-xdel:") {
            self.ask_xdel(index);
        }
        cx.notify();
    }

    /// 从头扫描：清空内存键索引，按当前模式与偏好重新开始后台扫描。
    fn scan_from_start(&mut self, cx: &mut Context<Self>) {
        self.follow = Follow::None;
        self.note.clear();
        self.rescan(cx);
    }

    /// 清空索引后重扫，保留当前提示（写命令执行后的“已执行”不被扫描冲掉）。
    fn rescan(&mut self, cx: &mut Context<Self>) {
        self.scanned_pattern = self.pattern.trim().to_owned();
        self.set_keys(Vec::new());
        self.done = false;
        self.scan.resume = ScanResume::default();
        self.scan.total = None;
        self.load_keys(cx);
    }

    /// 开始（或从续扫位置继续）后台扫描。已读结果保留；达到上限时停下并提示改用模式搜索。
    fn load_keys(&mut self, cx: &mut Context<Self>) {
        if self.connection_id.is_empty() {
            self.show_error_from(ErrorSource::Scan, self.text("redis.tab.needConnection"));
            return;
        }
        self.scan.cancel.store(true, Ordering::SeqCst);
        let cancel = Arc::new(AtomicBool::new(false));
        self.scan.cancel = Arc::clone(&cancel);
        self.scan.epoch = self.scan.epoch.wrapping_add(1);
        self.scan.capped = false;
        self.scan.cancelled = false;
        if self.error_source == ErrorSource::Scan {
            self.error.clear();
        }
        let budget = self.prefs.scan_cap.saturating_sub(self.keys.len());
        if budget == 0 {
            self.scan.capped = true;
            self.scan.running = false;
            return;
        }
        self.scan.running = true;
        let scan_epoch = self.scan.epoch;
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let resume = self.scan.resume.clone();
        let pattern = {
            let trimmed = self.pattern.trim();
            if trimmed.is_empty() { None } else { Some(trimmed.to_owned()) }
        };
        let options = ScanOptions { pattern, count: self.prefs.scan_count, budget };
        self.run_stream(
            cx,
            move |send| {
                Box::pin(async move {
                    crate::application::redis::scan_keyspace(connections.as_ref(), &connection_id, resume, options, cancel, move |event| {
                        send((scan_epoch, event))
                    })
                    .await;
                })
            },
            Self::apply_scan_events,
        );
    }

    fn apply_scan_events(&mut self, events: Vec<(u64, ScanEvent)>, cx: &mut Context<Self>) {
        let mut added = false;
        for (epoch, event) in events {
            if epoch != self.scan.epoch {
                continue;
            }
            match event {
                ScanEvent::Total(total) => self.scan.total = total,
                ScanEvent::Batch(keys) => {
                    self.absorb_keys(keys);
                    added = true;
                }
                ScanEvent::Finished { resume, cancelled, capped } => {
                    self.scan.running = false;
                    self.done = resume.finished();
                    self.scan.resume = resume;
                    self.scan.cancelled = cancelled;
                    self.scan.capped = capped;
                    if self.follow == Follow::Read && !self.selected.is_empty() {
                        self.follow = Follow::None;
                        let name = self.selected.clone();
                        self.select_key(name, cx);
                    }
                }
                ScanEvent::Failed(message) => self.show_error_from(ErrorSource::Scan, message),
            }
        }
        if added && self.view_mode == ViewMode::Tree && self.expanded.is_empty() {
            self.expand_top_level();
        }
    }

    /// 取消后台扫描：已读结果保留，可从续扫位置继续。
    fn cancel_scan(&mut self) {
        if !self.scan.running {
            return;
        }
        self.scan.cancel.store(true, Ordering::SeqCst);
        self.scan.running = false;
        self.scan.cancelled = true;
    }

    /// 整体替换内存键索引。
    fn set_keys(&mut self, keys: Vec<RedisKeyInfo>) {
        self.key_pos = keys.iter().enumerate().map(|(index, key)| (key.name.clone(), index)).collect();
        self.keys = keys;
        self.gone.clear();
        self.invalidate_rows();
    }

    /// 并入一批扫描结果：已有的键原位更新，新键追加。键集合（及类型、TTL）没变时不作废列表缓存。
    fn absorb_keys(&mut self, page: Vec<RedisKeyInfo>) {
        let mut changed = false;
        for item in page {
            match self.key_pos.get(&item.name) {
                Some(&index) => {
                    if self.keys[index] != item {
                        self.keys[index] = item;
                        changed = true;
                    }
                }
                None => {
                    self.key_pos.insert(item.name.clone(), self.keys.len());
                    self.keys.push(item);
                    changed = true;
                }
            }
        }
        if changed {
            self.invalidate_rows();
        }
    }

    /// 从索引里移除一批键（删除、批量删除之后）。
    fn drop_keys(&mut self, names: &std::collections::HashSet<String>) {
        let keys = std::mem::take(&mut self.keys).into_iter().filter(|key| !names.contains(&key.name)).collect();
        self.set_keys(keys);
    }

    /// 键列表、模板分组的缓存作废（键、展开、过滤、视图或分隔符变了）。
    fn invalidate_rows(&mut self) {
        self.rows_cache = None;
        self.templates_cache = None;
    }

    /// 写入之后：重读选中的键（顺带更新索引里的类型/TTL）与各库键数，不重扫整个键空间。
    fn after_write(&mut self, cx: &mut Context<Self>) {
        self.refresh_keyspace_counts(cx);
        if !self.selected.is_empty() {
            let name = self.selected.clone();
            self.select_key(name, cx);
        }
    }

    /// 选键：旧内容保留到新值到达后整体替换（不清空、不出骨架），列表也不插加载行。
    /// 读取有自己的代数：读取中可以再选别的键，旧响应直接丢弃；不占用写入的 busy。
    fn select_key(&mut self, name: String, cx: &mut Context<Self>) {
        if self.selected != name {
            // 换键时收起条目表单、清掉条目筛选：上一个键的草稿不能被误写到新键上。
            self.entry_form_open = false;
            self.clear_entry_drafts();
            self.hash_query.clear();
            self.set_query.clear();
            self.entry_sort = 0;
            self.ttl_open = false;
            self.rename_open = false;
            self.rename_to.clear();
        }
        self.selected = name.clone();
        self.value_loading = false;
        self.value_more_error = None;
        self.placement = self.topology.cluster.then(|| cluster::placement(&self.topology.nodes, &name));
        self.stream_focus = None;
        if self.viewport_w < kit::NARROW_PAGE_WIDTH {
            self.nav_choice = Some(false);
        }
        if self.connection_id.is_empty() {
            self.show_error_from(ErrorSource::Detail, self.text("redis.tab.needConnection"));
            return;
        }
        if self.error_source == ErrorSource::Detail {
            self.error.clear();
        }
        let generation = self.begin_read();
        let epoch = self.epoch;
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        let view = cx.entity().downgrade();
        let key = name.clone();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let _ = tx.send(crate::application::redis::read_key(connections.as_ref(), &connection_id, key).await);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |pane, cx| {
                            if pane.epoch == epoch && pane.deliver_read(generation, name, result, cx) {
                                cx.notify();
                            }
                        });
                    }
                });
            }
        })
        .detach();
    }

    /// 开始一次读取：代数加一，之前还没回来的读取全部作废。
    fn begin_read(&mut self) -> u64 {
        self.read_generation = self.read_generation.wrapping_add(1);
        self.reading = true;
        self.read_generation
    }

    /// 读取结果落地；不是最后一次选键的结果直接丢弃（返回 false）。
    fn deliver_read(&mut self, generation: u64, name: String, result: Result<RedisKeyDetail, String>, cx: &mut Context<Self>) -> bool {
        if self.read_generation != generation {
            return false;
        }
        self.reading = false;
        self.finish_read(name, result, cx);
        true
    }

    fn finish_read(&mut self, name: String, result: Result<RedisKeyDetail, String>, _: &mut Context<Self>) {
        match result {
            Ok(detail) => {
                let same_key = self.detail_key == name;
                // 顺带更新内存索引里这个键的类型与 TTL；键已不存在时留在列表里画删除线，详情说明它已不存在。
                if detail.key_type == "none" {
                    self.gone.insert(name.clone());
                    self.invalidate_rows();
                } else {
                    if self.gone.remove(&name) {
                        self.invalidate_rows();
                    }
                    self.absorb_keys(vec![RedisKeyInfo { name: name.clone(), key_type: detail.key_type.clone(), ttl_ms: detail.ttl_ms }]);
                }
                self.ttl_input = match detail.ttl_ms {
                    Some(ms) if ms >= 0 => (ms / 1000).to_string(),
                    _ => String::new(),
                };
                let len = match &detail.value {
                    RedisValue::Hash { entries } => entries.len(),
                    RedisValue::List { items } | RedisValue::Set { items } => items.len(),
                    RedisValue::ZSet { entries } => entries.len(),
                    RedisValue::Stream { messages } => messages.len(),
                    _ => 0,
                };
                // 同一个键重读（写入之后）：保留选中的条目；换了键：默认选中第一行。
                self.entry_focus = match self.entry_focus {
                    Some(index) if same_key && index < len => Some(index),
                    _ if len > 0 => Some(0),
                    _ => None,
                };
                self.detail = Some(Arc::new(detail));
                self.entry_append = None;
                self.detail_key = name;
                self.value_more_error = None;
                self.reload_viewer = true;
                self.entry_viewer_tag.clear();
                if self.error_source == ErrorSource::Detail {
                    self.error.clear();
                }
            }
            Err(message) => self.show_error_from(ErrorSource::Detail, message),
        }
    }

    fn save_ttl(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.show_error(self.text("redis.tab.needKey"));
            return;
        }
        let seconds = match parse_ttl(&self.ttl_input) {
            Ok(seconds) => seconds,
            Err(()) => {
                self.show_error(self.text("redis.tab.ttlInvalid"));
                return;
            }
        };
        self.pending_ttl = seconds;
        self.ttl_was_persist = seconds.is_none();
        let key = self.selected.clone();
        let preview = match seconds {
            Some(seconds) => format!("EXPIRE {key} {seconds}"),
            None => format!("PERSIST {key}"),
        };
        self.confirm_prompt = i18n::fill(&self.text("redis.confirm.pending"), &[("command", &preview)]);
        self.confirm = Some(Confirm::Ttl { key, seconds });
        self.note.clear();
        self.error.clear();
        let _ = cx;
    }

    fn finish_ttl(&mut self, result: Result<TtlOutcome, String>, cx: &mut Context<Self>) {
        match result {
            Ok(outcome) if outcome.applied == 0 => self.show_error(self.text("redis.tab.ttlMissed")),
            Ok(_) => {
                self.ttl_open = false;
                self.note = if self.ttl_was_persist {
                    self.text("redis.tab.ttlRemoved")
                } else {
                    i18n::fill(&self.text("redis.tab.ttlSet"), &[("n", &self.pending_ttl.unwrap_or(0).to_string())])
                };
                self.error.clear();
                self.after_write(cx);
            }
            Err(message) => self.show_error(message),
        }
    }

    fn ask_delete(&mut self) {
        if self.selected.is_empty() {
            self.show_error(self.text("redis.tab.needKey"));
            return;
        }
        let key = self.selected.clone();
        self.confirm_prompt = i18n::fill(&self.text("redis.tab.deletePrompt"), &[("key", &key)]);
        self.confirm = Some(Confirm::Delete { key });
        self.note.clear();
        self.error.clear();
    }

    fn ask_rename(&mut self) {
        if self.selected.is_empty() {
            self.show_error(self.text("redis.tab.needKey"));
            return;
        }
        let from = self.selected.clone();
        let to = self.rename_to.trim().to_owned();
        if to.is_empty() {
            self.show_error(self.text("redis.tab.renameNeedName"));
            return;
        }
        self.confirm_prompt = i18n::fill(&self.text("redis.tab.renamePrompt"), &[("key", &from), ("target", &to)]);
        self.confirm = Some(Confirm::Rename { from, to });
        self.note.clear();
        self.error.clear();
    }

    fn ask_exec(&mut self, cx: &mut Context<Self>) {
        let (command, args) = split_commandline(&self.command_input);
        if command.is_empty() {
            self.show_error(self.text("redis.tab.emptyCommand"));
            return;
        }
        match exec::command_risk(&command) {
            CommandRisk::Blocked => {
                // 拒绝写在控制台记录里，紧跟这条命令。
                let line = self.command_input.clone();
                let message = self.text("redis.tab.keysRefused");
                self.note.clear();
                self.push_console(line, message, false, None);
            }
            _ if self.topology.cluster && self.crossslot_refused(&command, &args) => {}
            CommandRisk::Dangerous => {
                let line = self.command_input.clone();
                self.confirm_prompt = danger_prompt(self.locale, &command);
                self.confirm = Some(Confirm::Exec { command, args, line });
                self.note.clear();
                self.error.clear();
            }
            CommandRisk::Readonly => self.exec_command(command, args, false, self.command_input.clone(), cx),
        }
    }

    fn run_confirm(&mut self, cx: &mut Context<Self>) {
        if self.confirm.is_none() {
            return;
        }
        // 生产库：输入的连接名不一致时确认框留着，不发送任何命令。
        if !self.confirm_ready() {
            self.confirm_typed_error = true;
            return;
        }
        let action = self.confirm.take();
        self.confirm_prompt.clear();
        self.confirm_typed.clear();
        self.confirm_typed_error = false;
        match action {
            Some(Confirm::Batch { template, keys, op, db, .. }) => self.run_batch(template, keys, op, db, cx),
            Some(Confirm::SlowlogReset { node }) => self.run_slowlog_reset(node, cx),
            Some(Confirm::ClientKill { node, id, .. }) => self.run_client_kill(node, id, cx),
            Some(Confirm::Publish { channel, message }) => self.run_publish(channel, message, cx),
            Some(Confirm::Delete { key }) => self.run_delete(key, cx),
            Some(Confirm::Rename { from, to }) => self.run_rename(from, to, cx),
            Some(Confirm::Exec { command, args, line }) => self.exec_command(command, args, true, line, cx),
            Some(Confirm::Ttl { key, seconds }) => self.run_ttl(key, seconds, cx),
            Some(Confirm::Mutation { mutation, key, guard }) => self.run_mutation(mutation, key, guard, cx),
            None => {}
        }
    }

    fn run_ttl(&mut self, key: String, seconds: Option<i64>, cx: &mut Context<Self>) {
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.spawn(
            cx,
            false,
            async move { crate::application::redis::set_ttl(connections.as_ref(), &connection_id, key, seconds, true).await },
            Self::finish_ttl,
        );
    }

    fn run_mutation(&mut self, mutation: RedisMutation, key: String, guard: Option<(keys::ValueProbe, u64)>, cx: &mut Context<Self>) {
        self.selected = key;
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.spawn(
            cx,
            false,
            async move {
                match guard {
                    Some((probe, expected)) => crate::application::redis::mutate_unchanged(connections.as_ref(), &connection_id, mutation, probe, expected, true).await,
                    None => crate::application::redis::mutate(connections.as_ref(), &connection_id, mutation, true).await.map(Some),
                }
            },
            Self::finish_mutate,
        );
    }

    /// `Ok(None)`：写入前的再次核对发现值已被别人改过，什么都没写。
    fn finish_mutate(&mut self, result: Result<Option<i64>, String>, cx: &mut Context<Self>) {
        match result {
            Ok(None) => {
                self.stale_value = true;
                self.show_error(self.text("redis.value.stale"));
            }
            Ok(Some(_)) => {
                self.note = self.text("redis.tab.executed");
                self.error.clear();
                self.new_open = false;
                self.new_overwrite = false;
                self.entry_form_open = false;
                self.clear_entry_drafts();
                self.after_write(cx);
            }
            Err(message) => self.show_error(message),
        }
    }

    fn stage_mutation(&mut self, mutation: RedisMutation) {
        self.stage_guarded(mutation, None);
    }

    /// 进入写入确认；带 guard 时确认之后、写入之前还会再核对一次当前值。
    fn stage_guarded(&mut self, mutation: RedisMutation, guard: Option<(keys::ValueProbe, u64)>) {
        let preview = keys::mutation_preview(&mutation);
        let key = mutation_key(&mutation);
        self.confirm_prompt = i18n::fill(&self.text("redis.confirm.pending"), &[("command", &preview)]);
        self.confirm = Some(Confirm::Mutation { mutation, key, guard });
        self.note.clear();
        self.error.clear();
    }

    fn run_delete(&mut self, key: String, cx: &mut Context<Self>) {
        if key.is_empty() {
            self.show_error(self.text("redis.tab.needKey"));
            return;
        }
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.spawn(
            cx,
            false,
            // 删除的目标随结果一起回来：等待期间可以选别的键，完成时不能拿当时的选中去猜。
            async move { crate::application::redis::delete_keys(connections.as_ref(), &connection_id, vec![key.clone()], true).await.map(|outcome| (key, outcome)) },
            Self::finish_delete,
        );
    }

    fn finish_delete(&mut self, result: Result<(String, DeleteOutcome), String>, cx: &mut Context<Self>) {
        match result {
            Ok((key, outcome)) => {
                self.note = i18n::fill(&self.text("redis.tab.deletedCount"), &[("n", &outcome.deleted.to_string())]);
                self.error.clear();
                let gone: std::collections::HashSet<String> = std::iter::once(key.clone()).collect();
                self.drop_keys(&gone);
                if self.selected == key {
                    self.selected.clear();
                    self.placement = None;
                }
                if self.detail_key == key {
                    self.detail = None;
                    self.detail_key.clear();
                }
                self.refresh_keyspace_counts(cx);
            }
            Err(message) => self.show_error(message),
        }
    }

    fn run_rename(&mut self, from: String, to: String, cx: &mut Context<Self>) {
        if from.is_empty() || to.is_empty() {
            self.show_error(self.text("redis.tab.renameNeedName"));
            return;
        }
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.spawn(
            cx,
            false,
            // 源键和目标键随结果一起回来：等待期间可以选别的键，完成时只动这一对键。
            async move {
                crate::application::redis::rename_key(connections.as_ref(), &connection_id, from.clone(), to.clone(), true).await.map(|_| (from, to))
            },
            Self::finish_rename,
        );
    }

    fn finish_rename(&mut self, result: Result<(String, String), String>, cx: &mut Context<Self>) {
        match result {
            Ok((from, target)) => {
                self.rename_open = false;
                self.note = i18n::fill(&self.text("redis.tab.renamedNote"), &[("target", &target)]);
                self.error.clear();
                let moved = self.keys.iter().find(|key| key.name == from).map(|key| RedisKeyInfo { name: target.clone(), key_type: key.key_type.clone(), ttl_ms: key.ttl_ms });
                let gone: std::collections::HashSet<String> = std::iter::once(from.clone()).collect();
                self.drop_keys(&gone);
                if self.detail_key == from {
                    self.detail = None;
                    self.detail_key.clear();
                }
                if self.selected == from {
                    // 仍停在被改名的键上：跟到新名字并重读（重读会把新名字收进列表）。
                    self.selected = target;
                    self.rename_to.clear();
                    self.after_write(cx);
                } else {
                    // 等待期间换了键：选中和详情不动，只把新名字收进列表。
                    if let Some(info) = moved {
                        self.absorb_keys(vec![info]);
                    }
                    self.refresh_keyspace_counts(cx);
                }
            }
            Err(message) => self.show_error(message),
        }
    }

    fn exec_command(&mut self, command: String, args: Vec<String>, confirmed: bool, line: String, cx: &mut Context<Self>) {
        if command.is_empty() {
            self.show_error(self.text("redis.tab.emptyCommand"));
            return;
        }
        if let Err(message) = exec::guard_invocation(&command, &args, confirmed) {
            let refused = if exec::command_risk(&exec::invocation_word(&command, &args)) == CommandRisk::Blocked {
                self.text("redis.tab.keysRefused")
            } else {
                message
            };
            self.push_console(line, refused, false, None);
            return;
        }
        if command.eq_ignore_ascii_case("SELECT") {
            if self.db_zero_only() && args.first().is_some_and(|arg| arg.trim() != "0") {
                self.show_error(self.text("redis.cluster.dbOnlyZero"));
                return;
            }
            if self.batch_running() {
                self.show_error(self.text("redis.batch.switchBlocked"));
                return;
            }
            if let Some(arg) = args.first() {
                if let Ok(db) = arg.parse::<u8>() {
                    self.database = db.to_string();
                }
            }
        }
        self.remember_command(line.clone());
        self.console_pending = Some((line, self.database.clone(), std::time::Instant::now()));
        let connections = Arc::clone(&self.services.connections);
        let connection_id = self.connection_id.clone();
        self.spawn(
            cx,
            false,
            async move { crate::application::redis::exec(connections.as_ref(), &connection_id, command, args, confirmed).await },
            Self::finish_exec,
        );
    }

    fn finish_exec(&mut self, result: Result<ExecOutcome, String>, cx: &mut Context<Self>) {
        match result {
            Ok(outcome) => {
                self.reply = reply_text(&outcome.reply);
                let pending = self.console_pending.take();
                let elapsed = pending.as_ref().map(|(_, _, at)| at.elapsed());
                let (line, db) = pending.map(|(line, db, _)| (line, db)).unwrap_or_default();
                self.push_console_in(db, line, reply_pretty(&outcome.reply, 0), true, elapsed);
                // 和 redis-cli 一样：执行成功后输入框清空，↑ 可以取回刚才的命令。
                self.command_input.clear();
                self.history_at = None;
                self.complete_open = false;
                self.note = self.text("redis.tab.executed");
                self.error.clear();
                self.refresh_keyspace_counts(cx);
                // 写命令可能增删任意键：重扫键空间（选中的键在扫描结束后重读）；只读命令不动列表。
                if exec::command_risk(&outcome.command) == CommandRisk::Dangerous {
                    self.follow = if self.selected.is_empty() { Follow::None } else { Follow::Read };
                    self.rescan(cx);
                }
            }
            Err(message) => match self.console_pending.take() {
                // 命令出错写进控制台记录，紧跟在命令下面，不再弹到别处。
                Some((line, db, at)) => {
                    self.note.clear();
                    self.push_console_in(db, line, message, false, Some(at.elapsed()));
                }
                None => self.show_error(message),
            },
        }
    }

    /// 切到控制台页并把焦点放进命令输入框。
    fn show_console(&mut self) {
        self.page = Page::Console;
        self.focus_request = Some(Field::Command);
    }

    fn push_console(&mut self, line: String, output: String, ok: bool, elapsed: Option<std::time::Duration>) {
        let db = self.database.clone();
        self.push_console_in(db, line, output, ok, elapsed);
    }

    /// 追加一条执行记录并滚到底部；超过上限丢掉最早的。
    fn push_console_in(&mut self, db: String, line: String, output: String, ok: bool, elapsed: Option<std::time::Duration>) {
        if self.console_log.len() >= CONSOLE_LOG_MAX {
            self.console_log.pop_front();
        }
        let output = if ok { output } else { console_error_text(&output) };
        self.console_log.push_back(ConsoleEntry { db, line: line.trim().to_owned(), output, ok, elapsed });
        self.console_scroll.scroll_to_bottom();
    }

    pub fn console_log(&self) -> &VecDeque<ConsoleEntry> {
        &self.console_log
    }

    fn copy_value(&mut self, cx: &mut Context<Self>) {
        let Some(raw) = self.detail.as_ref().and_then(|detail| value_copy_text(&detail.value, self.locale)) else {
            self.show_error(self.text("redis.tab.pickKey"));
            return;
        };
        if raw.is_empty() {
            self.copy_text(raw, "redis.copied.value", cx);
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(raw.clone()));
        self.copied = raw;
        self.note = self.text("redis.copied.value");
        self.error.clear();
    }

    fn remember_command(&mut self, line: String) {
        let line = line.trim().to_owned();
        if line.is_empty() {
            return;
        }
        if self.history.last().is_some_and(|item| item == &line) {
            self.history_at = None;
            return;
        }
        self.history.push(line);
        self.history_at = None;
    }

    fn history_step(&mut self, older: bool) {
        if self.confirm.is_some() || self.history.is_empty() {
            return;
        }
        if self.history_at.is_none() {
            self.history_draft = self.command_input.clone();
        }
        if older {
            let next = self.history_at.map(|index| index.saturating_sub(1)).unwrap_or(self.history.len() - 1);
            self.history_at = Some(next);
            self.command_input = self.history[next].clone();
            return;
        }
        match self.history_at {
            Some(index) if index + 1 < self.history.len() => {
                self.history_at = Some(index + 1);
                self.command_input = self.history[index + 1].clone();
            }
            Some(_) => {
                self.history_at = None;
                self.command_input = std::mem::take(&mut self.history_draft);
            }
            None => {}
        }
    }

    /// 构建列表行：平铺只列键；树按连接配置的分隔符拆层并穿插文件夹与叶子键；模板按归并结果列出，
    /// 展开的模板下列出成员键（至多 [`TEMPLATE_MEMBERS_SHOWN`] 个）。
    fn build_key_rows(&self) -> Vec<KeyRow> {
        let filtered: Vec<(usize, &str)> = self
            .keys
            .iter()
            .enumerate()
            .filter(|(_, key)| self.type_filter.is_empty() || key.key_type == self.type_filter)
            .map(|(index, key)| (index, key.name.as_str()))
            .collect();
        match self.view_mode {
            ViewMode::Flat => filtered.into_iter().map(|(index, name)| KeyRow::Key { index, display: name.to_owned(), depth: 0 }).collect(),
            ViewMode::Tree => keyspace::build_tree(&filtered, &self.prefs.separators, &self.expanded)
                .into_iter()
                .map(|row| match row {
                    TreeRow::Folder { prefix, segment, depth, count, open } => KeyRow::Folder { prefix, segment, depth, count, open },
                    TreeRow::Key { index, display, depth } => KeyRow::Key { index, display, depth },
                })
                .collect(),
            ViewMode::Template => {
                let groups = self.build_templates();
                let mut rows = Vec::new();
                for (group, item) in groups.iter().enumerate() {
                    let open = self.open_templates.contains(&item.template);
                    rows.push(KeyRow::Template { group, open });
                    if open {
                        for &index in item.members.iter().take(TEMPLATE_MEMBERS_SHOWN) {
                            rows.push(KeyRow::Key { index, display: self.keys[index].name.clone(), depth: 1 });
                        }
                        if item.members.len() > TEMPLATE_MEMBERS_SHOWN {
                            rows.push(KeyRow::TemplateMore {
                                template: item.template.clone(),
                                shown: TEMPLATE_MEMBERS_SHOWN,
                                total: item.members.len(),
                            });
                        }
                    }
                }
                rows
            }
        }
    }

    fn build_templates(&self) -> Vec<TemplateGroup> {
        let keys: Vec<(usize, &str, &str)> = self
            .keys
            .iter()
            .enumerate()
            .filter(|(_, key)| self.type_filter.is_empty() || key.key_type == self.type_filter)
            .map(|(index, key)| (index, key.name.as_str(), key.key_type.as_str()))
            .collect();
        keyspace::group_templates(&keys, &self.prefs.separators, &self.prefs.rules)
    }

    /// 缓存的模板分组（模板视图与模板操作共用）。
    fn templates(&mut self) -> Rc<Vec<TemplateGroup>> {
        if self.templates_cache.is_none() {
            self.templates_cache = Some(Rc::new(self.build_templates()));
        }
        Rc::clone(self.templates_cache.as_ref().expect("templates"))
    }

    /// 缓存的列表行（渲染用；键或条件变了才重建）。
    fn key_rows(&mut self) -> Rc<Vec<KeyRow>> {
        if self.view_mode == ViewMode::Template {
            let _ = self.templates();
        }
        if self.rows_cache.is_none() {
            let rows = match self.view_mode {
                ViewMode::Template => {
                    let groups = self.templates();
                    let mut rows = Vec::new();
                    for (group, item) in groups.iter().enumerate() {
                        let open = self.open_templates.contains(&item.template);
                        rows.push(KeyRow::Template { group, open });
                        if open {
                            for &index in item.members.iter().take(TEMPLATE_MEMBERS_SHOWN) {
                                rows.push(KeyRow::Key { index, display: self.keys[index].name.clone(), depth: 1 });
                            }
                            if item.members.len() > TEMPLATE_MEMBERS_SHOWN {
                                rows.push(KeyRow::TemplateMore {
                                    template: item.template.clone(),
                                    shown: TEMPLATE_MEMBERS_SHOWN,
                                    total: item.members.len(),
                                });
                            }
                        }
                    }
                    rows
                }
                _ => self.build_key_rows(),
            };
            self.rows_cache = Some(Rc::new(rows));
        }
        Rc::clone(self.rows_cache.as_ref().expect("rows"))
    }

    /// 模板的成员键名。
    fn template_members(&mut self, template: &str) -> Vec<String> {
        let groups = self.templates();
        groups
            .iter()
            .find(|group| group.template == template)
            .map(|group| group.members.iter().filter_map(|&index| self.keys.get(index).map(|key| key.name.clone())).collect())
            .unwrap_or_default()
    }

    fn expand_top_level(&mut self) {
        let separators = self.prefs.separators.clone();
        for key in &self.keys {
            let segments = keyspace::split_segments(&key.name, &separators);
            if segments.len() >= 2 && !segments[0].text.is_empty() {
                self.expanded.insert(segments[0].text.to_owned());
            }
        }
        self.invalidate_rows();
    }

    fn expand_all_folders(&mut self) {
        let separators = self.prefs.separators.clone();
        for key in &self.keys {
            let segments = keyspace::split_segments(&key.name, &separators);
            if segments.len() < 2 {
                continue;
            }
            let mut prefix = String::new();
            for segment in &segments[..segments.len() - 1] {
                if !prefix.is_empty() {
                    prefix.push_str(segment.sep);
                }
                prefix.push_str(segment.text);
                if !prefix.is_empty() {
                    self.expanded.insert(prefix.clone());
                }
            }
        }
        self.invalidate_rows();
    }

    fn fill_command_for_key(&mut self, name: &str, key_type: &str) {
        let quoted = if name.chars().any(char::is_whitespace) { format!("\"{name}\"") } else { name.to_owned() };
        let cmd_str = match key_type {
            "hash" => format!("HGETALL {quoted}"),
            "list" => format!("LRANGE {quoted} 0 -1"),
            "set" => format!("SMEMBERS {quoted}"),
            "zset" => format!("ZRANGE {quoted} 0 -1 WITHSCORES"),
            "stream" => format!("XRANGE {quoted} - + COUNT 50"),
            _ => format!("GET {quoted}"),
        };
        self.show_console();
        self.command_input = cmd_str;
        self.complete_open = false;
    }

    fn ask_create(&mut self) {
        let key = self.new_key.trim().to_owned();
        if key.is_empty() {
            self.show_error(self.text("redis.key.name"));
            return;
        }
        let ttl = match parse_ttl(&self.new_ttl) {
            Ok(ttl) => ttl,
            Err(()) => {
                self.show_error(self.text("redis.tab.ttlInvalid"));
                return;
            }
        };
        let initial = match self.new_type.as_str() {
            "hash" => RedisInitialValue::Hash { field: self.new_field.clone(), value: self.new_value.clone() },
            "list" => RedisInitialValue::List(self.new_value.clone()),
            "set" => RedisInitialValue::Set(self.new_value.clone()),
            "zset" => {
                let score = match self.new_score.trim().parse::<f64>() {
                    Ok(score) if score.is_finite() => score,
                    _ => {
                        self.show_error(self.text("redis.zset.scoreInvalid"));
                        return;
                    }
                };
                RedisInitialValue::ZSet { score, member: self.new_value.clone() }
            }
            "stream" => RedisInitialValue::Stream {
                id: self.stream_id.clone(),
                fields: vec![(self.new_field.clone(), self.new_value.clone())],
            },
            _ => RedisInitialValue::String(self.new_value.clone()),
        };
        let mutation = RedisMutation::CreateKey { key: key.clone(), ttl_secs: ttl, initial, overwrite: self.new_overwrite };
        if let Err(message) = keys::prepare_mutation(&mutation, true) {
            self.show_error(message);
            return;
        }
        self.stage_mutation(mutation);
    }

    fn ask_string(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.show_error(self.text("redis.tab.needKey"));
            return;
        }
        let Some(viewer) = self.string_viewer.clone() else {
            self.show_error(self.text("redis.tab.pickKey"));
            return;
        };
        let result = viewer.update(cx, |viewer, cx| viewer.commit(cx));
        match result {
            CommitResult::Applied(value) => {
                self.stage_mutation(RedisMutation::SetString { key: self.selected.clone(), value });
            }
            CommitResult::Blocked { block, .. } => {
                let message = match block {
                    ApplyBlock::InvalidJson(error) => i18n::fill(&self.text("mongo.docs.syntaxError"), &[("message", &error.message)]),
                    _ => self.text("redis.value.none"),
                };
                self.show_error(message);
            }
        }
    }

    fn ask_hash_set(&mut self) {
        self.stage_if_key(RedisMutation::HashSet {
            key: self.selected.clone(),
            field: self.entry_field.clone(),
            value: self.entry_value.clone(),
        });
    }

    fn ask_hash_del(&mut self, index: &str) {
        let Some(field) = self.hash_field(index) else { return };
        self.stage_if_key(RedisMutation::HashDel { key: self.selected.clone(), fields: vec![field] });
    }

    fn ask_list_push(&mut self, head: bool) {
        self.stage_if_key(RedisMutation::ListPush { key: self.selected.clone(), head, value: self.entry_value.clone() });
    }

    fn ask_list_set(&mut self, index: &str) {
        let Ok(index) = index.parse::<i64>() else { return };
        self.entry_index = index.to_string();
        self.stage_if_key(RedisMutation::ListSet { key: self.selected.clone(), index, value: self.entry_value.clone() });
    }

    fn ask_list_rem(&mut self, index: &str) {
        let Some(value) = self.list_item(index) else { return };
        self.stage_if_key(RedisMutation::ListRem { key: self.selected.clone(), count: 1, value });
    }

    fn ask_set_add(&mut self) {
        self.stage_if_key(RedisMutation::SetAdd { key: self.selected.clone(), member: self.entry_member.clone() });
    }

    fn ask_set_rem(&mut self, index: &str) {
        let Some(member) = self.set_member(index) else { return };
        self.stage_if_key(RedisMutation::SetRem { key: self.selected.clone(), member });
    }

    fn ask_zadd(&mut self) {
        let score = match self.entry_score.trim().parse::<f64>() {
            Ok(score) if score.is_finite() => score,
            _ => {
                self.show_error(self.text("redis.zset.scoreInvalid"));
                return;
            }
        };
        self.stage_if_key(RedisMutation::ZSetAdd { key: self.selected.clone(), score, member: self.entry_member.clone() });
    }

    fn ask_zrem(&mut self, index: &str) {
        let Some(member) = self.zset_member(index) else { return };
        self.stage_if_key(RedisMutation::ZSetRem { key: self.selected.clone(), member });
    }

    fn ask_xadd(&mut self) {
        self.stage_if_key(RedisMutation::StreamAdd {
            key: self.selected.clone(),
            id: self.stream_id.clone(),
            fields: vec![(self.stream_field.clone(), self.stream_value.clone())],
        });
    }

    fn ask_xdel(&mut self, index: &str) {
        let Some(id) = self.stream_message(index) else { return };
        self.stage_if_key(RedisMutation::StreamDel { key: self.selected.clone(), id });
    }

    fn stage_if_key(&mut self, mutation: RedisMutation) {
        self.stage_if_key_guarded(mutation, None);
    }

    fn stage_if_key_guarded(&mut self, mutation: RedisMutation, guard: Option<(keys::ValueProbe, u64)>) {
        if self.selected.is_empty() {
            self.show_error(self.text("redis.tab.needKey"));
            return;
        }
        // 正在换键：界面上还是上一个键的内容，条目写入不能落到新键上。
        if self.reading && self.detail_key != self.selected {
            self.show_error(self.text("redis.value.reading"));
            return;
        }
        if let Err(message) = keys::prepare_mutation(&mutation, true) {
            self.show_error(message);
            return;
        }
        self.stage_guarded(mutation, guard);
    }

    fn copy_hash(&mut self, index: &str, cx: &mut Context<Self>) {
        let Ok(index) = index.parse::<usize>() else { return };
        let Some(RedisKeyDetail { value: RedisValue::Hash { entries }, .. }) = self.detail.as_deref() else { return };
        let Some(entry) = entries.get(index) else { return };
        let text = entry.value.clone();
        self.copy_text(text, "redis.copied.value", cx);
    }

    fn open_stream(&mut self, index: &str) {
        let Ok(index) = index.parse::<usize>() else { return };
        self.stream_focus = if self.stream_focus == Some(index) { None } else { Some(index) };
        self.entry_focus = Some(index);
        self.error.clear();
    }

    fn fill_hash(&mut self, index: &str) {
        let Ok(index) = index.parse::<usize>() else { return };
        if let Some(RedisKeyDetail { value: RedisValue::Hash { entries }, .. }) = self.detail.as_deref() {
            if let Some(entry) = entries.get(index) {
                self.entry_field = entry.field.clone();
                self.entry_value = entry.value.clone();
                self.entry_focus = Some(index);
                self.entry_form_open = true;
                self.focus_request = Some(Field::EntryValue);
            }
        }
    }

    /// 条目表单打开时光标先落在哪个输入框。
    fn entry_first_field(&self) -> Field {
        match self.detail.as_deref().map(|detail| &detail.value) {
            Some(RedisValue::Hash { .. }) => Field::EntryField,
            Some(RedisValue::Set { .. }) => Field::EntryMember,
            Some(RedisValue::ZSet { .. }) => Field::EntryScore,
            Some(RedisValue::Stream { .. }) => Field::StreamField,
            _ => Field::EntryValue,
        }
    }

    fn hash_field(&self, index: &str) -> Option<String> {
        let index = index.parse::<usize>().ok()?;
        match self.detail.as_deref() {
            Some(RedisKeyDetail { value: RedisValue::Hash { entries }, .. }) => entries.get(index).map(|entry| entry.field.clone()),
            _ => None,
        }
    }

    fn list_item(&self, index: &str) -> Option<String> {
        let index = index.parse::<usize>().ok()?;
        match self.detail.as_deref() {
            Some(RedisKeyDetail { value: RedisValue::List { items }, .. }) => items.get(index).cloned(),
            _ => None,
        }
    }

    fn set_member(&self, index: &str) -> Option<String> {
        let index = index.parse::<usize>().ok()?;
        match self.detail.as_deref() {
            Some(RedisKeyDetail { value: RedisValue::Set { items }, .. }) => items.get(index).cloned(),
            _ => None,
        }
    }

    fn zset_member(&self, index: &str) -> Option<String> {
        let index = index.parse::<usize>().ok()?;
        match self.detail.as_deref() {
            Some(RedisKeyDetail { value: RedisValue::ZSet { entries }, .. }) => entries.get(index).map(|entry| entry.member.clone()),
            _ => None,
        }
    }

    fn stream_message(&self, index: &str) -> Option<String> {
        let index = index.parse::<usize>().ok()?;
        match self.detail.as_deref() {
            Some(RedisKeyDetail { value: RedisValue::Stream { messages }, .. }) => messages.get(index).map(|message| message.id.clone()),
            _ => None,
        }
    }

    fn stream_payload_json(&self, index: &str) -> Option<String> {
        let index = index.parse::<usize>().ok()?;
        match self.detail.as_deref() {
            Some(RedisKeyDetail { value: RedisValue::Stream { messages }, .. }) => {
                let message = messages.get(index)?;
                let mut map = serde_json::Map::new();
                for field in &message.fields {
                    let parsed = serde_json::from_str::<serde_json::Value>(&field.value)
                        .unwrap_or_else(|_| serde_json::Value::String(field.value.clone()));
                    map.insert(field.field.clone(), parsed);
                }
                serde_json::to_string_pretty(&serde_json::Value::Object(map)).ok()
            }
            _ => None,
        }
    }

    fn show_error(&mut self, message: String) {
        self.show_error_from(ErrorSource::Command, message);
    }

    fn show_error_from(&mut self, source: ErrorSource, message: String) {
        self.error = message;
        self.error_source = source;
        self.note.clear();
    }

    fn keys_visible(&self, width: f32) -> bool {
        if width >= kit::NARROW_PAGE_WIDTH {
            return true;
        }
        self.nav_choice.unwrap_or(self.selected.is_empty())
    }

    fn text(&self, key: &str) -> String {
        i18n::text(self.locale, key)
    }

    fn spawn<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        clear_note: bool,
        work: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
        finish: fn(&mut Self, Result<T, String>, &mut Context<Self>),
    ) {
        let generation = self.begin(clear_note);
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let _ = tx.send(work.await);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |pane, cx| {
                            if pane.generation != generation {
                                return;
                            }
                            pane.busy = false;
                            finish(pane, result, cx);
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn begin(&mut self, clear_note: bool) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.busy = true;
        self.error.clear();
        if clear_note {
            self.note.clear();
        }
        self.generation
    }

    pub fn status_line(&self) -> String {
        let keys = self.keys.iter().map(|key| format!("{} {}", key.key_type, key.name)).collect::<Vec<_>>().join(" | ");
        format!(
            "{name} db={db} pattern={pattern} ttl={ttl} rename={rename} command={command} selected={selected} note={note} error={error} confirm={confirm} reply={reply} detail={detail} keys={keys}",
            name = self.connection_name,
            db = self.database,
            pattern = self.pattern,
            ttl = self.ttl_input,
            rename = self.rename_to,
            command = self.command_input,
            selected = self.selected,
            note = self.note,
            error = self.error,
            confirm = self.confirm_prompt,
            reply = self.reply,
            detail = self.detail_text().replace('\n', " "),
        )
    }

    fn open_key_context_menu(&mut self, index: usize, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.keys.get(index).is_none() {
            return;
        }
        self.context_menu = Some(ContextMenuState {
            at,
            items: vec![
                ContextItem { id: format!("redis-select:{index}"), icon: IconName::Eye, label: self.text("redis.menu.open"), danger: false },
                ContextItem { id: format!("redis-open-tab:{index}"), icon: IconName::Pin, label: self.text("redis.menu.openInTab"), danger: false },
                ContextItem { id: format!("redis-key-copy-name:{index}"), icon: IconName::Copy, label: self.text("redis.copy.keyName"), danger: false },
                ContextItem { id: format!("redis-key-fill-cmd:{index}"), icon: IconName::SquareTerminal, label: self.text("redis.menu.fillCommand"), danger: false },
                ContextItem { id: format!("redis-key-delete:{index}"), icon: IconName::Trash, label: self.text("redis.tab.remove"), danger: true },
            ],
        });
        cx.notify();
    }

    fn open_folder_context_menu(&mut self, prefix: String, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.context_menu = Some(ContextMenuState {
            at,
            items: vec![
                ContextItem { id: format!("redis-folder:{prefix}"), icon: IconName::FolderOpen, label: self.text("redis.menu.expandCollapse"), danger: false },
                ContextItem { id: "redis-tree-expand-all".into(), icon: IconName::ChevronsDown, label: self.text("redis.menu.expandAll"), danger: false },
                ContextItem { id: "redis-tree-collapse-all".into(), icon: IconName::ChevronUp, label: self.text("redis.menu.collapseAll"), danger: false },
                ContextItem { id: format!("redis-folder-filter:{prefix}"), icon: IconName::ListFilter, label: self.text("redis.menu.filterPrefix"), danger: false },
                ContextItem { id: format!("redis-folder-copy:{prefix}"), icon: IconName::Copy, label: self.text("redis.copy.prefix"), danger: false },
                ContextItem { id: format!("redis-folder-new:{prefix}"), icon: IconName::Plus, label: self.text("redis.menu.newInPrefix"), danger: false },
            ],
        });
        cx.notify();
    }

    /// 详情头部的「更多」：次要操作（窄窗口下放不下的主要操作也在这里）。
    fn open_more_menu(&mut self, at: Point<Pixels>, overflow: bool, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            return;
        }
        let mut items = Vec::new();
        if overflow {
            items.push(ContextItem { id: "redis-ttl-toggle".into(), icon: IconName::Timer, label: self.text("redis.tab.setTtl"), danger: false });
        }
        items.push(ContextItem { id: "redis-rename-toggle".into(), icon: IconName::Pencil, label: self.text("redis.tab.rename"), danger: false });
        items.push(ContextItem { id: "redis-copy".into(), icon: IconName::Copy, label: self.text("redis.copy.keyValue"), danger: false });
        items.push(ContextItem { id: "redis-copy-key".into(), icon: IconName::Tag, label: self.text("redis.copy.keyName"), danger: false });
        items.push(ContextItem { id: "redis-detail-fill-cmd".into(), icon: IconName::SquareTerminal, label: self.text("redis.menu.fillCommand"), danger: false });
        items.push(ContextItem { id: "redis-open-tab".into(), icon: IconName::Pin, label: self.text("redis.menu.openInTab"), danger: false });
        if overflow {
            items.push(ContextItem { id: "redis-delete".into(), icon: IconName::Trash, label: self.text("redis.tab.remove"), danger: true });
        }
        self.context_menu = Some(ContextMenuState { at, items });
        cx.notify();
    }
}

pub fn split_commandline(line: &str) -> (String, Vec<String>) {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in line.trim().chars() {
        match quote {
            Some(mark) if ch == mark => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    if tokens.len() >= 2 && exec::is_compound_command(&tokens[0], &tokens[1]) {
        let command = format!("{} {}", tokens[0], tokens[1]);
        return (command, tokens.into_iter().skip(2).collect());
    }
    let mut iter = tokens.into_iter();
    (iter.next().unwrap_or_default(), iter.collect())
}

#[allow(clippy::result_unit_err)]
pub fn parse_ttl(input: &str) -> Result<Option<i64>, ()> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if !trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(());
    }
    let seconds: i64 = trimmed.parse().map_err(|_| ())?;
    if seconds > 2_147_483_647 {
        return Err(());
    }
    Ok(Some(seconds))
}

fn parse_spec(spec: &str) -> Option<(String, String, String)> {
    let mut parts = spec.splitn(4, '|');
    let id = parts.next()?.trim().to_owned();
    let name = parts.next().unwrap_or("").trim().to_owned();
    let database = parts.next().unwrap_or("0").trim().to_owned();
    if id.is_empty() {
        return None;
    }
    Some((id, name, if database.is_empty() { "0".into() } else { database }))
}

/// 规格里的第四段：要直接打开的键（`连接|名称|库|键`，键名里可以有 `|`）。
fn spec_key(spec: &str) -> Option<String> {
    let key = spec.splitn(4, '|').nth(3)?;
    (!key.is_empty()).then(|| key.to_owned())
}

#[cfg(test)]
fn merge_keys(mut loaded: Vec<RedisKeyInfo>, page: Vec<RedisKeyInfo>) -> Vec<RedisKeyInfo> {
    let mut positions: std::collections::HashMap<String, usize> = loaded
        .iter()
        .enumerate()
        .map(|(index, key)| (key.name.clone(), index))
        .collect();
    for item in page {
        if let Some(index) = positions.get(&item.name).copied() {
            loaded[index] = item;
        } else {
            positions.insert(item.name.clone(), loaded.len());
            loaded.push(item);
        }
    }
    loaded
}

fn mutation_key(mutation: &RedisMutation) -> String {
    match mutation {
        RedisMutation::CreateKey { key, .. }
        | RedisMutation::SetString { key, .. }
        | RedisMutation::HashSet { key, .. }
        | RedisMutation::HashDel { key, .. }
        | RedisMutation::ListPush { key, .. }
        | RedisMutation::ListSet { key, .. }
        | RedisMutation::ListRem { key, .. }
        | RedisMutation::SetAdd { key, .. }
        | RedisMutation::SetRem { key, .. }
        | RedisMutation::ZSetAdd { key, .. }
        | RedisMutation::ZSetRem { key, .. }
        | RedisMutation::StreamAdd { key, .. }
        | RedisMutation::StreamDel { key, .. }
        | RedisMutation::SetTtl { key, .. }
        | RedisMutation::JsonSet { key, .. } => key.clone(),
    }
}

fn danger_prompt(locale: Locale, word: &str) -> String {
    let upper = word.to_ascii_uppercase();
    let key = match upper.as_str() {
        "FLUSHDB" => "redis.danger.flushdb",
        "FLUSHALL" => "redis.danger.flushall",
        "SHUTDOWN" => "redis.danger.shutdown",
        _ => "redis.danger.generic",
    };
    i18n::fill(&i18n::text(locale, key), &[("word", &upper)])
}

fn format_value(value: &RedisValue, locale: Locale) -> String {
    if matches!(value, RedisValue::None {}) {
        return i18n::text(locale, "redis.value.none");
    }
    if value_empty(value) {
        return i18n::text(locale, "redis.value.empty");
    }
    match value {
        RedisValue::String { text } | RedisValue::Json { text } => text.clone(),
        RedisValue::Hash { entries } => entries.iter().map(|entry| format!("{}={}", entry.field, entry.value)).collect::<Vec<_>>().join("\n"),
        RedisValue::List { items } | RedisValue::Set { items } => items.join("\n"),
        RedisValue::ZSet { entries } => entries.iter().map(|entry| format!("{} {}", entry.member, entry.score)).collect::<Vec<_>>().join("\n"),
        RedisValue::Stream { messages } => messages
            .iter()
            .map(|message| {
                let fields = message.fields.iter().map(|field| format!("{}={}", field.field, field.value)).collect::<Vec<_>>().join(" ");
                format!("{} {fields}", message.id)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        RedisValue::None {} => String::new(),
    }
}

/// 值区域里一格要怎么画（R20）：空字符串画成弱化的“空”提示，不当成普通数据，也不和“没有内容”混淆。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ValueCell {
    Empty,
    Text(String),
}

pub(crate) fn value_cell(text: &str) -> ValueCell {
    if text.is_empty() { ValueCell::Empty } else { ValueCell::Text(one_line(text)) }
}

/// “复制值”得到的文本：字符串原样（空字符串就是空文本，不是“这个键没有内容”这句说明）；
/// 集合按行展开；键不存在时没有可复制的值。
pub(crate) fn value_copy_text(value: &RedisValue, locale: Locale) -> Option<String> {
    match value {
        RedisValue::None {} => None,
        RedisValue::String { text } | RedisValue::Json { text } => Some(text.clone()),
        other => Some(format_value(other, locale)),
    }
}

/// 列表条目的“设置”按钮：表单没打开时先载入原值；表单打开时就是提交，空文本也照样写回。
pub(crate) fn list_set_fills(form_open: bool, draft: &str) -> bool {
    !form_open && draft.trim().is_empty()
}

fn value_empty(value: &RedisValue) -> bool {
    match value {
        // 空字符串是一个值，不是“没有内容”。
        RedisValue::String { .. } | RedisValue::Json { .. } => false,
        RedisValue::Hash { entries } => entries.is_empty(),
        RedisValue::List { items } | RedisValue::Set { items } => items.is_empty(),
        RedisValue::ZSet { entries } => entries.is_empty(),
        RedisValue::Stream { messages } => messages.is_empty(),
        RedisValue::None {} => false,
    }
}

fn reply_text(reply: &RespReply) -> String {
    match reply.kind {
        "nil" => "nil".to_owned(),
        "int" => reply.int.unwrap_or_default().to_string(),
        "array" => reply.items.as_ref().map(|items| items.iter().map(reply_text).collect::<Vec<_>>().join(" ")).unwrap_or_default(),
        _ => reply.text.clone().unwrap_or_default(),
    }
}

fn reply_pretty(reply: &RespReply, indent: usize) -> String {
    let pad = "  ".repeat(indent);
    match reply.kind {
        "nil" => format!("{pad}(nil)"),
        "int" => format!("{pad}(integer) {}", reply.int.unwrap_or_default()),
        "array" => {
            let Some(items) = reply.items.as_ref() else {
                return format!("{pad}(empty array)");
            };
            if items.is_empty() {
                return format!("{pad}(empty array)");
            }
            items
                .iter()
                .enumerate()
                .map(|(ix, item)| {
                    if item.kind == "array" {
                        format!("{pad}{}) \n{}", ix + 1, reply_pretty(item, indent + 1))
                    } else {
                        format!("{pad}{}) {}", ix + 1, reply_pretty(item, 0))
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => {
            let raw = reply.text.clone().unwrap_or_default();
            if raw.trim_start().starts_with('{') || raw.trim_start().starts_with('[') {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
                    if let Ok(pretty) = serde_json::to_string_pretty(&val) {
                        return pretty;
                    }
                }
            }
            format!("{pad}{raw}")
        }
    }
}

fn describe_ttl(locale: Locale, ttl_ms: Option<i64>) -> String {
    match ttl_ms {
        None => "—".to_owned(),
        Some(-1) => i18n::text(locale, "redis.keys.neverExpire"),
        Some(-2) => i18n::text(locale, "redis.keys.missing"),
        Some(ms) if ms >= 0 => i18n::fill(&i18n::text(locale, "redis.ttl.left"), &[("t", &long_ttl(ms))]),
        Some(_) => "—".to_owned(),
    }
}

/// 键列表行尾的紧凑剩余时间：`45s` / `12m` / `3h` / `2d`。
pub(crate) fn short_ttl(ms: i64) -> String {
    let secs = ms / 1000;
    if ms < 1000 {
        format!("{}ms", ms.max(0))
    } else if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// 详情头部的剩余时间：取两档单位，`1h 23m`、`5m 08s`、`42s`。
pub(crate) fn long_ttl(ms: i64) -> String {
    if ms < 1000 {
        return format!("{}ms", ms.max(0));
    }
    let secs = ms / 1000;
    let (d, h, m, s) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60, secs % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// 列表单元格里的一行预览：换行、制表符折成空格，超长值只取开头，完整内容留给结构化查看器。
pub(crate) fn one_line(text: &str) -> String {
    const MAX: usize = 240;
    let mut out = String::with_capacity(text.len().min(MAX + 1));
    for (count, ch) in text.chars().enumerate() {
        if count >= MAX {
            out.push('…');
            break;
        }
        out.push(if matches!(ch, '\n' | '\r' | '\t') { ' ' } else { ch });
    }
    out
}

mod actions;
mod pages;
mod tables;
mod view;

/// 确认门槛：非生产库直接可确认；生产库要求输入的文本（去首尾空白）与连接名完全一致。
pub(crate) fn confirm_gate(prod: bool, typed: &str, expected: &str) -> bool {
    !prod || (!expected.is_empty() && typed.trim() == expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_parsing_ttl_and_keys_merge_stay_local() {
        assert_eq!(split_commandline("SET key \"hello world\""), ("SET".into(), vec!["key".into(), "hello world".into()]));
        assert_eq!(split_commandline("CONFIG SET save 60"), ("CONFIG SET".into(), vec!["save".into(), "60".into()]));
        assert_eq!(split_commandline("ACL SETUSER default"), ("ACL SETUSER".into(), vec!["default".into()]));
        assert_eq!(short_ttl(45_000), "45s");
        assert_eq!(short_ttl(3_600_000), "1h");
        assert_eq!(long_ttl(5_000_000), "1h 23m");
        assert_eq!(long_ttl(308_000), "5m 08s");
        assert_eq!(one_line("a\nb\tc"), "a b c");
    }

    /// R20：Redis 值查看与网格同一套规则——空字符串单独分类、复制得到空文本、清空提交就是写入空字符串。
    #[test]
    fn empty_strings_are_classified_copied_and_written_as_values() {
        assert_eq!(value_cell(""), ValueCell::Empty);
        assert_eq!(value_cell("a\nb"), ValueCell::Text("a b".to_owned()));
        assert_eq!(value_cell(" "), ValueCell::Text(" ".to_owned()), "空白不是空字符串");
        let empty = RedisValue::String { text: String::new() };
        assert_eq!(value_copy_text(&empty, Locale::ZhCn).as_deref(), Some(""), "复制空字符串得到空文本");
        assert_eq!(format_value(&empty, Locale::ZhCn), "", "不再把空字符串说成“没有内容”");
        assert_eq!(value_copy_text(&RedisValue::None {}, Locale::ZhCn), None);
        assert_eq!(value_copy_text(&RedisValue::List { items: vec!["a".into(), String::new()] }, Locale::ZhCn).as_deref(), Some("a\n"));
        // 列表“设置”：表单没开时载入原值；表单开着时空文本也直接提交。
        assert!(list_set_fills(false, ""));
        assert!(!list_set_fills(true, ""));
        assert!(!list_set_fills(false, "x"));
        let plan = crate::adapters::redis::keys::prepare_mutation(&RedisMutation::ListSet { key: "k".into(), index: 0, value: String::new() }, true).expect("空字符串可以写入");
        assert_eq!(plan.steps[0].word, "LSET");
        assert_eq!(plan.steps[0].args, vec!["k".to_owned(), "0".to_owned(), String::new()]);
        let plan = crate::adapters::redis::keys::prepare_mutation(
            &RedisMutation::HashSet { key: "h".into(), field: "f".into(), value: String::new() },
            true,
        )
        .expect("哈希值可以是空字符串");
        assert_eq!(plan.steps[0].args.last().map(String::as_str), Some(""));
        assert_eq!(parse_ttl("").unwrap(), None);
        assert_eq!(parse_ttl("15").unwrap(), Some(15));
        assert!(parse_ttl("-1").is_err());
        assert!(parse_ttl("1.5").is_err());
        let merged = merge_keys(
            vec![RedisKeyInfo { name: "a".into(), key_type: "string".into(), ttl_ms: Some(-1) }],
            vec![
                RedisKeyInfo { name: "a".into(), key_type: "hash".into(), ttl_ms: Some(10) },
                RedisKeyInfo { name: "b".into(), key_type: "list".into(), ttl_ms: None },
            ],
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].key_type, "hash");
        assert_eq!(parse_spec("cache|缓存|0").unwrap().0, "cache");
        assert!(parse_spec("|缓存|0").is_none());
        let prompt = danger_prompt(Locale::ZhCn, "flushall");
        assert!(prompt.contains("FLUSHALL"), "{prompt}");
        assert_ne!(i18n::text(Locale::En, "redis.tab.keysRefused"), "redis.tab.keysRefused");
        assert_eq!(
            console_error_text("An error was signalled by the server - ResponseError: wrong number of arguments for 'ping' command"),
            "(error) wrong number of arguments for 'ping' command",
            "控制台错误去掉驱动前缀"
        );
        assert_eq!(console_error_text("KEYS 已禁用"), "KEYS 已禁用");
    }

    #[test]
    fn tree_mode_interleaves_folders_and_leaf_keys_without_duplicating_flat_list() {
        let dir = std::env::temp_dir().join(format!("rillsql-redis-tree-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let mut pane = RedisPane::new(services);
        pane.keys = vec![
            RedisKeyInfo { name: "user:1001".into(), key_type: "hash".into(), ttl_ms: None },
            RedisKeyInfo { name: "user:1002".into(), key_type: "hash".into(), ttl_ms: None },
            RedisKeyInfo { name: "global_flag".into(), key_type: "string".into(), ttl_ms: None },
        ];
        pane.view_mode = ViewMode::Flat;
        let flat = pane.build_key_rows();
        assert_eq!(flat.len(), 3);
        assert!(flat.iter().all(|row| matches!(row, KeyRow::Key { depth: 0, .. })));

        pane.view_mode = ViewMode::Tree;
        // 未展开 user 文件夹时：只出现 global_flag 与 user 文件夹（count=2），两个子键收起隐藏。
        let collapsed = pane.build_key_rows();
        assert_eq!(collapsed.len(), 2, "{collapsed:?}");
        assert!(collapsed.iter().any(|r| matches!(r, KeyRow::Folder { prefix, count: 2, open: false, .. } if prefix == "user")));

        // 展开 user 文件夹后：user 文件夹正下方紧跟缩进后的叶子名 1001、1002。
        pane.expanded.insert("user".into());
        let expanded = pane.build_key_rows();
        assert_eq!(expanded.len(), 4, "{expanded:?}");
        assert!(expanded.iter().any(|r| matches!(r, KeyRow::Key { display, depth: 1, .. } if display == "1001")));
        assert!(expanded.iter().any(|r| matches!(r, KeyRow::Key { display, depth: 1, .. } if display == "1002")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn large_key_pages_move_existing_storage_and_replace_duplicates_in_order() {
        let keys: Vec<_> = (0..10_000)
            .map(|index| RedisKeyInfo { name: format!("key-{index}"), key_type: "string".into(), ttl_ms: None })
            .collect();
        let first_name = keys[0].name.as_ptr();
        let merged = merge_keys(
            keys,
            vec![
                RedisKeyInfo { name: "key-9999".into(), key_type: "hash".into(), ttl_ms: Some(10) },
                RedisKeyInfo { name: "new".into(), key_type: "list".into(), ttl_ms: None },
                RedisKeyInfo { name: "new".into(), key_type: "set".into(), ttl_ms: Some(20) },
            ],
        );
        assert_eq!(merged.len(), 10_001);
        assert_eq!(merged[0].name.as_ptr(), first_name, "旧键字符串没有复制");
        assert_eq!(merged[9999].key_type, "hash");
        assert_eq!(merged[10_000].key_type, "set");
    }

    #[gpui_kit::test]
    fn switching_redis_target_clears_key_command_and_confirmation(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext as _;
        let dir = std::env::temp_dir().join(format!("rillsql-redis-reset-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(
            crate::connections::Service::new_for_test(dir.join("connections.sqlite")),
        )));
        let pane = cx.update(|cx| cx.new(|_| RedisPane::new(services)));
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.plant_key("old-key");
                pane.set_command("DEL old-key");
                pane.set_ttl("60");
                pane.set_rename("renamed");
                pane.command("redis-delete", cx);
                assert!(pane.confirm_open());
                pane.reset_target_state();
                pane.connection_id = "cache".into();
                pane.connection_name = "缓存".into();
                pane.database = "0".into();
            });
        });
        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(!pane.confirm_open(), "换 Redis 目标不能保留旧删除确认");
            assert!(pane.selected().is_empty());
            assert!(pane.command_input().is_empty());
            assert!(pane.ttl_input().is_empty());
            assert!(pane.rename_to().is_empty());
            assert!(pane.reply().is_empty());
            assert!(!pane.status_line().contains("old-key"), "{}", pane.status_line());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn config_set_ttl_score_and_history_stay_local(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext as _;
        let dir = std::env::temp_dir().join(format!("rillsql-redis-gate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(
            crate::connections::Service::new_for_test(dir.join("connections.sqlite")),
        )));
        let pane = cx.update(|cx| cx.new(|_| RedisPane::new(services)));
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.plant_key("kept");
                pane.set_command("CONFIG SET save 60");
                pane.command("redis-exec", cx);
                assert!(pane.confirm_open(), "{}", pane.status_line());
                assert!(pane.confirm_prompt().to_ascii_uppercase().contains("CONFIG"), "{}", pane.confirm_prompt());
                pane.command("redis-cancel", cx);
                assert!(!pane.confirm_open());
                assert_eq!(pane.command_input(), "CONFIG SET save 60");
                pane.set_ttl("30");
                pane.command("redis-ttl", cx);
                assert!(pane.confirm_open(), "TTL 必须先确认");
                assert_eq!(pane.ttl_input(), "30");
                pane.command("redis-cancel", cx);
                assert_eq!(pane.ttl_input(), "30");
                pane.detail = Some(Arc::new(RedisKeyDetail {
                    key_type: "zset".into(),
                    ttl_ms: Some(-1),
                    value: RedisValue::ZSet { entries: Vec::new() },
                    total: Some(0),
                    next: None,
                    memory: None,
                    encoding: None,
                }));
                pane.entry_score = "nope".into();
                pane.entry_member = "张伟".into();
                pane.command("redis-zadd", cx);
                assert!(!pane.confirm_open(), "非法分值不能进入确认");
                assert_eq!(pane.entry_score, "nope");
                assert!(pane.error().contains("分值") || pane.error().to_ascii_lowercase().contains("score"), "{}", pane.error());
                pane.history = vec!["GET a".into(), "GET b".into()];
                pane.command_input = "PING".into();
                pane.history_step(true);
                assert_eq!(pane.command_input(), "GET b");
                pane.history_step(true);
                assert_eq!(pane.command_input(), "GET a");
                pane.history_step(false);
                assert_eq!(pane.command_input(), "GET b");
                pane.history_step(false);
                assert_eq!(pane.command_input(), "PING");
                pane.page = Page::Keys;
                pane.command("redis-console", cx);
                assert_eq!(pane.page(), Page::Console, "控制台是一个独立页面");
                pane.command("redis-console", cx);
                assert_eq!(pane.page(), Page::Keys, "再按一次回到键");
            });
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod live_tests;

#[cfg(test)]
mod states;

#[cfg(test)]
mod workbench_tests;

#[cfg(test)]
mod ux_tests;
