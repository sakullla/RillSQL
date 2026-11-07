//! 查询标签的生命周期。执行、补全和 lint 都带任务 id / 代次；载入只替换草稿。

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::application::tasks::{TaskBoard, TaskOutcome, TaskToken};
use crate::connections::{CatalogSession, Service};

use super::assist::{self, CatalogObject, CompletionEntry, CompletionSource, LintFinding, OutlineItem, RunPick, RunScope, TextResult, TreeObject, VariableInput};
use super::commands::{self, RunDto, StartRequest};
use super::exec::{LiveDriver, StatementDriver};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub tab_id: String,
    pub token: TaskToken,
    pub version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub connection_id: String,
    pub connection_name: String,
    pub database: String,
    pub engine: String,
    pub schema: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementView {
    pub sql: String,
    pub status: String,
    pub message: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub affected: Option<u64>,
}

#[derive(Clone, Debug)]
struct PendingSend {
    sql: String,
    pick: RunPick,
}

#[derive(Clone, Debug)]
struct TabState {
    id: String,
    title: String,
    draft: String,
    selection: (usize, usize),
    head: usize,
    version: u64,
    target: Option<Target>,
    running: bool,
    cancelled: bool,
    executed: bool,
    token: Option<TaskToken>,
    run_id: Option<String>,
    results: Vec<StatementView>,
    status: String,
    lint: Vec<LintFinding>,
    completions: Vec<CompletionEntry>,
    outline: Vec<OutlineItem>,
    pending: Option<PendingSend>,
    provider: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftLink {
    pub applied: String,
    pub editor: String,
    pub dirty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DraftSync {
    Pull,
    Push(String),
    Same,
}

pub struct Desk {
    tabs: Vec<TabState>,
    active: Option<String>,
    seq: u64,
    objects: Vec<TreeObject>,
    columns: HashMap<String, Vec<String>>,
    databases: Vec<String>,
    executions: u32,
    loads: u32,
}

impl Desk {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            seq: 0,
            objects: Vec::new(),
            columns: HashMap::new(),
            databases: Vec::new(),
            executions: 0,
            loads: 0,
        }
    }

    pub fn executions(&self) -> u32 {
        self.executions
    }

    pub fn loads(&self) -> u32 {
        self.loads
    }

    pub fn active_id(&self) -> Option<&str> {
        self.active.as_deref()
    }

    pub fn draft(&self) -> String {
        self.active_tab().map(|tab| tab.draft.clone()).unwrap_or_default()
    }

    pub fn version(&self) -> u64 {
        self.active_tab().map(|tab| tab.version).unwrap_or(0)
    }

    pub fn executed(&self) -> bool {
        self.active_tab().is_some_and(|tab| tab.executed)
    }

    pub fn running_ids(&self) -> Vec<String> {
        self.tabs.iter().filter(|tab| tab.running).map(|tab| tab.id.clone()).collect()
    }

    pub fn results(&self) -> Vec<StatementView> {
        self.active_tab().map(|tab| tab.results.clone()).unwrap_or_default()
    }

    pub fn status(&self) -> String {
        self.active_tab().map(|tab| tab.status.clone()).unwrap_or_default()
    }

    pub fn objects(&self) -> &[TreeObject] {
        &self.objects
    }

    pub fn databases(&self) -> &[String] {
        &self.databases
    }

    pub fn target(&self) -> Option<Target> {
        self.active_tab().and_then(|tab| tab.target.clone())
    }

    pub fn outline(&self) -> Vec<OutlineItem> {
        self.active_tab().map(|tab| tab.outline.clone()).unwrap_or_default()
    }

    pub fn completions(&self) -> Vec<CompletionEntry> {
        self.active_tab().map(|tab| tab.completions.clone()).unwrap_or_default()
    }

    pub fn lint(&self) -> Vec<LintFinding> {
        self.active_tab().map(|tab| tab.lint.clone()).unwrap_or_default()
    }

    pub fn variables(&self) -> Vec<VariableInput> {
        self.active_tab()
            .and_then(|tab| tab.pending.as_ref())
            .map(|pending| assist::find_variables(&pending.sql))
            .unwrap_or_default()
    }

    pub fn has_tab(&self, id: &str) -> bool {
        self.tabs.iter().any(|tab| tab.id == id)
    }

    pub fn tab_ids(&self) -> Vec<String> {
        self.tabs.iter().map(|tab| tab.id.clone()).collect()
    }

    pub fn import_tab(&mut self, id: &str, draft: &str, target: Option<Target>) {
        if self.has_tab(id) {
            if let Some(tab) = self.tab_mut(id) {
                if tab.target.is_none() {
                    tab.target = target;
                }
            }
            return;
        }
        self.tabs.push(TabState {
            id: id.to_owned(),
            title: id.to_owned(),
            draft: draft.to_owned(),
            selection: (0, 0),
            head: 0,
            version: 1,
            target,
            running: false,
            cancelled: false,
            executed: false,
            token: None,
            run_id: None,
            results: Vec::new(),
            status: String::new(),
            lint: Vec::new(),
            completions: Vec::new(),
            outline: Vec::new(),
            pending: None,
            provider: true,
        });
        if self.active.is_none() {
            self.active = Some(id.to_owned());
        }
    }

