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

use crate::ui::kit::{self, Palette, Radius, Tip as _, TypeScale};

use crate::application::tasks::{TaskOutcome, TaskToken};
use crate::application::{AppServices, cancel_compare, compare_cancelled};
use crate::compare::{DataReport, DataRequest, DataTableRequest, SideRequest, StructureReport, StructureRequest};
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
    structure: Option<StructureReport>,
    data: Option<DataReport>,
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
            #[cfg(test)]
            armed_pause: false,
            #[cfg(test)]
            pause: None,
        }
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
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
        if id == "compare-object" || id == "compare-grid" {
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
        } else if let Some(name) = id.strip_prefix("compare-schema:target:") {
            self.target.schema = name.to_owned();
            self.invalidate_results();
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
            self.place_prompt.clear();
        } else if id == "compare-swap" {
            self.swap_sides();
        } else if id == "compare-select-all" {
            self.select_all_tables();
        } else if id == "compare-clear" {
            self.clear_table_selection();
        } else if id == "compare-filter:all" {
            self.diff_filter = DiffFilter::All;
        } else if id == "compare-filter:baseline" {
            self.diff_filter = DiffFilter::Baseline;
        } else if id == "compare-filter:target" {
            self.diff_filter = DiffFilter::Target;
        } else if id == "compare-filter:changed" {
            self.diff_filter = DiffFilter::Changed;
        } else if id == "compare-copy:structure" {
            self.copy_script(RunKind::Structure, cx);
        } else if id == "compare-copy:data" {
            self.copy_script(RunKind::Data, cx);
        }
        cx.notify();
    }

    fn swap_sides(&mut self) {
        std::mem::swap(&mut self.baseline, &mut self.target);
        let left = self.schema_draft.remove("baseline").unwrap_or_default();
        let right = self.schema_draft.remove("target").unwrap_or_default();
        self.schema_draft.insert("baseline".into(), right);
        self.schema_draft.insert("target".into(), left);
        self.schema_reload = true;
        self.invalidate_results();
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
        let database = if choice.engine == "sqlite" {
            "main".to_owned()
        } else {
            choice.databases.first().cloned().unwrap_or_default()
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
        self.structure = None;
        self.data = None;
        self.structure_status = Status::Idle;
        self.data_status = Status::Idle;
        self.selected.clear();
        self.chosen_key.clear();
        self.placed = None;
        self.pending_place = None;
        self.place_prompt.clear();
        self.clear_error();
    }

    fn ensure_schemas(&mut self, baseline: bool, cx: &mut Context<Self>) {
        let slot = if baseline { &self.baseline } else { &self.target };
        let Some(engine) = self.engine_of(&slot.connection_id) else {
            return;
        };
        if engine != "postgres" && engine != "sqlserver" {
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
            self.structure = Some(empty_structure(Some(self.text("compare.gates.chooseBoth"))));
            self.structure_status = Status::Refused;
            return;
        };
        if let Some(message) = self.refuse_local() {
            self.structure = Some(empty_structure(Some(message)));
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
            self.data = Some(empty_data(Some(self.text("compare.gates.chooseBoth"))));
            self.data_status = Status::Refused;
            return;
        };
        if let Some(message) = self.refuse_local() {
            self.data = Some(empty_data(Some(message)));
            self.data_status = Status::Refused;
            return;
        }
        if request.tables.is_empty() {
            self.data = Some(empty_data(Some(self.text("compare.gates.noTablesPicked"))));
            self.data_status = Status::Refused;
            return;
        }
        self.spawn_data(request, cx);
    }

    fn structure_request(&mut self) -> Option<StructureRequest> {
        let baseline = self.side_request(&self.baseline)?;
        let target = self.side_request(&self.target)?;
        let id = format!("compare-{}", uuid::Uuid::new_v4());
        Some(StructureRequest { id, baseline, target })
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
                    .map(|table| DataTableRequest {
                        name: table.name.clone(),
                        key: self.chosen_key.get(&table.name).filter(|key| !key.is_empty()).cloned(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let id = format!("compare-data-{}", uuid::Uuid::new_v4());
        Some(DataRequest { id, baseline, target, tables })
    }

    fn side_request(&self, side: &SidePick) -> Option<SideRequest> {
        let choice = self.choices.iter().find(|choice| choice.id == side.connection_id)?;
        let database = if choice.engine == "sqlite" { "main".to_owned() } else { side.database.trim().to_owned() };
        let schema = if choice.engine == "postgres" || choice.engine == "sqlserver" { side.schema.trim().to_owned() } else { String::new() };
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
        if left.engine != "sqlite" && (self.baseline.database.trim().is_empty() || self.target.database.trim().is_empty()) {
            return Some(self.text("compare.gates.databasesNeeded"));
        }
        if let Some(message) = self.schema_block(&self.baseline).or_else(|| self.schema_block(&self.target)) {
            return Some(message);
        }
        None
    }

    fn schema_block(&self, side: &SidePick) -> Option<String> {
        let engine = self.engine_of(&side.connection_id)?;
        if engine != "postgres" && engine != "sqlserver" {
            return None;
        }
        if side.database.trim().is_empty() {
            return None;
        }
        match self.schemas.get(&schema_key(&side.connection_id, &side.database)) {
            Some(SchemaState::Loading) | None => Some(self.text("compare.gates.schemaStillLoading")),
            Some(SchemaState::Ready(names)) => {
                let value = side.schema.trim();
                if value.is_empty() || !names.iter().any(|name| name == value) {
                    Some(if engine == "sqlserver" { self.text("compare.gates.sqlserverSchemaNeeded") } else { self.text("compare.gates.schemaBothNeeded") })
                } else {
                    None
                }
            }
            Some(SchemaState::Manual(_)) => {
                if side.schema.trim().is_empty() {
                    Some(if engine == "sqlserver" { self.text("compare.gates.sqlserverSchemaNeeded") } else { self.text("compare.gates.schemaBothNeeded") })
                } else {
                    None
                }
            }
        }
    }

    fn spawn_structure(&mut self, request: StructureRequest, cx: &mut Context<Self>) {
        let token = self.begin_run(request.id.clone(), RunKind::Structure);
        let flag = Arc::clone(&self.cancel_flag);
        let services = Arc::clone(&self.services);
        let gate = self.take_gate();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                if let Some(gate) = gate {
                    let _ = gate.await;
                }
                let outcome = crate::application::work::compare_structure(&services.connections, &services.tasks, OWNER, token, &request).await;
                let _ = tx.send(outcome);
            });
            let outcome = rx.await;
            cx.update(|cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |pane, cx| {
                        match outcome {
                            Ok(outcome) => pane.finish_structure(token, &flag, outcome),
                            Err(_) => pane.finish_lost(token),
                        }
                        cx.notify();
                    });
                }
            });
        })
        .detach();
    }

    fn spawn_data(&mut self, request: DataRequest, cx: &mut Context<Self>) {
        let token = self.begin_run(request.id.clone(), RunKind::Data);
        let flag = Arc::clone(&self.cancel_flag);
        let services = Arc::clone(&self.services);
        let gate = self.take_gate();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                if let Some(gate) = gate {
                    let _ = gate.await;
                }
                let outcome = crate::application::work::compare_data(&services.connections, &services.tasks, OWNER, token, &request).await;
                let _ = tx.send(outcome);
            });
            let outcome = rx.await;
            cx.update(|cx| {
                if let Some(view) = view.upgrade() {
                    view.update(cx, |pane, cx| {
                        match outcome {
                            Ok(outcome) => pane.finish_data(token, &flag, outcome),
                            Err(_) => pane.finish_lost(token),
                        }
                        cx.notify();
                    });
                }
            });
        })
        .detach();
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
        self.cancel_flag = Arc::new(AtomicBool::new(false));
        match kind {
            RunKind::Structure => self.structure_status = Status::Running,
            RunKind::Data => self.data_status = Status::Running,
        }
        token
    }

    fn cancel(&mut self) {
        if !self.running {
            return;
        }
        self.cancel_flag.store(true, Ordering::SeqCst);
        cancel_compare(&self.services.tasks, OWNER, &self.run_id);
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
        if report.refused.is_some() {
            self.structure_status = Status::Refused;
            self.structure = Some(report);
            return;
        }
        self.selected.clear();
        for table in &report.tables {
            self.selected.insert(table.name.clone(), table.on_both);
        }
        self.structure_status = if report.incomplete { Status::Incomplete } else { Status::Ready };
        self.structure = Some(report);
    }

    fn install_data(&mut self, report: DataReport) {
        if report.refused.is_some() {
            self.data_status = Status::Refused;
        } else if report.incomplete {
            self.data_status = Status::Incomplete;
        } else {
            self.data_status = Status::Ready;
        }
        self.data = Some(report);
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
        let lines = script.lines().count();
        self.place_prompt = if self.locale == Locale::En {
            format!("Put a {lines}-line write script into the query editor? It is not executed.")
        } else {
            format!("将把 {lines} 行写操作脚本放入查询编辑器，不会执行。确认放入？")
        };
        self.pending_place = Some(kind);
    }

    fn confirm_place(&mut self) {
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
        if engine != "postgres" && engine != "sqlserver" {
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
        let mut lines = vec![self.text("compare.dialog.title")];
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
            lines.push(self.text("compare.dialog.comparing"));
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
                lines.push(self.text(self.diff_filter.key()));
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
        tables: Vec::new(),
    }
}

