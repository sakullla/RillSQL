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
    self, CatalogObject, ColumnMeta, CompletionEntry, CompletionSource, LintFinding, MetadataState, OutlineItem, RunPick, RunScope, TextResult,
    TreeObject, VariableInput,
};
use super::complete;
use super::commands::{self, RunDto, StartRequest};
use super::exec::{ErrorAt, LiveDriver, StatementDriver};

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
    /// 这一条语句自己的耗时（毫秒）；执行中、被取消或没有执行时为 None。
    pub elapsed_ms: Option<u64>,
    /// 驱动报出的错误位置，相对这一条语句。
    pub error_at: Option<ErrorAt>,
    /// 这条语句在发起执行时草稿里的字节范围；对不上草稿时为 None，不猜位置。
    pub range: Option<(usize, usize)>,
    /// 失败语句在草稿里的错误标记：有驱动位置时给偏移或行，没有时整条语句标错。
    pub error_mark: Option<ErrorMark>,
}

/// 失败语句在草稿里的位置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorMark {
    /// 草稿里的字节偏移（驱动给的是字符位置，已换算）。
    Offset(usize),
    /// 草稿里从 0 开始的行号。
    Line(usize),
    /// 驱动没有给位置（如 SQLite）：整条语句的 range 标错。
    Statement,
}

/// 消息页里的一条执行记录：按执行顺序，每条语句一条；整次执行失败或被取消时补一条整体记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    /// 这条记录对应的结果序号（从 0 开始）；整次执行的失败/取消记录为 None。
    pub statement: Option<usize>,
    pub sql: String,
    /// ok / error / skipped / cancelled / unknown，与结果状态同一套写法。
    pub status: String,
    /// 有结果集时读取到的行数。
    pub rows: Option<usize>,
    pub affected: Option<u64>,
    pub elapsed_ms: Option<u64>,
    /// 错误或说明文本；成功语句为空。
    pub message: String,
}

impl LogEntry {
    pub fn failed(&self) -> bool {
        self.status == "error" || self.status == "failed"
    }
}

/// 右侧对象面板的内容：光标处标识符解析出的目录对象及其列。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectInfo {
    pub schema: String,
    pub name: String,
    pub kind: String,
    pub comment: String,
    pub columns: Vec<ColumnMeta>,
    /// 光标落在某一列上时是这一列的名字。
    pub column: Option<String>,
}

/// 被固定的结果：自己保留一份结果快照，之后的执行不替换它，只能手动关闭。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedResult {
    pub id: u64,
    /// 固定时它是第几次执行（标签内从 1 开始）的第几条结果（从 0 开始）。
    pub run: u64,
    pub statement: usize,
    pub view: StatementView,
    /// 产生这份结果的那次执行用的目标（连接/库/模式）。显示固定结果时按它绑定表格，
    /// 不跟着标签之后切换的库走；来源无法证明（外部写入的结果）时为 None，表格按未定位只读处理。
    pub target: Option<Target>,
    /// 用户给这个固定页签起的名字；没有时用默认的“固定 n · 结果 m”。
    pub custom_title: Option<String>,
}

/// 结果区当前显示的是哪一页：当前结果、某个固定结果，或消息页。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResultView {
    #[default]
    Current,
    Pinned(u64),
    Messages,
}

/// 一批最终结果对应的消息记录。
pub fn log_entries(results: &[StatementView]) -> Vec<LogEntry> {
    results
        .iter()
        .enumerate()
        .map(|(index, view)| LogEntry {
            statement: Some(index),
            sql: view.sql.clone(),
            status: view.status.clone(),
            rows: (!view.columns.is_empty()).then_some(view.rows.len()),
            affected: view.affected,
            elapsed_ms: view.elapsed_ms,
            message: view.message.clone(),
        })
        .collect()
}

