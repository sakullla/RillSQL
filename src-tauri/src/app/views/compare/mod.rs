//! 结构比较与数据比较。脚本只放入查询编辑器，不执行。取消优先于迟到结果。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div, px,
};

use crate::ui::kit::{self, Palette};

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

#[derive(Clone, Debug, PartialEq, Eq)]
enum SchemaState {
    Loading,
    Ready(Vec<String>),
    Manual(String),
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
                self.context_note = self.carried_note();
                if self.choices.is_empty() {
                    // 首次打开还没有连接清单：读完后再落两侧，期间保留上下文说明。
                    if !self.choices_loading {
                        self.refresh_choices(cx);
                    }
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

    fn carried_note(&self) -> String {
        match self.context.as_ref() {
            Some(context) => fill(&self.text("compare.context.carried"), &[("{path}", &context.path)]),
            None => self.text("compare.context.none"),
        }
    }

    /// 把带入的上下文落到两侧：当前对象放在对照侧，基准侧优先取另一个已打开连接。
    /// 只有一侧可用时保留说明文字，提示还缺一个已打开的连接。
    fn apply_context(&mut self, cx: &mut Context<Self>) {
        let Some(context) = self.context.clone() else { return };
        self.context_note = self.carried_note();
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
        let changed = self.target.connection_id != context.connection_id
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
            if self.baseline.connection_id != other.id || self.baseline.database != other_database {
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
        if id == "compare-object" {
            // 外壳负责把完整对象目标交给 open_context；这里只刷新连接清单。
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
        } else if id == "compare-place:structure" {
            self.ask_place(RunKind::Structure);
        } else if id == "compare-place:data" {
            self.ask_place(RunKind::Data);
        } else if id == "compare-place-confirm" {
            self.confirm_place();
        } else if id == "compare-place-cancel" {
            self.pending_place = None;
            self.place_prompt.clear();
        }
        cx.notify();
    }

    fn pick(&mut self, baseline: bool, connection_id: &str, cx: &mut Context<Self>) {
        let Some(choice) = self.choices.iter().find(|choice| choice.id == connection_id).cloned() else {
            self.error = self.text("compare.gates.sideNotOpen");
            return;
        };
        let database = if choice.engine == "sqlite" {
            "main".to_owned()
        } else {
            choice.databases.first().cloned().unwrap_or_default()
        };
        let slot = if baseline { &mut self.baseline } else { &mut self.target };
        let changed = slot.connection_id != choice.id || slot.database != database;
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
        self.error.clear();
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
                                    pane.error.clear();
                                    pane.choices_loading = false;
                                    // 连接清单到达后再把带入的上下文落到两侧。
                                    pane.apply_context(cx);
                                }
                                Err(message) => {
                                    pane.choices.clear();
                                    pane.error = message;
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
            self.error = self.text("compare.gates.busy");
            self.structure = None;
            self.structure_status = Status::Refused;
            return;
        }
        self.error.clear();
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
            self.error = self.text("compare.gates.busy");
            self.data = None;
            self.data_status = Status::Refused;
            return;
        }
        self.error.clear();
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
    }

    fn finish_structure(&mut self, token: TaskToken, flag: &AtomicBool, outcome: TaskOutcome<StructureReport>) {
        if self.token != Some(token) {
            return;
        }
        self.running = false;
        if flag.load(Ordering::SeqCst) || matches!(outcome, TaskOutcome::Cancelled { .. }) || compare_cancelled(&self.run_id) {
            self.structure = None;
            self.structure_status = Status::Cancelled;
            self.error = self.text("compare.status.cancelled");
            return;
        }
        let TaskOutcome::Current { value, .. } = outcome else {
            self.structure = None;
            self.structure_status = Status::Stale;
            self.error = self.text("compare.status.stale");
            return;
        };
        self.install_structure(value);
    }

    fn finish_data(&mut self, token: TaskToken, flag: &AtomicBool, outcome: TaskOutcome<DataReport>) {
        if self.token != Some(token) {
            return;
        }
        self.running = false;
        if flag.load(Ordering::SeqCst) || matches!(outcome, TaskOutcome::Cancelled { .. }) || compare_cancelled(&self.run_id) {
            self.data = None;
            self.data_status = Status::Cancelled;
            self.error = self.text("compare.status.cancelled");
            return;
        }
        let TaskOutcome::Current { value, .. } = outcome else {
            self.data = None;
            self.data_status = Status::Stale;
            self.error = self.text("compare.status.stale");
            return;
        };
        self.install_data(value);
    }