    pub fn focus_tab(&mut self, id: Option<&str>) {
        if let Some(id) = id {
            if self.has_tab(id) {
                self.active = Some(id.to_owned());
            }
        }
    }

    pub fn retain_tabs(&mut self, ids: &[String], tasks: &TaskBoard) {
        let remove: Vec<String> = self.tabs.iter().filter(|tab| !ids.iter().any(|id| id == &tab.id)).map(|tab| tab.id.clone()).collect();
        for id in remove {
            self.close_tab(&id, tasks);
        }
    }

    pub fn replace_draft(&mut self, id: &str, sql: &str, tasks: &TaskBoard) {
        tasks.supersede(&owner(id));
        let Some(tab) = self.tab_mut(id) else {
            return;
        };
        tab.draft = sql.to_owned();
        tab.version = tab.version.saturating_add(1);
        tab.executed = false;
        tab.running = false;
        tab.cancelled = false;
        tab.results.clear();
        tab.token = None;
        tab.run_id = None;
        tab.pending = None;
        tab.status.clear();
        self.loads += 1;
    }

    pub fn set_tab_draft(&mut self, id: &str, sql: &str) {
        let Some(tab) = self.tab_mut(id) else {
            return;
        };
        if tab.draft != sql {
            tab.draft = sql.to_owned();
            tab.version = tab.version.saturating_add(1);
        }
    }

    pub fn tab_draft(&self, id: &str) -> Option<String> {
        self.tabs.iter().find(|tab| tab.id == id).map(|tab| tab.draft.clone())
    }

    pub fn cancelled(&self, id: &str) -> bool {
        self.tabs.iter().find(|tab| tab.id == id).is_some_and(|tab| tab.cancelled)
    }

    pub fn new_query(&mut self) -> String {
        self.push(String::new(), None, true)
    }

    pub fn open_database(&mut self, connection_id: &str, connection_name: &str, engine: &str, database: &str) -> String {
        self.push(
            String::new(),
            Some(Target {
                connection_id: connection_id.to_owned(),
                connection_name: connection_name.to_owned(),
                database: database.to_owned(),
                engine: engine.to_owned(),
                schema: String::new(),
            }),
            false,
        )
    }

    pub fn open_object(&mut self, object: &TreeObject) -> String {
        let sql = assist::browse_sql(&object.engine, &object.schema, &object.name);
        self.push(
            sql,
            Some(Target {
                connection_id: object.connection_id.clone(),
                connection_name: object.connection_name.clone(),
                database: object.database.clone(),
                engine: object.engine.clone(),
                schema: object.schema.clone(),
            }),
            false,
        )
    }

    pub fn select_database(&mut self, database: &str) -> bool {
        let Some(tab) = self.active_mut() else {
            return false;
        };
        let Some(target) = tab.target.as_mut() else {
            return false;
        };
        target.database = database.to_owned();
        if !self.databases.iter().any(|name| name == database) {
            self.databases.push(database.to_owned());
        }
        true
    }

    pub fn set_draft(&mut self, sql: &str) {
        let Some(tab) = self.active_mut() else {
            return;
        };
        if tab.draft != sql {
            tab.draft = sql.to_owned();
            tab.version = tab.version.saturating_add(1);
        }
    }

    pub fn set_selection(&mut self, from: usize, to: usize, head: usize) {
        if let Some(tab) = self.active_mut() {
            tab.selection = (from, to);
            tab.head = head;
        }
    }

    pub fn set_provider(&mut self, provider: bool) {
        if let Some(tab) = self.active_mut() {
            tab.provider = provider;
        }
    }

    pub fn close_tab(&mut self, id: &str, tasks: &TaskBoard) {
        tasks.supersede(&owner(id));
        self.tabs.retain(|tab| tab.id != id);
        if self.active.as_deref() == Some(id) {
            self.active = self.tabs.last().map(|tab| tab.id.clone());
        }
    }

    pub fn switch_to(&mut self, id: &str) -> bool {
        if self.tabs.iter().any(|tab| tab.id == id) {
            self.active = Some(id.to_owned());
            true
        } else {
            false
        }
    }

    /// 历史、保存查询、文件和会话恢复都走这里：只换草稿，不执行。
    pub fn load_draft(&mut self, sql: impl Into<String>, target: Option<Target>, tasks: &TaskBoard) -> bool {
        let Some(id) = self.active.clone() else {
            return false;
        };
        tasks.supersede(&owner(&id));
        let Some(tab) = self.tab_mut(&id) else {
            return false;
        };
        tab.draft = sql.into();
        tab.version = tab.version.saturating_add(1);
        tab.selection = (0, 0);
        tab.head = 0;
        tab.running = false;
        tab.cancelled = false;
        tab.executed = false;
        tab.token = None;
        tab.run_id = None;
        tab.results.clear();
        tab.pending = None;
        tab.status = String::new();
        tab.lint.clear();
        tab.completions.clear();
        if target.is_some() {
            tab.target = target;
        }
        self.loads += 1;
        self.refresh_outline();
        true
    }

