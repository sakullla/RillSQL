//! 结构比较与数据比较。脚本只放入查询编辑器，不执行。取消优先于迟到结果。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Editor, EditorState, InputEvent, InputState};
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, Window, div, px, relative,
};

use crate::ui::kit::{self, Palette, Radius, Space, Tip as _, TypeScale};
use crate::ui::read_table::{ReadColumn, ReadRows, ReadTable};
use crate::ui::kit::ScrollAreaExt as _;

use crate::application::tasks::{TaskOutcome, TaskToken};
use crate::application::{AppServices, cancel_compare, compare_cancelled, compare_progress};
use crate::compare::{CompareProgress, CompareStage, DataReport, DataRequest, DataTableRequest, DiffItem, SideRequest, StructureReport, StructureRequest, TablePair};
use crate::connections::model::Engine;
use crate::i18n::{self, Locale};

const OWNER: &str = "compare";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Idle,
    Running,
    Cancelled,
    Refused,
    Incomplete,
    Ready,
    Stale,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunKind {
    Structure,
    Data,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffFilter {
    All,
    Baseline,
    Target,
    Changed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SchemaState {
    Loading,
    Ready(Vec<String>),
    Manual(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TableListState {
    Loading,
    Ready(Vec<String>),
    Error(String),
}

const PAGE_MAX_W: f32 = 1200.;
const DIFF_TREE_W: f32 = 300.;
const DIFF_TREE_MAX_H: f32 = 520.;
const DIFF_DEF_MAX_H: f32 = 380.;
const SCRIPT_EDITOR_H: f32 = 220.;
const TABLE_AC_MAX_H: f32 = 240.;
const FILTER_SEARCH_W: f32 = 200.;
const PROMPT_MIN_W: f32 = 180.;
const MARK_COL_W: f32 = 14.;
const ZERO_PX: f32 = 0.;
const ONE_PX: f32 = 1.;
const LIST_CHOICE_EXTRA_H: f32 = 2.;
const SIDE_LIST_VISIBLE_ROWS: f32 = 3.5;
const _: f32 = TypeScale::XL;

/// 提示的语义：门禁、取消、过期只是告知，读取失败才要给重试。
/// 两类排版与动作不同，不再把一切都画成同一条横幅（规范 S8.5 三态 + 主操作）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ErrorKind {
    None,
    /// 提示：门禁、取消、过期、上一次还在读；横幅说明原因，不提供重试。
    Notice,
    /// 失败：统一错误态 + 重试；`RetryAction` 说明重试会重发哪一步。
    Failure(RetryAction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetryAction {
    /// 失败的是连接清单本身：重试只重新读取清单。
    Choices,
    Structure,
    Data,
}

/// 两侧并排所需的最小视口宽度：每侧卡片留约 340 可用宽，加上分栏间距与 docked 侧栏（234），
/// 视口窄于它就把基准/对照竖排，不让两列各自压到只剩图标（R6 的 900/640 两档）。
const SIDES_SIDE_BY_SIDE_MIN_W: f32 = 940.;

/// 阈值必须真的容得下两列：每侧至少 340 可用宽，再加上 docked 侧栏（234）。
const _: () = assert!(SIDES_SIDE_BY_SIDE_MIN_W as i32 >= 2 * 340 + 234);

/// 两侧是否竖排。渲染与用例共用这一判定，避免测试自己再写一遍阈值。
fn sides_stacked(viewport_width: f32) -> bool {
    viewport_width < SIDES_SIDE_BY_SIDE_MIN_W
}

#[derive(Clone, Debug)]
struct Choice {
    id: String,
    name: String,
    engine: String,
    databases: Vec<String>,
    catalog_error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PlacedScript {
    pub sql: String,
    pub connection_id: Option<String>,
    pub connection_name: Option<String>,
    pub database: Option<String>,
    pub engine: Option<String>,
}

struct SidePick {
    connection_id: String,
    database: String,
    schema: String,
}

/// 打开对比时带进来的当前对象或连接/库范围；只有展示与预选用途，
/// 真正的对比请求仍由两侧的 SidePick 决定。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareContext {
    pub connection_id: String,
    pub connection_name: String,
    pub database: String,
    pub schema: String,
    pub object: String,
    /// 完整路径，用于说明“对比带入的是哪个对象”。
    pub path: String,
}

impl CompareContext {
    pub fn of(target: &crate::app::model::ObjectTarget) -> Self {
        Self {
            connection_id: target.connection_id.clone(),
            connection_name: target.connection_name.clone(),
            database: target.database.clone(),
            schema: target.schema.clone(),
            object: target.name.clone(),
            path: target.path(),
        }
    }
}

pub struct ComparePane {
    services: Arc<AppServices>,
    locale: Locale,
    choices: Vec<Choice>,
    baseline: SidePick,
    target: SidePick,
    schemas: BTreeMap<String, SchemaState>,
    structure: Option<Arc<StructureReport>>,
    data: Option<Arc<DataReport>>,
    structure_status: Status,
    data_status: Status,
    selected: BTreeMap<String, bool>,
    chosen_key: BTreeMap<String, String>,
    error: String,
    /// 上面那条提示的语义：决定它是横幅还是统一错误态，以及重试会重发什么。
    error_kind: ErrorKind,
    running: bool,
    run_id: String,
    token: Option<TaskToken>,
    run_kind: Option<RunKind>,
    cancel_flag: Arc<AtomicBool>,
    placed: Option<PlacedScript>,
    /// 把对比生成的写脚本放进查询编辑器前，再确认一次。
    pending_place: Option<RunKind>,
    /// 待确认放入的是差异树选中对象的语句，而不是整体脚本。
    pending_item_sql: Option<String>,
    place_prompt: String,
    schema_draft: BTreeMap<String, String>,
    schema_inputs: BTreeMap<&'static str, (Entity<InputState>, gpui_kit::Subscription)>,
    /// 打开对比时带进来的当前对象/范围；choices 读完后再落到两侧。
    context: Option<CompareContext>,
    /// 上下文说明或缺失条件；渲染在标题下方。
    context_note: String,
    /// 用户按了“返回工作项”；由外壳取走并恢复原工作项与焦点。
    back: bool,
    /// 连接清单正在读取；带入上下文时不再重复发起同一份读取。
    choices_loading: bool,
    diff_filter: DiffFilter,
    diff_query: String,
    schema_reload: bool,
    filter_input: Option<(Entity<InputState>, gpui_kit::Subscription)>,
    script_editors: BTreeMap<&'static str, (Entity<EditorState>, String)>,
    /// 设置里的查询超时（秒）；对比的每次读取都按它收口，0 时后端用默认上限。
    query_timeout_sec: u64,
    /// 后台对比任务的中止句柄：取消时直接中止，不等下一个读取点。
    abort: Option<tokio::task::AbortHandle>,
    /// 后台登记的进度（阶段、已读/总数），运行中定时轮询。
    progress: Option<CompareProgress>,
    ticker: Option<gpui_kit::Task<()>>,
    /// 差异树只列有差异的对象；关掉后也列出两侧相同的对象。
    only_diffs: bool,
    /// 差异树里选中的条目（`b:序号`、`t:序号`、`c:序号`、`s:序号`）。
    picked: Option<String>,
    /// 数据对比下钻的表。
    data_table: Option<String>,
    /// 表对表对比：(基准表名, 对照表名)。为空时是整库对比。
    table_pair: Option<(String, String)>,
    /// 从资源树带进来、还没落到两侧的表（基准，可选的对照）；连接清单读完后再落。
    pair_pending: Option<(CompareContext, Option<CompareContext>)>,
    table_inputs: BTreeMap<&'static str, (Entity<InputState>, gpui_kit::Subscription)>,
    /// 表名被命令改过（交换、重新带入）：下一帧把输入框同步成新值。
    table_reload: bool,
    /// 按 `(connection_id, database, schema)` 缓存的表名列表，供表对表输入框智能补全。
    tables_cache: BTreeMap<String, TableListState>,
    /// 当前展开表名补全弹层的一侧（`"baseline"` / `"target"`）。
    table_ac_side: Option<&'static str>,
    /// 补全弹层当前键盘高亮索引。
    table_ac_index: usize,
    /// 有报告时可一键收起/展开顶部对比配置卡片，把视口留给差异审阅。
    sources_collapsed: bool,
    /// 下钻表的行级差异表格（共享只读表：可选中、⌘C 与右键复制）。戳 = (报告指针, 表名, 语言)。
    rows_table: Option<(u64, Entity<ReadTable>)>,
    #[cfg(test)]
    armed_pause: bool,
    #[cfg(test)]
    pause: Option<tokio::sync::oneshot::Sender<()>>,
}

impl ComparePane {
    pub fn new(services: Arc<AppServices>) -> Self {
        Self {
            services,
            locale: Locale::ZhCn,
            choices: Vec::new(),
            baseline: SidePick { connection_id: String::new(), database: String::new(), schema: String::new() },
            target: SidePick { connection_id: String::new(), database: String::new(), schema: String::new() },
            schemas: BTreeMap::new(),
            structure: None,
            data: None,
            structure_status: Status::Idle,
            data_status: Status::Idle,
            selected: BTreeMap::new(),
            chosen_key: BTreeMap::new(),
            error: String::new(),
            error_kind: ErrorKind::None,
            running: false,
            run_id: String::new(),
            token: None,
            run_kind: None,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            placed: None,
            pending_place: None,
            pending_item_sql: None,
            place_prompt: String::new(),
            schema_draft: BTreeMap::new(),
            schema_inputs: BTreeMap::new(),
            context: None,
            context_note: String::new(),
            back: false,
            choices_loading: false,
            diff_filter: DiffFilter::All,
            diff_query: String::new(),
            schema_reload: false,
            filter_input: None,
            script_editors: BTreeMap::new(),
            query_timeout_sec: 0,
            abort: None,
            progress: None,
            ticker: None,
            only_diffs: true,
            picked: None,
            data_table: None,
            table_pair: None,
            pair_pending: None,
            table_inputs: BTreeMap::new(),
            table_reload: false,
            tables_cache: BTreeMap::new(),
            table_ac_side: None,
            table_ac_index: 0,
            sources_collapsed: false,
            rows_table: None,
            #[cfg(test)]
            armed_pause: false,
            #[cfg(test)]
            pause: None,
        }
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
    }

    pub fn set_query_timeout_sec(&mut self, secs: u64) {
        self.query_timeout_sec = secs;
    }

    pub fn running(&self) -> bool {
        self.running
    }

    pub fn structure_ready(&self) -> bool {
        !self.running && self.structure_status == Status::Ready && self.structure.as_ref().is_some_and(|report| report.refused.is_none() && !report.incomplete)
    }

    pub fn data_ready(&self) -> bool {
        !self.running && self.data_status == Status::Ready && self.data.as_ref().is_some_and(|report| report.refused.is_none() && !report.incomplete)
    }

    pub fn take_place(&mut self) -> Option<PlacedScript> {
        self.placed.take()
    }

    pub fn context_note(&self) -> &str {
        &self.context_note
    }

    pub fn context(&self) -> Option<&CompareContext> {
        self.context.as_ref()
    }

    /// 基准与对照当前的连接 id；测试与状态栏用它确认“带入到哪一侧”。
    pub fn sides(&self) -> (String, String) {
        (self.baseline.connection_id.clone(), self.target.connection_id.clone())
    }

    /// 用户按了“返回工作项”；外壳取走后回到原工作项与焦点。
    pub fn take_back(&mut self) -> bool {
        let back = self.back;
        self.back = false;
        back
    }

    /// 打开对比时带入当前对象或连接/库范围；没有上下文时说明缺什么，
    /// 而不是把某个连接当成“当前目标”。
    pub fn open_context(&mut self, context: Option<CompareContext>, cx: &mut Context<Self>) {
        // 整库对比入口：离开表对表模式。
        if self.table_pair.is_some() || self.pair_pending.is_some() {
            self.table_pair = None;
            self.pair_pending = None;
            self.invalidate_results();
        }
        match context {
            Some(context) => {
                self.context = Some(context);
                if self.choices.is_empty() {
                    // 首次打开还没有连接清单：读完后再落两侧，期间保留上下文说明。
                    if !self.choices_loading {
                        self.refresh_choices(cx);
                    }
                    // 清单还没到就还不知道有没有基准侧，先不说“缺连接”。
                    self.context_note = self.carried_note();
                } else {
                    self.apply_context(cx);
                }
            }
            None => {
                self.context = None;
                self.context_note = self.text("compare.context.none");
                cx.notify();
            }
        }
    }

    /// 表对表对比：基准是 `left` 这张表，对照是 `right`（单表“对比表…”时为空，由用户填写对照表名）。
    /// 两侧可以是不同连接、不同库；连接清单到达后再落到两侧。
    pub fn open_table_pair(&mut self, left: CompareContext, right: Option<CompareContext>, cx: &mut Context<Self>) {
        let pair = (left.object.clone(), right.as_ref().map(|right| right.object.clone()).unwrap_or_default());
        self.invalidate_results();
        self.context = None;
        self.table_pair = Some(pair);
        self.table_reload = true;
        self.pair_pending = Some((left, right));
        self.context_note = self.pair_note();
        if self.choices.is_empty() {
            if !self.choices_loading {
                self.refresh_choices(cx);
            }
        } else {
            self.apply_table_pair(cx);
        }
    }

    /// 是否是表对表对比。
    pub fn table_mode(&self) -> bool {
        self.table_pair.is_some()
    }

    /// 表对表对比的两张表名（基准，对照）。
    pub fn table_pair(&self) -> Option<(String, String)> {
        self.table_pair.clone()
    }

    fn pair_note(&self) -> String {
        let Some((left, right)) = &self.table_pair else { return String::new() };
        let right = if right.trim().is_empty() { self.text("compare.table.targetMissing") } else { right.clone() };
        fill(&self.text("compare.table.note"), &[("{baseline}", left), ("{target}", &right)])
    }

    /// 把带进来的两张表落到两侧：基准取左表所在的连接/库/schema，对照取右表的；
    /// 只带了一张表时对照先放在同一个库，用户再改连接、库或表名。
    fn apply_table_pair(&mut self, cx: &mut Context<Self>) {
        let Some((left, right)) = self.pair_pending.clone() else { return };
        let side_of = |this: &Self, context: &CompareContext| -> Option<SidePick> {
            let choice = this.choices.iter().find(|choice| choice.id == context.connection_id)?;
            let database = if context.database.is_empty() { choice.databases.first().cloned().unwrap_or_default() } else { context.database.clone() };
            Some(SidePick { connection_id: context.connection_id.clone(), database, schema: context.schema.clone() })
        };
        let Some(baseline) = side_of(self, &left) else {
            self.context_note = fill(&self.text("compare.context.sideMissing"), &[("{path}", &left.path)]);
            return;
        };
        let target = match right.as_ref() {
            Some(right) => match side_of(self, right) {
                Some(side) => side,
                None => {
                    self.context_note = fill(&self.text("compare.context.sideMissing"), &[("{path}", &right.path)]);
                    return;
                }
            },
            None => SidePick { connection_id: baseline.connection_id.clone(), database: baseline.database.clone(), schema: baseline.schema.clone() },
        };
        self.baseline = baseline;
        self.target = target;
        self.pair_pending = None;
        self.schema_reload = true;
        self.context_note = self.pair_note();
        self.ensure_schemas(true, cx);
        self.ensure_schemas(false, cx);
        self.ensure_tables(true, cx);
        self.ensure_tables(false, cx);
    }

    /// 改表对表对比的一侧表名；旧报告不再属于新的这对表。
    fn set_table_name(&mut self, baseline: bool, name: &str) {
        let Some(pair) = self.table_pair.as_mut() else { return };
        let slot = if baseline { &mut pair.0 } else { &mut pair.1 };
        if slot.as_str() == name.trim() {
            return;
        }
        *slot = name.trim().to_owned();
        self.invalidate_results();
        self.context_note = self.pair_note();
    }

    /// 带入说明只描述真正带入的对象；基准侧是否缺失单独拼在后面，
    /// 已经找到另一条可比连接时不再声称“还需要一个已打开的连接”。
    fn carried_note(&self) -> String {
        let Some(context) = self.context.as_ref() else {
            return self.text("compare.context.none");
        };
        let mut note = fill(&self.text("compare.context.carried"), &[("{path}", &context.path)]);
        if self.baseline.connection_id.is_empty() && !self.choices_loading {
            note.push_str(&self.text("compare.context.baselineMissing"));
        }
        note
    }

    /// 把带入的上下文落到两侧：当前对象放在对照侧，基准侧优先取另一个已打开连接。
    /// 两侧任何一侧变化都算新目标：旧报告、选择和已放入脚本不再属于这次带入的上下文。
    fn apply_context(&mut self, cx: &mut Context<Self>) {
        if self.pair_pending.is_some() {
            self.apply_table_pair(cx);
            return;
        }
        let Some(context) = self.context.clone() else { return };
        let Some(choice) = self.choices.iter().find(|choice| choice.id == context.connection_id).cloned() else {
            // 这个连接没有打开或不可比较：保留带入说明，并指出这一侧缺什么，不改两侧选择。
            self.context_note = fill(&self.text("compare.context.sideMissing"), &[("{path}", &context.path)]);
            return;
        };
        let database = if context.database.is_empty() {
            choice.databases.first().cloned().unwrap_or_default()
        } else {
            context.database.clone()
        };
        // 变更判定要同时覆盖基准侧：重新带入会把手动选过的基准改回候选连接，
        // 只看对照侧就会让界面上的两侧与仍显示的报告对不上。
        let mut changed = self.target.connection_id != context.connection_id
            || self.target.database != database
            || self.target.schema != context.schema;
        self.target = SidePick {
            connection_id: context.connection_id.clone(),
            database: database.clone(),
            schema: context.schema.clone(),
        };
        let engine = choice.engine.clone();
        if let Some(other) = self
            .choices
            .iter()
            .find(|other| other.id != context.connection_id && other.engine == engine)
            .cloned()
        {
            let other_database = other.databases.first().cloned().unwrap_or_default();
            if self.baseline.connection_id != other.id || self.baseline.database != other_database || !self.baseline.schema.is_empty() {
                changed = true;
                self.baseline = SidePick {
                    connection_id: other.id,
                    database: other_database,
                    schema: String::new(),
                };
            }
        }
        if changed {
            self.invalidate_results();
        }
        // 两侧落定后再生成说明，避免基线已经有了还说缺连接。
        self.context_note = self.carried_note();
        self.ensure_schemas(true, cx);
        self.ensure_schemas(false, cx);
    }

    /// 只改结果页呈现的命令（筛选、仅差异、选中差异条目、数据下钻）；处理了返回真。
    fn view_command(&mut self, id: &str) -> bool {
        match id {
            "compare-filter:all" => self.diff_filter = DiffFilter::All,
            "compare-filter:baseline" => self.diff_filter = DiffFilter::Baseline,
            "compare-filter:target" => self.diff_filter = DiffFilter::Target,
            "compare-filter:changed" => self.diff_filter = DiffFilter::Changed,
            "compare-only-diffs" => self.only_diffs = !self.only_diffs,
            _ => {
                if let Some(key) = id.strip_prefix("compare-pick:") {
                    self.picked = Some(key.to_owned());
                } else if let Some(table) = id.strip_prefix("compare-data-table:") {
                    self.data_table = if self.data_table.as_deref() == Some(table) { None } else { Some(table.to_owned()) };
                } else {
                    return false;
                }
            }
        }
        true
    }

    #[cfg(test)]
    pub fn arm_pause(&mut self) {
        self.armed_pause = true;
    }

    #[cfg(test)]
    pub fn release_pause(&mut self) {
        if let Some(sender) = self.pause.take() {
            let _ = sender.send(());
        }
    }

    pub fn command(&mut self, id: &str, cx: &mut Context<Self>) {
        if id == "compare" || id == "compare-refresh" {
            self.refresh_choices(cx);
            cx.notify();
            return;
        }
        if id == "compare-object" || id == "compare-grid" || id == "compare-table" || id == "compare-tables" {
            // 外壳负责把完整对象目标交给 open_context；这里只刷新连接清单，
            // 刚打开的目标连接才会出现在候选里。
            self.refresh_choices(cx);
            cx.notify();
            return;
        }
        if id == "compare-back" {
            self.back = true;
            cx.notify();
            return;
        }
        if self.view_command(id) {
            cx.notify();
            return;
        }
        if let Some(connection_id) = id.strip_prefix("compare-baseline:") {
            self.pick(true, connection_id, cx);
        } else if let Some(connection_id) = id.strip_prefix("compare-target:") {
            self.pick(false, connection_id, cx);
        } else if let Some(name) = id.strip_prefix("compare-database:baseline:") {
            self.set_database(true, name, cx);
        } else if let Some(name) = id.strip_prefix("compare-database:target:") {
            self.set_database(false, name, cx);
        } else if let Some(name) = id.strip_prefix("compare-schema:baseline:") {
            self.baseline.schema = name.to_owned();
            self.invalidate_results();
            self.ensure_tables(true, cx);
        } else if let Some(name) = id.strip_prefix("compare-schema:target:") {
            self.target.schema = name.to_owned();
            self.invalidate_results();
            self.ensure_tables(false, cx);
        } else if let Some(name) = id.strip_prefix("compare-table-name:baseline:") {
            self.set_table_name(true, name);
        } else if let Some(name) = id.strip_prefix("compare-table-name:target:") {
            self.set_table_name(false, name);
        } else if id == "compare-mode:table" {
            if self.table_pair.is_none() {
                self.table_pair = Some((String::new(), String::new()));
                self.table_reload = true;
                self.table_ac_side = None;
                self.sources_collapsed = false;
                self.invalidate_results();
                self.context_note = self.pair_note();
                self.ensure_tables(true, cx);
                self.ensure_tables(false, cx);
            }
        } else if id == "compare-table-exit" || id == "compare-mode:database" {
            // 回到整库对比：两侧连接与库保留。
            self.table_pair = None;
            self.pair_pending = None;
            self.table_ac_side = None;
            self.context_note.clear();
            self.invalidate_results();
        } else if id == "compare-toggle-sources" {
            self.sources_collapsed = !self.sources_collapsed;
            self.table_ac_side = None;
        } else if let Some(name) = id.strip_prefix("compare-toggle:") {
            let selected = self.selected.get(name).copied().unwrap_or(false);
            self.selected.insert(name.to_owned(), !selected);
        } else if let Some(rest) = id.strip_prefix("compare-key:") {
            if let Some((table, key)) = rest.split_once('|') {
                self.chosen_key.insert(table.to_owned(), key.to_owned());
            }
        } else if id == "compare-structure" {
            self.start_structure(cx);
        } else if id == "compare-data" {
            self.start_data(cx);
        } else if id == "compare-cancel" {
            self.cancel();
        } else if id == "compare-retry" {
            self.retry(cx);
        } else if id == "compare-place:structure" {
            self.ask_place(RunKind::Structure);
        } else if id == "compare-place:data" {
            self.ask_place(RunKind::Data);
        } else if id == "compare-place-confirm" {
            self.confirm_place();
        } else if id == "compare-place-cancel" {
            self.pending_place = None;
            self.pending_item_sql = None;
            self.place_prompt.clear();
        } else if id == "compare-swap" {
            self.swap_sides();
            self.ensure_tables(true, cx);
            self.ensure_tables(false, cx);
        } else if id == "compare-select-all" {
            self.select_all_tables();
        } else if id == "compare-clear" {
            self.clear_table_selection();
        } else if id == "compare-copy:item" {
            if let Some(sql) = self.picked_item().map(|item| item.sql.clone()).filter(|sql| !sql.trim().is_empty()) {
                cx.write_to_clipboard(ClipboardItem::new_string(sql));
            }
        } else if id == "compare-place:item" {
            self.ask_place_item();
        } else if id == "compare-copy:structure" {
            self.copy_script(RunKind::Structure, cx);
        } else if id == "compare-copy:data" {
            self.copy_script(RunKind::Data, cx);
        }
        cx.notify();
    }

    fn swap_sides(&mut self) {
        std::mem::swap(&mut self.baseline, &mut self.target);
        if let Some(pair) = self.table_pair.as_mut() {
            std::mem::swap(&mut pair.0, &mut pair.1);
            self.table_reload = true;
        }
        let left = self.schema_draft.remove("baseline").unwrap_or_default();
        let right = self.schema_draft.remove("target").unwrap_or_default();
        self.schema_draft.insert("baseline".into(), right);
        self.schema_draft.insert("target".into(), left);
        self.schema_reload = true;
        self.invalidate_results();
        if self.table_pair.is_some() {
            self.context_note = self.pair_note();
        }
    }

    fn select_all_tables(&mut self) {
        let Some(report) = &self.structure else { return };
        for table in &report.tables {
            if table.on_both && name_matches(&table.name, &self.diff_query) {
                self.selected.insert(table.name.clone(), true);
            }
        }
    }

    fn clear_table_selection(&mut self) {
        for selected in self.selected.values_mut() {
            *selected = false;
        }
    }

    fn copy_script(&self, kind: RunKind, cx: &mut Context<Self>) {
        let script = match kind {
            RunKind::Structure => self.structure.as_ref().map(|report| report.script.clone()),
            RunKind::Data => self.data.as_ref().map(|report| report.script.clone()),
        };
        let Some(script) = script.filter(|script| !script.trim().is_empty()) else { return };
        cx.write_to_clipboard(ClipboardItem::new_string(script));
    }

    fn pick(&mut self, baseline: bool, connection_id: &str, cx: &mut Context<Self>) {
        let Some(choice) = self.choices.iter().find(|choice| choice.id == connection_id).cloned() else {
            self.notify(self.text("compare.gates.sideNotOpen"));
            return;
        };
        // 没有“库”这一层的单文件引擎只有一个固定库。
        let database = match crate::adapters::fixed_database_named(&choice.engine) {
            Some(fixed) => fixed.to_owned(),
            None => choice.databases.first().cloned().unwrap_or_default(),
        };
        let slot = if baseline { &mut self.baseline } else { &mut self.target };
        // 重新选连接会清掉 schema，这也是新目标。
        let changed = slot.connection_id != choice.id || slot.database != database || !slot.schema.is_empty();
        slot.connection_id = choice.id;
        slot.database = database;
        slot.schema.clear();
        if changed {
            self.invalidate_results();
        }
        self.ensure_schemas(baseline, cx);
        self.ensure_tables(baseline, cx);
    }

    fn set_database(&mut self, baseline: bool, name: &str, cx: &mut Context<Self>) {
        let slot = if baseline { &mut self.baseline } else { &mut self.target };
        let changed = slot.database != name;
        slot.database = name.to_owned();
        slot.schema.clear();
        if changed {
            self.invalidate_results();
        }
        self.ensure_schemas(baseline, cx);
        self.ensure_tables(baseline, cx);
    }

    /// 清掉提示，连同它的语义与重试入口。
    fn clear_error(&mut self) {
        self.error.clear();
        self.error_kind = ErrorKind::None;
    }

    /// 门禁、取消、过期一类的提示：横幅说清原因，不提供重试。
    fn notify(&mut self, message: String) {
        self.error = message;
        self.error_kind = ErrorKind::Notice;
    }

    /// 需要重试的失败：用统一错误态呈现，重试按钮按 `action` 重发那一步。
    fn fail(&mut self, message: String, action: RetryAction) {
        self.error = message;
        self.error_kind = ErrorKind::Failure(action);
    }

    /// 失败的标题：清单本身读不出时说“选择连接”，比较中途丢了结果时说“比较没有完成”。
    fn error_heading(&self) -> String {
        match self.error_kind {
            ErrorKind::Failure(RetryAction::Choices) => self.text("compare.dialog.chooseConnection"),
            _ => self.text("compare.status.failed"),
        }
    }

    /// 失败的原因：能说清就说清；与标题同文时给可执行的下一步，不留空详情。
    fn error_detail(&self) -> String {
        let heading = self.error_heading();
        if self.error.trim().is_empty() || self.error.trim() == heading.trim() {
            self.text("compare.gates.chooseBoth")
        } else {
            self.error.clone()
        }
    }

    /// 重试只重发失败的那一步，不改两侧选择，也不清已选目标。
    fn retry(&mut self, cx: &mut Context<Self>) {
        let ErrorKind::Failure(action) = self.error_kind else {
            return;
        };
        self.clear_error();
        match action {
            RetryAction::Choices => self.refresh_choices(cx),
            RetryAction::Structure => self.start_structure(cx),
            RetryAction::Data => self.start_data(cx),
        }
    }

    /// 目标连接/库/schema 变化后，旧报告、选择和待放入脚本都不再属于新目标。
    /// 仍在跑的请求用代次和取消标记失效，迟到结果不会写回。
    fn invalidate_results(&mut self) {
        if self.running {
            self.cancel_flag.store(true, Ordering::SeqCst);
            if !self.run_id.is_empty() {
                cancel_compare(&self.services.tasks, OWNER, &self.run_id);
            }
            self.token = None;
            self.running = false;
        }
        if let Some(handle) = self.abort.take() {
            handle.abort();
        }
        self.structure = None;
        self.data = None;
        self.picked = None;
        self.data_table = None;
        self.structure_status = Status::Idle;
        self.data_status = Status::Idle;
        self.selected.clear();
        self.chosen_key.clear();
        self.placed = None;
        self.pending_place = None;
        self.pending_item_sql = None;
        self.place_prompt.clear();
        self.sources_collapsed = false;
        self.clear_error();
    }

    fn ensure_schemas(&mut self, baseline: bool, cx: &mut Context<Self>) {
        let slot = if baseline { &self.baseline } else { &self.target };
        let Some(engine) = self.engine_of(&slot.connection_id) else {
            return;
        };
        if !crate::adapters::shows_schemas_named(&engine) {
            return;
        }
        let connection_id = slot.connection_id.clone();
        let database = slot.database.clone();
        if database.trim().is_empty() {
            return;
        }
        let key = schema_key(&connection_id, &database);
        if self.schemas.contains_key(&key) {
            return;
        }
        self.schemas.insert(key.clone(), SchemaState::Loading);
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = crate::application::work::compare_schemas(&services.connections, &connection_id, &database).await;
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |pane, cx| {
                            pane.finish_schemas(&key, result);
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn finish_schemas(&mut self, key: &str, result: Result<Vec<String>, String>) {
        if !matches!(self.schemas.get(key), Some(SchemaState::Loading)) {
            return;
        }
        self.schemas.insert(
            key.to_owned(),
            match result {
                Ok(names) if !names.is_empty() => SchemaState::Ready(names),
                Ok(_) => SchemaState::Manual(String::new()),
                Err(message) => SchemaState::Manual(message),
            },
        );
    }

    fn side_table_scope(&self, baseline: bool) -> Option<(String, String, String)> {
        let slot = if baseline { &self.baseline } else { &self.target };
        let connection_id = slot.connection_id.trim();
        if connection_id.is_empty() {
            return None;
        }
        let engine = self.engine_of(connection_id)?;
        let database = match crate::adapters::fixed_database_named(&engine) {
            Some(fixed) => fixed.to_owned(),
            None => slot.database.trim().to_owned(),
        };
        if database.is_empty() {
            return None;
        }
        let schema = if crate::adapters::shows_schemas_named(&engine) {
            let s = slot.schema.trim();
            if s.is_empty() {
                return None;
            }
            s.to_owned()
        } else {
            String::new()
        };
        Some((connection_id.to_owned(), database, schema))
    }

    fn ensure_tables(&mut self, baseline: bool, cx: &mut Context<Self>) {
        if self.table_pair.is_none() {
            return;
        }
        let Some((connection_id, database, schema)) = self.side_table_scope(baseline) else {
            return;
        };
        let key = table_cache_key(&connection_id, &database, &schema);
        if self.tables_cache.contains_key(&key) {
            return;
        }
        self.tables_cache.insert(key.clone(), TableListState::Loading);
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = match crate::application::work::catalog_page(&services.connections, &connection_id, &database, &schema, 0, 500).await {
                    Ok(page) => {
                        let mut names: Vec<String> = page
                            .objects
                            .into_iter()
                            .filter(|obj| obj.kind == "table")
                            .map(|obj| obj.name)
                            .filter(|name| !name.trim().is_empty())
                            .collect();
                        names.sort();
                        names.dedup();
                        if names.is_empty() {
                            if let Some(error) = page.error {
                                Err(error)
                            } else {
                                Ok(names)
                            }
                        } else {
                            Ok(names)
                        }
                    }
                    Err(message) => Err(message),
                };
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |pane, cx| {
                            pane.finish_tables(&key, result);
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn finish_tables(&mut self, key: &str, result: Result<Vec<String>, String>) {
        if !matches!(self.tables_cache.get(key), Some(TableListState::Loading)) {
            return;
        }
        self.tables_cache.insert(
            key.to_owned(),
            match result {
                Ok(names) => TableListState::Ready(names),
                Err(message) => TableListState::Error(message),
            },
        );
    }

    fn table_list_state(&self, baseline: bool) -> Option<&TableListState> {
        let (connection_id, database, schema) = self.side_table_scope(baseline)?;
        self.tables_cache.get(&table_cache_key(&connection_id, &database, &schema))
    }

    fn table_candidates_for_side(&self, baseline: bool) -> Vec<String> {
        let query = self
            .table_pair
            .as_ref()
            .map(|(left, right)| if baseline { left.as_str() } else { right.as_str() })
            .unwrap_or("");
        match self.table_list_state(baseline) {
            Some(TableListState::Ready(tables)) => filter_table_candidates(tables, query),
            _ => Vec::new(),
        }
    }

    fn step_table_ac(&mut self, baseline: bool, delta: isize) {
        let count = self.table_candidates_for_side(baseline).len();
        if count == 0 {
            self.table_ac_index = 0;
            return;
        }
        let next = (self.table_ac_index as isize + delta).rem_euclid(count as isize) as usize;
        self.table_ac_index = next;
    }

    fn accept_table_candidate(&mut self, side: &'static str, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let baseline = side == "baseline";
        self.set_table_name(baseline, name);
        self.table_ac_side = None;
        self.table_ac_index = 0;
        if let Some((input, _)) = self.table_inputs.get(side) {
            let wanted = name.trim().to_owned();
            input.update(cx, |state, cx| {
                if state.value().as_ref() != wanted {
                    state.set_value(wanted, window, cx);
                }
            });
        }
        cx.notify();
    }

    fn side_summary_label(&self, baseline: bool) -> String {
        let side = if baseline { &self.baseline } else { &self.target };
        let conn = self
            .choices
            .iter()
            .find(|choice| choice.id == side.connection_id)
            .map(|choice| choice.name.as_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("—");
        let mut path = conn.to_owned();
        if !side.database.trim().is_empty() {
            path.push('/');
            path.push_str(side.database.trim());
        }
        if !side.schema.trim().is_empty() {
            path.push('.');
            path.push_str(side.schema.trim());
        }
        if let Some((left, right)) = &self.table_pair {
            let tbl = if baseline { left.trim() } else { right.trim() };
            if !tbl.is_empty() {
                path.push('.');
                path.push_str(tbl);
            }
        }
        path
    }

    fn refresh_choices(&mut self, cx: &mut Context<Self>) {
        self.choices_loading = true;
        let services = Arc::clone(&self.services);
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = services.list_connections().await;
                let choices = match result {
                    Ok(rows) => match crate::application::work::catalog_refresh(services.connections.as_ref(), &[], None, None, true).await {
                        Ok(catalogs) => {
                            let mut listed = BTreeMap::new();
                            for catalog in catalogs {
                                let mut databases: Vec<String> = catalog
                                    .databases
                                    .into_iter()
                                    .map(|database| database.name)
                                    .filter(|name| !name.trim().is_empty())
                                    .collect();
                                databases.sort();
                                databases.dedup();
                                listed.insert(catalog.id, (databases, catalog.error));
                            }
                            Ok(rows
                                .into_iter()
                                .filter(|row| row.saved.engine.is_relational() && services.connections.catalog_session(&row.saved.id).is_some())
                                .map(|row| {
                                    let engine = row.saved.engine.as_str().to_owned();
                                    let (databases, catalog_error) = listed.remove(&row.saved.id).unwrap_or_else(|| (Vec::new(), None));
                                    Choice { id: row.saved.id, name: row.saved.name, engine, databases, catalog_error }
                                })
                                .collect())
                        }
                        Err(message) => Err(message),
                    },
                    Err(message) => Err(message),
                };
                let _ = tx.send(choices);
            });
            if let Ok(choices) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |pane, cx| {
                            match choices {
                                Ok(choices) => {
                                    pane.choices = choices;
                                    pane.clear_error();
                                    pane.choices_loading = false;
                                    // 连接清单到达后再把带入的上下文落到两侧。
                                    pane.apply_context(cx);
                                }
                                Err(message) => {
                                    pane.choices.clear();
                                    pane.fail(message, RetryAction::Choices);
                                    pane.choices_loading = false;
                                }
                            }
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn start_structure(&mut self, cx: &mut Context<Self>) {
        if self.running {
            self.notify(self.text("compare.gates.busy"));
            self.structure = None;
            self.structure_status = Status::Refused;
            return;
        }
        self.clear_error();
        self.data = None;
        self.data_status = Status::Idle;
        self.selected.clear();
        self.chosen_key.clear();
        let Some(request) = self.structure_request() else {
            self.structure = Some(Arc::new(empty_structure(Some(self.text("compare.gates.chooseBoth")))));
            self.structure_status = Status::Refused;
            return;
        };
        if self.table_pair.as_ref().is_some_and(|(left, right)| left.trim().is_empty() || right.trim().is_empty()) {
            self.structure = Some(Arc::new(empty_structure(Some(self.text("compare.table.needBoth")))));
            self.structure_status = Status::Refused;
            return;
        }
        if let Some(message) = self.refuse_local() {
            self.structure = Some(Arc::new(empty_structure(Some(message))));
            self.structure_status = Status::Refused;
            return;
        }
        self.spawn_structure(request, cx);
    }

    fn start_data(&mut self, cx: &mut Context<Self>) {
        if self.running {
            self.notify(self.text("compare.gates.busy"));
            self.data = None;
            self.data_status = Status::Refused;
            return;
        }
        self.clear_error();
        self.data = None;
        let Some(request) = self.data_request() else {
            self.data = Some(Arc::new(empty_data(Some(self.text("compare.gates.chooseBoth")))));
            self.data_status = Status::Refused;
            return;
        };
        if let Some(message) = self.refuse_local() {
            self.data = Some(Arc::new(empty_data(Some(message))));
            self.data_status = Status::Refused;
            return;
        }
        if let Some(message) = self.table_data_block() {
            self.data = Some(Arc::new(empty_data(Some(message))));
            self.data_status = Status::Refused;
            return;
        }
        if request.tables.is_empty() {
            self.data = Some(Arc::new(empty_data(Some(self.text("compare.gates.noTablesPicked")))));
            self.data_status = Status::Refused;
            return;
        }
        self.spawn_data(request, cx);
    }

    fn structure_request(&mut self) -> Option<StructureRequest> {
        let baseline = self.side_request(&self.baseline)?;
        let target = self.side_request(&self.target)?;
        let id = format!("compare-{}", uuid::Uuid::new_v4());
        let objects = self
            .table_pair
            .as_ref()
            .filter(|(left, right)| !left.trim().is_empty() && !right.trim().is_empty())
            .map(|(left, right)| TablePair { baseline: left.clone(), target: right.clone() });
        Some(StructureRequest { id, baseline, target, timeout_secs: self.query_timeout_sec, objects })
    }

    /// 表对表对比时数据对比为什么不能做：还没对比结构，或者这对表没有主键/唯一键。
    /// 不能做时只做结构对比。
    pub fn table_data_block(&self) -> Option<String> {
        self.table_pair.as_ref()?;
        let Some(report) = self.structure.as_ref().filter(|report| report.refused.is_none()) else {
            return Some(self.text("compare.table.structureFirst"));
        };
        crate::compare::pair_data_refusal(report)
    }

    fn data_request(&self) -> Option<DataRequest> {
        let baseline = self.side_request(&self.baseline)?;
        let target = self.side_request(&self.target)?;
        let tables = self
            .structure
            .as_ref()
            .map(|report| {
                report
                    .tables
                    .iter()
                    .filter(|table| table.on_both && self.selected.get(&table.name).copied().unwrap_or(false))
                    .map(|table| match &self.table_pair {
                        // 表对表：报告里的表名是对照表名，基准按自己的表名读。
                        Some((left, right)) => DataTableRequest {
                            name: left.clone(),
                            key: self.chosen_key.get(&table.name).filter(|key| !key.is_empty()).cloned(),
                            right_name: Some(right.clone()),
                        },
                        None => DataTableRequest {
                            name: table.name.clone(),
                            key: self.chosen_key.get(&table.name).filter(|key| !key.is_empty()).cloned(),
                            right_name: None,
                        },
                    })
                    .collect()
            })
            .unwrap_or_default();
        let id = format!("compare-data-{}", uuid::Uuid::new_v4());
        Some(DataRequest { id, baseline, target, tables, timeout_secs: self.query_timeout_sec })
    }

    fn side_request(&self, side: &SidePick) -> Option<SideRequest> {
        let choice = self.choices.iter().find(|choice| choice.id == side.connection_id)?;
        let database = crate::adapters::fixed_database_named(&choice.engine).map(str::to_owned).unwrap_or_else(|| side.database.trim().to_owned());
        let schema = if crate::adapters::shows_schemas_named(&choice.engine) { side.schema.trim().to_owned() } else { String::new() };
        Some(SideRequest { connection_id: choice.id.clone(), database, schema, engine: choice.engine.clone() })
    }

    fn refuse_local(&self) -> Option<String> {
        let left = self.choices.iter().find(|choice| choice.id == self.baseline.connection_id);
        let right = self.choices.iter().find(|choice| choice.id == self.target.connection_id);
        let (Some(left), Some(right)) = (left, right) else {
            return Some(self.text("compare.gates.chooseBoth"));
        };
        if !Engine::parse(&left.engine).is_ok_and(|engine| engine.is_relational()) || !Engine::parse(&right.engine).is_ok_and(|engine| engine.is_relational()) {
            return Some(self.text("compare.gates.unsupported"));
        }
        if left.engine != right.engine {
            return Some(self.text("compare.gates.engineMismatch"));
        }
        if crate::adapters::fixed_database_named(&left.engine).is_none() && (self.baseline.database.trim().is_empty() || self.target.database.trim().is_empty()) {
            return Some(self.text("compare.gates.databasesNeeded"));
        }
        if let Some(message) = self.schema_block(&self.baseline).or_else(|| self.schema_block(&self.target)) {
            return Some(message);
        }
        None
    }

    fn schema_block(&self, side: &SidePick) -> Option<String> {
        let engine = self.engine_of(&side.connection_id)?;
        if !crate::adapters::shows_schemas_named(&engine) {
            return None;
        }
        let needed = || i18n::fill(&self.text("compare.gates.schemaNeeded"), &[("engine", &crate::adapters::label_named(&engine))]);
        if side.database.trim().is_empty() {
            return None;
        }
        match self.schemas.get(&schema_key(&side.connection_id, &side.database)) {
            Some(SchemaState::Loading) | None => Some(self.text("compare.gates.schemaStillLoading")),
            Some(SchemaState::Ready(names)) => {
                let value = side.schema.trim();
                if value.is_empty() || !names.iter().any(|name| name == value) {
                    Some(needed())
                } else {
                    None
                }
            }
            Some(SchemaState::Manual(_)) => {
                if side.schema.trim().is_empty() {
                    Some(needed())
                } else {
                    None
                }
            }
        }
    }

    fn spawn_structure(&mut self, request: StructureRequest, cx: &mut Context<Self>) {
        let token = self.begin_run(request.id.clone(), RunKind::Structure);
        let services = Arc::clone(&self.services);
        let gate = self.take_gate();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = crate::app::views::grid::runtime_for_views().spawn(async move {
            if let Some(gate) = gate {
                let _ = gate.await;
            }
            let outcome = crate::application::work::compare_structure(&services.connections, &services.tasks, OWNER, token, &request).await;
            let _ = tx.send(outcome);
        });
        self.abort = Some(handle.abort_handle());
        self.await_run(token, rx, Self::finish_structure, cx);
    }

    fn spawn_data(&mut self, request: DataRequest, cx: &mut Context<Self>) {
        let token = self.begin_run(request.id.clone(), RunKind::Data);
        let services = Arc::clone(&self.services);
        let gate = self.take_gate();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = crate::app::views::grid::runtime_for_views().spawn(async move {
            if let Some(gate) = gate {
                let _ = gate.await;
            }
            let outcome = crate::application::work::compare_data(&services.connections, &services.tasks, OWNER, token, &request).await;
            let _ = tx.send(outcome);
        });
        self.abort = Some(handle.abort_handle());
        self.await_run(token, rx, Self::finish_data, cx);
    }

    /// 等后台结果写回；任务被中止（取消）时发送端随之丢弃，token 已作废，不会再写回。
    fn await_run<T: Send + 'static>(
        &mut self,
        token: TaskToken,
        rx: tokio::sync::oneshot::Receiver<TaskOutcome<T>>,
        finish: fn(&mut Self, TaskToken, &AtomicBool, TaskOutcome<T>),
        cx: &mut Context<Self>,
    ) {
        let flag = Arc::clone(&self.cancel_flag);
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let outcome = rx.await;
            cx.update(|cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |pane, cx| {
                        match outcome {
                            Ok(outcome) => finish(pane, token, &flag, outcome),
                            Err(_) => pane.finish_lost(token),
                        }
                        cx.notify();
                    });
                }
            });
        })
        .detach();
        self.start_ticker(token, cx);
    }

    /// 运行中每 250ms 读一次后台进度并重绘；这次运行结束或被取消后自己停下。
    /// 测试里不开计时循环：执行器不会自己推进时钟。
    fn start_ticker(&mut self, token: TaskToken, cx: &mut Context<Self>) {
        if cfg!(test) {
            return;
        }
        self.ticker = Some(cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                let keep = view
                    .update(cx, |pane, cx| {
                        if pane.token != Some(token) || !pane.running {
                            return false;
                        }
                        let next = compare_progress(&pane.run_id);
                        if next != pane.progress {
                            pane.progress = next;
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    fn begin_run(&mut self, id: String, kind: RunKind) -> TaskToken {
        if !self.run_id.is_empty() {
            cancel_compare(&self.services.tasks, OWNER, &self.run_id);
        }
        self.services.tasks.supersede(OWNER);
        let token = self.services.tasks.begin(OWNER);
        self.run_id = id;
        self.token = Some(token);
        self.running = true;
        self.run_kind = Some(kind);
        self.progress = None;
        self.cancel_flag = Arc::new(AtomicBool::new(false));
        match kind {
            RunKind::Structure => self.structure_status = Status::Running,
            RunKind::Data => self.data_status = Status::Running,
        }
        token
    }

    /// 取消立即生效：中止后台任务、作废这次的 token、结束运行状态，可以马上再发起一次。
    fn cancel(&mut self) {
        if !self.running {
            return;
        }
        self.cancel_flag.store(true, Ordering::SeqCst);
        cancel_compare(&self.services.tasks, OWNER, &self.run_id);
        if let Some(handle) = self.abort.take() {
            handle.abort();
        }
        self.running = false;
        self.token = None;
        self.progress = None;
        self.ticker = None;
        // 先记成已取消。迟到的成功结果也不能把这份取消覆盖掉。
        self.notify(self.text("compare.status.cancelled"));
        match self.run_kind {
            Some(RunKind::Data) => {
                self.data = None;
                self.data_status = Status::Cancelled;
            }
            _ => {
                self.structure = None;
                self.structure_status = Status::Cancelled;
            }
        }
    }

    fn finish_structure(&mut self, token: TaskToken, flag: &AtomicBool, outcome: TaskOutcome<StructureReport>) {
        if self.token != Some(token) {
            return;
        }
        let cancelled = self.structure_status == Status::Cancelled
            || flag.load(Ordering::SeqCst)
            || matches!(outcome, TaskOutcome::Cancelled { .. })
            || compare_cancelled(&self.run_id);
        self.running = false;
        self.token = None;
        self.abort = None;
        self.progress = None;
        if cancelled {
            self.structure = None;
            self.structure_status = Status::Cancelled;
            self.notify(self.text("compare.status.cancelled"));
            return;
        }
        let TaskOutcome::Current { value, .. } = outcome else {
            self.structure = None;
            self.structure_status = Status::Stale;
            self.notify(self.text("compare.status.stale"));
            return;
        };
        self.install_structure(value);
    }

    fn finish_data(&mut self, token: TaskToken, flag: &AtomicBool, outcome: TaskOutcome<DataReport>) {
        if self.token != Some(token) {
            return;
        }
        let cancelled = self.data_status == Status::Cancelled
            || flag.load(Ordering::SeqCst)
            || matches!(outcome, TaskOutcome::Cancelled { .. })
            || compare_cancelled(&self.run_id);
        self.running = false;
        self.token = None;
        self.abort = None;
        self.progress = None;
        if cancelled {
            self.data = None;
            self.data_status = Status::Cancelled;
            self.notify(self.text("compare.status.cancelled"));
            return;
        }
        let TaskOutcome::Current { value, .. } = outcome else {
            self.data = None;
            self.data_status = Status::Stale;
            self.notify(self.text("compare.status.stale"));
            return;
        };
        self.install_data(value);
    }

    fn finish_lost(&mut self, token: TaskToken) {
        if self.token != Some(token) {
            return;
        }
        self.running = false;
        self.token = None;
        self.abort = None;
        self.progress = None;
        // 任务丢了结果拿不回来：这是失败，统一错误态直接给重试。
        self.fail(self.text("compare.status.failed"), retry_for(self.run_kind));
        match self.run_kind {
            Some(RunKind::Data) => {
                self.data = None;
                self.data_status = Status::Failed;
            }
            _ => {
                self.structure = None;
                self.structure_status = Status::Failed;
            }
        }
    }

    fn install_structure(&mut self, report: StructureReport) {
        self.picked = None;
        if report.timed_out {
            // 读取超时：明确的失败 + 重试，不留一份看似完整的结果。
            let message = report.refused.clone().unwrap_or_else(|| crate::compare::timeout_message(self.query_timeout_sec));
            self.structure = None;
            self.structure_status = Status::Failed;
            self.fail(message, RetryAction::Structure);
            return;
        }
        if report.refused.is_some() {
            self.structure_status = Status::Refused;
            self.structure = Some(Arc::new(report));
            return;
        }
        self.selected.clear();
        for table in &report.tables {
            self.selected.insert(table.name.clone(), table.on_both);
        }
        self.structure_status = if report.incomplete { Status::Incomplete } else { Status::Ready };
        // 默认选中第一条差异，右侧直接有并排定义可看。
        self.picked = first_entry(&report);
        self.structure = Some(Arc::new(report));
    }

    fn install_data(&mut self, report: DataReport) {
        self.data_table = None;
        if report.timed_out {
            let message = report.refused.clone().unwrap_or_else(|| crate::compare::timeout_message(self.query_timeout_sec));
            self.data = None;
            self.data_status = Status::Failed;
            self.fail(message, RetryAction::Data);
            return;
        }
        if report.refused.is_some() {
            self.data_status = Status::Refused;
        } else if report.incomplete {
            self.data_status = Status::Incomplete;
        } else {
            self.data_status = Status::Ready;
        }
        self.data = Some(Arc::new(report));
    }

    fn ask_place(&mut self, kind: RunKind) {
        let report = match kind {
            RunKind::Structure => self.structure.as_ref().map(|report| (report.script.clone(), report.refused.is_some(), self.structure_status)),
            RunKind::Data => self.data.as_ref().map(|report| (report.script.clone(), report.refused.is_some(), self.data_status)),
        };
        let Some((script, refused, status)) = report else {
            return;
        };
        if refused || status == Status::Cancelled || script.trim().is_empty() {
            return;
        }
        self.pending_item_sql = None;
        let lines = script.lines().count();
        self.place_prompt = if self.locale == Locale::En {
            format!("Put a {lines}-line write script into the query editor? It is not executed.")
        } else {
            format!("将把 {lines} 行写操作脚本放入查询编辑器，不会执行。确认放入？")
        };
        self.pending_place = Some(kind);
    }

    fn confirm_place(&mut self) {
        if let Some(sql) = self.pending_item_sql.take() {
            self.pending_place = None;
            if self.structure_status != Status::Cancelled && !sql.trim().is_empty() {
                self.placed = Some(self.place_for(&sql));
            }
            self.place_prompt.clear();
            return;
        }
        match self.pending_place.take() {
            Some(RunKind::Structure) => self.place_structure(),
            Some(RunKind::Data) => self.place_data(),
            None => {}
        }
        self.place_prompt.clear();
    }

    fn place_structure(&mut self) {
        let Some(report) = &self.structure else {
            return;
        };
        if self.structure_status == Status::Cancelled || report.refused.is_some() || report.script.trim().is_empty() {
            return;
        }
        self.placed = Some(self.place_for(&report.script));
    }

    fn place_data(&mut self) {
        let Some(report) = &self.data else {
            return;
        };
        if self.data_status == Status::Cancelled || report.refused.is_some() || report.script.trim().is_empty() {
            return;
        }
        self.placed = Some(self.place_for(&report.script));
    }

    fn place_for(&self, sql: &str) -> PlacedScript {
        let choice = self.choices.iter().find(|choice| choice.id == self.target.connection_id);
        PlacedScript {
            sql: sql.to_owned(),
            connection_id: choice.map(|choice| choice.id.clone()),
            connection_name: choice.map(|choice| choice.name.clone()),
            database: Some(if self.target.database.is_empty() { "main".to_owned() } else { self.target.database.clone() }),
            engine: choice.map(|choice| choice.engine.clone()),
        }
    }

    fn engine_of(&self, id: &str) -> Option<String> {
        self.choices.iter().find(|choice| choice.id == id).map(|choice| choice.engine.clone())
    }

    fn take_gate(&mut self) -> Option<tokio::sync::oneshot::Receiver<()>> {
        #[cfg(test)]
        if self.armed_pause {
            self.armed_pause = false;
            let (sender, receiver) = tokio::sync::oneshot::channel();
            self.pause = Some(sender);
            return Some(receiver);
        }
        None
    }

    fn text(&self, key: &str) -> String {
        i18n::text(self.locale, key)
    }

    fn push_side(&self, lines: &mut Vec<String>, baseline: bool) {
        let side = if baseline { &self.baseline } else { &self.target };
        if side.connection_id.is_empty() {
            return;
        }
        let title = self.text(if baseline { "compare.dialog.baseline" } else { "compare.dialog.target" });
        let name = self.choices.iter().find(|choice| choice.id == side.connection_id).map(|choice| choice.name.as_str()).unwrap_or(side.connection_id.as_str());
        lines.push(format!("{title} {name}"));
        if !side.database.is_empty() {
            lines.push(format!("{} {}", self.text("compare.dialog.database"), side.database));
        }
        let Some(engine) = self.engine_of(&side.connection_id) else {
            return;
        };
        if !crate::adapters::shows_schemas_named(&engine) {
            return;
        }
        match self.schemas.get(&schema_key(&side.connection_id, &side.database)) {
            Some(SchemaState::Loading) | None => lines.push(self.text("compare.dialog.schemaLoading")),
            Some(SchemaState::Ready(names)) => {
                lines.push(self.text("compare.dialog.chooseSchema"));
                for name in names {
                    let mark = if side.schema == *name { "schema-on" } else { "schema" };
                    lines.push(format!("{mark} {name}"));
                }
            }
            Some(SchemaState::Manual(reason)) => {
                if reason.is_empty() {
                    lines.push(self.text("compare.gates.schemaNotInCatalog"));
                } else {
                    lines.push(reason.clone());
                }
                if !side.schema.is_empty() {
                    lines.push(format!("schema-manual {}", side.schema));
                }
            }
        }
    }

    pub fn lines(&self) -> Vec<String> {
        // 只列实际画出来的文字：`compare.dialog.subtitle` 平时只在标题悬停提示里，初始指导出现时才常驻。
        let mut lines = vec![self.text(if self.table_pair.is_some() { "compare.table.title" } else { "compare.dialog.title" })];
        if let Some((left, right)) = &self.table_pair {
            lines.push(format!("{} {left}", self.text("compare.table.baselineTable")));
            lines.push(format!("{} {right}", self.text("compare.table.targetTable")));
            if let Some(reason) = self.table_data_block().filter(|_| self.structure.is_some()) {
                lines.push(reason);
            }
        }
        if self.show_empty_guide() {
            // 初始指导现在是一整块状态：标题、说明与主操作都要算进画出来的文字。
            lines.push(self.text("compare.empty.guide"));
            lines.push(self.text("compare.dialog.subtitle"));
            lines.push(self.text("compare.dialog.compareStructure"));
        }
        if self.choices.is_empty() {
            lines.push(self.text("compare.dialog.chooseConnection"));
        }
        for choice in &self.choices {
            lines.push(format!("{} {}", choice.name, choice.engine));
            for database in &choice.databases {
                lines.push(format!("database {} {database}", choice.id));
            }
            if let Some(error) = &choice.catalog_error {
                lines.push(format!("{} {error}", choice.name));
            }
        }
        self.push_side(&mut lines, true);
        self.push_side(&mut lines, false);
        if self.running {
            lines.push(self.progress_text());
        }
        match self.error_kind {
            // 失败：统一错误态里的标题、原因与重试按钮。
            ErrorKind::Failure(_) => {
                let heading = self.error_heading();
                lines.push(heading.clone());
                let detail = self.error_detail();
                if detail != heading {
                    lines.push(detail);
                }
                lines.push(self.text("common.retry"));
            }
            // 提示：横幅里那句原因。
            _ => {
                if !self.error.is_empty() {
                    lines.push(self.error.clone());
                }
            }
        }
        if self.pending_place.is_some() {
            lines.push(self.place_prompt.clone());
        }
        if let Some(report) = &self.structure {
            if let Some(refused) = &report.refused {
                lines.push(refused.clone());
            } else {
                if report.incomplete || self.structure_status == Status::Incomplete {
                    lines.push(self.text("compare.dialog.scriptIncomplete"));
                }
                lines.push(fill(
                    &self.text("compare.dialog.diffSummary"),
                    &[
                        ("{baseline}", &report.only_baseline.len().to_string()),
                        ("{target}", &report.only_target.len().to_string()),
                        ("{changed}", &report.changed.len().to_string()),
                    ],
                ));
                lines.push(format!("{} {}", self.text("compare.summary.same"), report.same.len()));
                for (kind, plus, minus, changed) in kind_summary(report) {
                    lines.push(format!("kind {kind} +{plus} −{minus} ~{changed}"));
                }
                if report.only_baseline.is_empty() && report.only_target.is_empty() && report.changed.is_empty() {
                    lines.push(self.text("compare.summary.noDiff"));
                }
                lines.push(self.text(self.diff_filter.key()));
                if self.only_diffs {
                    lines.push(self.text("compare.filter.onlyDiffs"));
                }
                if !self.diff_query.trim().is_empty() {
                    lines.push(format!("search {}", self.diff_query.trim()));
                }
                for item in report.only_baseline.iter().filter(|item| self.shows_diff(DiffFilter::Baseline, &item.name)) {
                    lines.push(fill(&self.text("compare.dialog.onlyBaseline"), &[("{kind}", &item.kind), ("{name}", &item.name)]));
                }
                for item in report.only_target.iter().filter(|item| self.shows_diff(DiffFilter::Target, &item.name)) {
                    lines.push(fill(&self.text("compare.dialog.onlyTarget"), &[("{kind}", &item.kind), ("{name}", &item.name)]));
                }
                for item in report.changed.iter().filter(|item| self.shows_diff(DiffFilter::Changed, &item.name)) {
                    lines.push(format!("{} {} {}", item.kind, item.name, item.detail));
                }
                self.tree_lines(report, &mut lines);
                for note in &report.notes {
                    lines.push(note.clone());
                }
                if report.tables.is_empty() {
                    lines.push(self.text("compare.dialog.noTables"));
                }
                for table in report.tables.iter().filter(|table| name_matches(&table.name, &self.diff_query)) {
                    let mark = if self.selected.get(&table.name).copied().unwrap_or(false) { "on" } else { "off" };
                    lines.push(format!("{mark} {}", table.name));
                    if !table.on_both {
                        lines.push(self.text("compare.dialog.oneSideOnly"));
                    }
                    if table.needs_key_choice {
                        lines.push(self.text("compare.dialog.chooseKey"));
                        for key in &table.keys {
                            lines.push(format!("{} {}", key.name, key.columns.join(", ")));
                        }
                    }
                    if let Some(key) = self.chosen_key.get(&table.name) {
                        lines.push(format!("chosen {} {key}", table.name));
                    }
                }
                if self.structure_status != Status::Cancelled && !report.script.trim().is_empty() {
                    lines.push(self.text("compare.dialog.structureScript"));
                    lines.push(self.text("compare.script.whole"));
                    lines.push(self.text("compare.script.copy"));
                    lines.push(report.script.clone());
                }
            }
        }
        if self.structure_status == Status::Cancelled {
            lines.push(self.text("compare.status.cancelled"));
        }
        if let Some(report) = &self.data {
            lines.push(self.text("compare.dialog.dataTitle"));
            if let Some(refused) = &report.refused {
                lines.push(refused.clone());
            } else {
                if report.incomplete || self.data_status == Status::Incomplete {
                    lines.push(self.text("compare.dialog.dataScriptIncomplete"));
                }
                for diff in &report.diffs {
                    let key = if diff.key.is_empty() { String::new() } else { format!(" {} {}", self.text("compare.dialog.key"), diff.key) };
                    lines.push(format!("{} {}{key}", diff.table, diff.detail));
                }
                self.data_lines(report, &mut lines);
                for note in &report.notes {
                    lines.push(note.clone());
                }
                if self.data_status != Status::Cancelled && !report.script.trim().is_empty() {
                    lines.push(self.text("compare.dialog.dataScript"));
                    lines.push(self.text("compare.script.copy"));
                    lines.push(report.script.clone());
                }
            }
        }
        if self.data_status == Status::Cancelled {
            lines.push(self.text("compare.status.cancelled"));
        }
        lines
    }
}

impl ComparePane {
    fn show_empty_guide(&self) -> bool {
        self.structure.is_none()
            && self.data.is_none()
            && !self.running
            && self.structure_status == Status::Idle
            && self.data_status == Status::Idle
            && self.error_kind == ErrorKind::None
    }

    fn shows_diff(&self, kind: DiffFilter, name: &str) -> bool {
        (self.diff_filter == DiffFilter::All || self.diff_filter == kind) && name_matches(name, &self.diff_query)
    }
}

impl DiffFilter {
    fn key(self) -> &'static str {
        match self {
            DiffFilter::All => "compare.filter.all",
            DiffFilter::Baseline => "compare.filter.baselineOnly",
            DiffFilter::Target => "compare.filter.targetOnly",
            DiffFilter::Changed => "compare.filter.changed",
        }
    }
}

fn name_matches(name: &str, query: &str) -> bool {
    let query = query.trim();
    query.is_empty() || name.to_ascii_lowercase().contains(&query.to_ascii_lowercase())
}

fn schema_key(id: &str, database: &str) -> String {
    format!("{id}\u{0}{database}")
}

fn table_cache_key(id: &str, database: &str, schema: &str) -> String {
    format!("{id}\u{0}{database}\u{0}{schema}")
}

/// 表名补全筛选：前缀匹配优先（不区分大小写），其次子串匹配；输入为空时返回全部可用表。
fn filter_table_candidates(tables: &[String], query: &str) -> Vec<String> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return tables.to_vec();
    }
    let mut prefix = Vec::new();
    let mut contains = Vec::new();
    for name in tables {
        let lower = name.to_ascii_lowercase();
        if lower.starts_with(&q) {
            prefix.push(name.clone());
        } else if lower.contains(&q) {
            contains.push(name.clone());
        }
    }
    prefix.extend(contains);
    prefix
}

/// 失败的那次比较属于哪一边：重试就重发同一边，而不是另起一次比较。
fn retry_for(kind: Option<RunKind>) -> RetryAction {
    match kind {
        Some(RunKind::Data) => RetryAction::Data,
        _ => RetryAction::Structure,
    }
}

fn fill(template: &str, pairs: &[(&str, &str)]) -> String {
    let mut text = template.to_owned();
    for (needle, value) in pairs {
        text = text.replace(needle, value);
    }
    text
}

fn empty_structure(refused: Option<String>) -> StructureReport {
    StructureReport {
        refused,
        script: String::new(),
        incomplete: false,
        notes: Vec::new(),
        only_baseline: Vec::new(),
        only_target: Vec::new(),
        changed: Vec::new(),
        same: Vec::new(),
        tables: Vec::new(),
        timed_out: false,
    }
}

fn empty_data(refused: Option<String>) -> DataReport {
    DataReport { refused, script: String::new(), incomplete: false, notes: Vec::new(), diffs: Vec::new(), tables: Vec::new(), timed_out: false }
}

use crate::ui::kit::stack_frame;

impl Render for ComparePane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let m = kit::Metrics::of(cx);
        let entity = cx.entity();
        let t = |this: &Self, key: &str| SharedString::from(this.text(key));

        let viewport = f32::from(window.viewport_size().width);
        let stacked = sides_stacked(viewport);
        let page_pad = m.page_pad_for(viewport);
        let guide_open = self.show_empty_guide();
        let has_report = self.structure.is_some() || self.data.is_some();
        let table_mode = self.table_pair.is_some();

        // 顶部统一操作栏：标题 + 紧凑模式切换（对比表/对比库）+ 对比结构/对比数据 + 运行态/取消 + 返回工作项。
        let header = stack_frame(|| {
            let mode_items = vec![
                (
                    SharedString::from("compare-mode:database"),
                    t(self, "compare.dialog.title"),
                    !table_mode,
                    kit::click({
                        let entity = entity.clone();
                        move |_, _, cx| entity.update(cx, |pane, cx| pane.command("compare-mode:database", cx))
                    }),
                ),
                (
                    SharedString::from("compare-mode:table"),
                    t(self, "compare.table.title"),
                    table_mode,
                    kit::click({
                        let entity = entity.clone();
                        move |_, _, cx| entity.update(cx, |pane, cx| pane.command("compare-mode:table", cx))
                    }),
                ),
            ];
            let structure_btn = if guide_open {
                kit::secondary_button("compare-structure", Some(IconName::Columns3), t(self, "compare.dialog.compareStructure"), compare_act("compare-structure", &entity))
            } else {
                kit::primary_button("compare-structure", Some(IconName::Columns3), t(self, "compare.dialog.compareStructure"), compare_act("compare-structure", &entity))
            };
            let mut right = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(Space::SM))
                .child(structure_btn)
                .child(kit::secondary_button("compare-data", Some(IconName::Rows3), t(self, "compare.dialog.compareData"), compare_act("compare-data", &entity)));
            if self.running {
                right = right
                    .child(kit::tool_button("compare-cancel", Some(IconName::X), t(self, "common.cancel"), compare_act("compare-cancel", &entity)))
                    .child(kit::inline_loading(SharedString::from(self.progress_text()), &m, &p));
            }
            right = right
                .child(kit::vline(&p))
                .child(kit::tool_button("compare-back", Some(IconName::ArrowLeft), t(self, "compare.back"), compare_act("compare-back", &entity)));

            div()
                .id("compare-title")
                .tooltip_text(t(self, "compare.dialog.subtitle"))
                .flex_none()
                .min_h(px(m.header_h()))
                .px(px(page_pad))
                .py(px(Space::SM))
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(px(Space::MD))
                .border_b_1()
                .border_color(p.line_soft)
                .bg(p.surface)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(Space::MD))
                        .child(kit::title(t(self, if table_mode { "compare.table.title" } else { "compare.dialog.title" }), TypeScale::LG, &p))
                        .child(kit::segmented_sized(mode_items, &p, m.control_h)),
                )
                .child(right)
        });

        // 统一来源配置卡：有报告时可一键收起成单行摘要胶囊，把 75%+ 视口留给差异审阅。
        let sources_card = if has_report && self.sources_collapsed {
            self.collapsed_sources_bar(&entity, &p)
        } else {
            let baseline_card = self.side_card(true, window, cx);
            let target_card = self.side_card(false, window, cx);
            self.schema_reload = false;
            let pair_row = table_mode.then(|| self.pair_card(&entity, window, cx));
            let swap = kit::icon_button("compare-swap", IconName::ArrowRightLeft, t(self, "compare.action.swap"), compare_act("compare-swap", &entity)).outline();
            let sides = stack_frame(|| {
                if stacked {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(Space::MD))
                        .child(div().w_full().min_w_0().child(baseline_card))
                        .child(div().flex().justify_center().child(swap))
                        .child(div().w_full().min_w_0().child(target_card))
                } else {
                    div()
                        .flex()
                        .gap(px(Space::MD))
                        .items_start()
                        .child(div().flex_1().flex_basis(px(ZERO_PX)).min_w_0().child(baseline_card))
                        .child(div().flex_none().pt(px(m.control_h)).child(swap))
                        .child(div().flex_1().flex_basis(px(ZERO_PX)).min_w_0().child(target_card))
                }
            });
            let collapse_label = if self.locale == Locale::En { "Collapse" } else { "收起配置" };
            let show_top_bar = !self.context_note.is_empty() || table_mode || has_report;
            let mut card = kit::card(&p)
                .id("compare-sources")
                .test_support()
                .p(px(m.card_pad()))
                .flex()
                .flex_col()
                .gap(px(Space::MD));
            if show_top_bar {
                card = card.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap(px(Space::SM))
                        .child(
                            div()
                                .id("compare-context")
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(TypeScale::XS))
                                .text_color(p.text_muted)
                                .child(SharedString::from(self.context_note.clone())),
                        )
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(Space::XS))
                                .when(table_mode, |this| {
                                    this.child(
                                        kit::tool_button("compare-table-exit", Some(IconName::Database), t(self, "compare.table.exit"), compare_act("compare-table-exit", &entity))
                                            .xsmall(),
                                    )
                                })
                                .when(has_report, |this| {
                                    this.child(
                                        kit::tool_button("compare-toggle-sources", Some(IconName::ChevronUp), SharedString::from(collapse_label), compare_act("compare-toggle-sources", &entity))
                                            .xsmall(),
                                    )
                                }),
                        ),
                );
            }
            card = card.when_some(pair_row, |this, row| this.child(row)).child(sides);
            card.into_any_element()
        };

        // 表对表：数据对比做不了时说明原因，只做结构对比。
        let data_block = self.table_data_block().filter(|_| self.structure.as_ref().is_some_and(|report| report.refused.is_none()));
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(Space::LG))
            .child(sources_card)
            .when_some(data_block, |this, reason| this.child(note_banner(IconName::Info, reason, p.warn, p.warn_soft)));
        body = stack_frame(|| {
            let mut body = body;
            // 正在比较且还没有报告：给骨架占位与一条局部加载，不让详情区空着。
            if self.running && self.structure.is_none() && self.data.is_none() {
                body = body.child(
                    kit::card(&p)
                        .p(px(m.card_pad()))
                        .flex()
                        .flex_col()
                        .gap(px(Space::MD))
                        .child(kit::inline_loading(SharedString::from(self.progress_text()), &m, &p))
                        .child(kit::skeleton_state(5, &m, &p)),
                );
            }
            match self.error_kind {
                // 失败：统一错误态 + 重试，重试只重发失败的那一步。
                ErrorKind::Failure(_) => {
                    let entity = entity.clone();
                    let retry = kit::secondary_button("compare-retry", Some(IconName::RefreshCw), t(self, "common.retry"), move |_, _, cx| {
                        entity.update(cx, |pane, cx| pane.command("compare-retry", cx));
                    });
                    body = body.child(kit::error_state_with_action(
                        SharedString::from(self.error_heading()),
                        SharedString::from(self.error_detail()),
                        retry,
                        &p,
                    ));
                }
                // 提示：门禁、取消、过期。横幅说原因，不提供重试。
                ErrorKind::Notice => {
                    body = body.child(note_banner(IconName::CircleAlert, self.error.clone(), p.danger, p.danger_soft));
                }
                ErrorKind::None => {}
            }
            if self.structure_status == Status::Cancelled || self.data_status == Status::Cancelled {
                body = body.child(note_banner(IconName::Info, self.text("compare.status.cancelled"), p.warn, p.warn_soft));
            }
            if self.pending_place.is_some() {
                body = body.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(Space::MD))
                        .px(px(Space::LG))
                        .py(px(Space::MD))
                        .rounded(px(Radius::PANEL))
                        .bg(p.warn_soft)
                        .text_color(p.warn)
                        .child(kit::icon(IconName::TriangleAlert, kit::IconSize::CONTROL, p.warn))
                        .child(div().flex_1().min_w(px(PROMPT_MIN_W)).text_size(px(TypeScale::SM)).child(SharedString::from(self.place_prompt.clone())))
                        .child(kit::secondary_button("compare-place-cancel", None, t(self, "common.cancel"), compare_act("compare-place-cancel", &entity)))
                        .child(kit::primary_button(
                            "compare-place-confirm",
                            Some(IconName::Check),
                            SharedString::from(if self.locale == Locale::En { "Confirm" } else { "确认放入" }),
                            compare_act("compare-place-confirm", &entity),
                        )),
                );
            }

            if guide_open {
                body = body.child(kit::empty_state_with_action(
                    IconName::GitCompare,
                    t(self, "compare.empty.guide"),
                    t(self, "compare.dialog.subtitle"),
                    kit::primary_button(
                        "compare-guide-start",
                        Some(IconName::Columns3),
                        t(self, "compare.dialog.compareStructure"),
                        compare_act("compare-structure", &entity),
                    ),
                    &p,
                ));
            }
            body
        });

        if let Some(report) = self.structure.clone() {
            let card = self.structure_card(&report, &entity, stacked, window, cx);
            body = body.child(card);
        }

        if let Some(report) = self.data.clone() {
            let card = self.data_card(&report, &entity, window, cx);
            body = body.child(card);
        } else {
            body = body.child(div().hidden().child(kit::tool_button("compare-place:data", None, t(self, "compare.dialog.dataDisclosure"), compare_act("compare-place:data", &entity))));
        }
        if self.structure.is_none() {
            body = body.child(div().hidden().child(kit::tool_button("compare-place:structure", None, t(self, "compare.dialog.placeToQuery"), compare_act("compare-place:structure", &entity))));
        }

        stack_frame(|| {
            div()
                .id("compare-pane")
                .size_full()
                .flex()
                .flex_col()
                .overflow_hidden()
                .bg(p.chrome)
                .child(header)
                .child(
                    div()
                        .id("compare-scroll")
                        .flex_1()
                        .min_h_0()
                        .scroll_area_y()
                        .child(
                            div()
                                .max_w(px(PAGE_MAX_W))
                                .mx_auto()
                                .px(px(page_pad))
                                .py(px(Space::LG))
                                .flex()
                                .flex_col()
                                .gap(px(Space::LG))
                                .child(body),
                        ),
                )
        })
    }
}