/// 发起执行时这批 SQL 在草稿里的位置：from 是 pick 在草稿里的起点，sql 是实际送出的文本。
#[derive(Clone, Debug, PartialEq, Eq)]
struct RunOrigin {
    /// 草稿里这次执行对应的文本及其起点。
    from: usize,
    sql: String,
    /// 实际送出的文本；变量替换后与 sql 不同。
    sent: String,
    engine: String,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct PendingSend {
    sql: String,
    pick: RunPick,
    /// confirm_variables 替换后的文本；随后按它发起的执行仍对应原来的 pick。
    confirmed: Option<String>,
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
    /// 最近一次发起执行的位置；结果回来时据此把错误位置换算到草稿。
    run_origin: Option<RunOrigin>,
    /// 最近一次发起执行时的目标；结果写进 results 时随之记到 results_target。
    run_target: Option<Target>,
    /// 当前 results 来自哪个目标。外部写入或已清空时为 None（来源无法证明）。
    results_target: Option<Target>,
    /// results 是否来自最近一次发起的执行；发起时置假，收到它的快照后置真。
    results_fresh: bool,
    /// 固定的结果；之后的执行不替换，只能手动关闭。
    pinned: Vec<PinnedResult>,
    /// 最近一次执行的消息记录；每次发起执行时清空，只保留当前这一批。
    log: Vec<LogEntry>,
    /// 这一批消息之前清空过几次（即这是第几次执行的记录）。
    log_batch: u64,
    result_view: ResultView,
    /// 已完成的执行次数；固定结果记下自己来自哪一次。
    run_seq: u64,
    pin_seq: u64,
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
    /// 按连接/库/模式/表记住的列及其元数据；同一个表名在不同模式里不互相覆盖。
    columns: HashMap<String, Vec<ColumnMeta>>,
    /// 按连接/库/模式/表记住的对象注释；读不到注释的对象不在表里。
    comments: HashMap<String, String>,
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
            comments: HashMap::new(),
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
            run_origin: None,
            run_target: None,
            results_target: None,
            results_fresh: false,
            pinned: Vec::new(),
            log: Vec::new(),
            log_batch: 0,
            result_view: ResultView::Current,
            run_seq: 0,
            pin_seq: 0,
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
        tab.results_target = None;
        tab.results_fresh = false;
        tab.log.clear();
        if tab.result_view == ResultView::Messages {
            tab.result_view = ResultView::Current;
        }
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
                elapsed_ms: None,
                error_at: None,
                range: None,
                error_mark: None,
            }];
            // 外部写入的结果说不清来自哪个库：固定后按未定位只读处理。
            tab.results_target = None;
            tab.results_fresh = false;
            tab.statement = 0;
        });
        if wrote.is_some() {
            self.view_gen = self.view_gen.saturating_add(1);
        }
    }

    /// 测试用：把当前草稿记成“刚执行过的文本”，错误定位才认这段位置。
    #[cfg(test)]
    pub fn remember_executed_draft(&mut self) {
        if let Some(tab) = self.active_mut() {
            tab.executed_sql = Some(tab.draft.clone());
        }
    }

    /// 测试用：把一批最终结果当作一次完成的执行写到活动标签（结果、消息、执行次数）。
    #[cfg(test)]
    pub fn plant_run(&mut self, results: Vec<StatementView>) {
        let wrote = self.active_mut().map(|tab| {
            tab.log = log_entries(&results);
            tab.log_batch = tab.log_batch.saturating_add(1);
            tab.results = results;
            tab.results_target = tab.target.clone();
            tab.statement = 0;
            tab.run_seq = tab.run_seq.saturating_add(1);
            tab.result_view = ResultView::Current;
        });
        if wrote.is_some() {
            self.view_gen = self.view_gen.saturating_add(1);
        }
    }

    /// 活动标签最近一次执行的消息记录。
    pub fn log(&self) -> Vec<LogEntry> {
        self.active_tab().map(|tab| tab.log.clone()).unwrap_or_default()
    }

    /// 消息记录是第几次执行的；从未执行过为 0。
    pub fn log_batch(&self) -> u64 {
        self.active_tab().map(|tab| tab.log_batch).unwrap_or(0)
    }

    pub fn pinned(&self) -> Vec<PinnedResult> {
        self.active_tab().map(|tab| tab.pinned.clone()).unwrap_or_default()
    }

    pub fn result_view(&self) -> ResultView {
        self.active_tab().map(|tab| tab.result_view).unwrap_or_default()
    }

    /// 切换结果区页签。固定结果已经关闭时不切换。
    /// 消息页与当前结果共用同一份表格内容，它们之间切换不算结果变化（表格里的暂存和选区保留）；
    /// 只有切到或切离固定结果才让表格重新绑定。
    pub fn set_result_view(&mut self, view: ResultView) -> bool {
        let Some(tab) = self.active_mut() else {
            return false;
        };
        if let ResultView::Pinned(id) = view {
            if !tab.pinned.iter().any(|pinned| pinned.id == id) {
                return false;
            }
        }
        let shown = |view: ResultView| match view {
            ResultView::Pinned(id) => Some(id),
            ResultView::Current | ResultView::Messages => None,
        };
        let rebind = shown(tab.result_view) != shown(view);
        tab.result_view = view;
        if rebind {
            self.view_gen = self.view_gen.saturating_add(1);
        }
        true
    }

    /// 当前结果里正在看的那一条能否固定：已经执行完、有结果，且这条还没固定过。
    pub fn can_pin(&self) -> bool {
        let Some(tab) = self.active_tab() else {
            return false;
        };
        if tab.running || tab.result_view != ResultView::Current || tab.results.is_empty() {
            return false;
        }
        let statement = tab.statement.min(tab.results.len() - 1);
        !tab.pinned.iter().any(|pinned| pinned.run == tab.run_seq && pinned.statement == statement)
    }

    /// 把当前正在看的结果固定成自己的页签（保留一份快照），并切到它。
    pub fn pin_current(&mut self) -> Option<u64> {
        if !self.can_pin() {
            return None;
        }
        let tab = self.active_mut()?;
        let statement = tab.statement.min(tab.results.len() - 1);
        tab.pin_seq = tab.pin_seq.saturating_add(1);
        let id = tab.pin_seq;
        let target = tab.results_target.clone();
        tab.pinned.push(PinnedResult { id, run: tab.run_seq, statement, view: tab.results[statement].clone(), target, custom_title: None });
        tab.result_view = ResultView::Pinned(id);
        self.view_gen = self.view_gen.saturating_add(1);
        Some(id)
    }

    /// 关掉一个固定结果；正在看它时回到当前结果。
    pub fn unpin(&mut self, id: u64) -> bool {
        let Some(tab) = self.active_mut() else {
            return false;
        };
        let before = tab.pinned.len();
        tab.pinned.retain(|pinned| pinned.id != id);
        if tab.pinned.len() == before {
            return false;
        }
        if tab.result_view == ResultView::Pinned(id) {
            tab.result_view = ResultView::Current;
        }
        self.view_gen = self.view_gen.saturating_add(1);
        true
    }

    /// 给固定页签一个名字。空白恢复成默认标题。不存在的 id 不改。
    pub fn rename_pin(&mut self, id: u64, title: &str) -> bool {
        let Some(pin) = self.active_mut().and_then(|tab| tab.pinned.iter_mut().find(|pin| pin.id == id)) else {
            return false;
        };
        let trimmed = title.trim();
        pin.custom_title = if trimmed.is_empty() { None } else { Some(trimmed.to_owned()) };
        true
    }

    /// 结果表格要显示的那一条：看固定结果时是它的快照，否则是当前结果里选中的语句。
    pub fn shown_result(&self) -> Option<StatementView> {
        let tab = self.active_tab()?;
        if let ResultView::Pinned(id) = tab.result_view {
            if let Some(pinned) = tab.pinned.iter().find(|pinned| pinned.id == id) {
                return Some(pinned.view.clone());
            }
        }
        if tab.results.is_empty() {
            return None;
        }
        tab.results.get(tab.statement.min(tab.results.len() - 1)).cloned()
    }

    /// 正在看固定结果时它自己的来源目标：Some(Some) 是产生它的那次执行的目标，Some(None) 是来源无法证明；
    /// 不在看固定结果时为 None（表格按标签当前目标绑定）。
    /// 当前结果来自哪个目标；外部写入或尚无执行结果时为 None。
    pub fn results_target(&self) -> Option<Target> {
        self.active_tab().and_then(|tab| tab.results_target.clone())
    }

    pub fn shown_pinned_target(&self) -> Option<Option<Target>> {
        let tab = self.active_tab()?;
        let ResultView::Pinned(id) = tab.result_view else {
            return None;
        };
        tab.pinned.iter().find(|pinned| pinned.id == id).map(|pinned| pinned.target.clone())
    }

    /// 光标处标识符对应的对象说明（右侧对象面板）。列也解析到它所在的对象，并记下是哪一列。
    pub fn object_info_at(&self, text: &str, offset: usize) -> Option<ObjectInfo> {
        let found = self.resolve_at(text, offset)?;
        let target = self.target()?;
        let mut info = match found.target {
            complete::Resolved::Object { schema, name, kind, comment, columns } => {
                ObjectInfo { schema, name, kind, comment, columns, column: None }
            }
            complete::Resolved::Column { schema, table, column, .. } => {
                let object = self.objects.iter().find(|object| {
                    object.connection_id == target.connection_id
                        && object.database == target.database
                        && object.schema.eq_ignore_ascii_case(&schema)
                        && object.name.eq_ignore_ascii_case(&table)
                })?;
                let key = column_key(&object.connection_id, &object.database, &object.schema, &object.name);
                ObjectInfo {
                    schema: object.schema.clone(),
                    name: object.name.clone(),
                    kind: object.kind.clone(),
                    comment: self.comments.get(&key).cloned().unwrap_or_default(),
                    columns: self.columns.get(&key).cloned().unwrap_or_default(),
                    column: Some(column.name),
                }
            }
        };
        // 主键留在列元数据里；外键按已读到的清单标到这一张表的列上。
        for column in &mut info.columns {
            column.foreign_key = self.column_is_fk(&target.connection_id, &target.database, &info.schema, &info.name, &column.name);
        }
        Some(info)
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
            // 已经流式到达的语句先按顺序记进消息页，再记整次执行的失败。
            fill_partial_log(tab);
            // 整次执行没有结果就失败（连接中断等）：消息页也要有这一条。
            tab.log.push(LogEntry {
                statement: None,
                sql: tab.run_origin.as_ref().map(|origin| origin.sent.clone()).unwrap_or_default(),
                status: "error".to_owned(),
                rows: None,
                affected: None,
                elapsed_ms: None,
                message: note.to_owned(),
            });
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
        tab.results_target = None;
        tab.results_fresh = false;
        tab.log.clear();
        if tab.result_view == ResultView::Messages {
            tab.result_view = ResultView::Current;
        }
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

    /// 收到一次执行快照。分批读取时先到的是中间快照，只有终态才算执行结束：
    /// 中间快照更新结果、状态和 run_id，但不写 executed，也不放开第二次执行。
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
        let pending = run_pending(&run.status);
        let accepted = if tab.cancelled {
            tab.running = false;
            tab.status = "cancelled".to_owned();
            false
        } else {
            match outcome {
                TaskOutcome::Current { .. } => {
                    tab.running = pending;
                    tab.status = run.status.clone();
                    let mut results: Vec<StatementView> = run.statements.into_iter().map(statement_view).collect();
                    if let Some(origin) = tab.run_origin.as_ref() {
                        place_statements(&tab.draft, origin, &mut results);
                    }
                    tab.results = results;
                    tab.results_target = tab.run_target.clone();
                    tab.results_fresh = true;
                    tab.run_id = Some(run.id);
                    if !pending {
                        tab.executed = true;
                        tab.executed_sql = Some(tab.draft.clone());
                        tab.statement = 0;
                        tab.run_seq = tab.run_seq.saturating_add(1);
                        tab.log = log_entries(&tab.results);
                    }
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

    /// 这次执行是否已经记到标签上；轮询拿到同一批快照时不重复发起取消。
    pub fn run_noted(&self, id: &str, run_id: &str) -> bool {
        self.tabs.iter().any(|tab| tab.id == id && tab.run_id.as_deref() == Some(run_id))
    }

    pub fn cancel(&mut self, tasks: &TaskBoard) -> Option<String> {
        let tab = self.active_mut()?;
        if let Some(token) = tab.token {
            tasks.cancel(&owner(&tab.id), token.id);
        }
        let was_running = tab.running;
        tab.cancelled = true;
        tab.running = false;
        tab.status = "cancelled".to_owned();
        if was_running {
            // 取消前已经完成的语句先按顺序记进消息页，再记整次执行被取消。
            fill_partial_log(tab);
            tab.log.push(LogEntry {
                statement: None,
                sql: tab.run_origin.as_ref().map(|origin| origin.sent.clone()).unwrap_or_default(),
                status: "cancelled".to_owned(),
                rows: None,
                affected: None,
                elapsed_ms: None,
                message: String::new(),
            });
        }
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
                    tab.pending = Some(PendingSend { sql: pick.sql.clone(), pick: pick.clone(), confirmed: None });
                }
                return BeginRun::Variables(variables);
            }
        }
        let token = tasks.begin(&owner(&tab.id));
        let origin = run_origin(&tab, &pick, override_sql.is_some(), &target.engine);
        if let Some(tab) = self.active_mut() {
            tab.running = true;
            tab.cancelled = false;
            tab.token = Some(token);
            tab.status = "running".to_owned();
            tab.pending = None;
            tab.draft = original;
            tab.run_origin = Some(origin);
            tab.run_target = Some(target.clone());
            tab.results_fresh = false;
            // 消息页只保留当前这一批；清空时记下批次，消息页据此说明这是第几次执行。
            tab.log.clear();
            tab.log_batch = tab.log_batch.saturating_add(1);
            // 新结果落在“当前结果”页；固定的结果不动。
            tab.result_view = ResultView::Current;
        }
        self.view_gen = self.view_gen.saturating_add(1);
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
        let pending = self.active_mut()?.pending.as_mut()?;
        let sql = assist::substitute_variables(&pending.sql, values);
        pending.confirmed = Some(sql.clone());
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
        columns: Vec<ColumnMeta>,
    ) {
        self.columns.insert(column_key(connection_id, database, schema, table), columns);
    }

    /// 只有库和表、没有模式的引擎（SQLite/MySQL）用它；只知道列名，类型和注释留空。
    pub fn remember_columns(&mut self, connection_id: &str, database: &str, table: &str, columns: Vec<String>) {
        self.remember_table_columns(connection_id, database, "", table, columns.into_iter().map(ColumnMeta::named).collect());
    }

    /// 对象注释随目录一起替换；键与列相同（连接/库/模式/表）。
    pub fn set_object_comments(&mut self, comments: HashMap<String, String>) {
        self.comments = comments;
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

    /// 已保存的列，外键标记按当前 Desk 的外键清单补上。主键留在列元数据里，不在这里推断。
    fn marked_columns(&self, connection_id: &str, database: &str, schema: &str, table: &str) -> Vec<ColumnMeta> {
        let mut columns = self.columns.get(&column_key(connection_id, database, schema, table)).cloned().unwrap_or_default();
        for column in &mut columns {
            column.foreign_key = self.column_is_fk(connection_id, database, schema, table, &column.name);
        }
        columns
    }

    fn column_is_fk(&self, connection_id: &str, database: &str, schema: &str, table: &str, column: &str) -> bool {
        self.foreign_keys.iter().any(|key| {
            key.connection_id == connection_id
                && key.database == database
                && key.from_schema.eq_ignore_ascii_case(schema)
                && key.from_table.eq_ignore_ascii_case(table)
                && key.from_columns.iter().any(|name| name.eq_ignore_ascii_case(column))
        })
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
                columns: self.marked_columns(&object.connection_id, &object.database, &object.schema, &object.name),
            })
            .collect()
    }

    /// 一次补全请求：语句全文和光标由编辑器传入，补全计划只读快照。
    pub fn completion_for(&self, text: &str, cursor: usize, locale: Locale) -> Vec<CompletionEntry> {
        self.completion_scope()
            .map(|scope| complete::plan(&scope, text, cursor, locale).entries())
            .unwrap_or_default()
    }

    /// 把全文里的偏移解析成当前目标库里的对象或列（悬停、跳转定义、对象信息共用）。
    /// 解析不到时为 None。
    pub fn resolve_at(&self, text: &str, offset: usize) -> Option<complete::Resolution> {
        let scope = self.completion_scope()?;
        complete::resolve(&scope, text, offset)
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
            .map(|object| CatalogObject {
                name: object.name.clone(),
                schema: object.schema.clone(),
                kind: object.kind.clone(),
                comment: self
                    .comments
                    .get(&column_key(&object.connection_id, &object.database, &object.schema, &object.name))
                    .cloned()
                    .unwrap_or_default(),
            })
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
        // 目标范围的目录是否真的枚举过：连接里列出了这个库，说明读取覆盖了它，
        // 此时没有对象就是真的空库；覆盖不到才按“可能过期”处理。
        let target_range_known = tab.target.as_ref().is_some_and(|target| {
            self.links
                .iter()
                .any(|link| link.id == target.connection_id && link.databases.iter().any(|name| name == &target.database))
        });
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
        } else if !target_range_known && !self.objects.is_empty() {
            // 树里有别的连接/库的对象，而当前目标范围没有读到过：状态可能过期。
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
            run_origin: None,
            run_target: None,
            results_target: None,
            results_fresh: false,
            pinned: Vec::new(),
            log: Vec::new(),
            log_batch: 0,
            result_view: ResultView::Current,
            run_seq: 0,
            pin_seq: 0,
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
    pub columns: HashMap<String, Vec<ColumnMeta>>,
    /// 对象注释，键与 columns 相同；读不到注释时不出现。
    pub comments: HashMap<String, String>,
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
    let mut comments = HashMap::new();
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
                    match crate::catalog::detail::read_column_meta(&session, parsed, &object.database, &object.schema, &object.name).await {
                        Ok(meta) if !meta.is_empty() => {
                            columns.insert(
                                column_key(&object.connection_id, &object.database, &object.schema, &object.name),
                                meta,
                            );
                        }
                        Ok(_) => {}
                        Err(error) => failures.push(format!("{}: {error}", object_label(object))),
                    }
                }
                // 对象注释按库/模式读一次；读不到只是没有注释，不记成失败，也不编造。
                let mut scopes: Vec<(String, String)> = Vec::new();
                for object in &listing.objects {
                    let scope = (object.database.clone(), object.schema.clone());
                    if !scopes.contains(&scope) {
                        scopes.push(scope);
                    }
                }
                for (database, schema) in &scopes {
                    if let Ok(found) = crate::catalog::detail::read_table_comments(&session, parsed, database, schema).await {
                        for (name, comment) in found {
                            comments.insert(column_key(&record.saved.id, database, schema, &name), comment);
                        }
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
    Ok(TreeLoad { objects, databases, columns, comments, foreign_keys, links, failures })
}

async fn read_columns(service: &Arc<Service>, connection_id: &str, table: &str) -> Result<Vec<ColumnMeta>, String> {
    let session = service.catalog_session(connection_id).ok_or_else(|| "连接没有打开".to_owned())?;
    let transport = service.cancel_transport(connection_id).unwrap_or(crate::connections::CancelTransport::None);
    let driver = LiveDriver::attach(session, "sqlite", "main", transport, None).await?;
    let sql = format!("PRAGMA table_info({})", assist::quote_ident("sqlite", table));
    let flag = Arc::new(AtomicBool::new(false));
    let info = driver.run(&sql, 200, &flag).await.map_err(|stop| match stop {
        super::exec::StatementStop::Error { message, .. } | super::exec::StatementStop::Unknown { message } | super::exec::StatementStop::Cancelled { message } => message,
    })?;
    let _ = driver.finish().await;
    Ok(sqlite_column_meta(&info.columns, &info.rows))
}

/// PRAGMA table_info 的结果按列名取 name / type / notnull / pk；SQLite 没有列注释。
fn sqlite_column_meta(columns: &[String], rows: &[Vec<Option<String>>]) -> Vec<ColumnMeta> {
    let find = |name: &str| columns.iter().position(|column| column.eq_ignore_ascii_case(name));
    let name_at = find("name").unwrap_or(1);
    let type_at = find("type");
    let notnull_at = find("notnull");
    let pk_at = find("pk");
    rows.iter()
        .filter_map(|row| {
            let name = row.get(name_at).and_then(|cell| cell.clone())?;
            let type_name = type_at.and_then(|at| row.get(at).cloned().flatten()).unwrap_or_default();
            let nullable = notnull_at.and_then(|at| row.get(at).cloned().flatten()).map(|flag| flag.trim() != "1");
            let primary = pk_at.and_then(|at| row.get(at).cloned().flatten()).is_some_and(|flag| {
                let flag = flag.trim();
                !flag.is_empty() && flag != "0"
            });
            Some(ColumnMeta { name, type_name, nullable, comment: String::new(), primary, foreign_key: false })
        })
        .collect()
}

/// 快照是否还表示执行中。只有 running / cancel-requested 会继续等下一批行，
/// 其余状态都是终态（完成、失败、取消、结果未知）。
pub fn run_pending(status: &str) -> bool {
    matches!(status, "running" | "cancel-requested")
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
        elapsed_ms: statement.elapsed_ms,
        error_at: statement.error_at,
        range: None,
        error_mark: None,
    }
}