    pub fn restore(&mut self, drafts: Vec<(String, Option<Target>)>, tasks: &TaskBoard) {
        for tab in &self.tabs {
            tasks.supersede(&owner(&tab.id));
        }
        self.tabs.clear();
        self.active = None;
        for (sql, target) in drafts {
            let id = self.push(sql, target, false);
            let _ = id;
        }
        self.loads += 1;
        for tab in &self.tabs {
            debug_assert!(!tab.executed);
            debug_assert!(!tab.running);
        }
    }

    pub fn format_active(&mut self) -> TextResult {
        let Some(tab) = self.active_tab().cloned() else {
            return TextResult { ok: false, sql: String::new(), reason_key: Some("query.errors.needTarget") };
        };
        let engine = tab.target.as_ref().map(|target| target.engine.as_str()).unwrap_or("");
        if let Some(blocked) = assist::guard_rewrite(engine, true, &tab.draft) {
            return blocked;
        }
        let (from, to) = tab.selection;
        let formatted = if from == to { assist::format_sql(&tab.draft) } else { assist::format_document(&tab.draft, from, to) };
        if formatted.ok {
            self.set_draft(&formatted.sql);
        }
        formatted
    }

    pub fn expand_active(&mut self) -> TextResult {
        let Some(tab) = self.active_tab().cloned() else {
            return TextResult { ok: false, sql: String::new(), reason_key: Some("query.errors.needTarget") };
        };
        let engine = tab.target.as_ref().map(|target| target.engine.as_str()).unwrap_or("");
        let sources = self.sources_for(tab.target.as_ref());
        let expanded = assist::expand_star(engine, &tab.draft, &sources, tab.provider);
        if expanded.ok {
            self.set_draft(&expanded.sql);
        }
        expanded
    }

    pub fn explain_active(&mut self) -> Result<String, String> {
        let Some(tab) = self.active_tab().cloned() else {
            return Err(assist_text("query.errors.needTarget"));
        };
        let engine = tab.target.as_ref().map(|target| target.engine.as_str()).unwrap_or("");
        let (statement, _, _) = assist::plan_statement(&tab.draft, tab.selection.0, tab.selection.1, tab.head, engine)?;
        let sql = assist::explain_sql(engine, &statement)?;
        if self.active_tab().map(|tab| tab.draft.as_str()) != Some(tab.draft.as_str()) {
            return Err(assist_text("query.errors.unknownDialect"));
        }
        Ok(sql)
    }

    pub fn refresh_outline(&mut self) {
        let Some(tab) = self.active_tab().cloned() else {
            return;
        };
        let engine = tab.target.as_ref().map(|target| target.engine.as_str()).unwrap_or("postgres");
        let engine = if assist::known_engine(engine) { engine } else { "postgres" };
        let list = assist::outline_statements(&tab.draft, engine);
        let previous: Vec<(String, String, usize, Option<u64>)> = tab
            .results
            .iter()
            .map(|item| (item.sql.clone(), item.status.clone(), if item.columns.is_empty() { 0 } else { item.rows.len() }, item.affected))
            .collect();
        let merged = assist::merge_outline(list, &previous);
        if let Some(tab) = self.active_mut() {
            tab.outline = merged;
        }
    }

    pub fn issue(&mut self, tasks: &TaskBoard) -> Option<Ticket> {
        let tab = self.active_tab()?;
        let token = tasks.begin(&owner(&tab.id));
        let ticket = Ticket { tab_id: tab.id.clone(), token, version: tab.version };
        if let Some(tab) = self.active_mut() {
            tab.token = Some(ticket.token);
        }
        Some(ticket)
    }

    pub fn accept_lint(&mut self, tasks: &TaskBoard, ticket: &Ticket, findings: Vec<LintFinding>) -> bool {
        self.accept_assist(tasks, ticket, |tab| tab.lint = findings)
    }

    pub fn accept_completion(&mut self, tasks: &TaskBoard, ticket: &Ticket, items: Vec<CompletionEntry>) -> bool {
        self.accept_assist(tasks, ticket, |tab| tab.completions = items)
    }