impl ComparePane {
    fn collapsed_sources_bar(&self, entity: &Entity<Self>, p: &Palette) -> AnyElement {
        let base_label = self.text("compare.dialog.baseline");
        let target_label = self.text("compare.dialog.target");
        let base_path = self.side_summary_label(true);
        let target_path = self.side_summary_label(false);
        let expand_label = if self.locale == Locale::En { "Configure" } else { "展开配置" };
        let swap_label = SharedString::from(self.text("compare.action.swap"));
        kit::card(p)
            .id("compare-sources-collapsed")
            .test_support()
            .px(px(Space::LG))
            .py(px(Space::MD))
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(Space::MD))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(Space::SM))
                    .min_w_0()
                    .child(kit::pill(SharedString::from(format!("{base_label}: {base_path}")), p.ok, p.ok_soft))
                    .child(kit::icon(IconName::ArrowRightLeft, kit::IconSize::INLINE, p.text_faint))
                    .child(kit::pill(SharedString::from(format!("{target_label}: {target_path}")), p.info, p.accent_soft)),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(Space::XS))
                    .child(kit::icon_button("compare-swap", IconName::ArrowRightLeft, swap_label, compare_act("compare-swap", entity)).xsmall())
                    .child(kit::tool_button("compare-toggle-sources", Some(IconName::ChevronDown), SharedString::from(expand_label), compare_act("compare-toggle-sources", entity)).xsmall()),
            )
            .into_any_element()
    }

    fn side_card(&mut self, baseline: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = kit::Metrics::of(cx);
        let entity = cx.entity();
        let which = if baseline { "baseline" } else { "target" };
        let title = self.text(if baseline { "compare.dialog.baseline" } else { "compare.dialog.target" });
        let side = if baseline { &self.baseline } else { &self.target };
        let side_connection = side.connection_id.clone();
        let side_database = side.database.clone();
        let side_schema = side.schema.clone();
        let summary = self.side_summary_label(baseline);
        let mut card = div()
            .id(SharedString::from(format!("compare-side:{which}")))
            .test_support()
            .rounded(px(Radius::CONTROL))
            .bg(p.chrome)
            .border_1()
            .border_color(p.line_soft)
            .p(px(Space::MD))
            .flex()
            .flex_col()
            .gap(px(Space::MD))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(Space::SM))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(Space::SM))
                            .child(kit::pill(SharedString::from(title.clone()), if baseline { p.ok } else { p.info }, if baseline { p.ok_soft } else { p.accent_soft }))
                            .child(kit::muted(SharedString::from(self.text("compare.dialog.connection")), &p)),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_right().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(summary))),
            );
        let mut connections = kit::sunken(&p)
            .id(SharedString::from(format!("compare-connections:{which}")))
            .p(px(Space::XS))
            .gap(px(Space::XXS))
            .flex()
            .flex_col()
            .max_h(px(m.control_h * SIDE_LIST_VISIBLE_ROWS))
            .scroll_area_y();
        if self.choices.is_empty() {
            // 清单还在读：骨架占位，不先写一句“选择连接”；读完了还是空才提示选连接。
            connections = if self.choices_loading {
                connections.child(kit::skeleton_state(3, &m, &p))
            } else {
                connections.child(div().flex_none().px(px(Space::MD)).py(px(Space::SM)).child(kit::muted(SharedString::from(self.text("compare.dialog.chooseConnection")), &p)))
            };
        }
        for choice in self.choices.clone() {
            let id = format!("compare-{which}:{}", choice.id);
            let selected = side_connection == choice.id;
            connections = connections.child(
                list_choice(SharedString::from(id.clone()), SharedString::from(format!("{title} {}", choice.name)), SharedString::from(choice.name.clone()), selected, IconName::Database, &p, m)
                    .on_click(compare_act(id, &entity)),
            );
            if selected {
                if let Some(error) = &choice.catalog_error {
                    card = card.child(note_banner(IconName::CircleAlert, error.clone(), p.danger, p.danger_soft));
                }
            }
        }
        card = card.child(connections);
        if let Some(choice) = self.choices.iter().find(|choice| choice.id == side_connection).cloned() {
            if !choice.databases.is_empty() {
                let database_label = self.text("compare.dialog.database");
                let mut list = kit::sunken(&p)
                    .id(SharedString::from(format!("compare-databases:{which}")))
                    .p(px(Space::XS))
                    .gap(px(Space::XXS))
                    .flex()
                    .flex_col()
                    .max_h(px(m.control_h * SIDE_LIST_VISIBLE_ROWS))
                    .scroll_area_y();
                for name in &choice.databases {
                    let id = format!("compare-database:{which}:{name}");
                    list = list.child(
                        list_choice(SharedString::from(id.clone()), SharedString::from(format!("{title} {database_label} {name}")), SharedString::from(name.clone()), side_database == *name, IconName::Database, &p, m)
                            .on_click(compare_act(id, &entity)),
                    );
                }
                card = card.child(div().flex().flex_col().gap(px(Space::XS)).child(kit::eyebrow(SharedString::from(database_label), &p)).child(list));
            }
            if crate::adapters::shows_schemas_named(&choice.engine) {
                let schema_label = self.text("compare.dialog.chooseSchema");
                match self.schemas.get(&schema_key(&side_connection, &side_database)).cloned() {
                    Some(SchemaState::Ready(names)) => {
                        let mut list = kit::sunken(&p)
                            .id(SharedString::from(format!("compare-schemas:{which}")))
                            .p(px(Space::XS))
                            .gap(px(Space::XXS))
                            .flex()
                            .flex_col()
                            .max_h(px(m.control_h * SIDE_LIST_VISIBLE_ROWS))
                            .scroll_area_y();
                        for name in names {
                            let id = format!("compare-schema:{which}:{name}");
                            list = list.child(
                                list_choice(SharedString::from(id.clone()), SharedString::from(format!("{title} {schema_label} {name}")), SharedString::from(name.clone()), side_schema == name, IconName::Layers, &p, m)
                                    .on_click(compare_act(id, &entity)),
                            );
                        }
                        card = card.child(div().flex().flex_col().gap(px(Space::XS)).child(kit::eyebrow(SharedString::from(schema_label), &p)).child(list));
                    }
                    Some(SchemaState::Manual(reason)) => {
                        let input = self.schema_input(which, window, cx);
                        let placeholder = self.text("compare.dialog.schemaManualPlaceholder");
                        let value = self.schema_draft.get(which).cloned().unwrap_or_default();
                        let label = if reason.is_empty() { format!("{placeholder} {value}") } else { format!("{reason} {placeholder} {value}") };
                        card = card.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(Space::XS))
                                .child(kit::eyebrow(SharedString::from(schema_label), &p))
                                .when(!reason.is_empty(), |this| this.child(div().text_size(px(TypeScale::SM)).text_color(p.warn).child(SharedString::from(reason.clone()))))
                                .child(
                                    div()
                                        .capture_key_down({
                                            let entity = entity.clone();
                                            let input = input.clone();
                                            move |event: &gpui_kit::KeyDownEvent, _, cx| {
                                                if event.keystroke.key != "enter" {
                                                    return;
                                                }
                                                // 回车当场提交：读文本框现值，不等输入事件排队送达。
                                                let name = input.read(cx).value().to_string();
                                                entity.update(cx, |pane, cx| {
                                                    pane.schema_draft.insert(which.to_owned(), name.clone());
                                                    pane.command(&format!("compare-schema:{which}:{name}"), cx);
                                                });
                                            }
                                        })
                                        .child(kit::text_input(SharedString::from(format!("compare-schema-input:{which}")), &input, label)),
                                ),
                        );
                    }
                    Some(SchemaState::Loading) | None => {
                        // 局部加载：一行小号转圈、贴左、单行高，不占整块也不自己拼转圈加文字。
                        card = card.child(kit::inline_loading(SharedString::from(self.text("compare.dialog.schemaLoading")), &m, &p));
                    }
                }
            }
        }
        card.into_any_element()
    }

    /// 表对表对比的表名行：左右对齐基准表与对照表，附智能表名补全浮层。
    fn pair_card(&mut self, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = kit::Metrics::of(cx);
        let baseline_input = self.table_input("baseline", window, cx);
        let target_input = self.table_input("target", window, cx);
        self.table_reload = false;
        let baseline_field = self.table_pair_field("baseline", self.text("compare.table.baselineTable"), baseline_input, p.ok, p.ok_soft, entity, &p, &m);
        let target_field = self.table_pair_field("target", self.text("compare.table.targetTable"), target_input, p.info, p.accent_soft, entity, &p, &m);
        div()
            .id("compare-table-pair")
            .test_support()
            .rounded(px(Radius::CONTROL))
            .bg(p.chrome)
            .border_1()
            .border_color(p.line_soft)
            .p(px(Space::MD))
            .flex()
            .flex_wrap()
            .gap(px(Space::MD))
            .child(baseline_field)
            .child(target_field)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn table_pair_field(
        &self,
        which: &'static str,
        label: String,
        input: Entity<InputState>,
        tone: gpui_kit::Hsla,
        soft: gpui_kit::Hsla,
        entity: &Entity<Self>,
        p: &Palette,
        m: &kit::Metrics,
    ) -> AnyElement {
        let baseline = which == "baseline";
        let ac_open = self.table_ac_side == Some(which);
        let ac_panel = ac_open.then(|| self.table_ac_panel(which, entity, p, m));
        let keys_entity = entity.clone();
        div()
            .flex_1()
            .min_w(px(kit::ControlSize::PICKER_MIN_W))
            .flex()
            .flex_col()
            .gap(px(Space::XS))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(Space::SM))
                    .child(kit::pill(SharedString::from(label.clone()), tone, soft))
                    .child(
                        div()
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text_faint)
                            .child(SharedString::from(if self.locale == Locale::En { "Type or pick from catalog" } else { "输入或从库中选择" })),
                    ),
            )
            .child(
                div()
                    .relative()
                    .capture_key_down(move |event: &gpui_kit::KeyDownEvent, window, cx| {
                        let key = event.keystroke.key.as_str();
                        match key {
                            "down" | "up" | "enter" | "tab" | "escape" => {}
                            _ => return,
                        }
                        let is_open = keys_entity.read(cx).table_ac_side == Some(which);
                        if !is_open {
                            return;
                        }
                        match key {
                            "down" => {
                                cx.stop_propagation();
                                keys_entity.update(cx, |pane, cx| {
                                    pane.step_table_ac(baseline, 1);
                                    cx.notify();
                                });
                            }
                            "up" => {
                                cx.stop_propagation();
                                keys_entity.update(cx, |pane, cx| {
                                    pane.step_table_ac(baseline, -1);
                                    cx.notify();
                                });
                            }
                            "enter" | "tab" => {
                                let picked = {
                                    let pane = keys_entity.read(cx);
                                    let list = pane.table_candidates_for_side(baseline);
                                    list.get(pane.table_ac_index).or_else(|| list.first()).cloned()
                                };
                                if let Some(name) = picked {
                                    cx.stop_propagation();
                                    keys_entity.update(cx, |pane, cx| pane.accept_table_candidate(which, &name, window, cx));
                                } else {
                                    keys_entity.update(cx, |pane, cx| {
                                        pane.table_ac_side = None;
                                        cx.notify();
                                    });
                                }
                            }
                            "escape" => {
                                cx.stop_propagation();
                                keys_entity.update(cx, |pane, cx| {
                                    pane.table_ac_side = None;
                                    cx.notify();
                                });
                            }
                            _ => {}
                        }
                    })
                    .child(kit::text_input(SharedString::from(format!("compare-table-input:{which}")), &input, label))
                    .when_some(ac_panel, |this, panel| {
                        this.child(
                            div().absolute().top_full().left_0().w_full().pt(px(Space::XS)).child(
                                gpui_kit::deferred(
                                    gpui_kit::anchored()
                                        .anchor(gpui_kit::Anchor::TopLeft)
                                        .snap_to_window_with_margin(px(kit::ControlSize::POPOVER_MARGIN))
                                        .child(panel),
                                )
                                .with_priority(10),
                            ),
                        )
                    }),
            )
            .into_any_element()
    }

    fn table_ac_panel(&self, which: &'static str, entity: &Entity<Self>, p: &Palette, m: &kit::Metrics) -> AnyElement {
        let baseline = which == "baseline";
        let state = self.table_list_state(baseline);
        let candidates = self.table_candidates_for_side(baseline);
        let active = if candidates.is_empty() { 0 } else { self.table_ac_index.min(candidates.len() - 1) };
        let close_entity = entity.clone();

        let body: AnyElement = match state {
            None => div()
                .flex_none()
                .px(px(Space::MD))
                .py(px(Space::MD))
                .text_size(px(TypeScale::XS))
                .text_color(p.text_muted)
                .child(SharedString::from(if self.locale == Locale::En {
                    "Select a connection and database to browse tables."
                } else {
                    "请先选择连接与数据库以加载表名，也可直接手动输入。"
                }))
                .into_any_element(),
            Some(TableListState::Loading) => div()
                .flex_none()
                .px(px(Space::MD))
                .py(px(Space::SM))
                .child(kit::inline_loading(
                    SharedString::from(if self.locale == Locale::En { "Loading tables…" } else { "正在读取表列表…" }),
                    m,
                    p,
                ))
                .into_any_element(),
            Some(TableListState::Error(reason)) => div()
                .flex_none()
                .px(px(Space::MD))
                .py(px(Space::MD))
                .flex()
                .flex_col()
                .gap(px(Space::XXS))
                .child(div().text_size(px(TypeScale::XS)).text_color(p.warn).child(SharedString::from(reason.clone())))
                .child(
                    div()
                        .text_size(px(TypeScale::XS))
                        .text_color(p.text_faint)
                        .child(SharedString::from(if self.locale == Locale::En { "Manual entry is still supported." } else { "仍可直接手动输入表名。" })),
                )
                .into_any_element(),
            Some(TableListState::Ready(_)) if candidates.is_empty() => div()
                .flex_none()
                .px(px(Space::MD))
                .py(px(Space::MD))
                .text_size(px(TypeScale::XS))
                .text_color(p.text_muted)
                .child(SharedString::from(if self.locale == Locale::En {
                    "No matching tables — manual entry is supported."
                } else {
                    "没有匹配的表，可直接手动输入。"
                }))
                .into_any_element(),
            Some(TableListState::Ready(_)) => {
                let mut list = div()
                    .id(SharedString::from(format!("compare-table-ac-list:{which}")))
                    .max_h(px(TABLE_AC_MAX_H))
                    .p(px(Space::XS))
                    .pr(px(kit::SCROLLBAR_GUTTER))
                    .flex()
                    .flex_col()
                    .gap(px(Space::XXS))
                    .scroll_area_y();
                for (ix, name) in candidates.into_iter().enumerate() {
                    let highlighted = ix == active;
                    let item_entity = entity.clone();
                    let name_for_click = name.clone();
                    list = list.child(
                        div()
                            .id(SharedString::from(format!("compare-table-ac:{which}:{name}")))
                            .test_support()
                            .flex_none()
                            .role(gpui_kit::Role::Button)
                            .aria_label(SharedString::from(name.clone()))
                            .aria_selected(highlighted)
                            .h(px(m.menu_row_h))
                            .px(px(Space::MD))
                            .rounded(px(Radius::CONTROL))
                            .flex()
                            .items_center()
                            .gap(px(Space::SM))
                            .text_size(px(TypeScale::SM))
                            .cursor_pointer()
                            .when(highlighted, |this| this.bg(p.accent_soft).text_color(p.accent_text).font_weight(gpui_kit::FontWeight::MEDIUM))
                            .when(!highlighted, |this| this.text_color(p.text).hover(|style| style.bg(p.hover)))
                            .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                            })
                            .on_click(move |_, window, cx| {
                                item_entity.update(cx, |pane, cx| pane.accept_table_candidate(which, &name_for_click, window, cx));
                            })
                            .child(kit::icon(IconName::Table, kit::IconSize::INLINE, if highlighted { p.accent_text } else { p.text_faint }))
                            .child(div().flex_1().min_w_0().truncate().child(SharedString::from(name))),
                    );
                }
                list.into_any_element()
            }
        };

        let footer_hint = if self.locale == Locale::En {
            "↑↓ Navigate · Enter/Tab Accept · Esc Close"
        } else {
            "↑↓ 选择 · Enter/Tab 确认 · Esc 关闭"
        };

        div()
            .id(SharedString::from(format!("compare-table-ac:{which}")))
            .test_support()
            .occlude()
            .w_full()
            .min_w(px(kit::ControlSize::PICKER_MIN_W))
            .bg(p.raised)
            .border_1()
            .border_color(p.line)
            .rounded(px(Radius::PANEL))
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .flex_col()
            .on_mouse_down_out(move |_, _, cx| {
                close_entity.update(cx, |pane, cx| {
                    if pane.table_ac_side == Some(which) {
                        pane.table_ac_side = None;
                        cx.notify();
                    }
                });
            })
            .child(body)
            .child(
                div()
                    .flex_none()
                    .bg(p.chrome)
                    .border_t_1()
                    .border_color(p.line_soft)
                    .px(px(Space::MD))
                    .py(px(Space::XS))
                    .text_size(px(TypeScale::XS))
                    .text_color(p.text_faint)
                    .child(SharedString::from(footer_hint)),
            )
            .into_any_element()
    }

    /// 表名输入：改动即生效（旧报告作废），聚焦或输入时打开智能补全，交换或重新带入时同步成新值。
    fn table_input(&mut self, side: &'static str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let wanted = self.table_pair.as_ref().map(|(left, right)| if side == "baseline" { left.clone() } else { right.clone() }).unwrap_or_default();
        if let Some((state, _)) = self.table_inputs.get(side) {
            if self.table_reload {
                state.update(cx, |input, cx| {
                    if input.value().as_ref() != wanted {
                        input.set_value(wanted.clone(), window, cx);
                    }
                });
            }
            return state.clone();
        }
        let placeholder = SharedString::from(self.text("compare.table.namePlaceholder"));
        let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(SharedString::from(wanted)));
        let subscription = cx.subscribe(&state, move |this, state, event: &InputEvent, cx| match event {
            InputEvent::Focus => {
                this.table_ac_side = Some(side);
                this.table_ac_index = 0;
                this.ensure_tables(side == "baseline", cx);
                cx.notify();
            }
            InputEvent::Blur => {
                if this.table_ac_side == Some(side) {
                    this.table_ac_side = None;
                    cx.notify();
                }
            }
            InputEvent::Change => {
                let text = state.read(cx).value().to_string();
                this.set_table_name(side == "baseline", &text);
                this.table_ac_side = Some(side);
                this.table_ac_index = 0;
                this.ensure_tables(side == "baseline", cx);
                cx.notify();
            }
            _ => {}
        });
        self.table_inputs.insert(side, (state.clone(), subscription));
        state
    }

    fn schema_input(&mut self, side: &'static str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some((state, _)) = self.schema_inputs.get(side) {
            let hint = SharedString::from(self.text("compare.dialog.schemaManualPlaceholder"));
            if state.read(cx).presentation().placeholder() != &hint {
                state.update(cx, |state, cx| state.set_placeholder(hint, window, cx));
            }
            if self.schema_reload {
                let wanted = self.schema_draft.get(side).cloned().unwrap_or_default();
                state.update(cx, |input, cx| {
                    if input.value().as_ref() != wanted {
                        input.set_value(wanted, window, cx);
                    }
                });
            }
            return state.clone();
        }
        let placeholder = SharedString::from(self.text("compare.dialog.schemaManualPlaceholder"));
        let initial = SharedString::from(self.schema_draft.get(side).cloned().unwrap_or_default());
        let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(initial));
        if self.schema_reload {
            let wanted = self.schema_draft.get(side).cloned().unwrap_or_default();
            state.update(cx, |input, cx| {
                if input.value().as_ref() != wanted {
                    input.set_value(wanted, window, cx);
                }
            });
        }
        let subscription = cx.subscribe(&state, move |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let text = state.read(cx).value().to_string();
                this.schema_draft.insert(side.to_owned(), text);
                cx.notify();
            }
        });
        self.schema_inputs.insert(side, (state.clone(), subscription));
        state
    }
}

