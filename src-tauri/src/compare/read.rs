//! 在已打开的会话上读取两侧。PostgreSQL 的另一个库只另开临时会话，不改连、不写配置。
//!
//! 每次读取都有超时（取设置的查询超时）；两侧并行读取，表结构能整库批量读的方言一次读完，
//! 其余对象有限并发逐个读取。进度按请求 id 登记，界面轮询。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use futures::{stream, StreamExt};

use crate::adapters::transfer::{relational_engine, transfer_of};
use crate::adapters::{adapter, catalog_of, engine_of_session, fixed_database_named, session_matches};
use crate::catalog::{read_definition, read_object_page, ObjectKind, ObjectRow};
use crate::connections::{CatalogSession, Service};
use crate::ddl::{load_partial, Action, DdlEngine, Request, Snapshot};
use crate::grid::{describe, fetch_cells, qualified_name, quote_ident, select_list, Cell, TableSchema};

use super::data::diff_data;
use super::model::{
    needs_ephemeral_postgres, Category, ColumnShape, DataColumn, DataInput, DataReport, DataRequest, DataTable, ForeignKeyShape, IndexShape, Kind, ObjectDef, Side,
    Engine, SideRequest, StructureReport, StructureRequest, TableShape,
};
use super::structure::{diff_structure, diff_table_pair, table_data_keys};

static CANCELLED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
static PROGRESS: Mutex<BTreeMap<String, Progress>> = Mutex::new(BTreeMap::new());

/// 查询超时设置为 0（不限）时，单次元数据/行读取仍用目录读取的默认上限，避免一直卡在“正在对比…”。
pub const DEFAULT_READ_TIMEOUT_SECS: u64 = 30;
/// 每侧同时读取对象定义的上限：会话池很小（MySQL 元数据池 2 条），再多只是在池上排队。
const ENRICH_CONCURRENCY: usize = 4;

pub fn begin_cancel(id: &str) {
    if !id.is_empty() {
        CANCELLED.lock().expect("cancel").remove(id);
    }
}

pub fn request_cancel(id: &str) {
    if !id.is_empty() {
        CANCELLED.lock().expect("cancel").insert(id.to_owned());
    }
}

pub fn is_cancelled(id: &str) -> bool {
    !id.is_empty() && CANCELLED.lock().expect("cancel").contains(id)
}

/// 对比进行到哪一步。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    /// 正在列出两侧的对象。
    #[default]
    Listing,
    /// 正在读取对象定义（列、索引、外键、定义原文）。
    Reading,
    /// 正在读取行（数据对比）。
    Rows,
    /// 正在比较并生成脚本。
    Diffing,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub stage: Stage,
    /// 已读对象数（数据对比时是已读表数）。
    pub done: usize,
    /// 要读的对象总数；还不知道时为 0。
    pub total: usize,
}

/// 正在进行的对比的进度；结束后不再登记。
pub fn progress_of(id: &str) -> Option<Progress> {
    PROGRESS.lock().expect("progress").get(id).copied()
}

/// 读取超时的说明：界面按失败显示并给重试。
pub fn timeout_message(secs: u64) -> String {
    let secs = if secs == 0 { DEFAULT_READ_TIMEOUT_SECS } else { secs };
    format!("对比读取超过 {secs} 秒没有完成（查询超时设置），已停止。可以重试，或在设置里调大查询超时。")
}

/// 一次对比的读取上下文：请求 id（取消与进度）、单次读取超时、是否已有读取超时。
struct Run {
    id: String,
    timeout: Duration,
    secs: u64,
    timed_out: AtomicBool,
}

impl Run {
    fn new(id: &str, timeout_secs: u64) -> Self {
        let secs = if timeout_secs == 0 { DEFAULT_READ_TIMEOUT_SECS } else { timeout_secs };
        begin_cancel(id);
        if !id.is_empty() {
            PROGRESS.lock().expect("progress").insert(id.to_owned(), Progress::default());
        }
        Self { id: id.to_owned(), timeout: Duration::from_secs(secs), secs, timed_out: AtomicBool::new(false) }
    }