    fn finish_lost(&mut self, token: TaskToken) {
        if self.token != Some(token) {
            return;
        }
        self.running = false;
        self.error = self.text("compare.status.failed");
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
        let mut lines = vec![self.text("compare.dialog.title"), self.text("compare.dialog.subtitle")];
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
        if !self.error.is_empty() {
            lines.push(self.error.clone());
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
                for item in &report.only_baseline {
                    lines.push(fill(&self.text("compare.dialog.onlyBaseline"), &[("{kind}", &item.kind), ("{name}", &item.name)]));
                }
                for item in &report.only_target {
                    lines.push(fill(&self.text("compare.dialog.onlyTarget"), &[("{kind}", &item.kind), ("{name}", &item.name)]));
                }
                for item in &report.changed {
                    lines.push(format!("{} {} {}", item.kind, item.name, item.detail));
                }
                for note in &report.notes {
                    lines.push(note.clone());
                }
                if report.tables.is_empty() {
                    lines.push(self.text("compare.dialog.noTables"));
                }
                for table in &report.tables {
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

fn schema_key(id: &str, database: &str) -> String {
    format!("{id}\u{0}{database}")
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
        let entity = cx.entity();
        let mono = cx.theme().mono_font_family.clone();
        let t = |this: &Self, key: &str| SharedString::from(this.text(key));

        let baseline_card = self.side_card(true, window, cx);
        let target_card = self.side_card(false, window, cx);
        let sides = div()
            .flex()
            .gap(px(14.))
            .items_start()
            .child(div().flex_1().flex_basis(px(0.)).min_w(px(0.)).child(baseline_card))
            .child(div().flex_none().pt(px(40.)).child(kit::icon(IconName::ArrowLeft, 18., p.text_faint)))
            .child(div().flex_1().flex_basis(px(0.)).min_w(px(0.)).child(target_card));

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
                .child(div().flex().items_center().gap(px(6.)).text_size(px(12.)).text_color(p.text_muted).child(Spinner::new().xsmall().color(p.accent)).child(t(self, "compare.dialog.comparing")));
        } else {
            actions = actions.child(div().invisible().child(kit::tool_button("compare-cancel", None, t(self, "common.cancel"), compare_act("compare-cancel", &entity))));
        }

        let mut body = div().flex().flex_col().gap(px(18.)).child(sides).child(actions);
        if !self.error.is_empty() {
            body = body.child(note_banner(IconName::CircleAlert, self.error.clone(), p.danger, p.danger_soft));
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
                    .rounded(px(8.))
                    .bg(p.warn_soft)
                    .text_color(p.warn)
                    .child(kit::icon(IconName::TriangleAlert, 15., p.warn))
                    .child(div().flex_1().min_w(px(180.)).text_size(px(12.5)).child(SharedString::from(self.place_prompt.clone())))
                    .child(kit::secondary_button("compare-place-cancel", None, t(self, "common.cancel"), compare_act("compare-place-cancel", &entity)))
                    .child(kit::primary_button(
                        "compare-place-confirm",
                        Some(IconName::Check),
                        SharedString::from(if self.locale == Locale::En { "Confirm" } else { "确认放入" }),
                        compare_act("compare-place-confirm", &entity),
                    )),
            );
        }

        if let Some(report) = self.structure.clone() {
            let mut card = kit::card(&p).p(px(16.)).flex().flex_col().gap(px(12.));
            let can_place = report.refused.is_none() && self.structure_status != Status::Cancelled && !report.script.trim().is_empty();
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(kit::title(t(self, "compare.dialog.structureScript"), 14., &p))
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
                let mut diffs = div().flex().flex_col().gap(px(2.));
                for item in &report.only_baseline {
                    diffs = diffs.child(diff_row("+", fill(&self.text("compare.dialog.onlyBaseline"), &[("{kind}", &item.kind), ("{name}", &item.name)]), p.ok, &p));
                }
                for item in &report.only_target {
                    diffs = diffs.child(diff_row("−", fill(&self.text("compare.dialog.onlyTarget"), &[("{kind}", &item.kind), ("{name}", &item.name)]), p.danger, &p));
                }
                for item in &report.changed {
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
                    let mut tables = div().flex().flex_wrap().gap(px(6.));
                    for table in &report.tables {
                        let on = self.selected.get(&table.name).copied().unwrap_or(false);
                        let mark = if on { "on" } else { "off" };
                        tables = tables.child(
                            chip(SharedString::from(format!("compare-toggle:{}", table.name)), SharedString::from(format!("{mark} {}", table.name)), SharedString::from(table.name.clone()), on, Some(if on { IconName::SquareCheck } else { IconName::Square }), &p)
                                .on_click(compare_act(format!("compare-toggle:{}", table.name), &entity)),
                        );
                    }
                    card = card.child(div().flex().flex_col().gap(px(6.)).child(kit::eyebrow(t(self, "compare.dialog.dataTitle"), &p)).child(tables));
                    for table in report.tables.iter().filter(|table| table.needs_key_choice) {
                        let mut keys = div().flex().flex_wrap().items_center().gap(px(6.)).child(div().text_size(px(12.)).text_color(p.text_muted).child(SharedString::from(format!("{} · {}", table.name, self.text("compare.dialog.chooseKey")))));
                        for key in &table.keys {
                            let chosen = self.chosen_key.get(&table.name).is_some_and(|name| name == &key.name);
                            let mark = if chosen { "chosen" } else { "key" };
                            let id = format!("compare-key:{}|{}", table.name, key.name);
                            keys = keys.child(
                                chip(SharedString::from(id.clone()), SharedString::from(format!("{mark} {} {}", table.name, key.name)), SharedString::from(format!("{} ({})", key.name, key.columns.join(", "))), chosen, Some(IconName::Key), &p)
                                    .on_click(compare_act(id, &entity)),
                            );
                        }
                        card = card.child(keys);
                    }
                }
                if self.structure_status != Status::Cancelled && !report.script.trim().is_empty() {
                    card = card.child(script_block("compare-structure-script", report.script.clone(), &mono, &p));
                }
            }
            body = body.child(card);
        }

        if let Some(report) = self.data.clone() {
            let can_place = report.refused.is_none() && self.data_status != Status::Cancelled && !report.script.trim().is_empty();
            let mut card = kit::card(&p).p(px(16.)).flex().flex_col().gap(px(12.)).child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(kit::title(t(self, "compare.dialog.dataTitle"), 14., &p))
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
                    card = card.child(script_block("compare-data-script", report.script.clone(), &mono, &p));
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
                .text_size(px(12.5))
                .text_color(p.text_muted)
                .child(SharedString::from(self.context_note.clone()))
        });