impl ComparePane {
    fn diff_toolbar(&mut self, _report: &crate::compare::StructureReport, entity: &Entity<Self>, p: &Palette, m: &kit::Metrics, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let search = self.filter_field(window, cx);
        let filters = [
            ("compare-filter:all", DiffFilter::All),
            ("compare-filter:baseline", DiffFilter::Baseline),
            ("compare-filter:target", DiffFilter::Target),
            ("compare-filter:changed", DiffFilter::Changed),
        ]
        .into_iter()
        .map(|(id, filter)| {
            let entity = entity.clone();
            (
                SharedString::from(id),
                SharedString::from(self.text(filter.key())),
                self.diff_filter == filter,
                kit::click(move |_, _, cx| {
                    entity.update(cx, |pane, cx| pane.command(id, cx));
                }),
            )
        })
        .collect();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(Space::MD))
            .child(kit::segmented_sized(filters, p, m.control_h))
            .child(
                kit::interactive_states(div().id("compare-only-diffs"), p, false)
                    .test_support()
                    .role(gpui_kit::Role::Switch)
                    .aria_label(SharedString::from(self.text("compare.filter.onlyDiffs")))
                    .aria_selected(self.only_diffs)
                    .flex()
                    .items_center()
                    .gap(px(Space::SM))
                    .px(px(Space::XS))
                    .rounded(px(Radius::CONTROL))
                    .text_size(px(TypeScale::SM))
                    .child(kit::switch_visual(self.only_diffs, p))
                    .child(SharedString::from(self.text("compare.filter.onlyDiffs")))
                    .on_click(compare_act("compare-only-diffs", entity)),
            )
            .child(div().w(px(FILTER_SEARCH_W)).child(kit::text_input("compare-filter-search", &search, self.text("compare.filter.search")).small()))
            .into_any_element()
    }

    fn filter_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some((state, _)) = &self.filter_input {
            return state.clone();
        }
        let placeholder = SharedString::from(self.text("compare.filter.search"));
        let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(self.diff_query.clone()));
        let subscription = cx.subscribe(&state, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.diff_query = input.read(cx).value().to_string();
                cx.notify();
            }
        });
        self.filter_input = Some((state.clone(), subscription));
        state
    }

    fn script_view(&mut self, id: &'static str, script: &str, window: &mut Window, cx: &mut Context<Self>, p: &Palette) -> AnyElement {
        if !self.script_editors.contains_key(id) {
            let initial = script.to_owned();
            let state = cx.new(|cx| EditorState::new(window, cx).language("sql").line_number(true).default_value(initial));
            self.script_editors.insert(id, (state, script.to_owned()));
        }
        let entry = self.script_editors.get_mut(id).expect("script editor");
        kit::sync_editor(&entry.0, &mut entry.1, script, window, cx);
        let state = entry.0.clone();
        div()
            .id(id)
            .h(px(SCRIPT_EDITOR_H))
            .overflow_hidden()
            .child(
                kit::sunken(p).size_full().overflow_hidden().child(
                    Editor::new(&state).h(relative(1.)).bordered(false).readonly(true).aria_label(SharedString::from(id)),
                ),
            )
            .into_any_element()
    }
}

