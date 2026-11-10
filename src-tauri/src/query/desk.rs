//! 查询标签的生命周期。执行、补全和 lint 都带任务 id / 代次；载入只替换草稿。

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::application::tasks::{TaskBoard, TaskOutcome, TaskToken};
use crate::i18n::Locale;
use crate::catalog::{read_catalog_session, ConnectionBody, ObjectKind};
use crate::connections::model::Engine;
use crate::connections::{CatalogSession, Service};

use super::assist::{
    self, CatalogObject, CompletionEntry, CompletionSource, LintFinding, MetadataState, OutlineItem, RunPick, RunScope, TextResult,
    TreeObject, VariableInput,
};
use super::complete;
use super::commands::{self, RunDto, StartRequest};
use super::exec::{LiveDriver, StatementDriver};

/// 词频表上限；超出时按次数与名称稳定截断。
const FREQUENCY_LIMIT: usize = 200;

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
    pub environment: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementView {
    pub sql: String,
    pub status: String,
    pub message: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    /// 结果因为读取上限被截断；查询结果据此提供“加载更多”。
    pub truncated: bool,
    pub affected: Option<u64>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct PendingSend {
    sql: String,
    pick: RunPick,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
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
    /// 最近一次成功送达的 SQL。关闭守卫据此判断草稿是否“已执行”。
    executed_sql: Option<String>,
    token: Option<TaskToken>,
    run_id: Option<String>,
    results: Vec<StatementView>,
    status: String,
    lint: Vec<LintFinding>,
    completions: Vec<CompletionEntry>,
    outline: Vec<OutlineItem>,
    pending: Option<PendingSend>,
    provider: bool,
    note: String,
    statement: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenLink {
    pub id: String,
    pub name: String,
    pub engine: String,
    pub databases: Vec<String>,
    pub environment: String,
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

#[allow(dead_code)]
pub struct Desk {
    tabs: Vec<TabState>,
    active: Option<String>,
    seq: u64,
    objects: Vec<TreeObject>,
    /// 按连接/库/模式/表记住的列；同一个表名在不同模式里不互相覆盖。
    columns: HashMap<String, Vec<String>>,
    /// 已读到的真实外键，带连接/库/模式身份。
    foreign_keys: Vec<assist::ForeignKey>,
    /// 本地标识符词频：只参与同分排序，不保存 SQL 文本。
    frequency: HashMap<String, u32>,
    databases: Vec<String>,
    links: Vec<OpenLink>,
    environments: HashMap<String, String>,
    executions: u32,
    loads: u32,
    view_gen: u64,
    idle_note: String,
    tree_error: String,
    /// 每次执行读取的行数上限；来自设置 page_size，默认 100。
    page_size: u64,
}

#[allow(dead_code)]
impl Desk {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            seq: 0,
            objects: Vec::new(),
            columns: HashMap::new(),
            foreign_keys: Vec::new(),
            frequency: HashMap::new(),
            databases: Vec::new(),
            links: Vec::new(),
            environments: HashMap::new(),
            executions: 0,
            loads: 0,
            view_gen: 0,
            idle_note: String::new(),
            tree_error: String::new(),
            page_size: 100,
        }
    }

    /// 查询结果读取的行数上限；设置 page_size 下发到这里。
    pub fn page_size(&self) -> u64 {
        self.page_size
    }

    pub fn set_page_size(&mut self, size: u64) {
        self.page_size = size.max(1);
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
            executed_sql: None,
            token: None,
            run_id: None,
            results: Vec::new(),
            status: String::new(),
            lint: Vec::new(),
            completions: Vec::new(),
            outline: Vec::new(),
            pending: None,
            provider: true,
            note: String::new(),
            statement: 0,
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
        tab.note.clear();
        tab.statement = 0;
        tab.token = None;
        tab.run_id = None;
        tab.pending = None;
        tab.status.clear();
        self.loads += 1;
        self.view_gen = self.view_gen.saturating_add(1);
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

    pub fn executed_sql(&self, id: &str) -> Option<String> {
        self.tabs.iter().find(|tab| tab.id == id).and_then(|tab| tab.executed_sql.clone())
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
        let environment = self.environment_of(connection_id);
        self.push(
            String::new(),
            Some(Target {
                connection_id: connection_id.to_owned(),
                connection_name: connection_name.to_owned(),
                database: database.to_owned(),
                engine: engine.to_owned(),
                schema: String::new(),
                environment,
            }),
            false,
        )
    }

    pub fn open_object(&mut self, object: &TreeObject) -> String {
        let sql = assist::browse_sql(&object.engine, &object.schema, &object.name);
        let environment = self.environment_of(&object.connection_id);
        self.push(
            sql,
            Some(Target {
                connection_id: object.connection_id.clone(),
                connection_name: object.connection_name.clone(),
                database: object.database.clone(),
                engine: object.engine.clone(),
                schema: object.schema.clone(),
                environment,
            }),
            false,
        )
    }

    pub fn select_database(&mut self, database: &str) -> bool {
        if database.is_empty() || self.active.is_none() {
            return false;
        }
        let bound = self.active_tab().and_then(|tab| tab.target.as_ref()).is_some_and(|target| !target.connection_id.is_empty());
        if bound {
            if let Some(target) = self.active_mut().and_then(|tab| tab.target.as_mut()) {
                target.database = database.to_owned();
            }
            self.remember_database(database);
            return true;
        }
        let Some(link) = self.link_for_database(database) else {
            return false;
        };
        self.bind_link(&link, database)
    }

    /// 给还没有目标的活动查询选一条已打开的连接。
    pub fn select_connection(&mut self, id: &str) -> bool {
        let Some(link) = self.links.iter().find(|link| link.id == id).cloned() else {
            return false;
        };
        let database = link
            .databases
            .iter()
            .find(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| if link.engine == "sqlite" { "main".to_owned() } else { String::new() });
        if database.is_empty() {
            return false;
        }
        self.bind_link(&link, &database)
    }

    pub fn set_links(&mut self, links: Vec<OpenLink>) {
        for link in &links {
            if !link.environment.is_empty() {
                self.environments.insert(link.id.clone(), link.environment.clone());
            }
        }
        self.links = links;
    }

    pub fn remember_environments(&mut self, pairs: impl IntoIterator<Item = (String, String)>) {
        for (id, environment) in pairs {
            if !id.is_empty() && !environment.is_empty() {
                self.environments.insert(id, environment);
            }
        }
    }

    pub fn environment_of(&self, connection_id: &str) -> String {
        if let Some(link) = self.links.iter().find(|link| link.id == connection_id) {
            if !link.environment.is_empty() {
                return link.environment.clone();
            }
        }
        self.environments.get(connection_id).cloned().filter(|value| !value.is_empty()).unwrap_or_else(|| "none".to_owned())
    }

    pub fn links(&self) -> &[OpenLink] {
        &self.links
    }

    pub fn view_gen(&self) -> u64 {
        self.view_gen
    }

    pub fn active_note(&self) -> String {
        self.active_tab().map(|tab| tab.note.clone()).unwrap_or_else(|| self.idle_note.clone())
    }

    pub fn note_of(&self, id: &str) -> String {
        self.tabs.iter().find(|tab| tab.id == id).map(|tab| tab.note.clone()).unwrap_or_default()
    }

    pub fn set_note(&mut self, id: &str, text: &str) {
        let found = self.tabs.iter_mut().find(|tab| tab.id == id).map(|tab| {
            tab.note = text.to_owned();
        });
        if found.is_some() {
            self.view_gen = self.view_gen.saturating_add(1);
        }
    }

    pub fn set_active_note(&mut self, text: &str) {
        if let Some(id) = self.active.clone() {
            self.set_note(&id, text);
        } else {
            self.idle_note = text.to_owned();
        }
    }

    pub fn statement(&self) -> usize {
        self.active_tab().map(|tab| tab.statement).unwrap_or(0)
    }

    pub fn set_statement(&mut self, index: usize) {
        if self.active_mut().is_some_and(|tab| {
            tab.statement = index;
            true
        }) {
            self.view_gen = self.view_gen.saturating_add(1);
        }
    }

    /// 测试或外部结果直接写到活动标签，不记成已执行。
    pub fn set_active_sheet(&mut self, columns: Vec<String>, rows: Vec<Vec<Option<String>>>) {
        let wrote = self.active_mut().map(|tab| {
            tab.results = vec![StatementView {
                sql: String::new(),
                status: "ok".to_owned(),
                message: String::new(),
                columns,
                rows,
                truncated: false,
                affected: None,
            }];
            tab.statement = 0;
        });
        if wrote.is_some() {
            self.view_gen = self.view_gen.saturating_add(1);
        }
    }

    pub fn has_token(&self, id: &str, token: TaskToken) -> bool {
        self.tabs.iter().any(|tab| tab.id == id && tab.token == Some(token))
    }

    /// 只结束仍持有该 token 的那次执行。更新的 begin 不受旧错误影响。
    pub fn stop_run(&mut self, id: &str, token: TaskToken, note: &str) -> bool {
        let applied = self.tab_mut(id).is_some_and(|tab| {
            if tab.token != Some(token) {
                return false;
            }
            tab.running = false;
            tab.status = "error".to_owned();
            tab.note = note.to_owned();
            true
        });
        if applied {
            self.view_gen = self.view_gen.saturating_add(1);
        }
        applied
    }

    /// 轮询的终态。旧票的成功、取消和错误都不改写更新 token 的运行状态、说明或结果。
    pub fn finish_polled(&mut self, tasks: &TaskBoard, ticket: &Ticket, result: Result<RunDto, String>, cancel_text: &str) {
        match result {
            Ok(run) => {
                let accepted = self.accept_run(tasks, ticket, run);
                if !self.has_token(&ticket.tab_id, ticket.token) {
                    return;
                }
                if accepted {
                    self.set_note(&ticket.tab_id, "");
                } else if self.note_of(&ticket.tab_id).is_empty() {
                    self.set_note(&ticket.tab_id, cancel_text);
                }
            }
            Err(error) => {
                let _ = self.stop_run(&ticket.tab_id, ticket.token, &error);
            }
        }
    }

    fn remember_database(&mut self, database: &str) {
        if !database.is_empty() && !self.databases.iter().any(|name| name == database) {
            self.databases.push(database.to_owned());
        }
    }

    fn link_for_database(&self, database: &str) -> Option<OpenLink> {
        let matches: Vec<&OpenLink> = self.links.iter().filter(|link| link.databases.iter().any(|name| name == database)).collect();
        if matches.len() == 1 {
            return Some(matches[0].clone());
        }
        if self.links.len() == 1 {
            return Some(self.links[0].clone());
        }
        None
    }

    fn bind_link(&mut self, link: &OpenLink, database: &str) -> bool {
        let environment = if link.environment.is_empty() { self.environment_of(&link.id) } else { link.environment.clone() };
        let Some(tab) = self.active_mut() else {
            return false;
        };
        tab.target = Some(Target {
            connection_id: link.id.clone(),
            connection_name: link.name.clone(),
            database: database.to_owned(),
            engine: link.engine.clone(),
            schema: String::new(),
            environment,
        });
        self.remember_database(database);
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
        tab.note.clear();
        tab.statement = 0;
        tab.pending = None;
        tab.status = String::new();
        tab.lint.clear();
        tab.completions.clear();
        if target.is_some() {
            tab.target = target;
        }
        self.loads += 1;
        self.view_gen = self.view_gen.saturating_add(1);
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
        let running = tab.running;
        let token = tasks.begin(&owner(&tab.id));
        let ticket = Ticket { tab_id: tab.id.clone(), token, version: tab.version };
        if !running {
            if let Some(tab) = self.active_mut() {
                tab.token = Some(ticket.token);
            }
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
        if tab.token != Some(ticket.token) {
            return false;
        }
        let accepted = if tab.cancelled {
            tab.running = false;
            tab.status = "cancelled".to_owned();
            false
        } else {
            match outcome {
                TaskOutcome::Current { .. } => {
                    tab.running = false;
                    tab.executed = true;
                    tab.executed_sql = Some(tab.draft.clone());
                    tab.status = run.status.clone();
                    tab.results = run.statements.into_iter().map(statement_view).collect();
                    tab.run_id = Some(run.id);
                    tab.statement = 0;
                    true
                }
                TaskOutcome::Cancelled { .. } | TaskOutcome::Stale { .. } => {
                    tab.running = false;
                    false
                }
            }
        };
        self.view_gen = self.view_gen.saturating_add(1);
        accepted
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
        let limit = self.page_size.max(1);
        self.begin_run_limited(tasks, scope, override_sql, limit)
    }

    /// 与 begin_run 相同，但显式指定本次读取上限；查询结果的“加载更多”用它放大窗口。
    pub fn begin_run_limited(
        &mut self,
        tasks: &TaskBoard,
        scope: RunScope,
        override_sql: Option<String>,
        limit: u64,
    ) -> BeginRun {
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
        self.record_frequency(&pick.sql);
        BeginRun::Ready(PreparedRun {
            tab_id: tab.id,
            token,
            version: tab.version,
            sql: pick.sql,
            connection_id: target.connection_id,
            connection_name: target.connection_name,
            database: target.database,
            engine: target.engine,
            limit: limit.max(1),
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

    pub fn remember_table_columns(
        &mut self,
        connection_id: &str,
        database: &str,
        schema: &str,
        table: &str,
        columns: Vec<String>,
    ) {
        self.columns.insert(column_key(connection_id, database, schema, table), columns);
    }

    /// 只有库和表、没有模式的引擎（SQLite/MySQL）用它。
    pub fn remember_columns(&mut self, connection_id: &str, database: &str, table: &str, columns: Vec<String>) {
        self.remember_table_columns(connection_id, database, "", table, columns);
    }

    /// 真实外键随目录一起替换；空集表示还没读到或引擎没有外键。
    pub fn set_foreign_keys(&mut self, keys: Vec<assist::ForeignKey>) {
        self.foreign_keys = keys;
    }

    pub fn set_objects(&mut self, objects: Vec<TreeObject>, databases: Vec<String>) {
        self.databases = databases;
        self.objects = objects;
    }

    pub fn set_tree_error(&mut self, error: &str) {
        self.tree_error = error.to_owned();
    }

    pub fn tree_error(&self) -> &str {
        &self.tree_error
    }

    /// 没有任何对象，并且读取本身没有失败。
    pub fn tree_is_empty(&self) -> bool {
        self.objects.is_empty() && self.tree_error.is_empty()
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
                label: if object.schema.is_empty() {
                    object.name.clone()
                } else {
                    format!("{}.{}", object.schema, object.name)
                },
                columns: self
                    .columns
                    .get(&column_key(&object.connection_id, &object.database, &object.schema, &object.name))
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// 一次补全请求：语句全文和光标由编辑器传入，补全计划只读快照。
    pub fn completion_for(&self, text: &str, cursor: usize, locale: Locale) -> Vec<CompletionEntry> {
        self.completion_scope()
            .map(|scope| complete::plan(&scope, text, cursor, locale).entries())
            .unwrap_or_default()
    }

    /// 当前标签的补全上下文。编辑器打字时据此给出与 Ctrl+Space 相同的候选。
    pub fn completion_scope(&self) -> Option<assist::CompletionScope> {
        let tab = self.active_tab()?;
        let engine = tab.target.as_ref().map(|target| target.engine.clone()).unwrap_or_default();
        let objects: Vec<CatalogObject> = self
            .objects
            .iter()
            .filter(|object| {
                tab.target
                    .as_ref()
                    .is_some_and(|target| target.connection_id == object.connection_id && target.database == object.database)
            })
            .map(|object| CatalogObject { name: object.name.clone(), schema: object.schema.clone(), kind: object.kind.clone() })
            .collect();
        let databases = tab
            .target
            .as_ref()
            .and_then(|target| self.links.iter().find(|link| link.id == target.connection_id))
            .map(|link| link.databases.clone())
            .unwrap_or_default();
        let mut schemas: Vec<String> = Vec::new();
        for object in &objects {
            if !object.schema.is_empty() && !schemas.iter().any(|have| have == &object.schema) {
                schemas.push(object.schema.clone());
            }
        }
        // 部分范围失败只说明部分失败：已读到的对象和列继续用；完全没有可用元数据才算失败。
        let state = if tab.target.is_none() {
            MetadataState::NoTarget
        } else if !tab.provider {
            MetadataState::NoProvider
        } else if !objects.is_empty() {
            if self.tree_error.is_empty() {
                MetadataState::Ready
            } else {
                MetadataState::Partial(self.tree_error.clone())
            }
        } else if !self.objects.is_empty() {
            // 树里有别的连接/库的对象，但不是当前目标的：状态可能过期。
            let scope = tab.target.as_ref().map(|target| format!("{}/{}", target.connection_name, target.database)).unwrap_or_default();
            MetadataState::Stale(scope)
        } else if !self.tree_error.is_empty() {
            MetadataState::Failed(self.tree_error.clone())
        } else {
            MetadataState::Empty
        };
        let foreign_keys = self
            .foreign_keys
            .iter()
            .filter(|key| {
                tab.target
                    .as_ref()
                    .is_some_and(|target| key.connection_id == target.connection_id && key.database == target.database)
            })
            .cloned()
            .collect();
        let mut frequency: Vec<(String, u32)> = self.frequency.iter().map(|(name, count)| (name.clone(), *count)).collect();
        frequency.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        Some(assist::CompletionScope {
            engine,
            objects,
            sources: self.sources_for(tab.target.as_ref()),
            provider: tab.provider,
            databases,
            schemas,
            frequency,
            state,
            foreign_keys,
        })
    }

    /// 记录一次实际执行的语句里的标识符词频；只用于同分排序。
    pub fn record_frequency(&mut self, sql: &str) {
        for name in complete::identifiers(sql) {
            let slot = self.frequency.entry(name).or_insert(0);
            *slot = slot.saturating_add(1);
        }
        if self.frequency.len() > FREQUENCY_LIMIT {
            let mut entries: Vec<(String, u32)> = self.frequency.iter().map(|(name, count)| (name.clone(), *count)).collect();
            entries.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
            entries.truncate(FREQUENCY_LIMIT);
            self.frequency = entries.into_iter().collect();
        }
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
            executed_sql: None,
            token: None,
            run_id: None,
            results: Vec::new(),
            status: String::new(),
            lint: Vec::new(),
            completions: Vec::new(),
            outline: Vec::new(),
            pending: None,
            provider: keep_provider,
            note: String::new(),
            statement: 0,
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

#[allow(dead_code)]
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

#[allow(dead_code)]
pub async fn cancel_run(service: &Arc<Service>, query: &commands::QueryState, run_id: &str) -> Result<RunDto, String> {
    commands::request_cancel(service, query, run_id).await
}

pub struct TreeLoad {
    pub objects: Vec<TreeObject>,
    pub databases: Vec<String>,
    pub columns: HashMap<String, Vec<String>>,
    pub foreign_keys: Vec<assist::ForeignKey>,
    pub links: Vec<OpenLink>,
    pub failures: Vec<String>,
}

pub struct CatalogListing {
    pub objects: Vec<TreeObject>,
    pub databases: Vec<String>,
    pub failures: Vec<String>,
}

/// 把已有 catalog 结果收成查询树。只保留表和视图，并带上连接、库和引擎。
/// 连接、库或 schema 的 error 记成失败，不用空列表表示没读到。
pub fn objects_from_catalog(connection_id: &str, connection_name: &str, engine: &str, body: &ConnectionBody) -> CatalogListing {
    let mut objects = Vec::new();
    let mut databases = Vec::new();
    let mut failures = Vec::new();
    if let Some(error) = body.error.as_ref().filter(|error| !error.is_empty()) {
        failures.push(error.clone());
    }
    for database in &body.databases {
        if !database.name.is_empty() && !databases.iter().any(|name| name == &database.name) {
            databases.push(database.name.clone());
        }
        if let Some(error) = database.error.as_ref().filter(|error| !error.is_empty()) {
            let scope = if database.name.is_empty() { "database".to_owned() } else { database.name.clone() };
            failures.push(format!("{scope}: {error}"));
        }
        for schema in &database.schemas {
            if let Some(error) = schema.error.as_ref().filter(|error| !error.is_empty()) {
                let scope = if schema.name.is_empty() { database.name.clone() } else { format!("{}.{}", database.name, schema.name) };
                failures.push(format!("{scope}: {error}"));
            }
            for object in &schema.objects {
                let kind = match object.kind {
                    ObjectKind::Table => "table",
                    ObjectKind::View => "view",
                    _ => continue,
                };
                objects.push(TreeObject {
                    connection_id: connection_id.to_owned(),
                    connection_name: connection_name.to_owned(),
                    engine: engine.to_owned(),
                    database: database.name.clone(),
                    schema: schema.name.clone(),
                    name: object.name.clone(),
                    kind: kind.to_owned(),
                });
            }
        }
    }
    CatalogListing { objects, databases, failures }
}

/// 对象树读取上限来自设置 object_page；默认 100，不把所有对象一次读进来。
pub async fn load_tree(service: &Arc<Service>, page: usize) -> Result<TreeLoad, String> {
    let page = page.max(1);
    let records = service.list().await?;
    let mut objects = Vec::new();
    let mut databases = Vec::new();
    let mut columns = HashMap::new();
    let mut foreign_keys = Vec::new();
    let mut links = Vec::new();
    let mut failures = Vec::new();
    for record in records {
        let Some(session) = service.catalog_session(&record.saved.id) else {
            continue;
        };
        let engine = record.saved.engine.as_str().to_owned();
        let mut db_names = Vec::new();
        if let Some(name) = record.saved.database.clone().filter(|name| !name.is_empty()) {
            db_names.push(name);
        }
        if engine != "sqlite" {
            if let Ok(parsed) = Engine::parse(&engine) {
                let body = read_catalog_session(parsed, &session, page, &[]).await;
                let listing = objects_from_catalog(&record.saved.id, &record.saved.name, &engine, &body);
                // 目录只给对象名；列和外键按对象按需再读，单个对象失败只记原因，不清空兄弟节点。
                for object in &listing.objects {
                    match crate::catalog::read_column_names(&session, parsed, &object.database, &object.schema, &object.name).await {
                        Ok(names) if !names.is_empty() => {
                            columns.insert(
                                column_key(&object.connection_id, &object.database, &object.schema, &object.name),
                                names,
                            );
                        }
                        Ok(_) => {}
                        Err(error) => failures.push(format!("{}: {error}", object_label(object))),
                    }
                }
                for object in listing.objects.iter().filter(|object| object.kind == "table") {
                    let Ok(keys) = super::meta::read_table_keys(&session, parsed, &object.database, &object.schema, &object.name).await else {
                        continue;
                    };
                    collect_keys(
                        &mut foreign_keys,
                        &object.connection_id,
                        &object.database,
                        &object.schema,
                        &object.name,
                        keys,
                    );
                }
                objects.extend(listing.objects);
                failures.extend(listing.failures);
                for name in listing.databases {
                    if !name.is_empty() && !db_names.iter().any(|have| have == &name) {
                        db_names.push(name);
                    }
                }
            }
        } else {
            if !db_names.iter().any(|name| name == "main") {
                db_names.push("main".to_owned());
            }
            let transport = service.cancel_transport(&record.saved.id).unwrap_or(crate::connections::CancelTransport::None);
            // 外键读取要另外借用同一个会话，执行器拿走它之前先留一份句柄。
            let key_session = session.clone();
            let driver = LiveDriver::attach(session, &engine, "main", transport, None).await?;
            let flag = Arc::new(AtomicBool::new(false));
            let listed = driver
                .run(
                    "SELECT name, type FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY name",
                    page,
                    &flag,
                )
                .await
                .map_err(|err| err.to_string())?;
            let _ = driver.finish().await;
            for row in listed.rows {
                let Some(name) = row.first().and_then(|cell| cell.clone()) else {
                    continue;
                };
                let kind = row.get(1).and_then(|cell| cell.clone()).unwrap_or_else(|| "table".to_owned());
                objects.push(TreeObject {
                    connection_id: record.saved.id.clone(),
                    connection_name: record.saved.name.clone(),
                    engine: engine.clone(),
                    database: "main".to_owned(),
                    schema: String::new(),
                    name: name.clone(),
                    kind,
                });
                if let Ok(info) = read_columns(service, &record.saved.id, &name).await {
                    columns.insert(column_key(&record.saved.id, "main", "", &name), info);
                }
                if let Ok(keys) = super::meta::read_table_keys(&key_session, Engine::Sqlite, "main", "", &name).await {
                    collect_keys(&mut foreign_keys, &record.saved.id, "main", "", &name, keys);
                }
            }
        }
        for name in &db_names {
            if !name.is_empty() && !databases.iter().any(|have| have == name) {
                databases.push(name.clone());
            }
        }
        links.push(OpenLink {
            id: record.saved.id.clone(),
            name: record.saved.name.clone(),
            engine,
            databases: db_names,
            environment: record.saved.env.clone(),
        });
    }
    Ok(TreeLoad { objects, databases, columns, foreign_keys, links, failures })
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
        truncated: statement.truncated,
        affected: statement.affected,
    }
}

fn owner(id: &str) -> String {
    format!("query-tab-{id}")
}

/// 把一次外键读取的结果并进树，身份取这张表所在的连接/库/模式。
fn collect_keys(
    into: &mut Vec<assist::ForeignKey>,
    connection_id: &str,
    database: &str,
    schema: &str,
    table: &str,
    keys: Vec<super::meta::TableKey>,
) {
    for key in keys {
        into.push(assist::ForeignKey {
            connection_id: connection_id.to_owned(),
            database: database.to_owned(),
            from_schema: schema.to_owned(),
            from_table: table.to_owned(),
            from_columns: key.columns,
            to_schema: key.ref_schema,
            to_table: key.ref_table,
            to_columns: key.ref_columns,
        });
    }
}

/// 失败说明里的对象路径：库.模式.对象，空的部分跳过。
fn object_label(object: &TreeObject) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if !object.database.is_empty() {
        parts.push(&object.database);
    }
    if !object.schema.is_empty() {
        parts.push(&object.schema);
    }
    parts.push(&object.name);
    parts.join(".")
}

fn column_key(connection_id: &str, database: &str, schema: &str, table: &str) -> String {
    format!(
        "{connection_id}\u{1}{database}\u{1}{}\u{1}{}",
        schema.to_ascii_lowercase(),
        table.to_ascii_lowercase()
    )
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
    async fn object_page_limits_how_many_tree_objects_are_read() {
        let dir = std::env::temp_dir().join(format!("rillsql-desk-page-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("many.sqlite");
        let setup = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
            .await
            .unwrap();
        for statement in [
            "CREATE TABLE alpha (id INTEGER PRIMARY KEY)",
            "CREATE TABLE beta (id INTEGER PRIMARY KEY)",
            "CREATE TABLE gamma (id INTEGER PRIMARY KEY)",
        ] {
            sqlx::query(statement).execute(&setup).await.unwrap();
        }
        setup.close().await;

        let service = Arc::new(Service::new_for_test(dir.join("connections.sqlite")));
        service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
        let limited = load_tree(&service, 2).await.unwrap();
        assert_eq!(limited.objects.len(), 2, "object_page 应限制对象读取数量");
        let all = load_tree(&service, 100).await.unwrap();
        assert_eq!(all.objects.len(), 3);
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
        let loaded = load_tree(&service, 100).await.unwrap();
        let objects = loaded.objects.clone();
        let databases = loaded.databases.clone();
        let columns = loaded.columns.clone();
        assert!(objects.iter().any(|object| object.name == "item"), "{objects:?}");
        assert!(databases.iter().any(|name| name == "main"));
        assert!(loaded.links.iter().any(|link| link.id == "notes" && link.engine == "sqlite"));

        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        desk.set_objects(objects.clone(), databases);
        for (key, cols) in &columns {
            let mut parts = key.split('\u{1}');
            let _ = parts.next();
            let _ = parts.next();
            let schema = parts.next().unwrap_or("");
            let table = parts.next().unwrap_or("");
            desk.remember_table_columns("notes", "main", schema, table, cols.clone());
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

    #[test]
    fn new_query_picks_a_connection_and_reaches_start_run() {
        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        desk.new_query();
        assert!(desk.target().is_none());
        assert!(matches!(desk.begin_run(&tasks, RunScope::Script, None), BeginRun::Rejected(_)));
        desk.set_links(vec![OpenLink {
            id: "notes".to_owned(),
            name: "笔记".to_owned(),
            engine: "sqlite".to_owned(),
            databases: vec!["main".to_owned()],
            environment: "dev".to_owned(),
        }]);
        assert!(desk.select_database("main"));
        let target = desk.target().expect("bound");
        assert_eq!(target.connection_id, "notes");
        assert_eq!(target.database, "main");
        assert_eq!(target.engine, "sqlite");
        assert_eq!(target.environment, "dev");
        desk.set_draft("SELECT 1");
        match desk.begin_run(&tasks, RunScope::Script, None) {
            BeginRun::Ready(prepared) => {
                assert_eq!(prepared.sql, "SELECT 1");
                assert_eq!(prepared.connection_id, "notes");
                assert_eq!(prepared.database, "main");
            }
            BeginRun::Rejected(reason) => panic!("{reason}"),
            BeginRun::Variables(_) => panic!("variables"),
        }
    }

    #[test]
    fn catalog_body_lists_tables_for_remote_engines() {
        // 没有连接 PostgreSQL、MySQL、MariaDB 或 SQL Server。这里只用 catalog 结构证明对象会进树。
        for (engine, schema) in [("postgres", "public"), ("mysql", ""), ("mariadb", ""), ("sqlserver", "dbo")] {
            let body = crate::catalog::ConnectionBody {
                error: None,
                foreign_keys: None,
                databases: vec![crate::catalog::DatabaseNode {
                    name: "app".to_owned(),
                    schemas: vec![crate::catalog::SchemaNode {
                        name: schema.to_owned(),
                        objects: vec![crate::catalog::ObjectRow::table("orders", None), crate::catalog::ObjectRow::view("orders_v")],
                        total: 2,
                        error: None,
                        kind_reports: Vec::new(),
                    }],
                    error: None,
                }],
            };
            let listing = objects_from_catalog("c1", "应用", engine, &body);
            assert!(listing.failures.is_empty(), "{engine}");
            assert_eq!(listing.databases, vec!["app".to_owned()]);
            let table = listing.objects.iter().find(|object| object.name == "orders").expect(engine);
            assert_eq!(table.kind, "table");
            assert_eq!(table.engine, engine);
            assert_eq!(table.database, "app");
            assert_eq!(table.connection_id, "c1");
            assert_eq!(table.schema, schema);
            let view = listing.objects.iter().find(|object| object.name == "orders_v").expect(engine);
            assert_eq!(view.kind, "view");
            assert_eq!(view.connection_name, "应用");
        }
    }

    #[test]
    fn truncated_result_is_kept_for_load_more() {
        let view = statement_view(commands::StatementDto {
            sql: "SELECT * FROM item".to_owned(),
            status: "ok".to_owned(),
            message: String::new(),
            columns: vec!["id".to_owned()],
            column_types: vec!["INTEGER".to_owned()],
            rows: vec![vec![Some("1".to_owned())]],
            truncated: true,
            affected: None,
        });
        assert!(view.truncated, "截断标记必须跟到界面，才能提供加载更多");
    }

    #[test]
    fn load_more_can_raise_the_read_limit() {
        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        desk.new_query();
        desk.open_database("notes", "笔记", "sqlite", "main");
        desk.set_draft("SELECT * FROM item");
        let prepared = match desk.begin_run_limited(
            &tasks,
            RunScope::Current,
            Some("SELECT * FROM item".to_owned()),
            250,
        ) {
            BeginRun::Ready(prepared) => prepared,
            BeginRun::Rejected(reason) => panic!("{reason}"),
            BeginRun::Variables(_) => panic!("variables"),
        };
        assert_eq!(prepared.limit, 250);
    }

    fn ready_ticket(desk: &mut Desk, tasks: &TaskBoard, sql: &str) -> Ticket {
        desk.set_draft(sql);
        match desk.begin_run(tasks, RunScope::Script, None) {
            BeginRun::Ready(prepared) => Ticket { tab_id: prepared.tab_id, token: prepared.token, version: prepared.version },
            BeginRun::Rejected(reason) => panic!("{reason}"),
            BeginRun::Variables(_) => panic!("variables"),
        }
    }

    #[test]
    fn a_restarted_run_ignores_the_previous_ticket() {
        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        desk.open_database("notes", "笔记", "sqlite", "main");
        let old = ready_ticket(&mut desk, &tasks, "SELECT 1");
        assert!(desk.cancel(&tasks).is_none());
        assert_eq!(desk.status(), "cancelled");
        assert!(desk.running_ids().is_empty());
        let restarted = ready_ticket(&mut desk, &tasks, "SELECT 2");
        assert!(desk.running_ids().contains(&restarted.tab_id));
        desk.finish_polled(&tasks, &old, Ok(empty_run("old-ok")), "已取消");
        assert!(desk.running_ids().contains(&restarted.tab_id), "旧取消终态清掉了新的 running");
        assert_eq!(desk.status(), "running");
        assert!(desk.results().is_empty());
        assert!(desk.note_of(&restarted.tab_id).is_empty());
        desk.finish_polled(&tasks, &old, Err("旧错误".to_owned()), "已取消");
        assert!(desk.running_ids().contains(&restarted.tab_id));
        assert_ne!(desk.status(), "error");
        assert!(desk.accept_run(&tasks, &restarted, empty_run("new-ok")));
        desk.set_note(&restarted.tab_id, "");
        assert_eq!(desk.results().len(), 1);
        desk.finish_polled(&tasks, &old, Ok(empty_run("old-after")), "已取消");
        desk.finish_polled(&tasks, &old, Err("旧错误".to_owned()), "已取消");
        assert!(desk.note_of(&restarted.tab_id).is_empty(), "旧终态补上了取消文案或旧错误");
        assert_eq!(desk.results().len(), 1);
        assert_eq!(desk.status(), "ok");
        assert!(!desk.running_ids().contains(&restarted.tab_id));

        let superseded = ready_ticket(&mut desk, &tasks, "SELECT 3");
        assert!(desk.load_draft("SELECT loaded", None, &tasks));
        assert!(!desk.executed());
        assert!(desk.results().is_empty());
        assert_eq!(desk.draft(), "SELECT loaded");
        let again = ready_ticket(&mut desk, &tasks, "SELECT 4");
        desk.finish_polled(&tasks, &superseded, Ok(empty_run("stale-ok")), "已取消");
        desk.finish_polled(&tasks, &superseded, Err("过期错误".to_owned()), "已取消");
        assert!(desk.running_ids().contains(&again.tab_id), "load_draft 之后的旧终态清掉了新的 running");
        assert!(desk.results().is_empty());
        assert!(desk.accept_run(&tasks, &again, empty_run("again-ok")));
        desk.set_note(&again.tab_id, "");
        desk.finish_polled(&tasks, &superseded, Err("过期错误".to_owned()), "已取消");
        assert!(desk.note_of(&again.tab_id).is_empty());
        assert_eq!(desk.results().len(), 1);
        assert_eq!(desk.status(), "ok");
    }

    #[test]
    fn completion_scope_reports_metadata_state_and_keeps_frequency_local() {
        let mut desk = Desk::new();
        desk.new_query();
        let no_target = desk.completion_scope().expect("没有目标也要有上下文");
        assert_eq!(no_target.state, MetadataState::NoTarget);

        desk.open_database("notes", "笔记", "sqlite", "main");
        desk.set_provider(true);
        desk.set_objects(
            vec![TreeObject {
                connection_id: "notes".to_owned(),
                connection_name: "笔记".to_owned(),
                engine: "sqlite".to_owned(),
                database: "main".to_owned(),
                schema: String::new(),
                name: "item".to_owned(),
                kind: "table".to_owned(),
            }],
            vec!["main".to_owned()],
        );
        desk.set_links(vec![OpenLink {
            id: "notes".to_owned(),
            name: "笔记".to_owned(),
            engine: "sqlite".to_owned(),
            databases: vec!["main".to_owned(), "archive".to_owned()],
            environment: "dev".to_owned(),
        }]);
        desk.remember_columns("notes", "main", "item", vec!["id".to_owned(), "status".to_owned()]);
        let ready = desk.completion_scope().expect("有目标");
        assert_eq!(ready.state, MetadataState::Ready);
        assert!(ready.objects.iter().any(|object| object.name == "item"));
        assert!(ready.sources.iter().any(|source| source.columns.contains(&"status".to_owned())));
        assert_eq!(ready.databases, vec!["main".to_owned(), "archive".to_owned()]);

        // 一个 schema 失败不等于整体失败：已读到的对象和列继续可用，状态是 Partial。
        desk.set_tree_error("读取库表失败：连接中断");
        let partial = desk.completion_scope().expect("部分失败也要有上下文");
        assert_eq!(partial.state, MetadataState::Partial("读取库表失败：连接中断".to_owned()));
        assert!(partial.objects.iter().any(|object| object.name == "item"), "部分失败时已读对象仍然可用");
        assert!(partial.sources.iter().any(|source| source.columns.contains(&"status".to_owned())));
        desk.set_tree_error("");

        // 树里有对象但没有当前目标的：说明可能过期，而不是空目录。
        desk.select_database("archive");
        let stale = desk.completion_scope().expect("过期也要有上下文");
        assert!(matches!(stale.state, MetadataState::Stale(_)), "{:?}", stale.state);
        assert!(stale.objects.is_empty());
        desk.select_database("main");

        desk.record_frequency("SELECT status FROM item WHERE status = 'status' -- status");
        desk.record_frequency("SELECT id FROM item");
        let scope = desk.completion_scope().expect("有目标");
        let weight = |name: &str| scope.frequency.iter().find(|(key, _)| key == name).map(|(_, count)| *count);
        assert_eq!(weight("status"), Some(2), "字符串和注释里的词不计数");
        assert_eq!(weight("id"), Some(1));
        assert_eq!(weight("item"), Some(2));
        assert_eq!(weight("select"), None, "关键字不计入词频");
    }

    #[test]
    fn schema_aware_columns_and_foreign_keys_follow_the_target() {
        let mut desk = Desk::new();
        desk.new_query();
        desk.open_database("pg", "销售库", "postgres", "shop");
        desk.set_provider(true);
        desk.set_objects(
            vec![
                TreeObject {
                    connection_id: "pg".to_owned(),
                    connection_name: "销售库".to_owned(),
                    engine: "postgres".to_owned(),
                    database: "shop".to_owned(),
                    schema: "public".to_owned(),
                    name: "orders".to_owned(),
                    kind: "table".to_owned(),
                },
                TreeObject {
                    connection_id: "pg".to_owned(),
                    connection_name: "销售库".to_owned(),
                    engine: "postgres".to_owned(),
                    database: "shop".to_owned(),
                    schema: "archive".to_owned(),
                    name: "orders".to_owned(),
                    kind: "table".to_owned(),
                },
            ],
            vec!["shop".to_owned()],
        );
        // 同名表在不同模式里各记各的列，不互相覆盖。
        desk.remember_table_columns("pg", "shop", "public", "orders", vec!["id".to_owned(), "customer_id".to_owned()]);
        desk.remember_table_columns("pg", "shop", "archive", "orders", vec!["id".to_owned(), "archived_at".to_owned()]);
        desk.set_foreign_keys(vec![
            assist::ForeignKey {
                connection_id: "pg".to_owned(),
                database: "shop".to_owned(),
                from_schema: "public".to_owned(),
                from_table: "orders".to_owned(),
                from_columns: vec!["customer_id".to_owned()],
                to_schema: "public".to_owned(),
                to_table: "customers".to_owned(),
                to_columns: vec!["id".to_owned()],
            },
            assist::ForeignKey {
                connection_id: "other".to_owned(),
                database: "shop".to_owned(),
                from_schema: "public".to_owned(),
                from_table: "orders".to_owned(),
                from_columns: vec!["id".to_owned()],
                to_schema: "public".to_owned(),
                to_table: "customers".to_owned(),
                to_columns: vec!["id".to_owned()],
            },
        ]);
        let scope = desk.completion_scope().expect("有目标");
        let public = scope.sources.iter().find(|source| source.label == "public.orders").expect("public.orders");
        assert_eq!(public.columns, vec!["id".to_owned(), "customer_id".to_owned()]);
        let archive = scope.sources.iter().find(|source| source.label == "archive.orders").expect("archive.orders");
        assert_eq!(archive.columns, vec!["id".to_owned(), "archived_at".to_owned()]);
        assert_eq!(scope.foreign_keys.len(), 1, "只保留当前连接的外键");
        assert_eq!(scope.foreign_keys[0].from_schema, "public");

        // 别名限定符只给该表自己的列。
        let planned = desk.completion_for("SELECT * FROM public.orders o WHERE o.", 39, Locale::ZhCn);
        let labels: Vec<String> = planned.iter().map(|entry| entry.label.clone()).collect();
        assert!(labels.contains(&"customer_id".to_owned()), "{labels:?}");
        assert!(!labels.contains(&"archived_at".to_owned()), "{labels:?}");
    }

    #[test]
    fn catalog_errors_are_not_reported_as_no_objects() {
        // 没有连接远程引擎。失败体只证明 error 不会被当成空对象树。
        let connection = crate::catalog::ConnectionBody {
            error: Some("读取库表失败：连接中断".to_owned()),
            foreign_keys: None,
            databases: Vec::new(),
        };
        let failed = objects_from_catalog("c1", "应用", "postgres", &connection);
        assert!(failed.objects.is_empty());
        assert!(failed.databases.is_empty());
        assert!(failed.failures.iter().any(|item| item.contains("连接中断")));

        let database = crate::catalog::ConnectionBody {
            error: None,
            foreign_keys: None,
            databases: vec![crate::catalog::DatabaseNode {
                name: "app".to_owned(),
                schemas: Vec::new(),
                error: Some("库不可用".to_owned()),
            }],
        };
        let failed_database = objects_from_catalog("c1", "应用", "mysql", &database);
        assert!(failed_database.failures.iter().any(|item| item.contains("库不可用")));
        assert_eq!(failed_database.databases, vec!["app".to_owned()]);

        let schema = crate::catalog::ConnectionBody {
            error: None,
            foreign_keys: None,
            databases: vec![crate::catalog::DatabaseNode {
                name: "app".to_owned(),
                error: None,
                schemas: vec![crate::catalog::SchemaNode {
                    name: "public".to_owned(),
                    objects: Vec::new(),
                    total: 0,
                    error: Some("schema 读取失败".to_owned()),
                    kind_reports: Vec::new(),
                }],
            }],
        };
        let failed_schema = objects_from_catalog("c1", "应用", "sqlserver", &schema);
        assert!(failed_schema.objects.is_empty());
        assert!(failed_schema.failures.iter().any(|item| item.contains("schema 读取失败")));

        let mut desk = Desk::new();
        desk.set_objects(failed.objects, failed.databases);
        desk.set_tree_error(&failed.failures.join("\n"));
        assert!(desk.objects().is_empty());
        assert!(!desk.tree_is_empty(), "读取失败不能显示成没有对象");
        desk.set_tree_error("");
        assert!(desk.tree_is_empty());
    }
}