    fn cancelled(&self) -> bool {
        is_cancelled(&self.id)
    }

    /// 已取消或已有读取超时：后面的读取不再发起。
    fn stopped(&self) -> bool {
        self.cancelled() || self.timed_out()
    }

    fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::SeqCst)
    }

    fn timeout_message(&self) -> String {
        timeout_message(self.secs)
    }

    /// 单次读取带超时；超时记在这次对比上，返回明确的超时说明。
    async fn bounded<T>(&self, read: impl std::future::Future<Output = T>) -> Result<T, String> {
        match tokio::time::timeout(self.timeout, read).await {
            Ok(value) => Ok(value),
            Err(_) => {
                self.timed_out.store(true, Ordering::SeqCst);
                Err(self.timeout_message())
            }
        }
    }

    fn update(&self, change: impl FnOnce(&mut Progress)) {
        if self.id.is_empty() {
            return;
        }
        if let Some(progress) = PROGRESS.lock().expect("progress").get_mut(&self.id) {
            change(progress);
        }
    }

    fn stage(&self, stage: Stage) {
        self.update(|progress| {
            if progress.stage != stage {
                progress.stage = stage;
                progress.done = 0;
                progress.total = 0;
            }
        });
    }

    fn add_total(&self, count: usize) {
        self.update(|progress| progress.total += count);
    }

    fn add_done(&self, count: usize) {
        self.update(|progress| progress.done += count);
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if !self.id.is_empty() {
            PROGRESS.lock().expect("progress").remove(&self.id);
        }
    }
}

struct Held {
    _ephemeral: Option<crate::connections::EphemeralRead>,
    session: CatalogSession,
}

pub async fn read_structure(service: &Service, request: &StructureRequest) -> StructureReport {
    let run = Run::new(&request.id, request.timeout_secs);
    // 表对表对比只读这一对表的定义；名单仍然读全，用来确认表在不在。
    let only = request.objects.as_ref().map(|pair| (BTreeSet::from([pair.baseline.clone()]), BTreeSet::from([pair.target.clone()])));
    let (baseline, target) = tokio::join!(
        load_side(service, &request.baseline, &run, only.as_ref().map(|(left, _)| left)),
        load_side(service, &request.target, &run, only.as_ref().map(|(_, right)| right)),
    );
    run.stage(Stage::Diffing);
    let mut report = match &request.objects {
        Some(pair) => diff_table_pair(&baseline, &target, pair),
        None => diff_structure(&baseline, &target),
    };
    report.timed_out = run.timed_out();
    report
}