/// 差异树里的一条：属于哪一类差异，以及它在报告里的位置（作为选中键）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryClass {
    Baseline,
    Target,
    Changed,
    Same,
}

impl EntryClass {
    fn mark(self) -> &'static str {
        match self {
            EntryClass::Baseline => "+",
            EntryClass::Target => "−",
            EntryClass::Changed => "~",
            EntryClass::Same => "=",
        }
    }

    fn filter(self) -> Option<DiffFilter> {
        match self {
            EntryClass::Baseline => Some(DiffFilter::Baseline),
            EntryClass::Target => Some(DiffFilter::Target),
            EntryClass::Changed => Some(DiffFilter::Changed),
            EntryClass::Same => None,
        }
    }
}

struct Entry<'a> {
    key: String,
    class: EntryClass,
    item: &'a DiffItem,
}

fn entries(report: &StructureReport) -> Vec<Entry<'_>> {
    let mut out = Vec::new();
    for (class, prefix, items) in [
        (EntryClass::Baseline, "b", &report.only_baseline),
        (EntryClass::Target, "t", &report.only_target),
        (EntryClass::Changed, "c", &report.changed),
        (EntryClass::Same, "s", &report.same),
    ] {
        for (index, item) in items.iter().enumerate() {
            out.push(Entry { key: format!("{prefix}:{index}"), class, item });
        }
    }
    out
}