/// 执行被取消或整体失败时：本次执行已经流式到达的语句（不含仍在执行的那条）按顺序补进消息页。
/// 消息页在发起执行时已清空；已经有逐条记录（终态之后）时不重复补。
fn fill_partial_log(tab: &mut TabState) {
    if tab.log.iter().any(|entry| entry.statement.is_some()) {
        return;
    }
    // results 里可能还是上一次执行的结果：只有本次执行的快照写进来过才算数。
    if !tab.results_fresh {
        return;
    }
    let mut entries = log_entries(&tab.results);
    entries.retain(|entry| !matches!(entry.status.as_str(), "pending" | "running" | "cancel-requested"));
    tab.log = entries;
}

/// 这次执行对应草稿里的哪段文本。普通执行就是 pick；变量确认后的执行对应替换前的 pick；
/// “加载更多”重放上一条结果时对应那条结果在草稿里的范围。都对不上时退回送出的文本本身。
fn run_origin(tab: &TabState, pick: &RunPick, overridden: bool, engine: &str) -> RunOrigin {
    let sent = pick.sql.clone();
    let exact = || RunOrigin { from: pick.from, sql: sent.clone(), sent: sent.clone(), engine: engine.to_owned() };
    if !overridden {
        return exact();
    }
    if let Some(pending) = tab.pending.as_ref().filter(|pending| pending.confirmed.as_deref() == Some(sent.as_str())) {
        return RunOrigin { from: pending.pick.from, sql: pending.pick.sql.clone(), sent, engine: engine.to_owned() };
    }
    let replayed = tab.results.iter().filter(|view| view.sql == sent).find_map(|view| {
        let (start, end) = view.range?;
        tab.draft.get(start..end).map(|text| (start, text.to_owned()))
    });
    if let Some((from, sql)) = replayed {
        return RunOrigin { from, sql, sent, engine: engine.to_owned() };
    }
    exact()
}