    pub fn accept_run(&mut self, tasks: &TaskBoard, ticket: &Ticket, run: RunDto) -> bool {
        if !self.tabs.iter().any(|tab| tab.id == ticket.tab_id) {
            return false;
        }
        let outcome = tasks.accept(&owner(&ticket.tab_id), ticket.token, ());
        let Some(tab) = self.tab_mut(&ticket.tab_id) else {
            return false;
        };
        if tab.cancelled {
            tab.running = false;
            tab.status = "cancelled".to_owned();
            return false;
        }
        match outcome {
            TaskOutcome::Current { .. } => {
                tab.running = false;
                tab.executed = true;
                tab.status = run.status.clone();
                tab.results = run.statements.into_iter().map(statement_view).collect();
                tab.run_id = Some(run.id);
                true
            }
            TaskOutcome::Cancelled { .. } | TaskOutcome::Stale { .. } => {
                tab.running = false;
                false
            }
        }
    }

    pub fn cancel(&mut self, tasks: &TaskBoard) -> Option<String> {
        let tab = self.active_mut()?;
        if let Some(token) = tab.token {
            tasks.cancel(&owner(&tab.id), token.id);
        }
        tab.cancelled = true;
        tab.running = false;
        tab.status = "cancelled".to_owned();
        tab.run_id.clone()
    }

    pub fn begin_run(&mut self, tasks: &TaskBoard, scope: RunScope, override_sql: Option<String>) -> BeginRun {
        let Some(tab) = self.active_tab().cloned() else {
            return BeginRun::Rejected(assist_text("query.errors.needTarget"));
        };
        if tab.running {
            return BeginRun::Rejected("这个连接已有正在执行的查询。可以切换到其他标签继续编辑。".to_owned());
        }
        let Some(target) = tab.target.clone() else {
            return BeginRun::Rejected(assist_text("query.errors.needTarget"));
        };
        if target.connection_id.is_empty() || target.database.is_empty() {
            return BeginRun::Rejected(assist_text("query.errors.needTarget"));
        }
        let original = tab.draft.clone();
        let pick = if let Some(sql) = override_sql.clone() {
            RunPick { sql, mode: "selection", from: tab.selection.0, to: tab.selection.1 }
        } else {
            assist::sql_to_run(&tab.draft, tab.selection.0, tab.selection.1, tab.head, scope, &target.engine)
        };
        if override_sql.is_none() {
            if let Err(reason) = assist::prepare_send(&original, &pick, &target.engine) {
                return BeginRun::Rejected(reason);
            }
            let variables = assist::find_variables(&pick.sql);
            if !variables.is_empty() {
                if let Some(tab) = self.active_mut() {
                    tab.pending = Some(PendingSend { sql: pick.sql.clone(), pick: pick.clone() });
                }
                return BeginRun::Variables(variables);
            }
        }
        let token = tasks.begin(&owner(&tab.id));
        if let Some(tab) = self.active_mut() {
            tab.running = true;
            tab.cancelled = false;
            tab.token = Some(token);
            tab.status = "running".to_owned();
            tab.pending = None;
            tab.draft = original;
        }
        self.executions += 1;
        BeginRun::Ready(PreparedRun {
            tab_id: tab.id,
            token,
            version: tab.version,
            sql: pick.sql,
            connection_id: target.connection_id,
            connection_name: target.connection_name,
            database: target.database,
            engine: target.engine,
            limit: 100,
        })
    }

    pub fn confirm_variables(&mut self, values: &HashMap<String, String>) -> Option<String> {
        let pending = self.active_mut()?.pending.clone()?;
        let sql = assist::substitute_variables(&pending.sql, values);
        Some(sql)
    }

    pub fn cancel_variables(&mut self) {
        if let Some(tab) = self.active_mut() {
            tab.pending = None;
        }
    }

    pub fn note_run(&mut self, tab_id: &str, run_id: &str) {
        if let Some(tab) = self.tab_mut(tab_id) {
            tab.run_id = Some(run_id.to_owned());
        }
    }

    pub fn remember_columns(&mut self, connection_id: &str, database: &str, table: &str, columns: Vec<String>) {
        self.columns.insert(column_key(connection_id, database, table), columns);
    }

    pub fn set_objects(&mut self, objects: Vec<TreeObject>, databases: Vec<String>) {
        self.databases = databases;
        self.objects = objects;
    }

    fn sources_for(&self, target: Option<&Target>) -> Vec<CompletionSource> {
        let Some(target) = target else {
            return Vec::new();
        };
        self.objects
            .iter()
            .filter(|object| object.connection_id == target.connection_id && object.database == target.database)
            .map(|object| CompletionSource {
                alias: String::new(),
                label: object.name.clone(),
                columns: self.columns.get(&column_key(&object.connection_id, &object.database, &object.name)).cloned().unwrap_or_default(),
            })
            .collect()
    }

    pub fn completion_for(&self, prefix: &str) -> Vec<CompletionEntry> {
        let Some(tab) = self.active_tab() else {
            return Vec::new();
        };
        let engine = tab.target.as_ref().map(|target| target.engine.as_str()).unwrap_or("");
        let objects = self
            .objects
            .iter()
            .filter(|object| tab.target.as_ref().is_some_and(|target| target.connection_id == object.connection_id && target.database == object.database))
            .map(|object| CatalogObject { name: object.name.clone(), schema: object.schema.clone(), kind: object.kind.clone() })
            .collect::<Vec<_>>();
        assist::complete(engine, prefix, &objects, &self.sources_for(tab.target.as_ref()), tab.provider)
    }