pub async fn read_data(service: &Service, request: &DataRequest) -> DataReport {
    let run = Run::new(&request.id, request.timeout_secs);
    if request.tables.is_empty() {
        return diff_data(&DataInput { baseline_engine: request.baseline.engine.clone(), target_engine: request.target.engine.clone(), tables: Vec::new(), cancelled: false });
    }
    // 数据对比只需要选中表的结构：名单照常读全，定义只读这些表。
    // 表对表对比时对照侧表名可以不同。
    let wanted_left: BTreeSet<String> = request.tables.iter().map(|table| table.name.clone()).collect();
    let wanted_right: BTreeSet<String> = request.tables.iter().map(|table| table.target_name().to_owned()).collect();
    let (baseline, target) = tokio::join!(
        load_side(service, &request.baseline, &run, Some(&wanted_left)),
        load_side(service, &request.target, &run, Some(&wanted_right)),
    );
    if let Some(message) = refuse_loaded(&baseline, &target) {
        let mut report = DataReport::refused(message);
        report.timed_out = run.timed_out();
        return report;
    }
    run.stage(Stage::Rows);
    run.add_total(request.tables.len());
    let mut tables = Vec::new();
    let mut cancelled = run.cancelled() || baseline.cancelled || target.cancelled;
    for wanted in &request.tables {
        if run.stopped() {
            cancelled |= run.cancelled();
            let reason = if run.timed_out() { run.timeout_message() } else { "对比已取消".to_owned() };
            tables.push(DataTable {
                name: wanted.target_name().to_owned(),
                qualify: String::new(),
                columns: Vec::new(),
                keys: Vec::new(),
                chosen_key: wanted.key.clone(),
                baseline: Vec::new(),
                target: Vec::new(),
                read_error: Some(reason),
                only_side: None,
                keys_unknown: false,
            });
            continue;
        }
        tables.push(load_data_table(service, &request.baseline, &request.target, &baseline, &target, &wanted.name, wanted.target_name(), wanted.key.clone(), &run).await);
        run.add_done(1);
        if run.cancelled() {
            cancelled = true;
        }
    }
    run.stage(Stage::Diffing);
    let mut report = diff_data(&DataInput { baseline_engine: request.baseline.engine.clone(), target_engine: request.target.engine.clone(), tables, cancelled });
    report.timed_out = run.timed_out();
    if report.timed_out {
        report.incomplete = true;
    }
    for wanted in &request.tables {
        if wanted.target_name() != wanted.name && report.refused.is_none() {
            report.notes.push(format!("基准表 {} 与对照表 {} 按键对应行，同步 SQL 针对对照表 {}。", wanted.name, wanted.target_name(), wanted.target_name()));
        }
    }
    report
}

fn refuse_loaded(baseline: &Side, target: &Side) -> Option<String> {
    if super::model::Engine::parse(&baseline.engine).ok() != super::model::Engine::parse(&target.engine).ok() {
        return Some(crate::adapters::transfer::ENGINE_MISMATCH.to_owned());
    }
    baseline.fatal.clone().or(target.fatal.clone()).map(|message| format!("{message} 不生成脚本。"))
}

/// `only` 给出时只读这些表的定义（数据对比）；对象名单仍然读全。
async fn load_side(service: &Service, request: &SideRequest, run: &Run, only: Option<&BTreeSet<String>>) -> Side {
    let fixed = fixed_database_named(&request.engine);
    let database = match fixed {
        Some(fixed) if request.database.is_empty() => fixed.to_owned(),
        _ => request.database.clone(),
    };
    let mut side = Side {
        engine: request.engine.clone(),
        database: database.clone(),
        schema: request.schema.clone(),
        connected: false,
        fatal: None,
        objects: Vec::new(),
        category_errors: Vec::new(),
        cancelled: false,
        listing_complete: false,
    };
    let Ok(engine) = super::model::Engine::parse(&request.engine) else {
        side.fatal = Some(format!("不支持的数据库 {}", request.engine));
        return side;
    };
    if fixed.is_none() && database.trim().is_empty() {
        side.fatal = Some("需要数据库名".to_owned());
        return side;
    }
    let Some(existing) = service.catalog_session(&request.connection_id) else {
        side.fatal = Some("这一侧还没有打开".to_owned());
        return side;
    };
    if !session_matches(engine, &existing) {
        side.fatal = Some("会话类型和目标引擎不一致".to_owned());
        return side;
    }
    side.connected = true;
    let held = match run.bounded(hold_session(service, &existing, request, &database)).await.and_then(|held| held) {
        Ok(held) => held,
        Err(message) => {
            side.fatal = Some(message);
            return side;
        }
    };
    if let Err(message) = run.bounded(transfer_of(engine).compare_scope(&held.session, &database, &request.schema)).await.and_then(|checked| checked) {
        side.fatal = Some(message);
        return side;
    }
    let mut offset = 0usize;
    let mut seen = 0usize;
    loop {
        if run.cancelled() {
            side.cancelled = true;
            side.listing_complete = false;
            break;
        }
        let page = match run.bounded(read_object_page(&held.session, engine, &database, &request.schema, offset, 200)).await {
            Ok(page) => page,
            Err(message) => {
                side.fatal = Some(message);
                return side;
            }
        };
        if let Some(message) = page.error {
            side.fatal = Some(message);
            return side;
        }
        for report in page.kind_reports {
            if let Some(message) = report.error {
                side.category_errors.push((category_of(report.kind), message));
            }
        }
        let count = page.objects.len();
        for object in page.objects {
            if let Some(def) = bare_object(object) {
                side.objects.push(def);
            }
        }
        seen += count;
        if count == 0 || seen >= page.total {
            side.listing_complete = true;
            break;
        }
        offset = seen;
    }
    if side.listing_complete {
        run.stage(Stage::Reading);
        enrich_all(&held.session, request, &database, engine, &mut side, run, only).await;
    }
    side.category_errors.sort();
    side.category_errors.dedup();
    side
}