        div()
            .id("compare-pane")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .max_w(px(1080.))
                    .mx_auto()
                    .px(px(24.))
                    .py(px(20.))
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(12.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(12.))
                                    .child(div().size(px(34.)).rounded(px(9.)).bg(p.accent_soft).flex().items_center().justify_center().child(kit::icon(IconName::GitCompare, 17., p.accent_text)))
                                    .child(div().flex().flex_col().child(kit::title(t(self, "compare.dialog.title"), 16., &p)).child(kit::muted(t(self, "compare.dialog.subtitle"), &p))),
                            )
                            // 对比是独立页面：返回入口把原工作项与焦点一起带回去。
                            .child(kit::tool_button("compare-back", Some(IconName::ArrowLeft), t(self, "compare.back"), compare_act("compare-back", &entity))),
                    )
                    .when_some(context_note, |this, note| this.child(note))
                    .child(body),
            )
    }
}

impl ComparePane {
    fn side_card(&mut self, baseline: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let entity = cx.entity();
        let which = if baseline { "baseline" } else { "target" };
        let title = self.text(if baseline { "compare.dialog.baseline" } else { "compare.dialog.target" });
        let side = if baseline { &self.baseline } else { &self.target };
        let side_connection = side.connection_id.clone();
        let side_database = side.database.clone();
        let side_schema = side.schema.clone();
        let mut card = kit::card(&p).p(px(14.)).flex().flex_col().gap(px(12.)).child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(kit::pill(SharedString::from(title.clone()), if baseline { p.ok } else { p.info }, if baseline { p.ok_soft } else { p.accent_soft }))
                .child(kit::muted(SharedString::from(self.text("compare.dialog.connection")), &p)),
        );
        let mut connections = div().flex().flex_wrap().gap(px(6.));
        if self.choices.is_empty() {
            connections = connections.child(kit::muted(SharedString::from(self.text("compare.dialog.chooseConnection")), &p));
        }
        for choice in self.choices.clone() {
            let id = format!("compare-{which}:{}", choice.id);
            let selected = side_connection == choice.id;
            connections = connections.child(
                chip(SharedString::from(id.clone()), SharedString::from(format!("{title} {}", choice.name)), SharedString::from(choice.name.clone()), selected, Some(IconName::Database), &p)
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
                let mut list = div().flex().flex_wrap().gap(px(6.));
                for name in &choice.databases {
                    let id = format!("compare-database:{which}:{name}");
                    list = list.child(
                        chip(SharedString::from(id.clone()), SharedString::from(format!("{title} {database_label} {name}")), SharedString::from(name.clone()), side_database == *name, Some(IconName::Database), &p)
                            .on_click(compare_act(id, &entity)),
                    );
                }
                card = card.child(div().flex().flex_col().gap(px(6.)).child(kit::eyebrow(SharedString::from(database_label), &p)).child(list));
            }
            if choice.engine == "postgres" || choice.engine == "sqlserver" {
                let schema_label = self.text("compare.dialog.chooseSchema");
                match self.schemas.get(&schema_key(&side_connection, &side_database)).cloned() {
                    Some(SchemaState::Ready(names)) => {
                        let mut list = div().flex().flex_wrap().gap(px(6.));
                        for name in names {
                            let id = format!("compare-schema:{which}:{name}");
                            list = list.child(
                                chip(SharedString::from(id.clone()), SharedString::from(format!("{title} {schema_label} {name}")), SharedString::from(name.clone()), side_schema == name, Some(IconName::Layers), &p)
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
                                .when(!reason.is_empty(), |this| this.child(div().text_size(px(12.)).text_color(p.warn).child(SharedString::from(reason.clone()))))
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
                        card = card.child(div().flex().items_center().gap(px(6.)).text_size(px(12.)).text_color(p.text_muted).child(Spinner::new().xsmall()).child(SharedString::from(self.text("compare.dialog.schemaLoading"))));
                    }
                }
            }
        }
        card.into_any_element()
    }

    fn schema_input(&mut self, side: &'static str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some((state, _)) = self.schema_inputs.get(side) {
            return state.clone();
        }
        let placeholder = SharedString::from(self.text("compare.dialog.schemaManualPlaceholder"));
        let initial = SharedString::from(self.schema_draft.get(side).cloned().unwrap_or_default());
        let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(initial));
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

fn compare_act(id: impl Into<String>, entity: &Entity<ComparePane>) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
    let id = id.into();
    let entity = entity.clone();
    move |_, _, cx| {
        let id = id.clone();
        entity.update(cx, |pane, cx| pane.command(&id, cx));
    }
}