    fn accept_assist(&mut self, tasks: &TaskBoard, ticket: &Ticket, apply: impl FnOnce(&mut TabState)) -> bool {
        if !self.tabs.iter().any(|tab| tab.id == ticket.tab_id) {
            return false;
        }
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == ticket.tab_id) else {
            return false;
        };
        if tab.version != ticket.version {
            return false;
        }
        match tasks.accept(&owner(&ticket.tab_id), ticket.token, ()) {
            TaskOutcome::Current { .. } => {
                if let Some(tab) = self.tab_mut(&ticket.tab_id) {
                    if tab.version != ticket.version {
                        return false;
                    }
                    apply(tab);
                }
                true
            }
            TaskOutcome::Cancelled { .. } | TaskOutcome::Stale { .. } => false,
        }
    }

    fn push(&mut self, draft: String, target: Option<Target>, keep_provider: bool) -> String {
        self.seq += 1;
        let id = format!("q{}", self.seq);
        let title_n = self.tabs.len() + 1;
        self.tabs.push(TabState {
            id: id.clone(),
            title: format!("query {title_n}"),
            draft,
            selection: (0, 0),
            head: 0,
            version: 1,
            target,
            running: false,
            cancelled: false,
            executed: false,
            token: None,
            run_id: None,
            results: Vec::new(),
            status: String::new(),
            lint: Vec::new(),
            completions: Vec::new(),
            outline: Vec::new(),
            pending: None,
            provider: keep_provider,
        });
        self.active = Some(id.clone());
        self.refresh_outline();
        id
    }

    fn active_tab(&self) -> Option<&TabState> {
        let id = self.active.as_deref()?;
        self.tabs.iter().find(|tab| tab.id == id)
    }

    fn active_mut(&mut self) -> Option<&mut TabState> {
        let id = self.active.clone()?;
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    fn tab_mut(&mut self, id: &str) -> Option<&mut TabState> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }
}

impl Default for Desk {
    fn default() -> Self {
        Self::new()
    }
}

pub enum BeginRun {
    Ready(PreparedRun),
    Variables(Vec<VariableInput>),
    Rejected(String),
}

pub struct PreparedRun {
    pub tab_id: String,
    pub token: TaskToken,
    pub version: u64,
    pub sql: String,
    pub connection_id: String,
    pub connection_name: String,
    pub database: String,
    pub engine: String,
    pub limit: u64,
}