/// 读完定义的对象、它带来的类别读取失败、是否因取消而跳过。
type Enriched = (ObjectDef, Vec<(Category, String)>, bool);

/// 读取对象定义：先试整库批量读表结构，读不到的表和其余对象有限并发逐个读取。
async fn enrich_all(session: &CatalogSession, request: &SideRequest, database: &str, engine: Engine, side: &mut Side, run: &Run, only: Option<&BTreeSet<String>>) {
    let wanted = |object: &ObjectDef| only.is_none_or(|names| object.kind == Kind::Table && names.contains(&object.name));
    let total = side.objects.iter().filter(|object| wanted(object)).count();
    run.add_total(total);
    let has_tables = side.objects.iter().any(|object| wanted(object) && object.kind == Kind::Table);
    let mut batch: HashMap<String, Snapshot> = HashMap::new();
    if has_tables && !run.stopped() {
        match run.bounded(transfer_of(engine).compare_table_snapshots(session, database, &request.schema)).await {
            Ok(Some(Ok(snapshots))) => batch = snapshots.into_iter().collect(),
            // 批量读失败时退回逐表读取，逐表读取会把具体原因记在对象上。
            Ok(Some(Err(_))) | Ok(None) => {}
            Err(message) => {
                side.fatal = Some(message);
                return;
            }
        }
    }
    let objects = std::mem::take(&mut side.objects);
    let results: Vec<Enriched> = stream::iter(objects)
        .map(|mut object| {
            let snapshot = if object.kind == Kind::Table { batch.remove(&object.name) } else { None };
            let selected = wanted(&object);
            async move {
                if !selected {
                    return (object, Vec::new(), false);
                }
                if let Some(snapshot) = snapshot {
                    let errors = absorb_snapshot(&mut object, &snapshot);
                    run.add_done(1);
                    return (object, errors, false);
                }
                if run.stopped() {
                    let cancelled = run.cancelled();
                    object.read_error = Some(if cancelled { "对比已取消".to_owned() } else { run.timeout_message() });
                    return (object, Vec::new(), cancelled);
                }
                let errors = match run.bounded(enrich(session, request, database, &mut object)).await {
                    Ok(errors) => errors,
                    Err(message) => {
                        object.read_error = Some(message);
                        Vec::new()
                    }
                };
                run.add_done(1);
                (object, errors, false)
            }
        })
        .buffered(ENRICH_CONCURRENCY)
        .collect()
        .await;
    for (object, errors, cancelled) in results {
        side.cancelled |= cancelled;
        side.category_errors.extend(errors);
        side.objects.push(object);
    }
    if run.timed_out() && side.fatal.is_none() {
        side.fatal = Some(run.timeout_message());
    }
}

