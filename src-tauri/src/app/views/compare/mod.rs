//! 结构比较与数据比较。脚本只放入查询编辑器，不执行。取消优先于迟到结果。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gpui_kit::{
    Context, Entity, FocusHandle, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div, px,
};

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
    schema_draft: BTreeMap<String, String>,
    schema_focus_side: Option<String>,
    baseline_schema_focus: Option<FocusHandle>,
    target_schema_focus: Option<FocusHandle>,
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
            schema_draft: BTreeMap::new(),
            schema_focus_side: None,
            baseline_schema_focus: None,
            target_schema_focus: None,
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
        } else if let Some(name) = id.strip_prefix("compare-schema:target:") {
            self.target.schema = name.to_owned();
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
            self.place_structure();
        } else if id == "compare-place:data" {
            self.place_data();
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
        slot.connection_id = choice.id;
        slot.database = database;
        slot.schema.clear();
        self.ensure_schemas(baseline, cx);
    }

    fn set_database(&mut self, baseline: bool, name: &str, cx: &mut Context<Self>) {
        let slot = if baseline { &mut self.baseline } else { &mut self.target };
        slot.database = name.to_owned();
        slot.schema.clear();
        self.ensure_schemas(baseline, cx);
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
                                }
                                Err(message) => {
                                    pane.choices.clear();
                                    pane.error = message;
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locale = self.locale;
        let entity = cx.entity();
        if self.baseline_schema_focus.is_none() {
            self.baseline_schema_focus = Some(cx.focus_handle());
        }
        if self.target_schema_focus.is_none() {
            self.target_schema_focus = Some(cx.focus_handle());
        }
        let summary = SharedString::from(self.lines().join("\n"));
        let controls = self.scope_controls();
        let baseline_manual = self.manual_reason(true);
        let target_manual = self.manual_reason(false);
        let baseline_focus = self.baseline_schema_focus.clone().unwrap();
        let target_focus = self.target_schema_focus.clone().unwrap();
        let baseline_draft = self.schema_draft.get("baseline").cloned().unwrap_or_default();
        let target_draft = self.schema_draft.get("target").cloned().unwrap_or_default();
        let mut column = div().id("compare-pane").flex().flex_col().gap(px(4.));
        column = column.child(button("compare-structure", "compare.dialog.compareStructure", entity.clone(), locale));
        column = column.child(button("compare-data", "compare.dialog.compareData", entity.clone(), locale));
        column = column.child(button("compare-cancel", "common.cancel", entity.clone(), locale));
        column = column.child(button("compare-place:structure", "compare.dialog.placeToQuery", entity.clone(), locale));
        column = column.child(button("compare-place:data", "compare.dialog.dataDisclosure", entity.clone(), locale));
        for control in controls {
            column = column.child(command_button(control.0, control.1, entity.clone()));
        }
        if let Some(reason) = baseline_manual {
            column = column.child(schema_box("baseline", &baseline_draft, &reason, baseline_focus, entity.clone(), locale));
        }
        if let Some(reason) = target_manual {
            column = column.child(schema_box("target", &target_draft, &reason, target_focus, entity.clone(), locale));
        }
        column.child(summary)
    }
}

impl ComparePane {
    fn scope_controls(&self) -> Vec<(SharedString, SharedString)> {
        let mut controls = Vec::new();
        for choice in &self.choices {
            controls.push((
                format!("compare-baseline:{}", choice.id).into(),
                format!("{} {}", self.text("compare.dialog.baseline"), choice.name).into(),
            ));
            controls.push((
                format!("compare-target:{}", choice.id).into(),
                format!("{} {}", self.text("compare.dialog.target"), choice.name).into(),
            ));
        }
        self.push_database_controls(&mut controls, true);
        self.push_database_controls(&mut controls, false);
        self.push_schema_controls(&mut controls, true);
        self.push_schema_controls(&mut controls, false);
        if let Some(report) = &self.structure {
            if report.refused.is_none() {
                for table in &report.tables {
                    let on = self.selected.get(&table.name).copied().unwrap_or(false);
                    let mark = if on { "on" } else { "off" };
                    controls.push((format!("compare-toggle:{}", table.name).into(), format!("{mark} {}", table.name).into()));
                    if table.needs_key_choice {
                        for key in &table.keys {
                            let chosen = self.chosen_key.get(&table.name).is_some_and(|name| name == &key.name);
                            let mark = if chosen { "chosen" } else { "key" };
                            controls.push((
                                format!("compare-key:{}|{}", table.name, key.name).into(),
                                format!("{mark} {} {}", table.name, key.name).into(),
                            ));
                        }
                    }
                }
            }
        }
        controls
    }

    fn push_database_controls(&self, controls: &mut Vec<(SharedString, SharedString)>, baseline: bool) {
        let side = if baseline { &self.baseline } else { &self.target };
        let Some(choice) = self.choices.iter().find(|choice| choice.id == side.connection_id) else {
            return;
        };
        let which = if baseline { "baseline" } else { "target" };
        let title = self.text(if baseline { "compare.dialog.baseline" } else { "compare.dialog.target" });
        let database = self.text("compare.dialog.database");
        for name in &choice.databases {
            controls.push((format!("compare-database:{which}:{name}").into(), format!("{title} {database} {name}").into()));
        }
    }