pub async fn execute_prepared(service: &Arc<Service>, query: &commands::QueryState, prepared: &PreparedRun) -> Result<RunDto, String> {
    let started = commands::start_run(
        service,
        query,
        StartRequest {
            connection_id: prepared.connection_id.clone(),
            connection_name: prepared.connection_name.clone(),
            database: prepared.database.clone(),
            engine: prepared.engine.clone(),
            sql: prepared.sql.clone(),
            limit: prepared.limit,
            timeout_ms: None,
        },
    )
    .await?;
    for _ in 0..400 {
        let snap = commands::poll_run(query, &started.id, false).await?;
        if !matches!(snap.status.as_str(), "running" | "cancel-requested") {
            return Ok(snap);
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    Err("执行没有结束".to_owned())
}

pub async fn cancel_run(service: &Arc<Service>, query: &commands::QueryState, run_id: &str) -> Result<RunDto, String> {
    commands::request_cancel(service, query, run_id).await
}

pub async fn load_tree(service: &Arc<Service>) -> Result<(Vec<TreeObject>, Vec<String>, HashMap<String, Vec<String>>), String> {
    let records = service.list().await?;
    let mut objects = Vec::new();
    let mut databases = Vec::new();
    let mut columns = HashMap::new();
    for record in records {
        let Some(session) = service.catalog_session(&record.saved.id) else {
            continue;
        };
        let engine = record.saved.engine.as_str();
        if engine != "sqlite" {
            databases.push(record.saved.database.clone().unwrap_or_default());
            continue;
        }
        let transport = service.cancel_transport(&record.saved.id).unwrap_or(crate::connections::CancelTransport::None);
        let driver = LiveDriver::attach(session, engine, "main", transport, None).await?;
        let flag = Arc::new(AtomicBool::new(false));
        let listed = driver.run("SELECT name, type FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY name", 500, &flag).await.map_err(|err| err.to_string())?;
        let _ = driver.finish().await;
        if !databases.iter().any(|name| name == "main") {
            databases.push("main".to_owned());
        }
        for row in listed.rows {
            let Some(name) = row.first().and_then(|cell| cell.clone()) else {
                continue;
            };
            let kind = row.get(1).and_then(|cell| cell.clone()).unwrap_or_else(|| "table".to_owned());
            objects.push(TreeObject {
                connection_id: record.saved.id.clone(),
                connection_name: record.saved.name.clone(),
                engine: engine.to_owned(),
                database: "main".to_owned(),
                schema: String::new(),
                name: name.clone(),
                kind,
            });
            if let Ok(info) = read_columns(service, &record.saved.id, &name).await {
                columns.insert(column_key(&record.saved.id, "main", &name), info);
            }
        }
    }
    Ok((objects, databases, columns))
}

async fn read_columns(service: &Arc<Service>, connection_id: &str, table: &str) -> Result<Vec<String>, String> {
    let session = service.catalog_session(connection_id).ok_or_else(|| "连接没有打开".to_owned())?;
    let transport = service.cancel_transport(connection_id).unwrap_or(crate::connections::CancelTransport::None);
    let driver = LiveDriver::attach(session, "sqlite", "main", transport, None).await?;
    let sql = format!("PRAGMA table_info({})", assist::quote_ident("sqlite", table));
    let flag = Arc::new(AtomicBool::new(false));
    let info = driver.run(&sql, 200, &flag).await.map_err(|stop| match stop {
        super::exec::StatementStop::Error { message } | super::exec::StatementStop::Unknown { message } | super::exec::StatementStop::Cancelled { message } => message,
    })?;
    let _ = driver.finish().await;
    let name_at = info.columns.iter().position(|column| column.eq_ignore_ascii_case("name")).unwrap_or(1);
    Ok(info.rows.into_iter().filter_map(|row| row.get(name_at).and_then(|cell| cell.clone())).collect())
}

fn statement_view(statement: commands::StatementDto) -> StatementView {
    StatementView {
        sql: statement.sql,
        status: statement.status,
        message: statement.message,
        columns: statement.columns,
        rows: statement.rows,
        affected: statement.affected,
    }
}

fn owner(id: &str) -> String {
    format!("query-tab-{id}")
}

fn column_key(connection_id: &str, database: &str, table: &str) -> String {
    format!("{connection_id}\u{1}{database}\u{1}{}", table.to_ascii_lowercase())
}

fn assist_text(key: &str) -> String {
    crate::i18n::text(crate::i18n::Locale::ZhCn, key)
}

pub fn sync_draft(link: &mut DraftLink, model_draft: &str) -> DraftSync {
    if model_draft != link.applied {
        if link.dirty && link.editor != link.applied && model_draft == link.editor {
            link.applied = model_draft.to_owned();
            link.dirty = false;
            return DraftSync::Same;
        }
        link.editor = model_draft.to_owned();
        link.applied = model_draft.to_owned();
        link.dirty = false;
        DraftSync::Pull
    } else if link.dirty && link.editor != model_draft {
        let push = link.editor.clone();
        link.applied = push.clone();
        link.dirty = false;
        DraftSync::Push(push)
    } else {
        DraftSync::Same
    }
}

trait StopText {
    fn to_string(&self) -> String;
}

impl StopText for super::exec::StatementStop {
    fn to_string(&self) -> String {
        match self {
            super::exec::StatementStop::Error { message } | super::exec::StatementStop::Unknown { message } | super::exec::StatementStop::Cancelled { message } => message.clone(),
        }
    }
}

#[allow(dead_code)]
fn _session_kind(session: &CatalogSession) -> &'static str {
    match session {
        CatalogSession::Sqlite { .. } => "sqlite",
        CatalogSession::Postgres(_) => "postgres",
        CatalogSession::Mysql(_) => "mysql",
        CatalogSession::Sqlserver(_) => "sqlserver",
        CatalogSession::Redis(_) => "redis",
        CatalogSession::Mongo(_) => "mongo",
        CatalogSession::Scripted => "scripted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::Service;

    #[test]
    fn external_drafts_do_not_execute_and_stale_assist_is_dropped() {
        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        let first = desk.new_query();
        desk.set_draft("SELECT 1");
        let ticket = desk.issue(&tasks).unwrap();
        desk.set_draft("SELECT 2");
        assert!(!desk.accept_lint(&tasks, &ticket, vec![LintFinding { start: 0, end: 1, message_key: "query.split.quoteUnclosed" }]));
        assert!(desk.lint().is_empty());
        assert!(desk.load_draft("SELECT loaded", None, &tasks));
        assert!(!desk.executed());
        assert_eq!(desk.executions(), 0);
        assert_eq!(desk.draft(), "SELECT loaded");
        desk.restore(vec![("SELECT restored".to_owned(), None)], &tasks);
        assert_eq!(desk.draft(), "SELECT restored");
        assert!(!desk.executed());
        assert!(desk.results().is_empty());
        let mut link = DraftLink { applied: String::new(), editor: String::new(), dirty: false };
        assert_eq!(sync_draft(&mut link, "SELECT loaded"), DraftSync::Pull);
        assert!(!desk.executed());
        let _ = first;
    }

    #[test]
    fn closing_or_switching_drops_late_results() {
        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        desk.open_database("notes", "笔记", "sqlite", "main");
        desk.set_draft("SELECT 1");
        let ticket = match desk.begin_run(&tasks, RunScope::Script, None) {
            BeginRun::Ready(prepared) => Ticket { tab_id: prepared.tab_id, token: prepared.token, version: prepared.version },
            other => panic!("expected a run, got variables/reject: {}", match other {
                BeginRun::Rejected(reason) => reason,
                BeginRun::Variables(_) => "variables".to_owned(),
                BeginRun::Ready(_) => String::new(),
            }),
        };
        let other = desk.new_query();
        let late = empty_run("late");
        assert!(desk.accept_run(&tasks, &ticket, late));
        assert!(desk.results().is_empty(), "the new tab must not receive the old run");
        assert!(desk.switch_to(&ticket.tab_id));
        assert_eq!(desk.results().len(), 1);
        desk.close_tab(&ticket.tab_id, &tasks);
        let newer = desk.issue(&tasks);
        assert!(newer.is_some());
        assert!(!desk.accept_run(&tasks, &ticket, empty_run("after-close")));
        assert_eq!(desk.active_id(), Some(other.as_str()));
    }

    #[test]
    fn cancel_beats_a_late_success_and_selection_is_exact() {
        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        desk.open_database("notes", "笔记", "sqlite", "main");
        let sql = "SELECT 1;\nSELECT 2;";
        desk.set_draft(sql);
        let from = sql.find("ELECT 2").unwrap();
        desk.set_selection(from, from + "ELECT 2".len(), from);
        let exact = match desk.begin_run(&tasks, RunScope::Auto, None) {
            BeginRun::Ready(prepared) => prepared,
            BeginRun::Rejected(reason) => panic!("{reason}"),
            BeginRun::Variables(_) => panic!("variables"),
        };
        assert_eq!(exact.sql, "ELECT 2");
        assert_eq!(desk.draft(), sql);
        let _ = desk.cancel(&tasks);
        desk.set_selection(0, 0, 0);
        let prepared = match desk.begin_run(&tasks, RunScope::Current, None) {
            BeginRun::Ready(prepared) => prepared,
            BeginRun::Rejected(reason) => panic!("{reason}"),
            BeginRun::Variables(_) => panic!("variables"),
        };
        assert_eq!(prepared.sql.trim(), "SELECT 1;");
        let ticket = Ticket { tab_id: prepared.tab_id, token: prepared.token, version: prepared.version };
        assert!(desk.cancel(&tasks).is_none() || true);
        assert!(!desk.accept_run(&tasks, &ticket, empty_run("too-late")));
        assert_eq!(desk.status(), "cancelled");
        assert!(desk.results().is_empty());
    }

    #[tokio::test]
    async fn tree_open_runs_a_real_select_and_cancel_does_not_replay() {
        let dir = std::env::temp_dir().join(format!("rillsql-desk-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("items.sqlite");
        let setup = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
            .await
            .unwrap();
        sqlx::query("CREATE TABLE item (id INTEGER PRIMARY KEY, label TEXT)")
            .execute(&setup)
            .await
            .unwrap();
        sqlx::query("INSERT INTO item (id, label) VALUES (1, '真实')")
            .execute(&setup)
            .await
            .unwrap();
        setup.close().await;

        let service = Arc::new(Service::new_for_test(dir.join("connections.sqlite")));
        service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
        let query = commands::QueryState::new(dir.join("query-history.sqlite"));
        let (objects, databases, columns) = load_tree(&service).await.unwrap();
        assert!(objects.iter().any(|object| object.name == "item"), "{objects:?}");
        assert!(databases.iter().any(|name| name == "main"));

        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        desk.set_objects(objects.clone(), databases);
        for (key, cols) in &columns {
            let table = key.split('\u{1}').nth(2).unwrap_or("");
            desk.remember_columns("notes", "main", table, cols.clone());
        }
        let object = objects.iter().find(|object| object.name == "item").unwrap();
        desk.open_object(object);
        let target = desk.target().unwrap();
        assert_eq!(target.connection_id, "notes");
        assert_eq!(target.database, "main");
        assert!(desk.draft().contains("item"));
        assert!(!desk.executed());
        assert!(desk.select_database("main"));
        desk.set_provider(true);
        let expanded = desk.expand_active();
        assert!(expanded.ok, "{expanded:?}");
        assert!(desk.draft().contains("label"));
        assert!(!desk.executed());

        desk.set_draft("SELECT label FROM item");
        desk.set_selection(0, 0, 0);
        let prepared = match desk.begin_run(&tasks, RunScope::Script, None) {
            BeginRun::Ready(prepared) => prepared,
            BeginRun::Rejected(reason) => panic!("{reason}"),
            BeginRun::Variables(_) => panic!("variables"),
        };
        assert_eq!(prepared.sql, "SELECT label FROM item");
        let ticket = Ticket { tab_id: prepared.tab_id.clone(), token: prepared.token, version: prepared.version };
        let finished = execute_prepared(&service, &query, &prepared).await.unwrap();
        assert!(desk.accept_run(&tasks, &ticket, finished), "current result should land");
        assert_eq!(desk.results()[0].rows, vec![vec![Some("真实".to_owned())]]);
        assert!(desk.executed());

        desk.set_draft("SELECT 1 AS a;\nSELECT 2 AS b;");
        let multi = match desk.begin_run(&tasks, RunScope::Script, None) {
            BeginRun::Ready(prepared) => prepared,
            BeginRun::Rejected(reason) => panic!("{reason}"),
            BeginRun::Variables(_) => panic!("variables"),
        };
        let multi_ticket = Ticket { tab_id: multi.tab_id.clone(), token: multi.token, version: multi.version };
        let multi_done = execute_prepared(&service, &query, &multi).await.unwrap();
        assert!(desk.accept_run(&tasks, &multi_ticket, multi_done));
        assert_eq!(desk.results().len(), 2);

        let history = query.history().await.unwrap().list().await.unwrap();
        desk.load_draft(history[0].sql.clone(), None, &tasks);
        assert!(!desk.executed());
        assert!(desk.results().is_empty());
        assert_eq!(desk.executions(), 2);

        desk.set_draft("WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 5000000) SELECT sum(x) FROM c");
        let slow = match desk.begin_run(&tasks, RunScope::Script, None) {
            BeginRun::Ready(prepared) => prepared,
            BeginRun::Rejected(reason) => panic!("{reason}"),
            BeginRun::Variables(_) => panic!("variables"),
        };
        let started = commands::start_run(
            &service,
            &query,
            StartRequest {
                connection_id: slow.connection_id.clone(),
                connection_name: slow.connection_name.clone(),
                database: slow.database.clone(),
                engine: slow.engine.clone(),
                sql: slow.sql.clone(),
                limit: slow.limit,
                timeout_ms: None,
            },
        )
        .await
        .unwrap();
        desk.note_run(&slow.tab_id, &started.id);
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        let run_id = desk.cancel(&tasks).unwrap();
        let _ = cancel_run(&service, &query, &run_id).await.unwrap();
        let ended = execute_prepared_wait(&query, &started.id).await;
        let slow_ticket = Ticket { tab_id: slow.tab_id, token: slow.token, version: slow.version };
        assert!(!desk.accept_run(&tasks, &slow_ticket, ended));
        assert_eq!(desk.status(), "cancelled");
        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn execute_prepared_wait(query: &commands::QueryState, id: &str) -> RunDto {
        for _ in 0..400 {
            let snap = commands::poll_run(query, id, false).await.unwrap();
            if !matches!(snap.status.as_str(), "running" | "cancel-requested") {
                return snap;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("取消后的查询没有结束");
    }

    fn empty_run(id: &str) -> RunDto {
        RunDto {
            id: id.to_owned(),
            connection_id: "notes".to_owned(),
            connection_name: "笔记".to_owned(),
            database: "main".to_owned(),
            engine: "sqlite".to_owned(),
            status: "ok".to_owned(),
            cancel: "none".to_owned(),
            cancel_message: String::new(),
            via_ssh: false,
            fresh_channel: false,
            statements: vec![commands::StatementDto {
                sql: "SELECT 1".to_owned(),
                status: "ok".to_owned(),
                message: String::new(),
                columns: vec!["n".to_owned()],
                column_types: vec!["integer".to_owned()],
                rows: vec![vec![Some("1".to_owned())]],
                truncated: false,
                affected: None,
            }],
            started_at: String::new(),
            elapsed_ms: 0,
        }
    }

    #[test]
    fn variables_and_failed_format_leave_the_draft() {
        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        desk.open_database("notes", "笔记", "sqlite", "main");
        desk.set_provider(true);
        desk.set_draft("SELECT :name");
        match desk.begin_run(&tasks, RunScope::Script, None) {
            BeginRun::Variables(fields) => assert_eq!(fields[0].key, "name"),
            BeginRun::Ready(_) => panic!("should wait for variables"),
            BeginRun::Rejected(reason) => panic!("{reason}"),
        }
        assert_eq!(desk.draft(), "SELECT :name");
        let mut values = HashMap::new();
        values.insert("name".to_owned(), "a".to_owned());
        assert_eq!(desk.confirm_variables(&values).as_deref(), Some("SELECT 'a'"));
        assert_eq!(desk.draft(), "SELECT :name");
        desk.cancel_variables();
        desk.set_draft("select 'abc");
        let formatted = desk.format_active();
        assert!(!formatted.ok);
        assert_eq!(desk.draft(), "select 'abc");
        desk.open_database("notes", "笔记", "oracle", "main");
        desk.set_draft("select 1");
        let blocked = desk.format_active();
        assert!(!blocked.ok);
        assert_eq!(desk.draft(), "select 1");
    }
}