#[allow(clippy::too_many_arguments)]
async fn load_data_table(
    service: &Service,
    baseline_request: &SideRequest,
    target_request: &SideRequest,
    baseline: &Side,
    target: &Side,
    left_name: &str,
    name: &str,
    chosen_key: Option<String>,
    run: &Run,
) -> DataTable {
    // `name` 是对照侧表名：同步语句针对它。基准侧按 `left_name` 读取。
    let left = baseline.objects.iter().find(|object| object.kind == Kind::Table && object.name == left_name);
    let right = target.objects.iter().find(|object| object.kind == Kind::Table && object.name == name);
    if left.is_none() || right.is_none() {
        let only_side = if left.is_some() { "基准" } else { "对照" };
        return DataTable {
            name: name.to_owned(),
            qualify: String::new(),
            columns: Vec::new(),
            keys: Vec::new(),
            keys_unknown: false,
            chosen_key,
            baseline: Vec::new(),
            target: Vec::new(),
            read_error: None,
            only_side: Some(only_side.to_owned()),
        };
    }
    if let Some(message) = left.and_then(|object| object.read_error.clone()).or(right.and_then(|object| object.read_error.clone())) {
        return failed_table(name, chosen_key, message);
    }
    let left_shape = left.and_then(|object| object.table.as_ref());
    let right_shape = right.and_then(|object| object.table.as_ref());
    if left_shape.or(right_shape).is_none() {
        return failed_table(name, chosen_key, "没有读到表结构".to_owned());
    }
    let Ok(engine) = Engine::parse(&target_request.engine) else {
        return failed_table(name, chosen_key, format!("不支持的数据库 {}", target_request.engine));
    };
    let Ok(keys) = table_data_keys(engine, left_shape, right_shape) else {
        return unread_keys_table(name, chosen_key);
    };
    let Some(base_session) = service.catalog_session(&baseline_request.connection_id) else {
        return failed_table(name, chosen_key, "基准连接还没有打开".to_owned());
    };
    let Some(target_session) = service.catalog_session(&target_request.connection_id) else {
        return failed_table(name, chosen_key, "对照连接还没有打开".to_owned());
    };
    let side_database = |request: &SideRequest| fixed_database_named(&request.engine).map(str::to_owned).unwrap_or_else(|| request.database.clone());
    let base_database = side_database(baseline_request);
    let target_database = side_database(target_request);
    let base_held = match run.bounded(hold_session(service, &base_session, baseline_request, &base_database)).await.and_then(|held| held) {
        Ok(held) => held,
        Err(message) => return failed_table(name, chosen_key, message),
    };
    let target_held = match run.bounded(hold_session(service, &target_session, target_request, &target_database)).await.and_then(|held| held) {
        Ok(held) => held,
        Err(message) => return failed_table(name, chosen_key, message),
    };
    let described = match run.bounded(describe(&target_held.session, &target_database, &target_request.schema, name, &target_request.engine)).await.and_then(|schema| schema) {
        Ok(schema) => schema,
        Err(message) => return failed_table(name, chosen_key, message),
    };
    let mut table = DataTable {
        name: name.to_owned(),
        qualify: described.schema.clone(),
        columns: described.columns.iter().map(|column| DataColumn { name: column.name.clone(), type_name: column.type_name.clone(), nullable: column.nullable }).collect(),
        keys,
        keys_unknown: false,
        chosen_key,
        baseline: Vec::new(),
        target: Vec::new(),
        read_error: None,
        only_side: None,
    };
    // 两侧的行并行读取。
    let ((baseline_rows, baseline_error), (target_rows, target_error)) = tokio::join!(
        read_all_rows(&base_held.session, &base_database, &baseline_request.schema, left_name, &baseline_request.engine, run),
        read_all_rows(&target_held.session, &target_database, &target_request.schema, name, &target_request.engine, run),
    );
    table.baseline = baseline_rows;
    table.target = target_rows;
    table.read_error = baseline_error.or(target_error);
    table
}

fn failed_table(name: &str, chosen_key: Option<String>, message: String) -> DataTable {
    DataTable {
        name: name.to_owned(),
        qualify: String::new(),
        columns: Vec::new(),
        keys: Vec::new(),
        keys_unknown: false,
        chosen_key,
        baseline: Vec::new(),
        target: Vec::new(),
        read_error: Some(message),
        only_side: None,
    }
}