fn empty_data(refused: Option<String>) -> DataReport {
    DataReport { refused, script: String::new(), incomplete: false, notes: Vec::new(), diffs: Vec::new() }
}

impl Render for ComparePane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let m = kit::Metrics::of(cx);
        let entity = cx.entity();
        let t = |this: &Self, key: &str| SharedString::from(this.text(key));

        let viewport = f32::from(window.viewport_size().width);
        let stacked = sides_stacked(viewport);
        let page_pad = m.page_pad_for(viewport);
        let baseline_card = self.side_card(true, window, cx);
        let target_card = self.side_card(false, window, cx);
        self.schema_reload = false;
        let swap = kit::icon_button("compare-swap", IconName::ArrowRightLeft, t(self, "compare.action.swap"), compare_act("compare-swap", &entity));
        // 窄窗口里两侧竖排：并排会把每侧压到只剩图标；竖排后卡片各占整行，交换按钮居中（R6 900/640）。
        let sides = if stacked {
            div()
                .flex()
                .flex_col()
                .gap(px(m.pane_pad()))
                .child(div().w_full().min_w(px(0.)).child(baseline_card))
                .child(div().flex().justify_center().child(swap))
                .child(div().w_full().min_w(px(0.)).child(target_card))
        } else {
            div()
                .flex()
                .gap(px(m.pane_pad()))
                .items_start()
                .child(div().flex_1().flex_basis(px(0.)).min_w(px(0.)).child(baseline_card))
                .child(div().flex_none().pt(px(m.header_h() / 2.)).child(swap))
                .child(div().flex_1().flex_basis(px(0.)).min_w(px(0.)).child(target_card))
        };

        let mut actions = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .child(kit::primary_button("compare-structure", Some(IconName::Columns3), t(self, "compare.dialog.compareStructure"), compare_act("compare-structure", &entity)))
            .child(kit::secondary_button("compare-data", Some(IconName::Rows3), t(self, "compare.dialog.compareData"), compare_act("compare-data", &entity)));
        if self.running {
            actions = actions
                .child(kit::tool_button("compare-cancel", Some(IconName::X), t(self, "common.cancel"), compare_act("compare-cancel", &entity)))
                .child(kit::inline_loading(t(self, "compare.dialog.comparing"), &m, &p));
        } else {
            actions = actions.child(div().invisible().child(kit::tool_button("compare-cancel", None, t(self, "common.cancel"), compare_act("compare-cancel", &entity))));
        }

        let mut body = div().flex().flex_col().gap(px(m.section_gap())).child(sides).child(actions);
        // 正在比较且还没有报告：给骨架占位与一条局部加载，不让详情区空着。
        if self.running && self.structure.is_none() && self.data.is_none() {
            body = body.child(
                kit::card(&p)
                    .p(px(m.card_pad()))
                    .flex()
                    .flex_col()
                    .gap(px(m.pane_pad()))
                    .child(kit::inline_loading(t(self, "compare.dialog.comparing"), &m, &p))
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
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(Radius::PANEL))
                    .bg(p.warn_soft)
                    .text_color(p.warn)
                    .child(kit::icon(IconName::TriangleAlert, 15., p.warn))
                    .child(div().flex_1().min_w(px(180.)).text_size(px(TypeScale::SM)).child(SharedString::from(self.place_prompt.clone())))
                    .child(kit::secondary_button("compare-place-cancel", None, t(self, "common.cancel"), compare_act("compare-place-cancel", &entity)))
                    .child(kit::primary_button(
                        "compare-place-confirm",
                        Some(IconName::Check),
                        SharedString::from(if self.locale == Locale::En { "Confirm" } else { "确认放入" }),
                        compare_act("compare-place-confirm", &entity),
                    )),
            );
        }

        if self.show_empty_guide() {
            // 初始指导：说清对比产出什么，并把主操作直接放进空态（38px 内缩，与标题对齐）。
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

        if let Some(report) = self.structure.clone() {
            let mut card = kit::card(&p).p(px(m.card_pad())).flex().flex_col().gap(px(m.pane_pad()));
            let can_place = report.refused.is_none() && self.structure_status != Status::Cancelled && !report.script.trim().is_empty();
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(kit::title(t(self, "compare.dialog.structureScript"), TypeScale::LG, &p))
                    .when(can_place, |this| {
                        this.child(kit::primary_button("compare-place:structure", Some(IconName::SquareTerminal), t(self, "compare.dialog.placeToQuery"), compare_act("compare-place:structure", &entity)))
                    }),
            );
            if let Some(refused) = &report.refused {
                card = card.child(note_banner(IconName::Ban, refused.clone(), p.warn, p.warn_soft));
            } else {
                card = card.child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(stat(self.text("compare.dialog.baseline"), report.only_baseline.len(), p.ok, &p))
                        .child(stat(self.text("compare.dialog.target"), report.only_target.len(), p.danger, &p))
                        .child(stat(self.text("ui.compare.changed"), report.changed.len(), p.warn, &p)),
                );
                card = card.child(self.diff_toolbar(&report, &entity, &p, &m, window, cx));
                let mut diffs = div().flex().flex_col().gap(px(2.));
                for item in report.only_baseline.iter().filter(|item| self.shows_diff(DiffFilter::Baseline, &item.name)) {
                    diffs = diffs.child(diff_row("+", fill(&self.text("compare.dialog.onlyBaseline"), &[("{kind}", &item.kind), ("{name}", &item.name)]), p.ok, &p));
                }
                for item in report.only_target.iter().filter(|item| self.shows_diff(DiffFilter::Target, &item.name)) {
                    diffs = diffs.child(diff_row("−", fill(&self.text("compare.dialog.onlyTarget"), &[("{kind}", &item.kind), ("{name}", &item.name)]), p.danger, &p));
                }
                for item in report.changed.iter().filter(|item| self.shows_diff(DiffFilter::Changed, &item.name)) {
                    diffs = diffs.child(diff_row("~", format!("{} {} · {}", item.kind, item.name, item.detail), p.warn, &p));
                }
                card = card.child(diffs);
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
                        .gap(px(6.))
                        .child(kit::tool_button("compare-select-all", Some(IconName::SquareCheck), t(self, "compare.action.selectAll"), compare_act("compare-select-all", &entity)))
                        .child(kit::tool_button("compare-clear", Some(IconName::Square), t(self, "compare.action.clear"), compare_act("compare-clear", &entity)));
                    for table in report.tables.iter().filter(|table| name_matches(&table.name, &self.diff_query)) {
                        let on = self.selected.get(&table.name).copied().unwrap_or(false);
                        let mark = if on { "on" } else { "off" };
                        tables = tables.child(
                            chip(SharedString::from(format!("compare-toggle:{}", table.name)), SharedString::from(format!("{mark} {}", table.name)), SharedString::from(table.name.clone()), on, Some(if on { IconName::SquareCheck } else { IconName::Square }), &p, m)
                                .on_click(compare_act(format!("compare-toggle:{}", table.name), &entity)),
                        );
                    }
                    card = card.child(div().flex().flex_col().gap(px(6.)).child(kit::eyebrow(t(self, "compare.dialog.dataTitle"), &p)).child(tables));
                    for table in report.tables.iter().filter(|table| table.needs_key_choice) {
                        let mut keys = div().flex().flex_wrap().items_center().gap(px(6.)).child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(format!("{} · {}", table.name, self.text("compare.dialog.chooseKey")))));
                        for key in &table.keys {
                            let chosen = self.chosen_key.get(&table.name).is_some_and(|name| name == &key.name);
                            let mark = if chosen { "chosen" } else { "key" };
                            let id = format!("compare-key:{}|{}", table.name, key.name);
                            keys = keys.child(
                                chip(SharedString::from(id.clone()), SharedString::from(format!("{mark} {} {}", table.name, key.name)), SharedString::from(format!("{} ({})", key.name, key.columns.join(", "))), chosen, Some(IconName::Key), &p, m)
                                    .on_click(compare_act(id, &entity)),
                            );
                        }
                        card = card.child(keys);
                    }
                }
                if self.structure_status != Status::Cancelled && !report.script.trim().is_empty() {
                    card = card.child(
                        div()
                            .flex()
                            .justify_end()
                            .child(kit::tool_button("compare-copy:structure", Some(IconName::Copy), t(self, "compare.script.copy"), compare_act("compare-copy:structure", &entity))),
                    );
                    card = card.child(self.script_view("compare-structure-script", &report.script, window, cx, &p));
                }
            }
            body = body.child(card);
        }

        if let Some(report) = self.data.clone() {
            let can_place = report.refused.is_none() && self.data_status != Status::Cancelled && !report.script.trim().is_empty();
            let mut card = kit::card(&p).p(px(m.card_pad())).flex().flex_col().gap(px(m.pane_pad())).child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(kit::title(t(self, "compare.dialog.dataTitle"), TypeScale::LG, &p))
                    .when(can_place, |this| {
                        this.child(kit::primary_button("compare-place:data", Some(IconName::SquareTerminal), t(self, "compare.dialog.dataDisclosure"), compare_act("compare-place:data", &entity)))
                    }),
            );
            if let Some(refused) = &report.refused {
                card = card.child(note_banner(IconName::Ban, refused.clone(), p.warn, p.warn_soft));
            } else {
                if report.incomplete || self.data_status == Status::Incomplete {
                    card = card.child(note_banner(IconName::TriangleAlert, self.text("compare.dialog.dataScriptIncomplete"), p.warn, p.warn_soft));
                }
                let mut diffs = div().flex().flex_col().gap(px(2.));
                for diff in &report.diffs {
                    let key = if diff.key.is_empty() { String::new() } else { format!(" · {} {}", self.text("compare.dialog.key"), diff.key) };
                    diffs = diffs.child(diff_row("•", format!("{} {}{key}", diff.table, diff.detail), p.info, &p));
                }
                card = card.child(diffs);
                for note in &report.notes {
                    card = card.child(kit::muted(SharedString::from(note.clone()), &p));
                }
                if self.data_status != Status::Cancelled && !report.script.trim().is_empty() {
                    card = card.child(
                        div().flex().justify_end().child(kit::tool_button("compare-copy:data", Some(IconName::Copy), t(self, "compare.script.copy"), compare_act("compare-copy:data", &entity))),
                    );
                    card = card.child(self.script_view("compare-data-script", &report.script, window, cx, &p));
                }
            }
            body = body.child(card);
        } else {
            body = body.child(div().hidden().child(kit::tool_button("compare-place:data", None, t(self, "compare.dialog.dataDisclosure"), compare_act("compare-place:data", &entity))));
        }
        if self.structure.is_none() {
            body = body.child(div().hidden().child(kit::tool_button("compare-place:structure", None, t(self, "compare.dialog.placeToQuery"), compare_act("compare-place:structure", &entity))));
        }
        // 带入的上下文或缺失条件：说明这次对比用的是哪个对象，缺的是哪一侧。
        let context_note = (!self.context_note.is_empty()).then(|| {
            div()
                .id("compare-context")
                .text_size(px(TypeScale::SM))
                .text_color(p.text_muted)
                .child(SharedString::from(self.context_note.clone()))
        });

        div()
            .id("compare-pane")
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div().id("compare-title").tooltip_text(t(self, "compare.dialog.subtitle")).child(
                    kit::page_header(t(self, "compare.dialog.title"), &[], &m, &p)
                        .child(div().flex_1())
                        .child(kit::tool_button("compare-back", Some(IconName::ArrowLeft), t(self, "compare.back"), compare_act("compare-back", &entity))),
                ),
            )
            .child(
                div()
                    .id("compare-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .child(
                        div()
                            .max_w(px(1080.))
                            .mx_auto()
                            .px(px(page_pad))
                            .py(px(m.page_pad_y))
                            .flex()
                            .flex_col()
                            .gap(px(m.section_gap()))
                            .when_some(context_note, |this, note| this.child(note))
                            .child(body),
                    ),
            )
    }
}