/// 可选中的小块：显示文字与无障碍名称分开，测试按名称找。
fn chip(id: SharedString, label: SharedString, text: SharedString, selected: bool, icon: Option<IconName>, p: &Palette) -> gpui_kit::base::ObservedElement<gpui_kit::Stateful<gpui_kit::Div>> {
    div()
        .id(id)
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(label)
        .aria_selected(selected)
        .h(px(28.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(7.))
        .border_1()
        .text_size(px(12.5))
        .cursor_pointer()
        .when(selected, |this| this.border_color(p.accent).bg(p.accent_soft).text_color(p.accent_text))
        .when(!selected, |this| this.border_color(p.line).hover(|style| style.border_color(p.line_strong).bg(p.hover)))
        .when_some(icon, |this, icon| this.child(kit::icon(icon, 13., if selected { p.accent_text } else { p.text_faint })))
        .child(text)
}

fn stat(label: String, value: usize, tone: gpui_kit::Hsla, p: &Palette) -> AnyElement {
    div()
        .flex_1()
        .p(px(10.))
        .rounded(px(8.))
        .bg(p.chrome)
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(div().text_size(px(20.)).font_weight(gpui_kit::FontWeight::SEMIBOLD).text_color(tone).child(SharedString::from(value.to_string())))
        .child(div().text_size(px(11.5)).text_color(p.text_muted).child(SharedString::from(label)))
        .into_any_element()
}

fn diff_row(mark: &'static str, text: String, tone: gpui_kit::Hsla, p: &Palette) -> AnyElement {
    div()
        .flex()
        .gap(px(8.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(6.))
        .hover(|style| style.bg(p.hover))
        .child(div().w(px(12.)).font_weight(gpui_kit::FontWeight::BOLD).text_color(tone).child(mark))
        .child(div().flex_1().min_w(px(0.)).text_size(px(12.5)).child(SharedString::from(text)))
        .into_any_element()
}

fn note_banner(icon: IconName, text: String, fg: gpui_kit::Hsla, bg: gpui_kit::Hsla) -> AnyElement {
    div()
        .px(px(12.))
        .py(px(8.))
        .flex()
        .gap(px(8.))
        .rounded(px(8.))
        .bg(bg)
        .text_color(fg)
        .text_size(px(12.5))
        .child(kit::icon(icon, 15., fg))
        .child(div().flex_1().min_w(px(0.)).child(SharedString::from(text)))
        .into_any_element()
}

fn script_block(id: &'static str, script: String, mono: &SharedString, p: &Palette) -> AnyElement {
    div()
        .id(id)
        .max_h(px(260.))
        .overflow_y_scroll()
        .p(px(12.))
        .rounded(px(8.))
        .bg(p.chrome)
        .border_1()
        .border_color(p.line)
        .font_family(mono.clone())
        .text_size(px(12.))
        .child(SharedString::from(script))
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