fn unread_keys_table(name: &str, chosen_key: Option<String>) -> DataTable {
    DataTable {
        name: name.to_owned(),
        qualify: String::new(),
        columns: Vec::new(),
        keys: Vec::new(),
        keys_unknown: true,
        chosen_key,
        baseline: Vec::new(),
        target: Vec::new(),
        read_error: None,
        only_side: None,
    }
}

async fn read_all_rows(session: &CatalogSession, database: &str, schema: &str, table: &str, engine: &str, run: &Run) -> (Vec<BTreeMap<String, Cell>>, Option<String>) {
    let described = match run.bounded(describe(session, database, schema, table, engine)).await.and_then(|schema| schema) {
        Ok(schema) => schema,
        Err(message) => return (Vec::new(), Some(message)),
    };
    let mut offset = 0u64;
    let limit = 200u64;
    let mut rows = Vec::new();
    loop {
        if run.cancelled() {
            return (rows, Some("对比已取消".to_owned()));
        }
        match run.bounded(fetch_page(session, &described, offset, limit)).await.and_then(|page| page) {
            Ok(page) => {
                let count = page.len() as u64;
                rows.extend(page);
                if count < limit {
                    break;
                }
                offset += count;
            }
            Err(message) => return (rows, Some(message)),
        }
    }
    (rows, None)
}

async fn fetch_page(session: &CatalogSession, schema: &TableSchema, offset: u64, limit: u64) -> Result<Vec<BTreeMap<String, Cell>>, String> {
    let list = select_list(schema, true);
    let order = if schema.key.is_empty() {
        String::new()
    } else {
        let columns = schema.key.iter().map(|name| format!("{} ASC", quote_ident(schema.engine, name))).collect::<Vec<_>>().join(", ");
        format!(" ORDER BY {columns}")
    };
    let tail = crate::adapters::query::sql_syntax(schema.engine).page_tail(limit, offset, !order.is_empty());
    let sql = format!("SELECT {list} FROM {}{order}{tail}", qualified_name(schema));
    // 与网格、导出同一套逐格读取（各引擎的取值规则在适配器里）。
    let matrix = fetch_cells(session, &sql, schema.columns.len()).await?;
    let mut rows = Vec::new();
    for cells in matrix {
        let mut map = BTreeMap::new();
        for (column, cell) in schema.columns.iter().zip(cells) {
            map.insert(column.name.clone(), cell);
        }
        rows.push(map);
    }
    Ok(rows)
}

async fn enrich(session: &CatalogSession, request: &SideRequest, database: &str, object: &mut ObjectDef) -> Vec<(Category, String)> {
    match object.kind {
        Kind::Table | Kind::View => {
            let action = Action::DropTable { table: object.name.clone() };
            let ddl_request = Request { database: database.to_owned(), schema: request.schema.clone(), engine: match DdlEngine::parse(&request.engine) {
                Ok(engine) => engine,
                Err(message) => {
                    object.read_error = Some(message);
                    return Vec::new();
                }
            }, action };
            match load_partial(session, &ddl_request, &object.name).await {
                Ok(snapshot) => absorb_snapshot(object, &snapshot),
                Err(message) => {
                    object.read_error = Some(message);
                    Vec::new()
                }
            }
        }
        _ => {
            let Ok(engine) = relational_engine(&request.engine) else {
                object.read_error = Some("引擎不一致".to_owned());
                return Vec::new();
            };
            match read_definition(session, engine, database, &request.schema, &object.name, kind_str(object.kind), &object.signature, &object.relation).await {
                Ok(sql) => {
                    object.definition = sql;
                    // serial / identity 的序列由列拥有：relation 记成 `表\u{1}列`，结构脚本不再无条件 DROP SEQUENCE。
                    if object.kind == Kind::Sequence {
                        if let Some(owner) = transfer_of(engine).sequence_owner(session, &request.schema, &object.name).await {
                            object.relation = owner;
                        }
                    }
                }
                Err(message) => object.read_error = Some(message),
            }
            Vec::new()
        }
    }
}