impl ComparePane {
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
        let mut card = kit::card(&p)
            .id(SharedString::from(format!("compare-side:{which}")))
            .test_support()
            .p(px(m.card_pad()))
            .flex()
            .flex_col()
            .gap(px(m.pane_pad()))
            .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(kit::pill(SharedString::from(title.clone()), if baseline { p.ok } else { p.info }, if baseline { p.ok_soft } else { p.accent_soft }))
                .child(kit::muted(SharedString::from(self.text("compare.dialog.connection")), &p))
                .child(div().ml_auto().child(kit::icon(IconName::ChevronDown, 14., p.text_faint))),
        );
        let mut connections = kit::sunken(&p)
            .id(SharedString::from(format!("compare-connections:{which}")))
            .flex()
            .flex_col()
            .max_h(px(m.control_h * 4.5))
            .overflow_y_scroll();
        if self.choices.is_empty() {
            // 清单还在读：骨架占位，不先写一句“选择连接”；读完了还是空才提示选连接。
            connections = if self.choices_loading {
                connections.child(kit::skeleton_state(3, &m, &p))
            } else {
                connections.child(kit::muted(SharedString::from(self.text("compare.dialog.chooseConnection")), &p))
            };
        }
        for choice in self.choices.clone() {
            let id = format!("compare-{which}:{}", choice.id);
            let selected = side_connection == choice.id;
            connections = connections.child(
                chip(SharedString::from(id.clone()), SharedString::from(format!("{title} {}", choice.name)), SharedString::from(choice.name.clone()), selected, Some(IconName::Database), &p, m)
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
                    .flex()
                    .flex_col()
                    .max_h(px(m.control_h * 4.5))
                    .overflow_y_scroll();
                for name in &choice.databases {
                    let id = format!("compare-database:{which}:{name}");
                    list = list.child(
                        chip(SharedString::from(id.clone()), SharedString::from(format!("{title} {database_label} {name}")), SharedString::from(name.clone()), side_database == *name, Some(IconName::Database), &p, m)
                            .on_click(compare_act(id, &entity)),
                    );
                }
                card = card.child(div().flex().flex_col().gap(px(6.)).child(kit::eyebrow(SharedString::from(database_label), &p)).child(list));
            }
            if choice.engine == "postgres" || choice.engine == "sqlserver" {
                let schema_label = self.text("compare.dialog.chooseSchema");
                match self.schemas.get(&schema_key(&side_connection, &side_database)).cloned() {
                    Some(SchemaState::Ready(names)) => {
                        let mut list = kit::sunken(&p)
                            .id(SharedString::from(format!("compare-schemas:{which}")))
                            .flex()
                            .flex_col()
                            .max_h(px(m.control_h * 4.5))
                            .overflow_y_scroll();
                        for name in names {
                            let id = format!("compare-schema:{which}:{name}");
                            list = list.child(
                                chip(SharedString::from(id.clone()), SharedString::from(format!("{title} {schema_label} {name}")), SharedString::from(name.clone()), side_schema == name, Some(IconName::Layers), &p, m)
                                    .on_click(compare_act(id, &entity)),
                            );
                        }
                        card = card.child(div().flex().flex_col().gap(px(6.)).child(kit::eyebrow(SharedString::from(schema_label), &p)).child(list));
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
                                .gap(px(6.))
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
            .gap(px(m.pane_pad()))
            .child(kit::segmented_sized(filters, p, m.control_h))
            .child(div().w(px(200.)).child(kit::text_input("compare-filter-search", &search, self.text("compare.filter.search")).small()))
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
            .h(px(200.))
            .overflow_hidden()
            .child(
                kit::sunken(p).size_full().overflow_hidden().child(
                    Editor::new(&state).h(relative(1.)).bordered(false).readonly(true).aria_label(SharedString::from(id)),
                ),
            )
            .into_any_element()
    }
}

fn compare_act(id: impl Into<String>, entity: &Entity<ComparePane>) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
    let id = id.into();
    let entity = entity.clone();
    move |_, _, cx| {
        let id = id.clone();
        entity.update(cx, |pane, cx| pane.command(&id, cx));
    }
}

/// 可选中的小块：显示文字与无障碍名称分开，测试按名称找。
fn chip(id: SharedString, label: SharedString, text: SharedString, selected: bool, icon: Option<IconName>, p: &Palette, m: kit::Metrics) -> gpui_kit::base::ObservedElement<gpui_kit::Stateful<gpui_kit::Div>> {
    kit::interactive_states(div().id(id), p, false)
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(label)
        .aria_selected(selected)
        .h(px(m.tree_row_h))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(Radius::CONTROL))
        .border_1()
        .text_size(px(TypeScale::SM))
        .when(selected, |this| this.border_color(p.accent).bg(p.accent_soft).text_color(p.accent_text))
        .when(!selected, |this| this.border_color(p.line).hover(|style| style.border_color(p.line_strong).bg(p.hover)))
        .when_some(icon, |this, icon| this.child(kit::icon(icon, 13., if selected { p.accent_text } else { p.text_faint })))
        .child(text)
}