    fn push_schema_controls(&self, controls: &mut Vec<(SharedString, SharedString)>, baseline: bool) {
        let side = if baseline { &self.baseline } else { &self.target };
        let Some(engine) = self.engine_of(&side.connection_id) else {
            return;
        };
        if engine != "postgres" && engine != "sqlserver" {
            return;
        }
        let Some(SchemaState::Ready(names)) = self.schemas.get(&schema_key(&side.connection_id, &side.database)) else {
            return;
        };
        let which = if baseline { "baseline" } else { "target" };
        let title = self.text(if baseline { "compare.dialog.baseline" } else { "compare.dialog.target" });
        let schema = self.text("compare.dialog.chooseSchema");
        for name in names {
            controls.push((format!("compare-schema:{which}:{name}").into(), format!("{title} {schema} {name}").into()));
        }
    }

    fn manual_reason(&self, baseline: bool) -> Option<String> {
        let side = if baseline { &self.baseline } else { &self.target };
        let engine = self.engine_of(&side.connection_id)?;
        if engine != "postgres" && engine != "sqlserver" {
            return None;
        }
        match self.schemas.get(&schema_key(&side.connection_id, &side.database)) {
            Some(SchemaState::Manual(reason)) => Some(reason.clone()),
            _ => None,
        }
    }
}

fn button(id: &'static str, key: &'static str, entity: Entity<ComparePane>, locale: Locale) -> impl IntoElement {
    let label = SharedString::from(i18n::text(locale, key));
    div()
        .id(id)
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .child(label)
        .on_click(move |_, _, cx| {
            entity.update(cx, |pane, cx| pane.command(id, cx));
        })
}

fn command_button(id: SharedString, label: SharedString, entity: Entity<ComparePane>) -> impl IntoElement {
    let command = id.clone();
    div()
        .id(id)
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .child(label)
        .on_click(move |_, _, cx| {
            let command = command.clone();
            entity.update(cx, |pane, cx| pane.command(&command, cx));
        })
}

fn schema_box(side: &'static str, value: &str, reason: &str, focus: FocusHandle, entity: Entity<ComparePane>, locale: Locale) -> impl IntoElement {
    let placeholder = i18n::text(locale, "compare.dialog.schemaManualPlaceholder");
    let label = if reason.is_empty() {
        SharedString::from(format!("{placeholder} {value}"))
    } else {
        SharedString::from(format!("{reason} {placeholder} {value}"))
    };
    div()
        .id(SharedString::from(format!("compare-schema-input:{side}")))
        .test_support()
        .track_focus(&focus)
        .min_h(px(28.))
        .w(px(280.))
        .role(gpui_kit::Role::TextInput)
        .aria_label(label.clone())
        .child(label)
        .on_mouse_down(gpui_kit::MouseButton::Left, {
            let entity = entity.clone();
            let focus = focus.clone();
            move |_, window, cx| {
                window.focus(&focus, cx);
                entity.update(cx, |pane, cx| {
                    pane.schema_focus_side = Some(side.to_owned());
                    cx.notify();
                });
            }
        })
        .on_key_down(move |event: &gpui_kit::KeyDownEvent, _, cx| {
            let action = schema_key_action(event);
            entity.update(cx, |pane, cx| {
                if pane.schema_focus_side.as_deref() != Some(side) {
                    return;
                }
                match action {
                    SchemaKey::Ignore => {}
                    SchemaKey::Backspace => {
                        pane.schema_draft.entry(side.to_owned()).or_default().pop();
                        cx.notify();
                    }
                    SchemaKey::Type(text) => {
                        pane.schema_draft.entry(side.to_owned()).or_default().push_str(&text);
                        cx.notify();
                    }
                    SchemaKey::Commit => {
                        let name = pane.schema_draft.get(side).cloned().unwrap_or_default();
                        pane.command(&format!("compare-schema:{side}:{name}"), cx);
                    }
                }
            });
        })
}

enum SchemaKey {
    Ignore,
    Backspace,
    Type(String),
    Commit,
}

fn schema_key_action(event: &gpui_kit::KeyDownEvent) -> SchemaKey {
    if event.keystroke.modifiers.control || event.keystroke.modifiers.platform || event.keystroke.modifiers.alt {
        return SchemaKey::Ignore;
    }
    match event.keystroke.key.as_str() {
        "backspace" => SchemaKey::Backspace,
        "enter" => SchemaKey::Commit,
        "escape" | "tab" | "up" | "down" | "left" | "right" => SchemaKey::Ignore,
        _ => event.keystroke.key_char.clone().filter(|text| !text.is_empty()).map(SchemaKey::Type).unwrap_or(SchemaKey::Ignore),
    }
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
}

#[cfg(test)]
mod workbench {
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext as _, Point, TestAppContext, WindowBounds, WindowOptions, px, size};

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
            window.click("compare-schema:baseline:public", cx);
            window.click("compare-schema-input:target", cx);
            window.input("dbo", cx);
            window.press("enter", cx);
            window.click("compare-key:orders|uk_email", cx);
            window.click("compare-toggle:orders", cx);
            view.update(cx, |pane, _| {
                let text = pane.lines().join("\n");
                assert!(text.contains("schema-on public"), "{text}");
                assert!(text.contains("schema-manual dbo"), "{text}");
                assert!(text.contains("chosen orders uk_email"), "{text}");
                assert!(text.contains("off orders"), "{text}");
                assert!(pane.refuse_local().is_none(), "{text} {:?}", pane.refuse_local());
                assert!(!pane.structure_ready());
                assert!(pane.structure.as_ref().is_some_and(|report| report.script.is_empty()));
            });
            window.click("compare-toggle:orders", cx);
            window.click("compare-database:baseline:warehouse", cx);
            view.update(cx, |pane, _| {
                let text = pane.lines().join("\n");
                assert!(text.contains("on orders"), "{text}");
                assert!(text.contains("数据库 warehouse"), "{text}");
                assert!(pane.refuse_local().is_some_and(|message| message.contains("不生成脚本")), "{text}");
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