pub(super) fn absorb_snapshot(object: &mut ObjectDef, snapshot: &Snapshot) -> Vec<(Category, String)> {
    let mut errors = Vec::new();
    if let Some(message) = snapshot.index_error.clone() {
        errors.push((Category::Index, message));
    }
    if let Some(message) = snapshot.foreign_key_error.clone() {
        errors.push((Category::ForeignKey, message));
    }
    apply_snapshot(object, snapshot);
    errors
}

fn apply_snapshot(object: &mut ObjectDef, snapshot: &Snapshot) {
    if snapshot.kind == "view" || object.kind == Kind::View {
        object.kind = Kind::View;
        object.materialized = snapshot.materialized;
        if let Some(message) = &snapshot.definition_error {
            object.read_error = Some(message.clone());
            return;
        }
        if snapshot.table_sql.trim().is_empty() || snapshot.table_sql.trim().starts_with("--") {
            object.read_error = Some("没有读到视图定义".to_owned());
            return;
        }
        object.definition = snapshot.table_sql.clone();
        return;
    }
    object.table = Some(TableShape {
        columns: snapshot
            .columns
            .iter()
            .map(|column| ColumnShape {
                name: column.name.clone(),
                type_name: column.type_name.clone(),
                nullable: column.nullable,
                default_sql: crate::ddl::sql::existing_default_sql(snapshot.engine, &column.type_name, column.default_sql.as_deref().unwrap_or_default()),
                primary: column.primary,
                default_constraint: column.default_constraint.clone().unwrap_or_default(),
            })
            .collect(),
        indexes: snapshot
            .indexes
            .iter()
            .map(|index| IndexShape {
                name: index.name.clone(),
                unique: index.unique,
                columns: index.columns.iter().map(|column| (column.name.clone(), column.descending)).collect(),
                protected: index.protected,
                reason: index.reason.clone(),
                partial: index.partial,
                expression: index.expression,
                sql: index.sql.clone().unwrap_or_default(),
            })
            .collect(),
        foreign_keys: snapshot
            .foreign_keys
            .iter()
            .map(|key| ForeignKeyShape {
                name: key.name.clone(),
                columns: key.columns.clone(),
                ref_schema: key.ref_schema.clone(),
                ref_table: key.ref_table.clone(),
                ref_columns: key.ref_columns.clone(),
                on_delete: key.on_delete.clone(),
                on_update: key.on_update.clone(),
            })
            .collect(),
        indexes_read: snapshot.index_error.is_none(),
        foreign_keys_read: snapshot.foreign_key_error.is_none(),
    });
}

fn bare_object(row: ObjectRow) -> Option<ObjectDef> {
    Some(ObjectDef {
        kind: map_kind(row.kind)?,
        name: row.name,
        signature: row.signature,
        relation: row.relation,
        definition: String::new(),
        materialized: row.materialized,
        table: None,
        read_error: None,
    })
}

fn map_kind(kind: ObjectKind) -> Option<Kind> {
    Some(match kind {
        ObjectKind::Table => Kind::Table,
        ObjectKind::View => Kind::View,
        ObjectKind::Function => Kind::Function,
        ObjectKind::Procedure => Kind::Procedure,
        ObjectKind::Trigger => Kind::Trigger,
        ObjectKind::Sequence => Kind::Sequence,
        ObjectKind::Enum => Kind::Enum,
        ObjectKind::Domain => Kind::Domain,
        // 扩展与事件不是可比较的结构对象，比较侧不列它们。
        ObjectKind::Extension | ObjectKind::Event => return None,
    })
}

fn category_of(kind: ObjectKind) -> Category {
    Category::from_kind(map_kind(kind).unwrap_or(Kind::Table))
}

fn kind_str(kind: Kind) -> &'static str {
    match kind {
        Kind::Table => "table",
        Kind::View => "view",
        Kind::Function => "function",
        Kind::Procedure => "procedure",
        Kind::Trigger => "trigger",
        Kind::Sequence => "sequence",
        Kind::Enum => "enum",
        Kind::Domain => "domain",
    }
}