/// 把这批结果放回草稿：按发起时草稿一侧的切分算出每条语句的范围，再把失败语句的错误位置加上语句起点。
/// 送出的语句与草稿里那条一致时按驱动给的位置标记；不一致（例如变量已替换）但两边切出的条数相同时，
/// 按序号对应，位置不再可信，失败语句整条标错。草稿在这期间改过时不给范围，也不给标记。
fn place_statements(draft: &str, origin: &RunOrigin, results: &mut [StatementView]) {
    let drafted = super::split::split_sql(&origin.engine, &origin.sql).pieces;
    let aligned = origin.sent != origin.sql
        && super::split::split_sql(&origin.engine, &origin.sent).pieces.len() == drafted.len();
    for (view, piece) in results.iter_mut().zip(drafted.iter()) {
        let exact = view.sql == piece.text;
        if !exact && !aligned {
            continue;
        }
        let start = origin.from + piece.start;
        let end = origin.from + piece.end;
        if draft.get(start..end) != Some(piece.text.as_str()) {
            continue;
        }
        view.range = Some((start, end));
        if view.status == "error" {
            let at = if exact { view.error_at } else { None };
            view.error_mark = Some(error_mark(draft, (start, end), at));
        }
    }
}

/// 语句内的错误位置换算成草稿位置。字符偏移换成字节偏移；行号加上语句起点所在行，
/// 并且不越过语句最后一行。没有位置时整条语句标错。
pub fn error_mark(draft: &str, range: (usize, usize), at: Option<ErrorAt>) -> ErrorMark {
    let (start, end) = range;
    let statement = draft.get(start..end).unwrap_or("");
    match at {
        Some(ErrorAt::Char(offset)) => {
            let byte = statement.char_indices().nth(offset).map(|(byte, _)| byte).unwrap_or(statement.len());
            ErrorMark::Offset(start + byte)
        }
        Some(ErrorAt::Line(line)) => {
            let first = draft.get(..start).unwrap_or("").matches('\n').count();
            let last = first + statement.matches('\n').count();
            ErrorMark::Line((first + line.saturating_sub(1)).min(last))
        }
        None => ErrorMark::Statement,
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
            super::exec::StatementStop::Error { message, .. } | super::exec::StatementStop::Unknown { message } | super::exec::StatementStop::Cancelled { message } => message.clone(),
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
                elapsed_ms: None,
                error_at: None,
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
            elapsed_ms: None,
            error_at: None,
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

    /// 分批读取的一次快照：行数随批次增长，status 描述这一批是否已结束。
    fn streaming_run(id: &str, status: &str, rows: &[&str]) -> RunDto {
        let mut run = empty_run(id);
        run.status = status.to_owned();
        let statement = &mut run.statements[0];
        statement.status = if run_pending(status) { "running" } else { "ok" }.to_owned();
        statement.rows = rows.iter().map(|value| vec![Some((*value).to_owned())]).collect();
        run
    }

    #[test]
    fn intermediate_snapshots_land_without_finishing_the_run() {
        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        desk.open_database("notes", "笔记", "sqlite", "main");
        let ticket = ready_ticket(&mut desk, &tasks, "SELECT n FROM item");
        assert!(desk.running_ids().contains(&ticket.tab_id));
        assert!(!desk.run_noted(&ticket.tab_id, "run-1"));

        // 第一批行到了：结果可见，但这条 SELECT 还没有结束。
        assert!(desk.accept_run(&tasks, &ticket, streaming_run("run-1", "running", &["1", "2"])));
        assert_eq!(desk.results()[0].rows.len(), 2);
        assert_eq!(desk.status(), "running");
        assert!(desk.running_ids().contains(&ticket.tab_id), "中间快照清掉了运行态");
        assert!(!desk.executed(), "只到一半的结果不能算已执行");
        assert_eq!(desk.executed_sql(&ticket.tab_id), None);
        assert!(desk.run_noted(&ticket.tab_id, "run-1"));
        match desk.begin_run(&tasks, RunScope::Script, None) {
            BeginRun::Rejected(reason) => assert!(reason.contains("正在执行"), "{reason}"),
            _ => panic!("中间快照之后又允许了一次执行"),
        }

        // 终态：行合并进最新结果，运行态和“已执行”这时才落定。
        assert!(desk.accept_run(&tasks, &ticket, streaming_run("run-1", "ok", &["1", "2", "3"])));
        assert_eq!(desk.results()[0].rows.len(), 3);
        assert!(!desk.running_ids().contains(&ticket.tab_id));
        assert_eq!(desk.status(), "ok");
        assert!(desk.executed());
        assert_eq!(desk.executed_sql(&ticket.tab_id).as_deref(), Some("SELECT n FROM item"));
    }

    #[test]
    fn a_cancel_requested_snapshot_still_counts_as_running() {
        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        desk.open_database("notes", "笔记", "sqlite", "main");
        let ticket = ready_ticket(&mut desk, &tasks, "SELECT n FROM item");
        desk.note_run(&ticket.tab_id, "run-2");
        assert!(desk.accept_run(&tasks, &ticket, streaming_run("run-2", "cancel-requested", &["1"])));
        assert!(desk.running_ids().contains(&ticket.tab_id), "取消请求还没落地，仍要等终态");
        assert_eq!(desk.status(), "cancel-requested");
        assert!(!desk.executed());
        assert!(desk.accept_run(&tasks, &ticket, streaming_run("run-2", "cancelled", &["1"])));
        assert!(!desk.running_ids().contains(&ticket.tab_id));
        assert_eq!(desk.status(), "cancelled");
    }

    /// 三条语句（第三条失败）的一次执行快照。
    fn three_statement_run(id: &str, marker: &str) -> RunDto {
        let mut run = empty_run(id);
        let ok = run.statements[0].clone();
        let mut first = ok.clone();
        first.sql = format!("SELECT '{marker}'");
        first.columns = vec!["v".to_owned()];
        first.rows = vec![vec![Some(marker.to_owned())]];
        first.elapsed_ms = Some(3);
        let mut second = ok.clone();
        second.sql = "UPDATE item SET n = 1".to_owned();
        second.columns = Vec::new();
        second.rows = Vec::new();
        second.affected = Some(2);
        second.elapsed_ms = Some(4);
        let mut third = ok;
        third.sql = "SELECT * FROM missing".to_owned();
        third.status = "error".to_owned();
        third.message = "no such table: missing".to_owned();
        third.columns = Vec::new();
        third.rows = Vec::new();
        third.elapsed_ms = Some(1);
        run.status = "error".to_owned();
        run.statements = vec![first, second, third];
        run
    }

    #[test]
    fn the_log_lists_each_statement_and_is_cleared_per_run() {
        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        desk.open_database("notes", "笔记", "sqlite", "main");
        assert!(desk.log().is_empty());
        assert_eq!(desk.log_batch(), 0);
        let ticket = ready_ticket(&mut desk, &tasks, "SELECT 'a'; UPDATE item SET n = 1; SELECT * FROM missing");
        assert_eq!(desk.log_batch(), 1, "发起执行时记下新的一批");
        // 中间快照不写消息：只有终态才是这一批的记录。
        assert!(desk.accept_run(&tasks, &ticket, streaming_run("run-1", "running", &["1"])));
        assert!(desk.log().is_empty());
        assert!(desk.accept_run(&tasks, &ticket, three_statement_run("run-1", "a")));
        let log = desk.log();
        assert_eq!(log.len(), 3, "{log:?}");
        assert_eq!(log.iter().map(|entry| entry.status.as_str()).collect::<Vec<_>>(), vec!["ok", "ok", "error"]);
        assert_eq!(log.iter().map(|entry| entry.statement).collect::<Vec<_>>(), vec![Some(0), Some(1), Some(2)]);
        assert_eq!(log[0].rows, Some(1), "结果集记行数");
        assert_eq!((log[1].rows, log[1].affected), (None, Some(2)), "写语句记影响行数");
        assert_eq!(log.iter().map(|entry| entry.elapsed_ms).collect::<Vec<_>>(), vec![Some(3), Some(4), Some(1)]);
        assert!(log[2].failed() && log[2].message.contains("missing"));

        // 下一次执行开始就清空，只留当前这一批；整次失败也记一条。
        assert!(desk.set_result_view(ResultView::Messages));
        let next = ready_ticket(&mut desk, &tasks, "SELECT 2");
        assert!(desk.log().is_empty(), "新的一次执行清掉上一批消息");
        assert_eq!(desk.log_batch(), 2);
        assert_eq!(desk.result_view(), ResultView::Current, "新结果落在当前结果页");
        desk.finish_polled(&tasks, &next, Err("连接中断".to_owned()), "已取消");
        let log = desk.log();
        assert_eq!(log.len(), 1);
        assert_eq!((log[0].statement, log[0].status.as_str(), log[0].message.as_str()), (None, "error", "连接中断"));

        // 取消运行中的执行也留一条记录。
        let _ = ready_ticket(&mut desk, &tasks, "SELECT 3");
        let _ = desk.cancel(&tasks);
        let log = desk.log();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].status, "cancelled");
    }

    #[test]
    fn pinned_results_survive_later_runs_until_closed() {
        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        desk.open_database("notes", "笔记", "sqlite", "main");
        assert!(!desk.can_pin(), "没有结果时不能固定");
        let first = ready_ticket(&mut desk, &tasks, "SELECT 'a'");
        assert!(!desk.can_pin(), "执行中不能固定");
        assert!(desk.accept_run(&tasks, &first, three_statement_run("run-1", "a")));
        assert!(desk.can_pin());
        let pin = desk.pin_current().expect("固定第一条");
        assert_eq!(desk.result_view(), ResultView::Pinned(pin), "固定后切到它");
        assert!(!desk.can_pin(), "同一条结果不重复固定");
        let snapshot = desk.pinned()[0].view.clone();
        assert_eq!(snapshot.rows, vec![vec![Some("a".to_owned())]]);

        // 再执行一次：新结果进当前页，固定的内容不变。
        let second = ready_ticket(&mut desk, &tasks, "SELECT 'b'");
        assert!(desk.accept_run(&tasks, &second, three_statement_run("run-2", "b")));
        assert_eq!(desk.result_view(), ResultView::Current);
        assert_eq!(desk.shown_result().unwrap().rows, vec![vec![Some("b".to_owned())]], "当前页是新结果");
        assert_eq!(desk.pinned().len(), 1);
        assert_eq!(desk.pinned()[0].view, snapshot, "固定结果没有被新执行替换");
        assert!(desk.set_result_view(ResultView::Pinned(pin)));
        assert_eq!(desk.shown_result().unwrap().rows, vec![vec![Some("a".to_owned())]]);
        assert!(!desk.set_result_view(ResultView::Pinned(pin + 9)), "不存在的固定结果不切换");
        // 未固定的结果照旧被替换；新一次的同一条语句可以再固定。
        assert!(desk.set_result_view(ResultView::Current));
        assert!(desk.can_pin());
        // 手动关闭：正在看它时回到当前结果。
        assert!(desk.set_result_view(ResultView::Pinned(pin)));
        assert!(desk.unpin(pin));
        assert!(desk.pinned().is_empty());
        assert_eq!(desk.result_view(), ResultView::Current);
        assert!(!desk.unpin(pin));
    }

    #[test]
    fn pinned_results_can_be_renamed_and_a_blank_name_restores_the_default() {
        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        desk.open_database("notes", "笔记", "sqlite", "main");
        let ticket = ready_ticket(&mut desk, &tasks, "SELECT 'a'");
        assert!(desk.accept_run(&tasks, &ticket, three_statement_run("run-1", "a")));
        let pin = desk.pin_current().expect("固定");
        assert!(desk.pinned()[0].custom_title.is_none());
        assert!(desk.rename_pin(pin, "  订单预览  "));
        assert_eq!(desk.pinned()[0].custom_title.as_deref(), Some("订单预览"));
        assert!(desk.rename_pin(pin, "   "));
        assert!(desk.pinned()[0].custom_title.is_none(), "空白标题恢复默认");
        assert!(!desk.rename_pin(pin + 3, "没有这一页"));
    }

    #[test]
    fn sqlite_column_meta_reads_the_primary_key_flag() {
        let columns = ["cid", "name", "type", "notnull", "dflt_value", "pk"].map(str::to_owned).to_vec();
        let rows = vec![
            vec![Some("0".into()), Some("id".into()), Some("INTEGER".into()), Some("1".into()), None, Some("1".into())],
            vec![Some("1".into()), Some("name".into()), Some("TEXT".into()), Some("0".into()), None, Some("0".into())],
        ];
        let meta = sqlite_column_meta(&columns, &rows);
        assert!(meta[0].primary);
        assert_eq!(meta[0].nullable, Some(false));
        assert!(!meta[1].primary);
        assert_eq!(meta[1].nullable, Some(true));
        assert!(!meta[0].foreign_key);
        assert!(meta[0].comment.is_empty());
    }

    #[test]
    fn switching_between_current_and_messages_keeps_the_grid_binding() {
        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        desk.open_database("notes", "笔记", "sqlite", "main");
        let ticket = ready_ticket(&mut desk, &tasks, "SELECT 'a'");
        assert!(desk.accept_run(&tasks, &ticket, three_statement_run("run-1", "a")));
        let generation = desk.view_gen();
        assert!(desk.set_result_view(ResultView::Messages));
        assert!(desk.set_result_view(ResultView::Current));
        assert_eq!(desk.view_gen(), generation, "消息页与当前结果共用表格，切换不重绑（暂存不丢）");
        assert!(desk.pin_current().is_some());
        assert!(desk.view_gen() > generation, "切到固定结果要重绑表格");
        let generation = desk.view_gen();
        assert!(desk.set_result_view(ResultView::Messages));
        assert!(desk.view_gen() > generation, "从固定结果切走也要重绑");
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
        assert!(ready.sources.iter().any(|source| source.columns.iter().any(|column| column.name == "status")));
        assert_eq!(ready.databases, vec!["main".to_owned(), "archive".to_owned()]);

        // 一个 schema 失败不等于整体失败：已读到的对象和列继续可用，状态是 Partial。
        desk.set_tree_error("读取库表失败：连接中断");
        let partial = desk.completion_scope().expect("部分失败也要有上下文");
        assert_eq!(partial.state, MetadataState::Partial("读取库表失败：连接中断".to_owned()));
        assert!(partial.objects.iter().any(|object| object.name == "item"), "部分失败时已读对象仍然可用");
        assert!(partial.sources.iter().any(|source| source.columns.iter().any(|column| column.name == "status")));
        desk.set_tree_error("");

        // 连接列出的库确实读过了却没有对象：是真空白库，不能报成过期。
        desk.select_database("archive");
        let blank = desk.completion_scope().expect("空库也要有上下文");
        assert_eq!(blank.state, MetadataState::Empty, "{:?}", blank.state);
        assert!(blank.objects.is_empty());
        // 目标范围不在已读的库里：树里有别的连接/库的对象，状态才可能过期。
        desk.select_database("shadow");
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
        desk.remember_table_columns("pg", "shop", "public", "orders", vec![ColumnMeta::named("id"), ColumnMeta::named("customer_id")]);
        desk.remember_table_columns("pg", "shop", "archive", "orders", vec![ColumnMeta::named("id"), ColumnMeta::named("archived_at")]);
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
        let mut customer = ColumnMeta::named("customer_id");
        customer.foreign_key = true;
        assert_eq!(public.columns, vec![ColumnMeta::named("id"), customer]);
        let archive = scope.sources.iter().find(|source| source.label == "archive.orders").expect("archive.orders");
        assert_eq!(archive.columns, vec![ColumnMeta::named("id"), ColumnMeta::named("archived_at")]);
        assert_eq!(scope.foreign_keys.len(), 1, "只保留当前连接的外键");
        assert_eq!(scope.foreign_keys[0].from_schema, "public");

        // 别名限定符只给该表自己的列。
        let planned = desk.completion_for("SELECT * FROM public.orders o WHERE o.", 39, Locale::ZhCn);
        let labels: Vec<String> = planned.iter().map(|entry| entry.label.clone()).collect();
        assert!(labels.contains(&"customer_id".to_owned()), "{labels:?}");
        assert!(!labels.contains(&"archived_at".to_owned()), "{labels:?}");

        let sql = "select customer_id from public.orders";
        let at = sql.find("public.orders").unwrap() + "public.".len() + 1;
        let info = desk.object_info_at(sql, at).expect("表名解析到对象");
        assert_eq!(info.schema, "public");
        assert!(info.columns.iter().any(|column| column.name == "id" && !column.foreign_key));
        assert!(info.columns.iter().any(|column| column.name == "customer_id" && column.foreign_key), "{:?}", info.columns);
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