fn stat(label: String, value: usize, tone: gpui_kit::Hsla, p: &Palette) -> AnyElement {
    div()
        .flex_1()
        .p(px(10.))
        .rounded(px(Radius::PANEL))
        .bg(p.chrome)
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(div().text_size(px(TypeScale::XL)).font_weight(gpui_kit::FontWeight::SEMIBOLD).text_color(tone).child(SharedString::from(value.to_string())))
        .child(div().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(SharedString::from(label)))
        .into_any_element()
}

fn diff_row(mark: &'static str, text: String, tone: gpui_kit::Hsla, p: &Palette) -> AnyElement {
    div()
        .flex()
        .gap(px(8.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(Radius::CONTROL))
        .hover(|style| style.bg(p.hover))
        .child(div().w(px(12.)).font_weight(gpui_kit::FontWeight::BOLD).text_color(tone).child(mark))
        .child(div().flex_1().min_w(px(0.)).text_size(px(TypeScale::SM)).child(SharedString::from(text)))
        .into_any_element()
}

fn note_banner(icon: IconName, text: String, fg: gpui_kit::Hsla, bg: gpui_kit::Hsla) -> AnyElement {
    div()
        .px(px(12.))
        .py(px(8.))
        .flex()
        .gap(px(8.))
        .rounded(px(Radius::PANEL))
        .bg(bg)
        .text_color(fg)
        .text_size(px(TypeScale::SM))
        .child(kit::icon(icon, 15., fg))
        .child(div().flex_1().min_w(px(0.)).child(SharedString::from(text)))
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
        pane.structure = Some(empty_structure(Some(message)));
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
        pane.structure = Some(report);
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
        pane.structure = Some(report);
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
            pane.structure = Some(empty_structure(None));
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
        let (services, target_file) = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("sqlite compare setup");
        cx.update(gpui_kit::init);
        let (window, view) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds { origin: Point::default(), size: size(px(1200.), px(800.)) };
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
                        pane.structure = Some(StructureReport {
                            refused: None,
                            script: String::new(),
                            incomplete: true,
                            notes: Vec::new(),
                            only_baseline: Vec::new(),
                            only_target: Vec::new(),
                            changed: Vec::new(),
                            tables: vec![crate::compare::TableChoice {
                                name: "orders".into(),
                                on_both: true,
                                needs_key_choice: true,
                                keys: vec![
                                    crate::compare::KeyChoice { name: "uk_code".into(), columns: vec!["code".into()], primary: false },
                                    crate::compare::KeyChoice { name: "uk_email".into(), columns: vec!["email".into()], primary: false },
                                ],
                            }],
                        });
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
mod live;