/// 会话绑定单库的引擎（PostgreSQL）要读的库不是当前库时另开临时会话，不改连、不写配置。
async fn hold_session(service: &Service, existing: &CatalogSession, request: &SideRequest, database: &str) -> Result<Held, String> {
    let Ok(engine) = relational_engine(&request.engine) else {
        return Ok(Held { _ephemeral: None, session: existing.clone() });
    };
    if !adapter(engine).capabilities().database_scoped_session {
        return Ok(Held { _ephemeral: None, session: existing.clone() });
    }
    let current = transfer_of(engine).session_database(existing).await?;
    if !needs_ephemeral_postgres(&request.engine, &current, database.trim()) {
        return Ok(Held { _ephemeral: None, session: existing.clone() });
    }
    let ephemeral = service.open_ephemeral_postgres(&request.connection_id, database.trim()).await?;
    let session = ephemeral.catalog();
    Ok(Held { _ephemeral: Some(ephemeral), session })
}

/// 列出所选 PostgreSQL / SQL Server 库的 schema。PostgreSQL 的另一个库走临时会话，不改连、不写配置。
pub async fn list_schemas(service: &Service, connection_id: &str, database: &str) -> Result<Vec<String>, String> {
    let database = database.trim();
    if database.is_empty() {
        return Err("需要数据库名".to_owned());
    }
    let Some(existing) = service.catalog_session(connection_id) else {
        return Err("这一侧还没有打开".to_owned());
    };
    let Some(engine) = engine_of_session(&existing).filter(|engine| adapter(*engine).capabilities().shows_schemas) else {
        return Err(crate::adapters::transfer::schema_engines_note());
    };
    let request = SideRequest {
        connection_id: connection_id.to_owned(),
        database: database.to_owned(),
        schema: String::new(),
        engine: adapter(engine).meta().id().to_owned(),
    };
    let held = hold_session(service, &existing, &request, database).await?;
    let sql = catalog_of(engine)
        .metadata_sql()
        .schemas
        .ok_or_else(|| format!("{} 缺少 schema 查询", adapter(engine).meta().label()))?;
    transfer_of(engine).schema_names(&held.session, sql).await
}

#[cfg(test)]
mod run_tests {
    use super::*;

    #[tokio::test]
    async fn a_read_past_the_timeout_stops_the_run_with_a_clear_message() {
        let mut run = Run::new("read-timeout-test", 15);
        run.timeout = Duration::from_millis(20);
        assert!(progress_of("read-timeout-test").is_some(), "开始后登记进度");
        let result = run.bounded(std::future::pending::<()>()).await;
        let message = result.expect_err("挂起的读取必须超时");
        assert!(message.contains("15 秒") && message.contains("超时"), "{message}");
        assert!(run.timed_out() && run.stopped());
        assert!(run.bounded(async { 1 }).await.is_ok(), "超时只标记这次对比，不影响已完成的读取");
        drop(run);
        assert!(progress_of("read-timeout-test").is_none(), "结束后不再登记进度");
        assert!(timeout_message(0).contains(&DEFAULT_READ_TIMEOUT_SECS.to_string()), "超时设为不限时按默认上限说明");
    }

    #[tokio::test]
    async fn progress_counts_up_within_a_stage_and_resets_on_the_next() {
        let run = Run::new("progress-test", 0);
        run.add_total(10);
        run.add_done(3);
        assert_eq!(progress_of("progress-test"), Some(Progress { stage: Stage::Listing, done: 3, total: 10 }));
        run.stage(Stage::Reading);
        run.add_total(4);
        run.add_done(1);
        assert_eq!(progress_of("progress-test"), Some(Progress { stage: Stage::Reading, done: 1, total: 4 }));
        request_cancel("progress-test");
        assert!(run.cancelled() && run.stopped() && !run.timed_out());
    }
}