/// 差异条目的默认选中：第一条有差异的对象。
fn first_entry(report: &StructureReport) -> Option<String> {
    entries(report).into_iter().find(|entry| entry.class != EntryClass::Same).map(|entry| entry.key)
}

fn entry_item<'a>(report: &'a StructureReport, key: &str) -> Option<&'a DiffItem> {
    let (prefix, index) = key.split_once(':')?;
    let index: usize = index.parse().ok()?;
    match prefix {
        "b" => report.only_baseline.get(index),
        "t" => report.only_target.get(index),
        "c" => report.changed.get(index),
        "s" => report.same.get(index),
        _ => None,
    }
}

/// 差异树分组顺序：对象种类在前，表下属的索引、外键在后。后端给的是种类标签。
fn kind_rank(kind: &str) -> usize {
    const ORDER: &[&str] = &["表", "列", "视图", "函数", "过程", "触发器", "序列", "枚举", "域", "索引", "外键"];
    ORDER.iter().position(|label| *label == kind).unwrap_or(ORDER.len())
}

/// 并排定义的逐行标记：这一侧的行在另一侧找不到（按出现次数计）时为真。
fn line_marks(mine: &str, other: &str) -> Vec<(String, bool)> {
    let mut pool: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for line in other.lines() {
        *pool.entry(line.trim()).or_default() += 1;
    }
    mine.lines()
        .map(|line| {
            let key = line.trim();
            let found = pool.get_mut(key).filter(|count| **count > 0).map(|count| *count -= 1).is_some();
            (line.to_owned(), !found && !key.is_empty())
        })
        .collect()
}

/// 结果页按种类汇总：(种类, 仅基准, 仅对照, 变更)。
fn kind_summary(report: &StructureReport) -> Vec<(String, usize, usize, usize)> {
    let mut map: BTreeMap<(usize, String), (usize, usize, usize)> = BTreeMap::new();
    for item in &report.only_baseline {
        map.entry((kind_rank(&item.kind), item.kind.clone())).or_default().0 += 1;
    }
    for item in &report.only_target {
        map.entry((kind_rank(&item.kind), item.kind.clone())).or_default().1 += 1;
    }
    for item in &report.changed {
        map.entry((kind_rank(&item.kind), item.kind.clone())).or_default().2 += 1;
    }
    map.into_iter().map(|((_, kind), (b, t, c))| (kind, b, t, c)).collect()
}

/// 数据下钻最多列出的行数；完整语句在数据脚本里。
const DATA_ROWS_SHOWN: usize = 200;

impl ComparePane {
    /// 运行中显示的文字：总是以“正在对比…”开头，有后台进度时附上阶段与计数。
    fn progress_text(&self) -> String {
        let base = self.text("compare.dialog.comparing");
        let Some(progress) = self.progress else { return base };
        let stage = match progress.stage {
            CompareStage::Listing => self.text("compare.progress.listing"),
            CompareStage::Reading => fill(&self.text("compare.progress.reading"), &[("{done}", &progress.done.to_string()), ("{total}", &progress.total.to_string())]),
            CompareStage::Rows => fill(&self.text("compare.progress.rows"), &[("{done}", &progress.done.to_string()), ("{total}", &progress.total.to_string())]),
            CompareStage::Diffing => self.text("compare.progress.diffing"),
        };
        format!("{base} {stage}")
    }

    fn picked_item(&self) -> Option<&DiffItem> {
        let report = self.structure.as_ref()?;
        entry_item(report, self.picked.as_deref()?)
    }

    fn ask_place_item(&mut self) {
        let Some(sql) = self.picked_item().map(|item| item.sql.clone()).filter(|sql| !sql.trim().is_empty()) else {
            return;
        };
        if self.structure_status == Status::Cancelled {
            return;
        }
        let lines = sql.lines().count();
        self.place_prompt = if self.locale == Locale::En {
            format!("Put a {lines}-line write script into the query editor? It is not executed.")
        } else {
            format!("将把 {lines} 行写操作脚本放入查询编辑器，不会执行。确认放入？")
        };
        self.pending_item_sql = Some(sql);
        self.pending_place = Some(RunKind::Structure);
    }

    /// 差异树当前可见的条目：按筛选、“仅差异”与搜索过滤（搜索同时匹配所属对象）。
    fn visible_entries<'a>(&self, report: &'a StructureReport) -> Vec<Entry<'a>> {
        let mut list: Vec<Entry<'a>> = entries(report)
            .into_iter()
            .filter(|entry| match entry.class.filter() {
                Some(filter) => self.diff_filter == DiffFilter::All || self.diff_filter == filter,
                None => !self.only_diffs && self.diff_filter == DiffFilter::All,
            })
            .filter(|entry| name_matches(&entry.item.name, &self.diff_query) || name_matches(&entry.item.owner, &self.diff_query))
            .collect();
        list.sort_by_key(|entry| (kind_rank(&entry.item.kind), entry.item.kind.clone(), entry.class == EntryClass::Same));
        list
    }

    fn tree_lines(&self, report: &StructureReport, lines: &mut Vec<String>) {
        let mut current = None;
        for entry in self.visible_entries(report) {
            if current.as_deref() != Some(entry.item.kind.as_str()) {
                current = Some(entry.item.kind.clone());
                lines.push(format!("group {}", entry.item.kind));
            }
            let mark = if self.picked.as_deref() == Some(entry.key.as_str()) { "picked" } else { "item" };
            lines.push(format!("{mark} {} {} {} · {}", entry.class.mark(), entry.item.kind, entry.item.name, entry.item.detail));
        }
        if let Some(item) = self.picked_item() {
            lines.push(format!("detail {} {}", item.kind, item.name));
            if !item.baseline_definition.is_empty() {
                lines.push(format!("baseline-def {}", item.baseline_definition));
            }
            if !item.target_definition.is_empty() {
                lines.push(format!("target-def {}", item.target_definition));
            }
            lines.push(self.text("compare.detail.itemSql"));
            if item.sql.trim().is_empty() {
                lines.push(self.text("compare.detail.noSql"));
            } else {
                lines.push(format!("item-sql {}", item.sql));
            }
        }
    }

    fn data_lines(&self, report: &DataReport, lines: &mut Vec<String>) {
        for table in &report.tables {
            let counts = fill(
                &self.text("compare.data.summary"),
                &[("{insert}", &table.inserts.to_string()), ("{update}", &table.updates.to_string()), ("{delete}", &table.deletes.to_string())],
            );
            let mut line = format!("table {} {counts}", table.table);
            if table.rejected > 0 {
                line.push_str(&format!(" {}", fill(&self.text("compare.data.rejected"), &[("{count}", &table.rejected.to_string())])));
            }
            if let Some(reason) = &table.skipped {
                line.push_str(&format!(" skipped {reason}"));
            }
            lines.push(line);
        }
        if let Some(name) = &self.data_table {
            lines.push(format!("drill {name}"));
            for diff in report.diffs.iter().filter(|diff| &diff.table == name && diff.action != "skipped").take(DATA_ROWS_SHOWN) {
                lines.push(format!("row {} {}", diff.action, diff.key));
                for field in &diff.fields {
                    lines.push(format!(
                        "field {} {} -> {}",
                        field.column,
                        field.baseline.as_deref().unwrap_or("∅"),
                        field.target.as_deref().unwrap_or("∅")
                    ));
                }
                if !diff.sql.is_empty() {
                    lines.push(format!("row-sql {}", diff.sql));
                }
            }
        }
    }
}

impl ComparePane {
    fn structure_card(&mut self, report: &Arc<StructureReport>, entity: &Entity<Self>, stacked: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = kit::Metrics::of(cx);
        let t = |this: &Self, key: &str| SharedString::from(this.text(key));
        let mut card = kit::card(&p).p(px(m.card_pad())).flex().flex_col().gap(px(Space::MD));
        let can_place = report.refused.is_none() && self.structure_status != Status::Cancelled && !report.script.trim().is_empty();
        if let Some(refused) = &report.refused {
            return card
                .child(kit::title(t(self, "compare.dialog.structureScript"), TypeScale::LG, &p))
                .child(note_banner(IconName::Ban, refused.clone(), p.warn, p.warn_soft))
                .into_any_element();
        }
        // 紧凑内联摘要条：色标计数胶囊 + 按种类细分 + 放入新查询主按钮。
        let summary = kind_summary(report);
        let mut summary_left = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(Space::SM))
            .child(summary_pill("+", report.only_baseline.len(), self.text("compare.summary.added"), p.ok, p.ok_soft))
            .child(summary_pill("−", report.only_target.len(), self.text("compare.summary.removed"), p.danger, p.danger_soft))
            .child(summary_pill("~", report.changed.len(), self.text("compare.summary.changed"), p.warn, p.warn_soft))
            .child(summary_pill("=", report.same.len(), self.text("compare.summary.same"), p.text_muted, p.badge));
        if summary.is_empty() {
            summary_left = summary_left
                .child(kit::vline(&p))
                .child(kit::pill(SharedString::from(self.text("compare.summary.noDiff")), p.ok, p.ok_soft));
        } else {
            let mut kinds = div().id("compare-kind-summary").flex().flex_wrap().items_center().gap(px(Space::XS));
            for (kind, plus, minus, changed) in summary {
                let mut parts = Vec::new();
                if plus > 0 {
                    parts.push(format!("+{plus}"));
                }
                if minus > 0 {
                    parts.push(format!("−{minus}"));
                }
                if changed > 0 {
                    parts.push(format!("~{changed}"));
                }
                kinds = kinds.child(kit::pill(SharedString::from(format!("{kind} {}", parts.join(" "))), p.text_muted, p.surface));
            }
            summary_left = summary_left.child(kit::vline(&p)).child(kinds);
        }
        card = card.child(
            div()
                .px(px(Space::MD))
                .py(px(Space::SM))
                .rounded(px(Radius::CONTROL))
                .bg(p.chrome)
                .border_1()
                .border_color(p.line_soft)
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(px(Space::MD))
                .child(summary_left)
                .when(can_place, |this| {
                    this.child(kit::primary_button("compare-place:structure", Some(IconName::SquareTerminal), t(self, "compare.dialog.placeToQuery"), compare_act("compare-place:structure", entity)))
                }),
        );
        card = card.child(self.diff_toolbar(report, entity, &p, &m, window, cx));
        // 宽敞的双栏差异工作区：左侧差异树 + 右侧并排 DDL 与单对象同步 SQL。
        let tree = self.diff_tree(report, entity, &p, &m);
        let detail = self.diff_detail(entity, &p, &m, window, cx);
        let split = if stacked {
            div().flex().flex_col().gap(px(Space::MD)).child(tree).child(detail)
        } else {
            div()
                .flex()
                .items_start()
                .gap(px(Space::MD))
                .child(div().w(px(DIFF_TREE_W)).flex_none().child(tree))
                .child(div().flex_1().min_w_0().child(detail))
        };
        card = card.child(split);
        for note in &report.notes {
            card = card.child(kit::muted(SharedString::from(note.clone()), &p));
        }
        if report.incomplete || self.structure_status == Status::Incomplete {
            card = card.child(note_banner(IconName::TriangleAlert, self.text("compare.dialog.scriptIncomplete"), p.warn, p.warn_soft));
        }
        if report.tables.is_empty() {
            card = card.child(kit::muted(t(self, "compare.dialog.noTables"), &p));
        } else {
            let mut tables = div()
                .flex()
                .flex_wrap()
                .gap(px(Space::SM))
                .child(kit::tool_button("compare-select-all", Some(IconName::SquareCheck), t(self, "compare.action.selectAll"), compare_act("compare-select-all", entity)))
                .child(kit::tool_button("compare-clear", Some(IconName::Square), t(self, "compare.action.clear"), compare_act("compare-clear", entity)));
            for table in report.tables.iter().filter(|table| name_matches(&table.name, &self.diff_query)) {
                let on = self.selected.get(&table.name).copied().unwrap_or(false);
                let mark = if on { "on" } else { "off" };
                tables = tables.child(
                    chip(SharedString::from(format!("compare-toggle:{}", table.name)), SharedString::from(format!("{mark} {}", table.name)), SharedString::from(table.name.clone()), on, Some(if on { IconName::SquareCheck } else { IconName::Square }), &p, m)
                        .on_click(compare_act(format!("compare-toggle:{}", table.name), entity)),
                );
            }
            card = card.child(
                div()
                    .pt(px(Space::SM))
                    .border_t_1()
                    .border_color(p.line_soft)
                    .flex()
                    .flex_col()
                    .gap(px(Space::SM))
                    .child(kit::eyebrow(t(self, "compare.dialog.dataTitle"), &p))
                    .child(tables),
            );
            for table in report.tables.iter().filter(|table| table.needs_key_choice) {
                let mut keys = div().flex().flex_wrap().items_center().gap(px(Space::SM)).child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(format!("{} · {}", table.name, self.text("compare.dialog.chooseKey")))));
                for key in &table.keys {
                    let chosen = self.chosen_key.get(&table.name).is_some_and(|name| name == &key.name);
                    let mark = if chosen { "chosen" } else { "key" };
                    let id = format!("compare-key:{}|{}", table.name, key.name);
                    keys = keys.child(
                        chip(SharedString::from(id.clone()), SharedString::from(format!("{mark} {} {}", table.name, key.name)), SharedString::from(format!("{} ({})", key.name, key.columns.join(", "))), chosen, Some(IconName::Key), &p, m)
                            .on_click(compare_act(id, entity)),
                    );
                }
                card = card.child(keys);
            }
        }
        if self.structure_status != Status::Cancelled && !report.script.trim().is_empty() {
            card = card.child(
                div()
                    .pt(px(Space::SM))
                    .border_t_1()
                    .border_color(p.line_soft)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(kit::eyebrow(t(self, "compare.script.whole"), &p))
                    .child(kit::tool_button("compare-copy:structure", Some(IconName::Copy), t(self, "compare.script.copy"), compare_act("compare-copy:structure", entity))),
            );
            card = card.child(self.script_view("compare-structure-script", &report.script, window, cx, &p));
        }
        card.into_any_element()
    }

    fn diff_tree(&self, report: &StructureReport, entity: &Entity<Self>, p: &Palette, m: &kit::Metrics) -> AnyElement {
        let mut tree = kit::sunken(p)
            .id("compare-diff-tree")
            .p(px(Space::XS))
            .pr(px(kit::SCROLLBAR_GUTTER))
            .flex()
            .flex_col()
            .gap(px(Space::XXS))
            .max_h(px(DIFF_TREE_MAX_H))
            .scroll_area_y();
        let visible = self.visible_entries(report);
        if visible.is_empty() {
            tree = tree.child(div().flex_none().px(px(Space::MD)).py(px(Space::SM)).child(kit::muted(SharedString::from(self.text("compare.tree.empty")), p)));
        }
        let mut current: Option<String> = None;
        for entry in visible {
            if current.as_deref() != Some(entry.item.kind.as_str()) {
                current = Some(entry.item.kind.clone());
                tree = tree.child(
                    div()
                        .flex_none()
                        .px(px(Space::MD))
                        .pt(px(Space::SM))
                        .pb(px(Space::XXS))
                        .child(kit::eyebrow(SharedString::from(entry.item.kind.clone()), p)),
                );
            }
            let tone = match entry.class {
                EntryClass::Baseline => p.ok,
                EntryClass::Target => p.danger,
                EntryClass::Changed => p.warn,
                EntryClass::Same => p.text_faint,
            };
            let picked = self.picked.as_deref() == Some(entry.key.as_str());
            let id = format!("compare-pick:{}", entry.key);
            let row = kit::interactive_states(div().id(SharedString::from(id.clone())), p, false)
                .test_support()
                .flex_none()
                .role(gpui_kit::Role::Button)
                .aria_label(SharedString::from(format!("{} {} {}", entry.class.mark(), entry.item.kind, entry.item.name)))
                .aria_selected(picked)
                .min_h(px(m.tree_row_h))
                .px(px(Space::MD))
                .py(px(Space::XS))
                .flex()
                .items_center()
                .gap(px(Space::MD))
                .rounded(px(Radius::CONTROL))
                .text_size(px(TypeScale::SM))
                .when(picked, |this| this.bg(p.accent_soft).text_color(p.accent_text))
                .when(!picked, |this| this.text_color(p.text).hover(|style| style.bg(p.hover)))
                .child(div().w(px(MARK_COL_W)).flex_none().font_weight(gpui_kit::FontWeight::BOLD).text_color(tone).child(entry.class.mark()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(Space::XXS))
                        .child(div().truncate().child(SharedString::from(entry.item.name.clone())))
                        .when(entry.class != EntryClass::Same, |this| {
                            this.child(div().truncate().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(SharedString::from(entry.item.detail.clone())))
                        }),
                )
                .on_click(compare_act(id, entity));
            tree = tree.child(row);
        }
        tree.into_any_element()
    }

    fn diff_detail(&mut self, entity: &Entity<Self>, p: &Palette, _m: &kit::Metrics, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.picked_item().cloned() else {
            return kit::sunken(p).p(px(Space::LG)).child(kit::muted(SharedString::from(self.text("compare.detail.pick")), p)).into_any_element();
        };
        let mono = gpui_kit::component::ActiveTheme::theme(&**cx).mono_font_family.clone();
        let side = |label: String, tone: gpui_kit::Hsla, soft: gpui_kit::Hsla, mine: &str, other: &str, missing: String| {
            let mut block = kit::sunken(p)
                .id(SharedString::from(format!("compare-def:{label}")))
                .p(px(Space::SM))
                .pr(px(kit::SCROLLBAR_GUTTER))
                .flex()
                .flex_col()
                .max_h(px(DIFF_DEF_MAX_H))
                .scroll_area_y()
                .font_family(mono.clone())
                .text_size(px(TypeScale::XS));
            if mine.trim().is_empty() {
                block = block.child(div().flex_none().px(px(Space::XS)).text_color(p.text_faint).child(SharedString::from(missing)));
            } else {
                for (line, differs) in line_marks(mine, other) {
                    block = block.child(
                        div()
                            .flex_none()
                            .px(px(Space::XS))
                            .whitespace_nowrap()
                            .when(differs, |this| this.bg(soft).text_color(tone))
                            .when(!differs, |this| this.text_color(p.text))
                            .child(SharedString::from(if line.is_empty() { " ".to_owned() } else { line })),
                    );
                }
            }
            div().flex_1().flex_basis(px(ZERO_PX)).min_w_0().flex().flex_col().gap(px(Space::XS)).child(kit::eyebrow(SharedString::from(label), p)).child(block)
        };
        let missing = self.text("compare.detail.missing");
        let baseline = side(self.text("compare.dialog.baseline"), p.ok, p.ok_soft, &item.baseline_definition, &item.target_definition, missing.clone());
        let target = side(self.text("compare.dialog.target"), p.danger, p.danger_soft, &item.target_definition, &item.baseline_definition, missing);
        let title = if item.owner.is_empty() || item.owner == item.name { format!("{} {}", item.kind, item.name) } else { format!("{} {} · {}", item.kind, item.name, item.owner) };
        let mut detail = div()
            .id("compare-diff-detail")
            .flex()
            .flex_col()
            .gap(px(Space::MD))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(Space::XXS))
                    .child(div().text_size(px(TypeScale::MD)).font_weight(gpui_kit::FontWeight::SEMIBOLD).child(SharedString::from(title)))
                    .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(format!("{} · {}", item.detail, self.text("compare.detail.legend"))))),
            )
            .child(div().flex().gap(px(Space::MD)).child(baseline).child(target));
        let can_place = self.structure_status != Status::Cancelled && !item.sql.trim().is_empty();
        detail = detail.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(Space::MD))
                .child(kit::eyebrow(SharedString::from(self.text("compare.detail.itemSql")), p))
                .when(can_place, |this| {
                    this.child(
                        div()
                            .flex()
                            .gap(px(Space::SM))
                            .child(kit::tool_button("compare-copy:item", Some(IconName::Copy), SharedString::from(self.text("compare.detail.copyItem")), compare_act("compare-copy:item", entity)))
                            .child(kit::tool_button("compare-place:item", Some(IconName::SquareTerminal), SharedString::from(self.text("compare.detail.placeItem")), compare_act("compare-place:item", entity))),
                    )
                }),
        );
        if item.sql.trim().is_empty() {
            detail = detail.child(kit::muted(SharedString::from(self.text("compare.detail.noSql")), p));
        } else {
            detail = detail.child(self.script_view("compare-item-script", &item.sql, window, cx, p));
        }
        detail.into_any_element()
    }

    fn data_card(&mut self, report: &Arc<DataReport>, entity: &Entity<Self>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = kit::Metrics::of(cx);
        let t = |this: &Self, key: &str| SharedString::from(this.text(key));
        let can_place = report.refused.is_none() && self.data_status != Status::Cancelled && !report.script.trim().is_empty();
        let mut card = kit::card(&p).p(px(m.card_pad())).flex().flex_col().gap(px(Space::MD)).child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(kit::title(t(self, "compare.dialog.dataTitle"), TypeScale::LG, &p))
                .when(can_place, |this| {
                    this.child(kit::primary_button("compare-place:data", Some(IconName::SquareTerminal), t(self, "compare.dialog.dataDisclosure"), compare_act("compare-place:data", entity)))
                }),
        );
        if let Some(refused) = &report.refused {
            return card.child(note_banner(IconName::Ban, refused.clone(), p.warn, p.warn_soft)).into_any_element();
        }
        if report.incomplete || self.data_status == Status::Incomplete {
            card = card.child(note_banner(IconName::TriangleAlert, self.text("compare.dialog.dataScriptIncomplete"), p.warn, p.warn_soft));
        }
        let changed_rows: usize = report.tables.iter().map(|table| table.inserts + table.updates + table.deletes).sum();
        if changed_rows == 0 && report.tables.iter().all(|table| table.skipped.is_none() && table.rejected == 0) {
            card = card.child(note_banner(IconName::Check, self.text("compare.data.noDiff"), p.ok, p.ok_soft));
        } else {
            card = card.child(kit::muted(t(self, "compare.data.drillHint"), &p));
        }
        let mut list = kit::sunken(&p).id("compare-data-tables").p(px(Space::XS)).flex().flex_col().gap(px(ONE_PX));
        for table in &report.tables {
            let id = format!("compare-data-table:{}", table.table);
            let open = self.data_table.as_deref() == Some(table.table.as_str());
            let counts = fill(
                &self.text("compare.data.summary"),
                &[("{insert}", &table.inserts.to_string()), ("{update}", &table.updates.to_string()), ("{delete}", &table.deletes.to_string())],
            );
            let mut info = div().flex_1().min_w_0().flex().flex_col().gap(px(ONE_PX)).child(div().truncate().font_weight(gpui_kit::FontWeight::MEDIUM).child(SharedString::from(table.table.clone())));
            let sub = if let Some(reason) = &table.skipped {
                reason.clone()
            } else if !table.key.is_empty() {
                fill(&self.text("compare.data.keyColumns"), &[("{columns}", &table.key.join(", "))])
            } else {
                String::new()
            };
            if !sub.is_empty() {
                info = info.child(div().truncate().text_size(px(TypeScale::XS)).text_color(if table.skipped.is_some() { p.warn } else { p.text_muted }).child(SharedString::from(sub)));
            }
            let mut stats = div().flex_none().flex().items_center().gap(px(Space::SM)).text_size(px(TypeScale::SM));
            if table.skipped.is_none() {
                if table.inserts + table.updates + table.deletes == 0 && table.rejected == 0 {
                    stats = stats.child(div().text_color(p.ok).child(SharedString::from(self.text("compare.data.same"))));
                } else {
                    stats = stats.child(div().text_color(p.text_muted).child(SharedString::from(counts)));
                }
                if table.rejected > 0 {
                    stats = stats.child(div().text_color(p.danger).child(SharedString::from(fill(&self.text("compare.data.rejected"), &[("{count}", &table.rejected.to_string())]))));
                }
            }
            let row = kit::interactive_states(div().id(SharedString::from(id.clone())), &p, false)
                .test_support()
                .flex_none()
                .role(gpui_kit::Role::Button)
                .aria_label(SharedString::from(format!("{} {}", table.table, self.text("compare.data.drillHint"))))
                .aria_selected(open)
                .px(px(Space::MD))
                .py(px(Space::SM))
                .flex()
                .items_center()
                .gap(px(Space::MD))
                .rounded(px(Radius::CONTROL))
                .text_size(px(TypeScale::SM))
                .when(open, |this| this.bg(p.accent_soft))
                .when(!open, |this| this.hover(|style| style.bg(p.hover)))
                .child(kit::icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, kit::IconSize::INLINE, p.text_faint))
                .child(info)
                .child(stats)
                .on_click(compare_act(id, entity));
            list = list.child(row);
            if open {
                list = list.child(self.data_rows(report, &table.table, &p, window, cx));
            }
        }
        card = card.child(list);
        for note in &report.notes {
            card = card.child(kit::muted(SharedString::from(note.clone()), &p));
        }
        if self.data_status != Status::Cancelled && !report.script.trim().is_empty() {
            card = card.child(
                div()
                    .pt(px(Space::SM))
                    .border_t_1()
                    .border_color(p.line_soft)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(kit::eyebrow(t(self, "compare.dialog.dataScript"), &p))
                    .child(kit::tool_button("compare-copy:data", Some(IconName::Copy), t(self, "compare.script.copy"), compare_act("compare-copy:data", entity))),
            );
            card = card.child(self.script_view("compare-data-script", &report.script, window, cx, &p));
        }
        card.into_any_element()
    }

    /// 一张表的行级差异：每个字段一行（动作、键、列、基准值、对照值、是否不同），第一行附同步语句。
    /// 用共享只读表渲染：虚拟化、可选中，⌘C 与右键「复制值 / 行 / 列 / TSV / CSV / JSON」。
    fn data_rows(&mut self, report: &Arc<DataReport>, table: &str, p: &Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let block = div().id(SharedString::from(format!("compare-data-rows:{table}"))).test_support().ml(px(Space::XL)).p(px(Space::SM)).flex().flex_col().gap(px(Space::SM));
        let shown = report.diffs.iter().filter(|diff| diff.table == table && diff.action != "skipped").count();
        if shown == 0 {
            self.rows_table = None;
            let reason = report.tables.iter().find(|item| item.table == table).and_then(|item| item.skipped.clone()).unwrap_or_else(|| self.text("compare.data.same"));
            return block.child(kit::muted(SharedString::from(reason), p)).into_any_element();
        }
        let stamp = {
            use std::hash::{Hash as _, Hasher as _};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (Arc::as_ptr(report) as usize).hash(&mut hasher);
            table.hash(&mut hasher);
            matches!(self.locale, Locale::En).hash(&mut hasher);
            hasher.finish()
        };
        let (columns, rows) = data_row_cells(report, table, DATA_ROWS_SHOWN, |key| self.text(key));
        let entity = match &self.rows_table {
            Some((old, entity)) if *old == stamp => entity.clone(),
            Some((_, entity)) => {
                let entity = entity.clone();
                let locale = self.locale;
                entity.update(cx, |view, cx| {
                    view.set_locale(locale, cx);
                    view.set_data(columns, ReadRows::Static(rows), cx);
                });
                self.rows_table = Some((stamp, entity.clone()));
                entity
            }
            None => {
                let locale = self.locale;
                let entity = cx.new(|cx| ReadTable::new(columns, ReadRows::Static(rows), locale, window, cx));
                self.rows_table = Some((stamp, entity.clone()));
                entity
            }
        };
        let m = kit::Metrics::of(cx);
        let visible = data_row_count(report, table, DATA_ROWS_SHOWN).clamp(1, 14) as f32;
        let mut block = block.child(div().id("compare-data-rows-table").test_support().h(px((visible + 1.) * m.row_h + Space::SM)).child(entity));
        if shown > DATA_ROWS_SHOWN {
            block = block.child(kit::muted(SharedString::from(fill(&self.text("compare.data.truncated"), &[("{count}", &DATA_ROWS_SHOWN.to_string())])), p));
        }
        block.into_any_element()
    }

    /// 下钻表格（测试取选区与复制文本用）。
    #[cfg(test)]
    pub(crate) fn rows_table(&self) -> Option<Entity<ReadTable>> {
        self.rows_table.as_ref().map(|(_, entity)| entity.clone())
    }
}

/// 下钻表格的行数（与 `data_row_cells` 一致）：每个字段一行，没有字段的差异一行。
fn data_row_count(report: &DataReport, table: &str, limit: usize) -> usize {
    report.diffs.iter().filter(|diff| diff.table == table && diff.action != "skipped").take(limit).map(|diff| diff.fields.len().max(1)).sum()
}

/// 行级差异的表格数据：动作、键、列、基准值、对照值、不同（≠）、同步语句（只在该差异的第一行）。
/// 未生成的行把原因放在基准值一格。值为 NULL 的一侧缺行时显示 ∅。
fn data_row_cells(report: &DataReport, table: &str, limit: usize, text: impl Fn(&str) -> String) -> (Vec<ReadColumn>, Vec<Vec<SharedString>>) {
    let columns = vec![
        ReadColumn::new(text("compare.data.col.action")).width(72.),
        ReadColumn::new(text("compare.dialog.key")).width(160.).mono(),
        ReadColumn::new(text("compare.data.col.column")).width(140.).mono(),
        ReadColumn::new(text("compare.data.col.baseline")).width(180.).mono(),
        ReadColumn::new(text("compare.data.col.target")).width(180.).mono(),
        ReadColumn::new(text("compare.data.col.changed")).width(48.),
        ReadColumn::new(text("compare.data.col.sql")).width(360.).mono(),
    ];
    let value = |text: &Option<String>| SharedString::from(text.clone().unwrap_or_else(|| "∅".to_owned()));
    let mut rows = Vec::new();
    for diff in report.diffs.iter().filter(|diff| diff.table == table && diff.action != "skipped").take(limit) {
        let action = SharedString::from(match diff.action.as_str() {
            "insert" => text("compare.data.action.insert"),
            "delete" => text("compare.data.action.delete"),
            "update" => text("compare.data.action.update"),
            _ => text("compare.data.action.rejected"),
        });
        let key = SharedString::from(diff.key.clone());
        let sql = SharedString::from(diff.sql.clone());
        if diff.fields.is_empty() {
            let detail = SharedString::from(diff.detail.clone());
            rows.push(vec![action, key, SharedString::default(), detail, SharedString::default(), SharedString::default(), sql]);
            continue;
        }
        for (ix, field) in diff.fields.iter().enumerate() {
            let baseline = if ix == 0 && diff.action == "rejected" && !diff.detail.is_empty() { SharedString::from(diff.detail.clone()) } else { value(&field.baseline) };
            rows.push(vec![
                action.clone(),
                key.clone(),
                SharedString::from(field.column.clone()),
                baseline,
                value(&field.target),
                SharedString::from(if field.changed { "≠" } else { "" }),
                if ix == 0 { sql.clone() } else { SharedString::default() },
            ]);
        }
    }
    (columns, rows)
}

fn compare_act(id: impl Into<String>, entity: &Entity<ComparePane>) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
    let id = id.into();
    let entity = entity.clone();
    move |_, _, cx| {
        let id = id.clone();
        entity.update(cx, |pane, cx| pane.command(&id, cx));
    }
}

/// 凹陷选择列表里的一行：未选中时不画第二圈方框，选中时以柔和强调底 + 右侧勾标清晰表达。
fn list_choice(
    id: SharedString,
    label: SharedString,
    text: SharedString,
    selected: bool,
    icon: IconName,
    p: &Palette,
    m: kit::Metrics,
) -> gpui_kit::base::ObservedElement<gpui_kit::Stateful<gpui_kit::Div>> {
    kit::interactive_states(div().id(id), p, false)
        .test_support()
        .flex_none()
        .role(gpui_kit::Role::Button)
        .aria_label(label)
        .aria_selected(selected)
        .h(px(m.tree_row_h + LIST_CHOICE_EXTRA_H))
        .px(px(Space::MD))
        .flex()
        .items_center()
        .gap(px(Space::SM))
        .rounded(px(Radius::CONTROL))
        .text_size(px(TypeScale::SM))
        .when(selected, |this| {
            this.bg(p.accent_soft)
                .text_color(p.accent_text)
                .font_weight(gpui_kit::FontWeight::MEDIUM)
        })
        .when(!selected, |this| this.text_color(p.text).hover(|style| style.bg(p.hover)))
        .child(kit::icon(icon, kit::IconSize::INLINE, if selected { p.accent_text } else { p.text_faint }))
        .child(div().flex_1().min_w_0().truncate().child(text))
        .when(selected, |this| this.child(kit::icon(IconName::Check, kit::IconSize::INLINE, p.accent_text)))
}

/// 可选中的小块：显示文字与无障碍名称分开，测试按名称找。
fn chip(id: SharedString, label: SharedString, text: SharedString, selected: bool, icon: Option<IconName>, p: &Palette, m: kit::Metrics) -> gpui_kit::base::ObservedElement<gpui_kit::Stateful<gpui_kit::Div>> {
    kit::interactive_states(div().id(id), p, false)
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(label)
        .aria_selected(selected)
        .h(px(m.tree_row_h))
        .px(px(Space::MD))
        .flex()
        .items_center()
        .gap(px(Space::SM))
        .rounded(px(Radius::CONTROL))
        .border_1()
        .text_size(px(TypeScale::SM))
        .when(selected, |this| this.border_color(p.accent).bg(p.accent_soft).text_color(p.accent_text))
        .when(!selected, |this| this.border_color(p.line).hover(|style| style.border_color(p.line_strong).bg(p.hover)))
        .when_some(icon, |this, icon| this.child(kit::icon(icon, kit::IconSize::INLINE, if selected { p.accent_text } else { p.text_faint })))
        .child(text)
}

fn summary_pill(mark: &'static str, value: usize, label: String, fg: gpui_kit::Hsla, bg: gpui_kit::Hsla) -> AnyElement {
    div()
        .flex_none()
        .px(px(Space::MD))
        .py(px(Space::XS))
        .rounded(px(Radius::TAG))
        .bg(bg)
        .text_color(fg)
        .flex()
        .items_center()
        .gap(px(Space::XS))
        .text_size(px(TypeScale::XS))
        .child(div().font_weight(gpui_kit::FontWeight::BOLD).font_features(kit::tabular_figures()).child(SharedString::from(format!("{mark}{value}"))))
        .child(SharedString::from(label))
        .into_any_element()
}

fn note_banner(icon: IconName, text: String, fg: gpui_kit::Hsla, bg: gpui_kit::Hsla) -> AnyElement {
    div()
        .px(px(Space::LG))
        .py(px(Space::MD))
        .flex()
        .items_center()
        .gap(px(Space::MD))
        .rounded(px(Radius::PANEL))
        .bg(bg)
        .text_color(fg)
        .text_size(px(TypeScale::SM))
        .child(kit::icon(icon, kit::IconSize::CONTROL, fg))
        .child(div().flex_1().min_w_0().child(SharedString::from(text)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_or_unsupported_engines_do_not_describe_a_script() {
        assert_eq!(engine_gate("mysql", "mariadb"), Some("compare.gates.engineMismatch"));
        assert_eq!(engine_gate("redis", "redis"), Some("compare.gates.unsupported"));
        assert_eq!(engine_gate("sqlite", "sqlite"), None);
        let mut pane = temp_pane();
        pane.choices = vec![
            Choice { id: "mysql".into(), name: "甲".into(), engine: "mysql".into(), databases: vec!["app".into()], catalog_error: None },
            Choice { id: "maria".into(), name: "乙".into(), engine: "mariadb".into(), databases: vec!["app".into()], catalog_error: None },
        ];
        pane.baseline = SidePick { connection_id: "mysql".into(), database: "app".into(), schema: String::new() };
        pane.target = SidePick { connection_id: "maria".into(), database: "app".into(), schema: String::new() };
        let message = pane.refuse_local().expect("引擎不同");
        assert!(message.contains("不生成脚本"), "{message}");
        pane.structure = Some(Arc::new(empty_structure(Some(message))));
        assert!(pane.structure.as_ref().is_some_and(|report| report.script.is_empty() && report.refused.is_some()));
        assert!(!pane.structure_ready());

        pane.choices = vec![
            Choice { id: "cache".into(), name: "缓存".into(), engine: "redis".into(), databases: Vec::new(), catalog_error: None },
            Choice { id: "cache-b".into(), name: "缓存二".into(), engine: "redis".into(), databases: Vec::new(), catalog_error: None },
        ];
        pane.baseline.connection_id = "cache".into();
        pane.target.connection_id = "cache-b".into();
        let message = pane.refuse_local().expect("不支持的引擎");
        assert!(message.contains("不生成脚本"), "{message}");
        assert!(!message.contains("CREATE"));

        pane.choices = vec![Choice { id: "base".into(), name: "基准".into(), engine: "sqlite".into(), databases: vec!["main".into()], catalog_error: None }];
        pane.baseline.connection_id = "base".into();
        pane.target.connection_id.clear();
        let message = pane.refuse_local().expect("对照还没选");
        assert!(message.contains("请选择已打开的基准和对照"), "{message}");
        assert!(!message.contains("CREATE"));
    }

    #[test]
    fn schema_still_loading_blocks_the_script() {
        let mut pane = temp_pane();
        pane.choices = vec![
            Choice { id: "left".into(), name: "左".into(), engine: "postgres".into(), databases: vec!["app".into()], catalog_error: None },
            Choice { id: "right".into(), name: "右".into(), engine: "postgres".into(), databases: vec!["app".into()], catalog_error: None },
        ];
        pane.baseline = SidePick { connection_id: "left".into(), database: "app".into(), schema: String::new() };
        pane.target = SidePick { connection_id: "right".into(), database: "app".into(), schema: "public".into() };
        pane.schemas.insert(schema_key("left", "app"), SchemaState::Loading);
        pane.schemas.insert(schema_key("right", "app"), SchemaState::Ready(vec!["public".into()]));
        let message = pane.refuse_local().expect("schema 还在读");
        assert!(message.contains("不生成脚本"), "{message}");
        assert!(!pane.running());
        let text = pane.lines().join("\n");
        assert!(text.contains("schema 还在读取"), "{text}");
        assert!(text.contains("schema-on public"), "{text}");
        pane.schemas.insert(schema_key("left", "app"), SchemaState::Manual("读取 schema 失败：权限不足".into()));
        let text = pane.lines().join("\n");
        assert!(text.contains("读取 schema 失败：权限不足"), "{text}");
        let message = pane.refuse_local().expect("失败后还没填写 schema");
        assert!(message.contains("不生成脚本"), "{message}");
    }

    #[test]
    fn place_waits_for_confirmation_and_target_change_drops_it() {
        let mut pane = temp_pane();
        pane.choices = vec![Choice { id: "base".into(), name: "基准".into(), engine: "sqlite".into(), databases: vec!["main".into()], catalog_error: None }];
        pane.baseline = SidePick { connection_id: "base".into(), database: "main".into(), schema: String::new() };
        pane.target = SidePick { connection_id: "base".into(), database: "main".into(), schema: String::new() };
        let mut report = empty_structure(None);
        report.script = "CREATE TABLE copied (id INTEGER PRIMARY KEY);".into();
        pane.structure = Some(Arc::new(report));
        pane.structure_status = Status::Ready;
        pane.ask_place(RunKind::Structure);
        assert!(pane.pending_place.is_some(), "写脚本必须先确认");
        assert!(pane.placed.is_none(), "确认前不能放入查询");
        assert!(pane.place_prompt.contains("不会执行"), "{}", pane.place_prompt);
        pane.invalidate_results();
        assert!(pane.pending_place.is_none());
        assert!(pane.placed.is_none());
        assert!(pane.structure.is_none());
    }

    #[test]
    fn swap_filter_select_and_a_late_result_keep_cancel() {
        let mut pane = temp_pane();
        pane.choices = vec![
            Choice { id: "left".into(), name: "左".into(), engine: "sqlite".into(), databases: vec!["main".into()], catalog_error: None },
            Choice { id: "right".into(), name: "右".into(), engine: "sqlite".into(), databases: vec!["main".into()], catalog_error: None },
        ];
        pane.baseline = SidePick { connection_id: "left".into(), database: "main".into(), schema: String::new() };
        pane.target = SidePick { connection_id: "right".into(), database: "main".into(), schema: String::new() };
        pane.swap_sides();
        assert_eq!(pane.sides(), ("right".into(), "left".into()));

        let mut report = empty_structure(None);
        report.tables.push(crate::compare::TableChoice { name: "orders".into(), on_both: true, needs_key_choice: false, keys: Vec::new() });
        report.tables.push(crate::compare::TableChoice { name: "hidden".into(), on_both: true, needs_key_choice: false, keys: Vec::new() });
        pane.structure = Some(Arc::new(report));
        pane.structure_status = Status::Ready;
        pane.selected.insert("orders".into(), false);
        pane.selected.insert("hidden".into(), false);
        pane.diff_filter = DiffFilter::Baseline;
        pane.diff_query = "ord".into();
        assert!(pane.shows_diff(DiffFilter::Baseline, "orders"));
        assert!(!pane.shows_diff(DiffFilter::Baseline, "customers"));
        assert!(!pane.shows_diff(DiffFilter::Target, "orders"));
        pane.select_all_tables();
        assert_eq!(pane.selected.get("orders"), Some(&true));
        assert_eq!(pane.selected.get("hidden"), Some(&false));
        pane.clear_table_selection();
        assert!(pane.selected.values().all(|on| !*on));
        let text = pane.lines().join("\n");
        assert!(text.contains("on orders") || text.contains("off orders"), "{text}");
        assert!(!text.contains("hidden"), "{text}");
        assert!(text.contains("仅基准") || text.contains("Baseline only"), "{text}");

        let token = pane.services.tasks.begin(OWNER);
        pane.token = Some(token);
        pane.running = true;
        pane.run_kind = Some(RunKind::Structure);
        pane.run_id = "compare-late".into();
        pane.structure = None;
        pane.structure_status = Status::Running;
        let flag = Arc::clone(&pane.cancel_flag);
        pane.cancel();
        assert_eq!(pane.structure_status, Status::Cancelled);
        let mut late = empty_structure(None);
        late.script = "CREATE TABLE late (id INTEGER);".into();
        pane.finish_structure(token, &flag, TaskOutcome::Current { token, value: late });
        assert_eq!(pane.structure_status, Status::Cancelled);
        assert!(pane.structure.is_none());
        let text = pane.lines().join("\n");
        assert!(text.contains("已取消"), "{text}");
        assert!(!text.contains("CREATE TABLE"), "{text}");
    }

    /// 三档宽度只决定排法：1360 并排，900/640 竖排；阈值本身有正反两侧。
    #[test]
    fn sides_only_stack_below_the_side_by_side_width() {
        assert!(!sides_stacked(1360.), "1360 并排");
        assert!(!sides_stacked(SIDES_SIDE_BY_SIDE_MIN_W), "阈值本身仍并排");
        assert!(sides_stacked(SIDES_SIDE_BY_SIDE_MIN_W - 1.), "阈值下一像素就竖排");
        assert!(sides_stacked(900.), "900 竖排");
        assert!(sides_stacked(640.), "640 竖排");
        // 阈值容得下两列（每侧 340 + docked 侧栏 234）由常量处的编译期断言保证。
    }

    /// 失败与提示分开：失败带重试动作，取消/门禁只说明原因，都不留空详情。
    #[test]
    fn a_lost_compare_fails_with_retry_while_cancel_stays_a_notice() {
        let mut pane = temp_pane();
        pane.fail(pane.text("compare.status.failed"), RetryAction::Structure);
        assert_eq!(pane.error_kind, ErrorKind::Failure(RetryAction::Structure));
        let text = pane.lines().join("\n");
        assert!(text.contains("比较没有完成"), "{text}");
        assert!(text.contains("重试"), "失败要有重试动作：{text}");
        assert!(!pane.show_empty_guide(), "失败不画初始指导");

        pane.notify(pane.text("compare.status.cancelled"));
        assert_eq!(pane.error_kind, ErrorKind::Notice);
        let text = pane.lines().join("\n");
        assert!(text.contains("已取消"), "{text}");
        assert!(!text.contains("重试"), "取消不该带重试：{text}");

        // 清单读取失败：标题换成“选择连接”，原因仍是后端那句话。
        pane.fail("读取连接清单失败：磁盘只读".to_owned(), RetryAction::Choices);
        let text = pane.lines().join("\n");
        assert!(text.contains("选择连接"), "{text}");
        assert!(text.contains("读取连接清单失败：磁盘只读"), "{text}");

        pane.clear_error();
        assert_eq!(pane.error_kind, ErrorKind::None);
        assert!(pane.error.is_empty());
        assert!(pane.show_empty_guide(), "清掉提示后回到初始指导");
    }

    pub(super) fn sqlite_pair(pane: &mut ComparePane) {
        pane.choices = vec![
            Choice { id: "left".into(), name: "左".into(), engine: "sqlite".into(), databases: vec!["main".into()], catalog_error: None },
            Choice { id: "right".into(), name: "右".into(), engine: "sqlite".into(), databases: vec!["main".into()], catalog_error: None },
        ];
        pane.baseline = SidePick { connection_id: "left".into(), database: "main".into(), schema: String::new() };
        pane.target = SidePick { connection_id: "right".into(), database: "main".into(), schema: String::new() };
    }

    /// 取消当场结束运行状态（不等后台下一个读取点），并且马上可以再次发起，而不是报“上一次还在读”。
    #[gpui_kit::test]
    fn cancel_ends_the_run_at_once_and_a_new_compare_can_start(cx: &mut gpui_kit::TestAppContext) {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let pane = cx.update(|cx| {
            cx.new(|_| {
                let mut pane = temp_pane();
                sqlite_pair(&mut pane);
                pane.set_query_timeout_sec(15);
                pane
            })
        });
        pane.update(cx, |pane, cx| {
            // 闸门不放开：后台任务一直停在第一个 await，模拟卡住的读取。
            pane.arm_pause();
            pane.command("compare-structure", cx);
            assert!(pane.running(), "{}", pane.lines().join("\n"));
            assert!(pane.lines().join("\n").contains("正在对比"));
            let abort = pane.abort.clone().expect("后台任务可中止");
            pane.command("compare-cancel", cx);
            assert!(!pane.running(), "取消后立即不再运行");
            assert!(pane.token.is_none(), "这次运行的 token 作废，迟到结果不会写回");
            assert_eq!(pane.structure_status, Status::Cancelled);
            assert!(pane.lines().join("\n").contains("已取消"));
            assert!(!pane.lines().join("\n").contains("正在对比"));
            let _ = abort;
            // 再次发起：不会被“上一次还在读”挡住。
            pane.arm_pause();
            pane.command("compare-structure", cx);
            assert!(pane.running());
            assert_ne!(pane.error_kind, ErrorKind::Notice, "{}", pane.error);
            pane.command("compare-cancel", cx);
            assert!(!pane.running());
        });
    }

    /// 读取超时：明确的失败说明 + 重试，重试重发同一步。
    #[test]
    fn a_timed_out_compare_is_a_failure_with_retry() {
        let mut pane = temp_pane();
        sqlite_pair(&mut pane);
        let mut report = empty_structure(Some(format!("{} 不生成脚本。", crate::compare::timeout_message(15))));
        report.timed_out = true;
        pane.install_structure(report);
        assert_eq!(pane.structure_status, Status::Failed);
        assert!(pane.structure.is_none());
        assert_eq!(pane.error_kind, ErrorKind::Failure(RetryAction::Structure));
        let text = pane.lines().join("\n");
        assert!(text.contains("15 秒") && text.contains("重试"), "{text}");

        let mut data = empty_data(Some(crate::compare::timeout_message(0)));
        data.timed_out = true;
        pane.install_data(data);
        assert_eq!(pane.data_status, Status::Failed);
        assert_eq!(pane.error_kind, ErrorKind::Failure(RetryAction::Data));
    }

    pub(super) fn annotated(name: &str, detail: &str, sql: &str) -> DiffItem {
        DiffItem {
            kind: "表".into(),
            name: name.into(),
            detail: detail.into(),
            owner: name.into(),
            baseline_definition: format!("CREATE TABLE {name} (\n  id integer,\n  note text\n);"),
            target_definition: format!("CREATE TABLE {name} (\n  id integer,\n  note integer\n);"),
            sql: sql.into(),
        }
    }

    #[test]
    fn the_result_tree_groups_filters_and_shows_the_picked_item() {
        let mut pane = temp_pane();
        sqlite_pair(&mut pane);
        let mut report = empty_structure(None);
        report.changed.push(annotated("orders", "列不同", "ALTER TABLE orders ALTER COLUMN note TYPE text;"));
        report.only_baseline.push(DiffItem { kind: "视图".into(), name: "v_new".into(), detail: "仅基准拥有".into(), owner: "v_new".into(), baseline_definition: "CREATE VIEW v_new AS SELECT 1".into(), sql: "CREATE VIEW v_new AS SELECT 1;".into(), ..DiffItem::default() });
        report.same.push(DiffItem::new("表", "customers", "相同"));
        report.script = "ALTER TABLE orders ALTER COLUMN note TYPE text;\nCREATE VIEW v_new AS SELECT 1;".into();
        pane.install_structure(report);
        assert_eq!(pane.picked.as_deref(), Some("b:0"), "默认选中第一条差异");
        let text = pane.lines().join("\n");
        assert!(text.contains("kind 表 +0 −0 ~1") && text.contains("kind 视图 +1 −0 ~0"), "{text}");
        assert!(text.contains("group 表") && text.contains("group 视图"), "{text}");
        assert!(!text.contains("customers"), "仅差异时不列相同对象：{text}");
        assert!(text.contains("item-sql CREATE VIEW v_new"), "{text}");

        assert!(pane.view_command("compare-pick:c:0"));
        let text = pane.lines().join("\n");
        assert!(text.contains("picked ~ 表 orders"), "{text}");
        assert!(text.contains("baseline-def CREATE TABLE orders") && text.contains("note text"), "{text}");
        assert!(text.contains("item-sql ALTER TABLE orders"), "{text}");

        assert!(pane.view_command("compare-only-diffs"));
        assert!(pane.lines().join("\n").contains("item = 表 customers"), "关掉仅差异后列出相同对象");
        assert!(pane.view_command("compare-filter:changed"));
        let text = pane.lines().join("\n");
        assert!(!text.contains("item = 表 customers") && !text.contains("item + 视图 v_new"), "{text}");

        // 选中对象的语句放入查询前同样要确认，确认后只放这一个对象的语句。
        pane.ask_place_item();
        assert!(pane.pending_place.is_some() && pane.placed.is_none());
        pane.confirm_place();
        assert_eq!(pane.take_place().map(|placed| placed.sql), Some("ALTER TABLE orders ALTER COLUMN note TYPE text;".to_owned()));

        // 无差异：明确说一致。
        let mut same = empty_structure(None);
        same.same.push(DiffItem::new("表", "orders", "相同"));
        pane.install_structure(same);
        assert!(pane.picked.is_none());
        assert!(pane.lines().join("\n").contains("两侧结构一致"), "{}", pane.lines().join("\n"));
    }

    #[test]
    fn data_results_list_tables_and_drill_into_field_differences() {
        let mut pane = temp_pane();
        sqlite_pair(&mut pane);
        let mut report = empty_data(None);
        report.tables = vec![
            crate::compare::DataTableSummary { table: "orders".into(), inserts: 1, updates: 1, deletes: 0, rejected: 0, skipped: None, key: vec!["id".into()] },
            crate::compare::DataTableSummary { table: "notes".into(), skipped: Some("没有主键或唯一键，无法按键对应两侧的行".into()), ..Default::default() },
        ];
        report.diffs = vec![crate::compare::DataDiff {
            table: "orders".into(),
            action: "update".into(),
            key: "1".into(),
            detail: "键相同，其他值不同".into(),
            fields: vec![crate::compare::FieldDiff { column: "note".into(), baseline: Some("new".into()), target: Some("old".into()), changed: true }],
            sql: "UPDATE orders SET note = 'new' WHERE id = 1;".into(),
        }];
        report.script = "UPDATE orders SET note = 'new' WHERE id = 1;".into();
        pane.install_data(report);
        let text = pane.lines().join("\n");
        assert!(text.contains("table orders 插入 1 · 更新 1 · 删除 0"), "{text}");
        assert!(text.contains("table notes") && text.contains("没有主键"), "{text}");
        assert!(!text.contains("field note"), "下钻前不展开行：{text}");
        assert!(pane.view_command("compare-data-table:orders"));
        let text = pane.lines().join("\n");
        assert!(text.contains("drill orders") && text.contains("field note new -> old") && text.contains("row-sql UPDATE orders"), "{text}");
        assert!(pane.view_command("compare-data-table:orders"));
        assert!(!pane.lines().join("\n").contains("drill orders"), "再点一次收起");
    }

    fn drill_report() -> DataReport {
        let mut report = empty_data(None);
        report.tables = vec![crate::compare::DataTableSummary { table: "orders".into(), inserts: 1, updates: 1, deletes: 0, rejected: 0, skipped: None, key: vec!["id".into()] }];
        report.diffs = vec![
            crate::compare::DataDiff {
                table: "orders".into(),
                action: "update".into(),
                key: "1".into(),
                detail: String::new(),
                fields: vec![
                    crate::compare::FieldDiff { column: "note".into(), baseline: Some("new".into()), target: Some("old".into()), changed: true },
                    crate::compare::FieldDiff { column: "qty".into(), baseline: Some("2".into()), target: Some("2".into()), changed: false },
                ],
                sql: "UPDATE orders SET note = 'new' WHERE id = 1;".into(),
            },
            crate::compare::DataDiff {
                table: "orders".into(),
                action: "insert".into(),
                key: "2".into(),
                detail: String::new(),
                fields: vec![crate::compare::FieldDiff { column: "note".into(), baseline: Some("x".into()), target: None, changed: true }],
                sql: "INSERT INTO orders (id, note) VALUES (2, 'x');".into(),
            },
        ];
        report
    }

    /// R13：对比数据行用共享只读表：每个字段一行，同步语句只在该差异的第一行；缺行一侧显示 ∅。
    #[test]
    fn data_row_cells_flatten_fields_for_the_read_table() {
        let report = drill_report();
        let (columns, rows) = data_row_cells(&report, "orders", 200, |key| key.to_owned());
        assert_eq!(columns.len(), 7);
        assert_eq!(rows.len(), 3, "两个字段 + 一个字段");
        assert_eq!(rows[0][2].as_ref(), "note");
        assert_eq!(rows[0][5].as_ref(), "≠");
        assert_eq!(rows[1][5].as_ref(), "", "相同的列不标不同");
        assert!(rows[0][6].starts_with("UPDATE") && rows[1][6].is_empty(), "语句只在第一行");
        assert_eq!(rows[2][4].as_ref(), "∅");
        assert_eq!(data_row_count(&report, "orders", 200), 3);
        assert_eq!(data_row_count(&report, "orders", 1), 2, "按差异数截断");
    }

    /// R13：下钻后的行级差异可以选中，⌘C 复制选区（多格为含表头的 TSV）。
    #[gpui_kit::test]
    fn drilled_data_rows_are_selectable_and_copyable(cx: &mut gpui_kit::TestAppContext) {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let dir = std::env::temp_dir().join(format!("rillsql-compare-rows-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let (window, view) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds { origin: gpui_kit::Point::default(), size: gpui_kit::size(px(1200.), px(900.)) };
            let (window, view) = gpui_kit::open_window(gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() }, cx, |_, cx| {
                cx.new(|_| {
                    let mut pane = ComparePane::new(Arc::clone(&services));
                    sqlite_pair(&mut pane);
                    pane.install_data(drill_report());
                    assert!(pane.view_command("compare-data-table:orders"));
                    pane
                })
            })
            .expect("compare pane");
            (window, view)
        });
        cx.update_window(window, |_, window, cx| gpui_kit::test::TestWindowExt::render_frame(window, cx)).unwrap();
        let table = cx.update(|cx| view.read(cx).rows_table()).expect("下钻后建了只读表");
        let copied = cx.update(|cx| {
            assert_eq!(table.read(cx).rows_len(cx), 3);
            table.update(cx, |table, cx| {
                table.select(0, 2, false, cx);
                table.select(1, 4, true, cx);
                table.copy_selection(cx)
            })
        });
        assert_eq!(copied.as_deref(), Some("列\t基准值\t对照值\nnote\tnew\told\nqty\t2\t2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn engine_gate(left: &str, right: &str) -> Option<&'static str> {
        let left_engine = Engine::parse(left).ok();
        let right_engine = Engine::parse(right).ok();
        if left_engine.is_none_or(|engine| !engine.is_relational()) || right_engine.is_none_or(|engine| !engine.is_relational()) {
            return Some("compare.gates.unsupported");
        }
        if left_engine != right_engine {
            return Some("compare.gates.engineMismatch");
        }
        None
    }

    fn temp_pane() -> ComparePane {
        let dir = std::env::temp_dir().join(format!("rillsql-compare-gate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        ComparePane::new(services)
    }

    fn context_choice(id: &str, name: &str) -> Choice {
        // MySQL 不需要 schema 读取：这个测试只验证带入的上下文落到哪一侧。
        Choice { id: id.into(), name: name.into(), engine: "mysql".into(), databases: vec!["shop".into()], catalog_error: None }
    }

    #[gpui_kit::test]
    fn context_carries_the_object_side_and_explains_what_is_missing(cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_kit::init);
        let pane = cx.update(|cx| {
            let dir = std::env::temp_dir().join(format!("rillsql-compare-context-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
            cx.new(|_| {
                let mut pane = ComparePane::new(services);
                pane.choices = vec![context_choice("left", "左"), context_choice("right", "右")];
                pane
            })
        });
        let context = CompareContext {
            connection_id: "right".into(),
            connection_name: "右".into(),
            database: "shop".into(),
            schema: String::new(),
            object: "orders".into(),
            path: "shop.orders · 右".into(),
        };
        pane.update(cx, |pane, cx| pane.open_context(Some(context.clone()), cx));
        let (baseline, target) = pane.update(cx, |pane, _| pane.sides());
        let note = pane.update(cx, |pane, _| pane.context_note().to_owned());
        assert_eq!(target, "right", "当前对象放在对照侧");
        assert_eq!(baseline, "left", "基准侧取另一个已打开连接");
        assert!(note.contains("shop.orders"), "{note}");
        assert!(!note.contains("还需要一个已打开的连接"), "两侧都齐了就不能再报缺连接：{note}");
        assert_eq!(pane.update(cx, |pane, _| pane.context().cloned()), Some(context.clone()));

        // 只剩一侧可用：当前对象仍然带入，并说明另一侧还缺连接。
        pane.update(cx, |pane, _| {
            pane.choices.retain(|choice| choice.id == "right");
            pane.baseline = SidePick { connection_id: String::new(), database: String::new(), schema: String::new() };
        });
        pane.update(cx, |pane, cx| pane.open_context(Some(context.clone()), cx));
        let (baseline, target) = pane.update(cx, |pane, _| pane.sides());
        let note = pane.update(cx, |pane, _| pane.context_note().to_owned());
        assert_eq!(target, "right");
        assert!(baseline.is_empty());
        assert!(note.contains("对照侧"), "{note}");

        // 连接没打开用于对比：保留带入说明，不改动已选的两侧。
        let missing = CompareContext { connection_id: "gone".into(), ..context.clone() };
        pane.update(cx, |pane, cx| pane.open_context(Some(missing), cx));
        let (baseline_now, target_now) = pane.update(cx, |pane, _| pane.sides());
        let note = pane.update(cx, |pane, _| pane.context_note().to_owned());
        assert_eq!(target_now, "right", "缺失的一侧不能顶掉已选目标");
        assert_eq!(baseline_now, baseline);
        assert!(note.contains("还没有打开用于对比"), "{note}");

        // 没有上下文：说清缺什么，不假装带了目标。
        pane.update(cx, |pane, cx| pane.open_context(None, cx));
        assert!(pane.update(cx, |pane, _| pane.context().is_none()));
        assert_eq!(
            pane.update(cx, |pane, _| pane.context_note().to_owned()),
            pane.update(cx, |pane, _| pane.text("compare.context.none"))
        );

        // 返回入口只置位一次，由外壳取走后恢复原工作项与焦点。
        pane.update(cx, |pane, cx| pane.command("compare-back", cx));
        assert!(pane.update(cx, |pane, _| pane.take_back()));
        assert!(!pane.update(cx, |pane, _| pane.take_back()));
    }

    /// 重新带入上下文会把基准侧换回候选连接：旧报告、选择与待放入脚本必须一起失效，
    /// 否则页面显示的两侧和留在屏幕上的差异不再对应。
    #[gpui_kit::test]
    fn reopening_the_context_with_another_baseline_drops_the_old_report(cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_kit::init);
        let pane = cx.update(|cx| {
            let dir = std::env::temp_dir().join(format!("rillsql-compare-reopen-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
            cx.new(|_| {
                let mut pane = ComparePane::new(services);
                pane.choices = vec![context_choice("a", "A"), context_choice("b", "B"), context_choice("c", "C")];
                pane
            })
        });
        let context = CompareContext {
            connection_id: "a".into(),
            connection_name: "A".into(),
            database: "shop".into(),
            schema: String::new(),
            object: "orders".into(),
            path: "shop.orders · A".into(),
        };
        pane.update(cx, |pane, cx| pane.open_context(Some(context.clone()), cx));
        assert_eq!(pane.update(cx, |pane, _| pane.sides()), ("b".to_owned(), "a".to_owned()), "基准侧自动取另一个已打开连接");

        // 用户手动把基准侧改成 C，并跑出一份 C 对 A 的报告。
        pane.update(cx, |pane, cx| pane.pick(true, "c", cx));
        assert_eq!(pane.update(cx, |pane, _| pane.sides()), ("c".to_owned(), "a".to_owned()));
        pane.update(cx, |pane, _| {
            pane.structure = Some(Arc::new(empty_structure(None)));
            pane.structure_status = Status::Ready;
            pane.selected.insert("orders".into(), true);
            pane.chosen_key.insert("orders".into(), "id".into());
            pane.placed = Some(PlacedScript {
                sql: "ALTER TABLE orders ADD COLUMN note TEXT;".into(),
                connection_id: None,
                connection_name: None,
                database: None,
                engine: None,
            });
        });

        // 再次带入同一对象的上下文：基准侧回到 B，C 对 A 的报告不能再留在页面上。
        pane.update(cx, |pane, cx| pane.open_context(Some(context), cx));
        let (baseline, target) = pane.update(cx, |pane, _| pane.sides());
        assert_eq!((baseline.as_str(), target.as_str()), ("b", "a"), "重新带入会替换基准侧");
        pane.update(cx, |pane, _| {
            assert!(pane.structure.is_none(), "基准侧变了，旧结构差异不再是新目标的结论");
            assert!(pane.data.is_none());
            assert_eq!(pane.structure_status, Status::Idle);
            assert_eq!(pane.data_status, Status::Idle);
            assert!(pane.selected.is_empty(), "旧选择属于旧的一对侧");
            assert!(pane.chosen_key.is_empty());
            assert!(pane.placed.is_none() && pane.pending_place.is_none(), "待放入脚本也要失效");
        });
    }
}

#[cfg(test)]
mod workbench {
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{Point, TestAppContext, WindowBounds, WindowOptions, px, size};

    use super::*;

    #[gpui_kit::test]
    fn sqlite_compare_shows_diffs_cancel_and_places_script_without_running_it(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-compare-ui-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let baseline_file = dir.join("baseline.sqlite");
        let target_file = dir.join("target.sqlite");
        let config = dir.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        crate::app::views::grid::runtime_for_views().spawn(async move {
            let baseline = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&baseline_file).create_if_missing(true))
                .await
                .unwrap();
            sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT)").execute(&baseline).await.unwrap();
            sqlx::query("CREATE TABLE orders (id INTEGER PRIMARY KEY, note TEXT)").execute(&baseline).await.unwrap();
            sqlx::query("INSERT INTO orders (id, note) VALUES (1, 'a'), (3, NULL)").execute(&baseline).await.unwrap();
            baseline.close().await;
            let target = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&target_file).create_if_missing(true))
                .await
                .unwrap();
            sqlx::query("CREATE TABLE orders (id INTEGER PRIMARY KEY, note INTEGER)").execute(&target).await.unwrap();
            sqlx::query("INSERT INTO orders (id, note) VALUES (1, 2), (2, 9)").execute(&target).await.unwrap();
            target.close().await;
            let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(config.join("connections.sqlite")))));
            services.connections.open_sqlite_for_test("base", "基准", &baseline_file).await.unwrap();
            services.connections.open_sqlite_for_test("other", "对照", &target_file).await.unwrap();
            let extra = config.join("extra.sqlite");
            let extra_pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&extra).create_if_missing(true))
                .await
                .unwrap();
            sqlx::query("CREATE TABLE extra_marker (id INTEGER PRIMARY KEY)").execute(&extra_pool).await.unwrap();
            extra_pool.close().await;
            let attached = extra.display().to_string().replace('\'', "''").replace('\\', "\\\\");
            if let crate::connections::CatalogSession::Sqlite { pool, .. } = services.connections.catalog_session("base").expect("base session") {
                sqlx::query(sqlx::AssertSqlSafe(format!("ATTACH DATABASE '{attached}' AS extra"))).execute(&pool).await.unwrap();
            }
            let _ = tx.send((services, target_file));
        });
        let (services, target_file) = rx.recv_timeout(std::time::Duration::from_secs(120)).expect("sqlite compare setup");
        cx.update(gpui_kit::init);
        let (window, view) = cx.update(|cx| {
            // 结果页有差异树与并排定义，窗口拉高让下方的数据表选择也在可视区里。
            let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1200.), px(1800.)) };
            let (window, view) = gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |window, cx| cx.new(|cx| crate::app::shell::Workbench::with_services(window, cx, services, false)),
            )
            .expect("workbench");
            (window, view)
        });
        cx.update_window(window, |_, _, cx| {
            view.update(cx, |view, cx| view.perform("compare", cx));
        })
        .unwrap();
        assert!(
            pump(cx, window, &view, |view, app| {
                let text = view.compare_text(app);
                text.contains("基准 sqlite") && text.contains("对照 sqlite") && text.contains("database base extra") && text.contains("database base main")
            }),
            "没有列出已连接的 SQLite 或目录里的库"
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("compare-baseline:base").label(), Some("基准 基准"));
            assert_eq!(window.find("compare-target:other").label(), Some("对照 对照"));
            window.click("compare-structure", cx);
            view.update(cx, |view, cx| {
                let text = view.compare_text(cx);
                assert!(text.contains("请选择已打开的基准和对照"), "{text}");
                assert!(!text.contains("CREATE TABLE"), "{text}");
                assert!(!view.compare_structure_ready(cx), "{text}");
            });
            window.click("compare-baseline:base", cx);
            window.render_frame(cx);
            assert_eq!(window.find("compare-database:baseline:main").label(), Some("基准 数据库 main"));
            assert_eq!(window.find("compare-database:baseline:extra").label(), Some("基准 数据库 extra"));
            window.click("compare-database:baseline:main", cx);
            window.click("compare-target:other", cx);
            window.render_frame(cx);
            assert_eq!(window.find("compare-database:target:main").label(), Some("对照 数据库 main"));
            window.click("compare-database:target:main", cx);
            view.update(cx, |view, cx| {
                let text = view.compare_text(cx);
                assert!(text.contains("数据库 main"), "{text}");
                assert!(text.contains("基准 基准") && text.contains("对照 对照"), "{text}");
            });
            window.click("compare-structure", cx);
        })
        .unwrap();
        assert!(
            pump(cx, window, &view, |view, app| view.compare_structure_ready(app) && view.compare_text(app).contains("customers") && view.compare_text(app).contains("orders")),
            "结构差异没有出现在对比页"
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("compare-toggle:orders").label(), Some("on orders"));
            assert!(window.try_find("compare-toggle:customers").is_some());
            window.click("compare-toggle:orders", cx);
            view.update(cx, |view, cx| assert!(view.compare_text(cx).contains("off orders"), "{}", view.compare_text(cx)));
            window.click("compare-toggle:orders", cx);
            view.update(cx, |view, cx| assert!(view.compare_text(cx).contains("on orders"), "{}", view.compare_text(cx)));
            window.click("compare-data", cx);
        })
        .unwrap();
        assert!(pump(cx, window, &view, |view, app| {
            let text = view.compare_text(app);
            view.compare_data_ready(app) && text.contains("INSERT") && text.contains("DELETE") && text.contains("UPDATE")
        }), "数据差异没有出现在对比页");
        assert_db_unchanged(&target_file);
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| view.arm_compare_pause(cx));
            window.click("compare-structure", cx);
            view.update(cx, |view, cx| {
                assert!(view.compare_running_now(cx));
                assert!(!view.model.allow_system_close());
                assert!(view.model.close_prompt.is_none());
            });
            window.click("compare-cancel", cx);
            view.update(cx, |view, cx| view.release_compare_pause(cx));
        })
        .unwrap();
        assert!(pump(cx, window, &view, |view, app| !view.compare_running_now(app)), "取消后仍在运行");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            view.update(cx, |view, cx| {
                let text = view.compare_text(cx);
                assert!(!view.compare_running_now(cx), "{text}");
                assert!(!view.compare_structure_ready(cx), "{text}");
                assert!(text.contains("已取消"), "{text}");
                assert!(!text.contains("CREATE TABLE"), "{text}");
                assert!(!view.model.compare_running);
                view.model.close_prompt = None;
                assert!(view.model.allow_system_close(), "{:?}", view.model.facts());
            });
            window.click("compare-structure", cx);
        })
        .unwrap();
        assert!(pump(cx, window, &view, |view, app| view.compare_structure_ready(app)), "取消后的下一次结构比较没有完成");
        cx.update_window(window, |_, window, cx| {
            window.click("compare-place:structure", cx);
            window.render_frame(cx);
            view.update(cx, |view, cx| {
                assert!(view.compare_text(cx).contains("不会执行"), "{}", view.compare_text(cx));
            });
            window.click("compare-place-confirm", cx);
            window.render_frame(cx);
            view.update(cx, |view, cx| {
                let draft = view.model.active_tab().expect("query").draft.clone();
                assert!(draft.contains("customers") && draft.to_ascii_uppercase().contains("CREATE"), "{draft}");
                assert!(view.model.active_tab().unwrap().last_executed_sql.is_none());
                assert!(!view.model.active_tab().unwrap().running);
                assert!(!view.query_executed(cx));
                assert_eq!(view.query_runs(cx), 0);
                assert_eq!(view.model.executions, 0);
            });
        })
        .unwrap();
        assert_db_unchanged(&target_file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn rendered_controls_pick_database_schema_table_and_compare_key(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-compare-scope-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        cx.update(gpui_kit::init);
        let (window, view) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1200.), px(900.)) };
            let (window, view) = gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |_, cx| {
                    cx.new(|_| {
                        let mut pane = ComparePane::new(Arc::clone(&services));
                        pane.choices = vec![
                            Choice {
                                id: "left".into(),
                                name: "左".into(),
                                engine: "postgres".into(),
                                databases: vec!["app".into(), "warehouse".into()],
                                catalog_error: None,
                            },
                            Choice {
                                id: "right".into(),
                                name: "右".into(),
                                engine: "postgres".into(),
                                databases: vec!["app".into(), "warehouse".into()],
                                catalog_error: None,
                            },
                        ];
                        pane.baseline = SidePick { connection_id: "left".into(), database: "app".into(), schema: String::new() };
                        pane.target = SidePick { connection_id: "right".into(), database: "app".into(), schema: String::new() };
                        pane.schemas.insert(schema_key("left", "app"), SchemaState::Ready(vec!["public".into(), "analytics".into()]));
                        pane.schemas.insert(schema_key("right", "app"), SchemaState::Manual("读取 schema 失败：权限不足".into()));
                        pane.structure = Some(Arc::new(StructureReport {
                            refused: None,
                            script: String::new(),
                            incomplete: true,
                            notes: Vec::new(),
                            only_baseline: Vec::new(),
                            only_target: Vec::new(),
                            changed: Vec::new(),
                            same: Vec::new(),
                            timed_out: false,
                            tables: vec![crate::compare::TableChoice {
                                name: "orders".into(),
                                on_both: true,
                                needs_key_choice: true,
                                keys: vec![
                                    crate::compare::KeyChoice { name: "uk_code".into(), columns: vec!["code".into()], primary: false },
                                    crate::compare::KeyChoice { name: "uk_email".into(), columns: vec!["email".into()], primary: false },
                                ],
                            }],
                        }));
                        pane.structure_status = Status::Incomplete;
                        pane.selected.insert("orders".into(), true);
                        pane
                    })
                },
            )
            .expect("compare pane");
            (window, view)
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("compare-baseline:left").label(), Some("基准 左"));
            assert_eq!(window.find("compare-target:right").label(), Some("对照 右"));
            assert_eq!(window.find("compare-database:baseline:warehouse").label(), Some("基准 数据库 warehouse"));
            assert_eq!(window.find("compare-schema:baseline:public").label(), Some("基准 选择 schema public"));
            assert_eq!(window.find("compare-schema:baseline:analytics").label(), Some("基准 选择 schema analytics"));
            let manual = window.find("compare-schema-input:target");
            assert_eq!(manual.role(), Some(gpui_kit::Role::TextInput));
            assert!(manual.label().unwrap_or("").contains("读取 schema 失败：权限不足"), "{:?}", manual.label());
            assert_eq!(window.find("compare-toggle:orders").label(), Some("on orders"));
            assert_eq!(window.find("compare-key:orders|uk_code").label(), Some("key orders uk_code"));
            assert_eq!(window.find("compare-key:orders|uk_email").label(), Some("key orders uk_email"));
            view.update(cx, |pane, _| {
                let text = pane.lines().join("\n");
                assert!(text.contains("读取 schema 失败：权限不足"), "{text}");
                assert!(text.contains("schema public"), "{text}");
                assert!(pane.refuse_local().is_some_and(|message| message.contains("不生成脚本")), "{text}");
                assert!(!pane.structure_ready());
            });
            window.click("compare-baseline:left", cx);
            window.click("compare-target:right", cx);
            window.click("compare-key:orders|uk_email", cx);
            window.click("compare-toggle:orders", cx);
            view.update(cx, |pane, _| {
                let text = pane.lines().join("\n");
                assert!(text.contains("chosen orders uk_email"), "{text}");
                assert!(text.contains("off orders"), "{text}");
                assert!(!pane.structure_ready());
                assert!(pane.structure.as_ref().is_some_and(|report| report.script.is_empty()));
            });
            // 切 schema 后旧结构差异、选中表与待放入脚本都必须失效。
            window.click("compare-schema:baseline:public", cx);
            window.click("compare-schema-input:target", cx);
            window.input("dbo", cx);
            window.press("enter", cx);
            view.update(cx, |pane, _| {
                let text = pane.lines().join("\n");
                assert!(text.contains("schema-on public"), "{text}");
                assert!(text.contains("schema-manual dbo"), "{text}");
                assert!(!text.contains("chosen orders"), "切 schema 后旧选择必须清掉：{text}");
                assert!(pane.structure.is_none(), "切 schema 后旧结构差异必须清掉：{text}");
            });
            window.click("compare-database:baseline:warehouse", cx);
            view.update(cx, |pane, _| {
                let text = pane.lines().join("\n");
                assert!(text.contains("数据库 warehouse"), "{text}");
                assert!(!text.contains("on orders"), "换库后旧对比结果必须清掉：{text}");
                assert!(pane.structure.is_none(), "{text}");
            });
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 打开一个只带连接清单的对比页窗口，用于三档宽度与状态证据；两侧都还没选。
    fn open_compare_window(cx: &mut App, dir: &std::path::Path, width: f32) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<ComparePane>) {
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(width), px(900.)) };
        gpui_kit::open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, cx, |_, cx| {
            cx.new(|_| {
                let mut pane = ComparePane::new(services);
                pane.choices = vec![
                    Choice { id: "left".into(), name: "左库".into(), engine: "mysql".into(), databases: vec!["shop".into()], catalog_error: None },
                    Choice { id: "right".into(), name: "右库".into(), engine: "mysql".into(), databases: vec!["shop".into()], catalog_error: None },
                ];
                pane
            })
        })
        .expect("compare window")
    }

    /// 三档宽度 × 双主题 × 双密度：1360 两侧并排、900/640 竖排，两张卡片都不越出窗口右边界（R1/R3/R6）。
    #[gpui_kit::test]
    fn sides_reflow_at_1360_900_and_640_in_both_themes_and_densities(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-compare-widths-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        cx.update(gpui_kit::init);
        let setups = [
            ("浅色/紧凑", false, kit::Metrics::compact()),
            ("浅色/标准", false, kit::Metrics::standard()),
            ("深色/紧凑", true, kit::Metrics::compact()),
            ("深色/标准", true, kit::Metrics::standard()),
        ];
        for (label, dark, density) in setups {
            for (width, stacked) in [(1360., false), (900., true), (640., true)] {
                cx.update(|cx| {
                    kit::apply_theme(dark, cx);
                    density.publish(cx);
                });
                let (window, _) = cx.update(|cx| open_compare_window(cx, &dir, width));
                let (baseline, target, base_card, target_card) = cx
                    .update_window(window, |_, window, cx| {
                        window.render_frame(cx);
                        (
                            // 两侧都取同一个候选连接：同一行，竖向/横向位置才有可比性。
                            window.find("compare-baseline:left").bounds(),
                            window.find("compare-target:left").bounds(),
                            window.find("compare-side:baseline").bounds(),
                            window.find("compare-side:target").bounds(),
                        )
                    })
                    .unwrap();
                println!("{label} {width}: 卡片 基准 {base_card:?} 对照 {target_card:?}");
                assert!(baseline.size.width > px(0.) && target.size.width > px(0.), "{label}/{width}: 两侧都要有可用宽度");
                assert!(baseline.origin.x + baseline.size.width <= px(width) + px(1.), "{label}/{width}: 基准侧越出右边界");
                assert!(target.origin.x + target.size.width <= px(width) + px(1.), "{label}/{width}: 对照侧越出右边界");
                println!("{label} {width}: {} 基准 {:?} 对照 {:?}", if stacked { "竖排" } else { "并排" }, baseline, target);
                if stacked {
                    assert_eq!(baseline.origin.x, target.origin.x, "{label}/{width}: 竖排时两侧同列");
                    assert!(baseline.origin.y + baseline.size.height <= target.origin.y, "{label}/{width}: 窄宽要竖排，实际 基准{:?} / 对照{:?}", baseline, target);
                    assert!(base_card.origin.y + base_card.size.height <= target_card.origin.y, "{label}/{width}: 两张卡片也要竖排");
                } else {
                    assert_eq!(baseline.origin.y, target.origin.y, "{label}/{width}: 并排时两侧同一行");
                    assert!(baseline.origin.x + baseline.size.width <= target.origin.x, "{label}/{width}: 宽屏要并排，实际 基准{:?} / 对照{:?}", baseline, target);
                    assert_eq!(base_card.origin.y, target_card.origin.y, "{label}/{width}: 两张卡片顶部对齐");
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 640 窄窗口：初始指导是一整块状态并带主操作；写完两侧后交换仍可点，比较按钮没被裁掉。
    #[gpui_kit::test]
    fn narrow_640_keeps_the_guide_its_action_and_the_core_operations_reachable(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-compare-narrow-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        cx.update(gpui_kit::init);
        let (window, view) = cx.update(|cx| open_compare_window(cx, &dir, 640.));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("empty-state").is_some(), "初始指导要是统一空态");
            let guide = window.find("compare-guide-start");
            assert_eq!(guide.label(), Some("对比结构"), "空态主操作就在块内");
            assert!(guide.bounds().origin.x + guide.bounds().size.width <= px(640.) + px(1.), "主操作不能被裁出窗口");
            // 主操作就是比较入口：没有选两侧时给出门禁原因，而不是空白。
            window.click("compare-guide-start", cx);
            window.render_frame(cx);
            assert!(window.try_find("empty-state").is_none(), "触发比较后不该还画初始指导");
            view.update(cx, |pane, _| {
                let text = pane.lines().join("\n");
                assert!(text.contains("请选择已打开的基准和对照"), "{text}");
            });
            // 窄窗口里两侧选择与交换仍可达。
            window.click("compare-baseline:left", cx);
            window.click("compare-target:right", cx);
            view.update(cx, |pane, _| assert_eq!(pane.sides(), ("left".to_owned(), "right".to_owned())));
            window.click("compare-swap", cx);
            view.update(cx, |pane, _| assert_eq!(pane.sides(), ("right".to_owned(), "left".to_owned()), "窄窗口交换仍生效"));
            window.render_frame(cx);
            let compare = window.find("compare-structure");
            assert!(compare.bounds().origin.x + compare.bounds().size.width <= px(640.) + px(1.), "比较主按钮不能被裁出窗口");
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 失败用统一错误态 + 重试；取消只给横幅。重试后清掉错误，回到可继续的状态。
    #[gpui_kit::test]
    fn a_failed_compare_shows_the_error_state_and_retry_clears_it(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-compare-failed-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        cx.update(gpui_kit::init);
        let (window, view) = cx.update(|cx| open_compare_window(cx, &dir, 900.));
        view.update(cx, |pane, _| pane.fail(pane.text("compare.status.failed"), RetryAction::Structure));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("error-state").is_some(), "失败要用统一错误态");
            assert_eq!(window.find("compare-retry").label(), Some("重试"));
            assert!(window.try_find("empty-state").is_none(), "失败与初始指导互斥");
            window.click("compare-retry", cx);
            window.render_frame(cx);
            assert!(window.try_find("error-state").is_none(), "重试后错误态要收掉");
        })
        .unwrap();
        view.update(cx, |pane, _| {
            assert_eq!(pane.error_kind, ErrorKind::None);
            assert!(pane.error.is_empty());
            let text = pane.lines().join("\n");
            assert!(text.contains("请选择已打开的基准和对照"), "重试走的是重发比较，门禁照旧：{text}");
        });
        // 取消是提示：横幅，不是错误态，也没有重试。
        view.update(cx, |pane, _| pane.notify(pane.text("compare.status.cancelled")));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("error-state").is_none(), "取消不该画成错误态");
            assert!(window.try_find("compare-retry").is_none(), "取消不给重试");
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 资源树多选两张表 → “对比表”：两侧不同连接、表名不同；结构差异逐列列出，
    /// 数据对比针对对照表；同步 SQL 只放进编辑器，不执行。没有主键的一对表只做结构对比并说明原因。
    #[gpui_kit::test]
    fn compare_tables_from_the_tree_shows_column_diffs_and_previews_sql(cx: &mut TestAppContext) {
        use crate::catalog::model::{ConnStatus, DatabaseNode, ObjectRow, SchemaNode};
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-compare-pair-ui-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let baseline_file = dir.join("baseline.sqlite");
        let target_file = dir.join("target.sqlite");
        let config = dir.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let target_for_setup = target_file.clone();
        crate::app::views::grid::runtime_for_views().spawn(async move {
            let open = |file: std::path::PathBuf| async move {
                sqlx::sqlite::SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&file).create_if_missing(true))
                    .await
                    .unwrap()
            };
            let baseline = open(baseline_file.clone()).await;
            for sql in [
                "CREATE TABLE orders (id INTEGER PRIMARY KEY, note TEXT)",
                "INSERT INTO orders (id, note) VALUES (1, 'a'), (3, 'c')",
                "CREATE TABLE notes (label TEXT, body TEXT)",
            ] {
                sqlx::query(sql).execute(&baseline).await.unwrap();
            }
            baseline.close().await;
            let target = open(target_for_setup.clone()).await;
            for sql in [
                "CREATE TABLE orders_copy (id INTEGER PRIMARY KEY, note TEXT, extra TEXT)",
                "INSERT INTO orders_copy (id, note) VALUES (1, 'b'), (2, 'x')",
                "CREATE TABLE notes_old (label TEXT)",
            ] {
                sqlx::query(sql).execute(&target).await.unwrap();
            }
            target.close().await;
            let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(config.join("connections.sqlite")))));
            services.connections.open_sqlite_for_test("base", "基准", &baseline_file).await.unwrap();
            services.connections.open_sqlite_for_test("other", "对照", &target_for_setup).await.unwrap();
            let _ = tx.send(services);
        });
        let services = rx.recv_timeout(std::time::Duration::from_secs(120)).expect("sqlite pair setup");
        cx.update(gpui_kit::init);
        let (window, view) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1200.), px(1800.)) };
            gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |window, cx| cx.new(|cx| crate::app::shell::Workbench::with_services(window, cx, services, false)),
            )
            .expect("workbench")
        });
        let orders = ObjectRow::table("orders", Some(2));
        let orders_copy = ObjectRow::table("orders_copy", Some(2));
        let tree = |id: &str, name: &str, objects: Vec<ObjectRow>| {
            let mut connection = crate::catalog::NavConnection::new(id, name, crate::catalog::model::Engine::Sqlite, ConnStatus::Connected);
            let total = objects.len();
            connection.databases = vec![DatabaseNode {
                name: "main".to_owned(),
                schemas: vec![SchemaNode { name: String::new(), objects, total, error: None, kind_reports: Vec::new() }],
                error: None,
            }];
            connection
        };
        let left = crate::catalog::hierarchy::object_node_id("base", "main", "", &orders);
        let right = crate::catalog::hierarchy::object_node_id("other", "main", "", &orders_copy);
        cx.update_window(window, |_, _, cx| {
            view.update(cx, |view, cx| {
                view.model.apply_navigation(&[tree("base", "基准", vec![orders.clone()]), tree("other", "对照", vec![orders_copy.clone()])], false);
                view.perform(&format!("nav-select-toggle:{left}"), cx);
                view.perform(&format!("nav-select-toggle:{right}"), cx);
                assert_eq!(view.model.selected_table_targets().len(), 2);
                view.perform("compare-tables", cx);
            });
        })
        .unwrap();
        assert!(
            pump(cx, window, &view, |view, app| {
                let text = view.compare_text(app);
                text.starts_with("对比表")
                    && text.contains("基准表 orders")
                    && text.contains("对照表 orders_copy")
                    && text.contains("基准 基准")
                    && text.contains("对照 对照")
                    && view.model.shell.page == crate::app::AppPage::Compare
            }),
            "没有进入表对表对比"
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("compare-table-pair").is_some());
            window.click("compare-structure", cx);
        })
        .unwrap();
        assert!(
            pump(cx, window, &view, |view, app| view.compare_structure_ready(app) && view.compare_text(app).contains("extra")),
            "表对表的结构差异没有出现：{}",
            cx.update(|app| view.read(app).compare_text(app))
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            view.update(cx, |view, cx| {
                let text = view.compare_text(cx);
                assert!(text.contains("列 extra 仅对照拥有"), "逐列列出：{text}");
                assert!(text.contains("orders_copy") && !text.contains("仅基准拥有 表"), "{text}");
            });
            window.click("compare-data", cx);
        })
        .unwrap();
        assert!(
            pump(cx, window, &view, |view, app| {
                let text = view.compare_text(app);
                view.compare_data_ready(app) && text.contains("UPDATE") && text.contains("INSERT") && text.contains("DELETE")
            }),
            "表对表的数据差异没有出现：{}",
            cx.update(|app| view.read(app).compare_text(app))
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("compare-place:structure", cx);
            window.render_frame(cx);
            window.click("compare-place-confirm", cx);
            window.render_frame(cx);
            view.update(cx, |view, cx| {
                let draft = view.model.active_tab().expect("query").draft.clone();
                assert!(draft.contains("orders_copy"), "{draft}");
                assert!(view.model.active_tab().unwrap().last_executed_sql.is_none());
                assert!(!view.query_executed(cx));
                assert_eq!(view.model.executions, 0);
            });
        })
        .unwrap();

        // 单表“对比表…”：预填基准，填对照表名；没有主键时只做结构对比并说明原因。
        let notes = ObjectRow::table("notes", Some(0));
        let notes_id = crate::catalog::hierarchy::object_node_id("base", "main", "", &notes);
        cx.update_window(window, |_, _, cx| {
            view.update(cx, |view, cx| {
                view.model.apply_navigation(&[tree("base", "基准", vec![orders.clone(), notes.clone()])], false);
                view.perform("nav-select-clear", cx);
                view.perform(&format!("nav-focus:{notes_id}"), cx);
                assert!(view.model.open_nav_menu(&notes_id));
                view.perform("compare-table", cx);
            });
        })
        .unwrap();
        assert!(
            pump(cx, window, &view, |view, app| {
                let text = view.compare_text(app);
                text.contains("基准表 notes") && text.contains("对照表 ") && text.contains("基准 基准")
            }),
            "单表对比没有预填基准"
        );
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.perform("compare-structure", cx);
                assert!(view.compare_text(cx).contains("需要填写基准表和对照表"), "{}", view.compare_text(cx));
                view.perform("compare-target:other", cx);
                view.perform("compare-table-name:target:notes_old", cx);
            });
            window.render_frame(cx);
            view.update(cx, |view, cx| view.perform("compare-structure", cx));
        })
        .unwrap();
        assert!(
            pump(cx, window, &view, |view, app| view.compare_structure_ready(app)),
            "单表对比的结构差异没有完成：{}",
            cx.update(|app| view.read(app).compare_text(app))
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            view.update(cx, |view, cx| {
                let text = view.compare_text(cx);
                assert!(text.contains("列 body 仅基准拥有"), "{text}");
                assert!(text.contains("没有主键或唯一键") && text.contains("只做结构对比"), "{text}");
            });
            view.update(cx, |view, cx| {
                view.perform("compare-data", cx);
                let text = view.compare_text(cx);
                assert!(!view.compare_data_ready(cx), "{text}");
                assert!(text.contains("没有主键或唯一键"), "{text}");
            });
        })
        .unwrap();
        // 目标库没有被改动。
        let (tx, rx) = std::sync::mpsc::channel();
        crate::app::views::grid::runtime_for_views().spawn(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&target_file).read_only(true))
                .await
                .unwrap();
            let note: String = sqlx::query_scalar("SELECT note FROM orders_copy WHERE id = 1").fetch_one(&pool).await.unwrap();
            let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM orders_copy").fetch_one(&pool).await.unwrap();
            pool.close().await;
            let _ = tx.send((note, rows));
        });
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(30)).expect("read target"), ("b".to_owned(), 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 表名智能补全：前缀匹配优先于子串匹配，空输入返回全部表；选中候选项更新 `table_pair` 并收起弹层。
    #[gpui_kit::test]
    fn table_autocomplete_filters_prefix_first_and_selects_into_table_pair(cx: &mut TestAppContext) {
        let all: Vec<String> = vec![
            "audit_orders".into(),
            "customers".into(),
            "order_items".into(),
            "orders".into(),
            "pre_order_log".into(),
        ];
        assert_eq!(filter_table_candidates(&all, ""), all, "空输入返回全部表");
        assert_eq!(
            filter_table_candidates(&all, "ord"),
            vec!["order_items".to_owned(), "orders".to_owned(), "audit_orders".to_owned(), "pre_order_log".to_owned()],
            "前缀匹配排在子串匹配前面"
        );
        assert_eq!(
            filter_table_candidates(&all, "ORD"),
            vec!["order_items".to_owned(), "orders".to_owned(), "audit_orders".to_owned(), "pre_order_log".to_owned()],
            "大小写不敏感"
        );
        assert!(filter_table_candidates(&all, "nonexistent").is_empty());

        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-compare-ac-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        cx.update(gpui_kit::init);
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let (window, view) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1200.), px(900.)) };
            gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |_, cx| {
                    cx.new(|_| {
                        let mut pane = ComparePane::new(Arc::clone(&services));
                        tests::sqlite_pair(&mut pane);
                        pane.table_pair = Some(("ord".into(), String::new()));
                        pane.table_reload = true;
                        pane.tables_cache.insert(table_cache_key("left", "main", ""), TableListState::Ready(all.clone()));
                        pane.tables_cache.insert(table_cache_key("right", "main", ""), TableListState::Ready(all.clone()));
                        pane.table_ac_side = Some("baseline");
                        pane
                    })
                },
            )
            .expect("compare pane")
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("compare-table-ac:baseline").is_some(), "补全弹层应当渲染");
            assert!(window.try_find("compare-table-ac:baseline:order_items").is_some());
            assert!(window.try_find("compare-table-ac:baseline:orders").is_some());
            assert!(window.try_find("compare-table-ac:baseline:customers").is_none(), "不匹配的表不应出现");
            // 点击候选项 `orders` 更新 `table_pair.0` 并关闭弹层。
            window.click("compare-table-ac:baseline:orders", cx);
            window.render_frame(cx);
            view.update(cx, |pane, _| {
                assert_eq!(pane.table_pair(), Some(("orders".into(), String::new())));
                assert_eq!(pane.table_ac_side, None, "选中后关闭弹层");
            });
            assert!(window.try_find("compare-table-ac:baseline").is_none());

            // 对照侧：空输入时列出全部表，键盘步进后接受第 2 项。
            view.update(cx, |pane, cx| {
                pane.table_ac_side = Some("target");
                pane.table_ac_index = 0;
                pane.step_table_ac(false, 1);
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.try_find("compare-table-ac:target:customers").is_some());
            view.update(cx, |pane, cx| {
                let picked = pane.table_candidates_for_side(false)[pane.table_ac_index].clone();
                pane.accept_table_candidate("target", &picked, window, cx);
            });
            view.update(cx, |pane, _| {
                assert_eq!(pane.table_pair(), Some(("orders".into(), "customers".into())));
                assert_eq!(pane.table_ac_side, None);
            });
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回归测试：`compare-diff-tree` 放 25 条两行差异项时，行高必须为正且纵向绝不重叠。
    #[gpui_kit::test]
    fn diff_tree_rows_never_overlap_vertically_with_many_items(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = std::env::temp_dir().join(format!("rillsql-compare-overlap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        cx.update(gpui_kit::init);
        let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let (window, _view) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1200.), px(900.)) };
            gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |_, cx| {
                    cx.new(|_| {
                        let mut pane = ComparePane::new(Arc::clone(&services));
                        tests::sqlite_pair(&mut pane);
                        let mut report = empty_structure(None);
                        for i in 0..25 {
                            report.changed.push(tests::annotated(
                                &format!("table_{i:02}"),
                                "列类型不同，索引定义不一致",
                                &format!("ALTER TABLE table_{i:02} ALTER COLUMN note TYPE text;"),
                            ));
                        }
                        pane.install_structure(report);
                        pane
                    })
                },
            )
            .expect("compare pane")
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let mut prev: Option<gpui_kit::Bounds<gpui_kit::Pixels>> = None;
            for i in 0..25 {
                let id = format!("compare-pick:c:{i}");
                let bounds = window.find(SharedString::from(id)).bounds();
                assert!(bounds.size.height > px(20.), "第 {i} 行高度应容纳两行文字，实际 {:?}", bounds);
                if let Some(prev_bounds) = prev {
                    assert!(
                        prev_bounds.origin.y + prev_bounds.size.height <= bounds.origin.y + px(0.5),
                        "第 {} 行与第 {} 行发生纵向重叠：prev={:?}, curr={:?}",
                        i - 1,
                        i,
                        prev_bounds,
                        bounds
                    );
                }
                prev = Some(bounds);
            }
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn assert_db_unchanged(target_file: &std::path::Path) {
        let (tx, rx) = std::sync::mpsc::channel();
        let target_file = target_file.to_path_buf();
        crate::app::views::grid::runtime_for_views().spawn(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&target_file).read_only(true))
                .await
                .unwrap();
            let customers: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'customers'").fetch_one(&pool).await.unwrap();
            let note: i64 = sqlx::query_scalar("SELECT note FROM orders WHERE id = 1").fetch_one(&pool).await.unwrap();
            let extra: i64 = sqlx::query_scalar("SELECT count(*) FROM orders WHERE id = 2").fetch_one(&pool).await.unwrap();
            pool.close().await;
            let _ = tx.send((customers, note, extra));
        });
        let (customers, note, extra) = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("read target");
        assert_eq!(customers, 0, "结构脚本被执行了");
        assert_eq!(note, 2);
        assert_eq!(extra, 1, "数据脚本被执行了");
    }

    fn pump(
        cx: &mut TestAppContext,
        window: gpui_kit::AnyWindowHandle,
        view: &gpui_kit::Entity<crate::app::shell::Workbench>,
        mut ready: impl FnMut(&crate::app::shell::Workbench, &gpui_kit::App) -> bool,
    ) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let done = cx.update(|app| ready(view.read(app), app));
            if done {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
    }
}

#[cfg(test)]
mod live_tests;
