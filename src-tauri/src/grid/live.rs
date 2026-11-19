//! 在已经打开的会话上查询、逐条提交和流式导出。不新开数据库连接。

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::adapters::grid::ExportSink;
use crate::connections::CatalogSession;

use super::sql;
use super::store::{Applied, Cell, Column, CommitReport, Engine, Failed, Filter, Sort, TableSchema, COMMIT_RULE};

const MAX_PAGE: u64 = 10_000;

fn no_channel() -> String {
    "这个连接没有连着可查询的数据库".to_owned()
}

pub struct LiveRow {
    pub id: String,
    pub cells: BTreeMap<String, Cell>,
    pub located: bool,
    pub locator: Vec<(String, Cell)>,
}

pub struct LivePage {
    pub rows: Vec<LiveRow>,
    pub truncated: bool,
    pub sql: String,
}

pub enum MutationKind {
    Update,
    Insert,
    Delete,
}

pub struct Mutation {
    pub id: String,
    pub kind: MutationKind,
    pub sets: Vec<(String, Cell)>,
    pub locator: Vec<(String, Cell)>,
    pub values: Vec<(String, Cell)>,
    /// 与 sets 对齐。真表示该值来自已经完成的主键另读。
    pub resolved: Vec<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExportMode {
    TableLoaded,
    TableFull,
    Query,
}

pub struct ExportSpec {
    pub id: String,
    pub mode: ExportMode,
    pub database: String,
    pub schema: String,
    pub table: String,
    pub engine_hint: String,
    pub filters: Vec<Filter>,
    pub sort: Option<Sort>,
    /// 整表关键字（R3）。table 模式的全量导出与网格读到同一组行，口径一致。
    pub search: Option<String>,
    /// 已应用的直接 SQL 筛选表达式（R13）；与结构化条件互斥，和关键词一起进同一个 WHERE。
    pub filter_expression: Option<String>,
    pub limit: u64,
    pub truncated: bool,
    pub statement: String,
    pub acknowledged: bool,
    pub path: String,
    pub delimiter: String,
    pub null_token: String,
    pub bom: bool,
    pub header: bool,
    pub format: String,
    pub fail_after_rows: Option<u64>,
    pub cancel_after_rows: Option<u64>,
}

pub struct ExportOutcome {
    pub status: &'static str,
    pub success: bool,
    pub written_rows: u64,
    pub path: Option<String>,
    pub residual_path: Option<String>,
    pub message: String,
    pub partial: bool,
}

pub async fn describe(session: &CatalogSession, database: &str, schema: &str, table: &str, engine_hint: &str) -> Result<TableSchema, String> {
    if table.is_empty() {
        return Err("没有表名".to_owned());
    }
    session.grid_describe(database, schema, table, engine_hint).await
}

#[allow(clippy::too_many_arguments, dead_code)]
pub async fn query_page(
    session: &CatalogSession,
    database: &str,
    schema_name: &str,
    table: &str,
    engine_hint: &str,
    filters: &[Filter],
    sort: Option<&Sort>,
    offset: u64,
    limit: u64,
) -> Result<LivePage, String> {
    query_page_search(session, database, schema_name, table, engine_hint, filters, sort, None, offset, limit).await
}

/// query_page 的整表关键字形态（R3）：搜索子句与筛选在 WHERE 里 AND 组合。
#[allow(clippy::too_many_arguments)]
pub async fn query_page_search(
    session: &CatalogSession,
    database: &str,
    schema_name: &str,
    table: &str,
    engine_hint: &str,
    filters: &[Filter],
    sort: Option<&Sort>,
    search: Option<&str>,
    offset: u64,
    limit: u64,
) -> Result<LivePage, String> {
    query_page_search_preview(session, database, schema_name, table, engine_hint, filters, sort, search, None, offset, limit, 1024).await
}

/// 与 query_page_search 相同，但大字段预览的上限来自设置 preview_kb。
/// expression 是已应用的直接 SQL 条件（R13），与结构化条件、关键词处在同一份 WHERE 里。
#[allow(clippy::too_many_arguments)]
pub async fn query_page_search_preview(
    session: &CatalogSession,
    database: &str,
    schema_name: &str,
    table: &str,
    engine_hint: &str,
    filters: &[Filter],
    sort: Option<&Sort>,
    search: Option<&str>,
    expression: Option<&str>,
    offset: u64,
    limit: u64,
    preview_bytes: usize,
) -> Result<LivePage, String> {
    let schema = describe(session, database, schema_name, table, engine_hint).await?;
    query_page_with_schema(session, &schema, filters, sort, search, expression, offset, limit, preview_bytes).await
}

#[allow(clippy::too_many_arguments)]
pub async fn query_page_with_schema(
    session: &CatalogSession, schema: &TableSchema, filters: &[Filter], sort: Option<&Sort>,
    search: Option<&str>, expression: Option<&str>, offset: u64, limit: u64, preview_bytes: usize,
) -> Result<LivePage, String> {
    let limit = limit.min(MAX_PAGE);
    if let Some(sort) = sort {
        if !schema.columns.iter().any(|column| column.name == sort.column) {
            return Err(format!("未知排序列 {}", sort.column));
        }
    }
    let display = page_sql(schema, filters, search, expression, sort, limit, offset, preview_bytes)?;
    if limit == 0 {
        return Ok(LivePage { rows: Vec::new(), truncated: false, sql: display });
    }
    let executed = page_sql(schema, filters, search, expression, sort, limit.saturating_add(1), offset, preview_bytes)?;
    let width = schema.columns.len();
    let mut fetched = fetch_cells(session, &executed, width).await?;
    let truncated = fetched.len() as u64 > limit;
    if truncated {
        fetched.truncate(limit as usize);
    }
    let rows = fetched
        .into_iter()
        .enumerate()
        .map(|(index, cells)| live_row(schema, cells, offset, index))
        .collect();
    Ok(LivePage { rows, truncated, sql: display })
}

#[allow(dead_code)]
pub async fn count_rows(
    session: &CatalogSession,
    database: &str,
    schema_name: &str,
    table: &str,
    engine_hint: &str,
    filters: &[Filter],
) -> Result<i64, String> {
    count_rows_search(session, database, schema_name, table, engine_hint, filters, None, None).await
}

/// count_rows 的整表关键字形态（R3）：统计行数与分页读到同一组行。
/// expression 与分页使用同一份已应用条件（R13）。
#[allow(clippy::too_many_arguments)]
pub async fn count_rows_search(
    session: &CatalogSession,
    database: &str,
    schema_name: &str,
    table: &str,
    engine_hint: &str,
    filters: &[Filter],
    search: Option<&str>,
    expression: Option<&str>,
) -> Result<i64, String> {
    let schema = describe(session, database, schema_name, table, engine_hint).await?;
    let sql = count_sql(&schema, filters, search, expression)?;
    fetch_count(session, &sql).await
}

/// 不带预览上限的提交：调用方拿不到读出这些值时的上限时才用，写回判断回退到默认上限。
#[allow(dead_code)]
pub async fn commit(
    session: &CatalogSession,
    database: &str,
    schema_name: &str,
    table: &str,
    engine_hint: &str,
    changes: &[Mutation],
) -> Result<CommitReport, String> {
    commit_with_preview(session, database, schema_name, table, engine_hint, changes, sql::DEFAULT_PREVIEW_BYTES).await
}

/// 带预览上限的提交：这些值读出时用的上限决定什么算信封，写回前不再另找一个默认值。
pub async fn commit_with_preview(
    session: &CatalogSession,
    database: &str,
    schema_name: &str,
    table: &str,
    engine_hint: &str,
    changes: &[Mutation],
    preview_bytes: usize,
) -> Result<CommitReport, String> {
    let schema = describe(session, database, schema_name, table, engine_hint).await?;
    let mut applied = Vec::new();
    for (index, change) in changes.iter().enumerate() {
        let pending: Vec<String> = changes.iter().skip(index).map(|item| item.id.clone()).collect();
        if schema.kind != "table" {
            return Ok(fail(applied, &change.id, String::new(), "只有数据表可以新增、修改或删除，视图不行", pending));
        }
        match change.kind {
            MutationKind::Update => {
                if let Some(message) = reject_sets(&schema, &change.sets, &change.resolved, preview_bytes) {
                    return Ok(fail(applied, &change.id, String::new(), message, pending));
                }
                let sql = match located_sql(&schema, &change.locator, true) {
                    Ok(locator) => sql::update_sql(&schema, &change.sets, locator),
                    Err(message) => return Ok(fail(applied, &change.id, String::new(), message, pending)),
                };
                if change.sets.is_empty() {
                    return Ok(fail(applied, &change.id, sql, "没有要修改的列", pending));
                }
                if let Err(message) = expect_one(session, &schema, &change.locator).await {
                    return Ok(fail(applied, &change.id, sql, message, pending));
                }
                match execute_sql(session, &sql).await {
                    Ok(1) => applied.push(Applied { id: change.id.clone(), sql }),
                    Ok(affected) => {
                        return Ok(fail(applied, &change.id, sql, format!("影响 {affected} 行，已停止。已成功的语句不撤销。"), pending));
                    }
                    Err(message) => return Ok(fail(applied, &change.id, sql, message, pending)),
                }
            }
            MutationKind::Delete => {
                let sql = match located_sql(&schema, &change.locator, false) {
                    Ok(locator) => sql::delete_sql(&schema, locator),
                    Err(message) => return Ok(fail(applied, &change.id, String::new(), message, pending)),
                };
                if let Err(message) = expect_one(session, &schema, &change.locator).await {
                    return Ok(fail(applied, &change.id, sql, message, pending));
                }
                match execute_sql(session, &sql).await {
                    Ok(1) => applied.push(Applied { id: change.id.clone(), sql }),
                    Ok(affected) => {
                        return Ok(fail(applied, &change.id, sql, format!("影响 {affected} 行，已停止。已成功的语句不撤销。"), pending));
                    }
                    Err(message) => return Ok(fail(applied, &change.id, sql, message, pending)),
                }
            }
            MutationKind::Insert => {
                if let Some(message) = reject_sets(&schema, &change.values, &[], preview_bytes) {
                    return Ok(fail(applied, &change.id, String::new(), message, pending));
                }
                let sql = sql::insert_sql(&schema, &change.values);
                match execute_sql(session, &sql).await {
                    Ok(0) => return Ok(fail(applied, &change.id, sql, "新增没有写入行，已停止。已成功的语句不撤销。", pending)),
                    Ok(_) => applied.push(Applied { id: change.id.clone(), sql }),
                    Err(message) => return Ok(fail(applied, &change.id, sql, message, pending)),
                }
            }
        }
    }
    Ok(CommitReport { applied, failed: None, pending_ids: Vec::new(), rolled_back: false, rule: COMMIT_RULE })
}

fn fail(applied: Vec<Applied>, id: &str, sql: String, message: impl Into<String>, pending: Vec<String>) -> CommitReport {
    CommitReport {
        applied,
        failed: Some(Failed { id: id.to_owned(), sql, message: message.into() }),
        pending_ids: pending,
        rolled_back: false,
        rule: COMMIT_RULE,
    }
}

fn reject_sets(schema: &TableSchema, sets: &[(String, Cell)], resolved: &[bool], preview_bytes: usize) -> Option<String> {
    sets.iter().enumerate().find_map(|(index, (name, value))| {
        let column = schema.columns.iter().find(|column| column.name == *name)?;
        sql::reject_literal_with(column, value, resolved.get(index).copied().unwrap_or(false), preview_bytes)
    })
}

fn located_sql<'a>(schema: &'a TableSchema, locator: &'a [(String, Cell)], update: bool) -> Result<&'a [(String, Cell)], String> {
    let verb = if update { "修改" } else { "删除" };
    if schema.key.is_empty() {
        return Err(format!("这张表没有主键，无法唯一确定这一行，不能{verb}"));
    }
    if locator.len() != schema.key.len() || locator.iter().zip(&schema.key).any(|(item, key)| item.0 != *key) {
        return Err(format!("主键值不完整，无法唯一确定这一行，不能{verb}"));
    }
    if locator.iter().any(|(_, value)| matches!(value, Cell::Null)) {
        return Err(format!("主键值里有空值，无法唯一确定这一行，不能{verb}"));
    }
    Ok(locator)
}

async fn expect_one(session: &CatalogSession, schema: &TableSchema, locator: &[(String, Cell)]) -> Result<(), String> {
    let where_sql = locator
        .iter()
        .map(|(name, value)| {
            let column = schema.columns.iter().find(|column| column.name == *name);
            format!("{} = {}", sql::quote_ident(schema.engine, name), sql::sql_cell(schema.engine, column, value))
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    let sql = format!("SELECT COUNT(*) FROM {} WHERE {where_sql}", sql::qualified_name(schema));
    let count = fetch_count(session, &sql).await?;
    if count == 1 {
        Ok(())
    } else if count == 0 {
        Err("数据库里已找不到这一行，不能修改或删除".to_owned())
    } else {
        Err(format!("影响 {count} 行，已停止。已成功的语句不撤销。"))
    }
}

pub async fn run_export(session: &CatalogSession, spec: &ExportSpec) -> ExportOutcome {
    let flag = begin_job(&spec.id);
    let outcome = if flag.load(Ordering::Relaxed) {
        cancelled(0)
    } else {
        export_body(session, spec, &flag).await
    };
    finish_job(&spec.id);
    if outcome.success && outcome.status != "done" {
        return ExportOutcome { success: false, status: "failed", ..outcome };
    }
    outcome
}

pub fn request_cancel(id: &str) {
    let mut jobs = jobs().lock().expect("exports");
    if let Some(flag) = jobs.get(id) {
        flag.store(true, Ordering::Relaxed);
    } else {
        jobs.insert(id.to_owned(), std::sync::Arc::new(AtomicBool::new(true)));
    }
}

fn jobs() -> &'static Mutex<HashMap<String, std::sync::Arc<AtomicBool>>> {
    static JOBS: OnceLock<Mutex<HashMap<String, std::sync::Arc<AtomicBool>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn begin_job(id: &str) -> std::sync::Arc<AtomicBool> {
    let mut jobs = jobs().lock().expect("exports");
    if let Some(flag) = jobs.get(id) {
        return std::sync::Arc::clone(flag);
    }
    let flag = std::sync::Arc::new(AtomicBool::new(false));
    jobs.insert(id.to_owned(), std::sync::Arc::clone(&flag));
    flag
}

fn finish_job(id: &str) {
    jobs().lock().expect("exports").remove(id);
}

fn export_format(spec: &ExportSpec) -> Result<&'static str, ExportOutcome> {
    match spec.format.as_str() {
        "" | "csv" => Ok("csv"),
        "json" => Ok("json"),
        "markdown" => Ok("markdown"),
        "insert" => {
            if spec.table.trim().is_empty() {
                Err(blocked("INSERT 导出需要一张表的名字"))
            } else {
                Ok("insert")
            }
        }
        "xlsx" => Ok("xlsx"),
        other => Err(blocked(format!("未知导出格式 {other}"))),
    }
}

async fn export_body(session: &CatalogSession, spec: &ExportSpec, flag: &AtomicBool) -> ExportOutcome {
    if let Err(outcome) = export_format(spec) {
        return outcome;
    }
    if session.is_scripted() {
        return failed_message(no_channel());
    }
    match spec.mode {
        ExportMode::Query => {
            let statement = match sql::select_export_gate_for(&spec.engine_hint, &spec.statement) {
                Ok(statement) => statement,
                Err(reason) => return blocked(reason),
            };
            if !spec.acknowledged {
                return blocked("执行前需说明会重新执行这条 SELECT".to_owned());
            }
            stream_statement(session, &statement, spec, flag, false, None, crate::adapters::registry::sql_engine(&spec.engine_hint), &[]).await
        }
        ExportMode::TableLoaded | ExportMode::TableFull => {
            let schema = match describe(session, &spec.database, &spec.schema, &spec.table, &spec.engine_hint).await {
                Ok(schema) => schema,
                Err(message) => return failed_message(message),
            };
            let partial = spec.mode == ExportMode::TableLoaded && spec.truncated;
            let sql = match if spec.mode == ExportMode::TableFull {
                page_sql_unlimited(&schema, &spec.filters, spec.search.as_deref(), spec.filter_expression.as_deref(), spec.sort.as_ref())
            } else {
                page_sql_full(&schema, &spec.filters, spec.search.as_deref(), spec.filter_expression.as_deref(), spec.sort.as_ref(), spec.limit)
            } {
                Ok(sql) => sql,
                Err(message) => return failed_message(message),
            };
            let columns: Vec<String> = schema.columns.iter().map(|column| column.name.clone()).collect();
            stream_statement(session, &sql, spec, flag, partial, Some(columns), schema.engine, &schema.columns).await
        }
    }
}

fn plain_columns(names: &[String], known: &[Column]) -> Vec<Column> {
    if !known.is_empty() {
        return known.to_vec();
    }
    names
        .iter()
        .map(|name| Column {
            name: name.clone(),
            type_name: String::new(),
            nullable: true,
            primary: false,
            unique: false,
            auto: false,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn stream_statement(
    session: &CatalogSession,
    sql: &str,
    spec: &ExportSpec,
    flag: &AtomicBool,
    partial: bool,
    known_names: Option<Vec<String>>,
    engine: Engine,
    columns: &[Column],
) -> ExportOutcome {
    let mut sink = FileSink { spec, flag, partial, engine, columns, csv: None, outcome: None };
    session.grid_export(sql, known_names, &mut sink).await;
    sink.outcome.unwrap_or_else(|| failed_message("导出没有完成"))
}

/// 导出文件一侧：读到列名才建文件写表头；之后每行先查取消/注入失败，再写。
struct FileSink<'a> {
    spec: &'a ExportSpec,
    flag: &'a AtomicBool,
    partial: bool,
    engine: Engine,
    columns: &'a [Column],
    csv: Option<CsvFile>,
    outcome: Option<ExportOutcome>,
}

impl FileSink<'_> {
    fn stop(&mut self, outcome: ExportOutcome) -> bool {
        self.outcome = Some(outcome);
        false
    }
}

impl ExportSink for FileSink<'_> {
    fn start(&mut self, names: &[String]) -> bool {
        let typed = plain_columns(names, self.columns);
        let mut csv = match CsvFile::create(&self.spec.path, self.partial, self.spec, &typed, self.engine) {
            Ok(csv) => csv,
            Err(outcome) => return self.stop(outcome),
        };
        csv.set_names(names);
        if let Some(outcome) = csv.control(self.flag, self.spec) {
            return self.stop(outcome);
        }
        if self.spec.header {
            if let Some(outcome) = csv.write_header(names) {
                return self.stop(outcome);
            }
        }
        self.csv = Some(csv);
        true
    }

    fn proceed(&mut self) -> bool {
        let Some(csv) = self.csv.as_mut() else { return false };
        match csv.control(self.flag, self.spec) {
            Some(outcome) => self.stop(outcome),
            None => true,
        }
    }

    fn row(&mut self, cells: &[Cell]) -> bool {
        let Some(csv) = self.csv.as_mut() else { return false };
        match csv.write_cells(cells) {
            Some(outcome) => self.stop(outcome),
            None => true,
        }
    }

    fn fail(&mut self, message: &str) {
        let outcome = match self.csv.as_mut() {
            Some(csv) => csv.fail(message),
            None => failed_message(message.to_owned()),
        };
        self.outcome = Some(outcome);
    }

    fn finish(&mut self) {
        if let Some(csv) = self.csv.take() {
            self.outcome = Some(csv.finish());
        }
    }
}

struct CsvFile {
    file: Option<File>,
    partial_path: PathBuf,
    final_path: PathBuf,
    delimiter: String,
    null_token: String,
    bom: bool,
    format: String,
    names: Vec<String>,
    columns: Vec<Column>,
    engine: Engine,
    schema_name: String,
    table_name: String,
    marked_partial: bool,
    started: bool,
    written: u64,
    xlsx_rows: Vec<Vec<Option<String>>>,
}

impl CsvFile {
    fn create(path: &str, partial: bool, spec: &ExportSpec, columns: &[Column], engine: Engine) -> Result<Self, ExportOutcome> {
        let final_path = output_path(path, partial);
        if let Some(parent) = final_path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                return Err(failed_message(format!("路径不可写：目录不存在 {}。没有生成完整结果。", parent.display())));
            }
        }
        let partial_path = PathBuf::from(format!("{}.partial", final_path.display()));
        let file = match File::create(&partial_path) {
            Ok(file) => file,
            Err(err) => return Err(failed_message(format!("路径不可写：{err}。没有生成完整结果。"))),
        };
        Ok(Self {
            file: Some(file),
            partial_path,
            final_path,
            delimiter: if spec.delimiter.is_empty() { ",".to_owned() } else { spec.delimiter.clone() },
            null_token: spec.null_token.clone(),
            bom: spec.bom,
            format: if spec.format.is_empty() { "csv".to_owned() } else { spec.format.clone() },
            names: Vec::new(),
            columns: columns.to_vec(),
            engine,
            schema_name: spec.schema.clone(),
            table_name: spec.table.clone(),
            marked_partial: partial,
            started: false,
            written: 0,
            xlsx_rows: Vec::new(),
        })
    }

    fn set_names(&mut self, names: &[String]) {
        self.names = names.to_vec();
    }

    fn write_header(&mut self, names: &[String]) -> Option<ExportOutcome> {
        self.names = names.to_vec();
        if self.format == "markdown" {
            let rule = format!("| {} |", vec!["---"; names.len()].join(" | "));
            if let Err(err) = self.write_line(&markdown_row(names.iter().map(|name| Some(name.as_str())), &self.null_token)).and_then(|()| self.write_line(&rule)) {
                return Some(self.fail(&crate::error_text::chain(&err)));
            }
            return None;
        }
        if self.format != "csv" {
            return None;
        }
        let fields = names.iter().map(|name| encode_field(Some(name), &self.null_token)).collect::<Vec<_>>();
        self.write_line(&fields.join(&self.delimiter)).err().map(|err| self.fail(&crate::error_text::chain(&err)))
    }

    fn write_cells(&mut self, cells: &[Cell]) -> Option<ExportOutcome> {
        if self.format == "xlsx" {
            self.xlsx_rows.push(cells.iter().map(xlsx_value).collect());
            self.written += 1;
            return None;
        }
        if self.format == "json" {
            return self.write_json(cells);
        }
        if self.format == "insert" {
            return self.write_insert(cells);
        }
        if self.format == "markdown" {
            return self.write_markdown(cells);
        }
        let fields = cells
            .iter()
            .map(|cell| match cell {
                Cell::Null => encode_field(None, &self.null_token),
                Cell::Text(text) => encode_field(Some(text), &self.null_token),
            })
            .collect::<Vec<_>>();
        if let Err(err) = self.write_line(&fields.join(&self.delimiter)) {
            return Some(self.fail(&crate::error_text::chain(&err)));
        }
        self.written += 1;
        None
    }

    fn write_markdown(&mut self, cells: &[Cell]) -> Option<ExportOutcome> {
        let row = markdown_row(
            cells.iter().map(|cell| match cell {
                Cell::Null => None,
                Cell::Text(text) => Some(text.as_str()),
            }),
            &self.null_token,
        );
        if let Err(err) = self.write_line(&row) {
            return Some(self.fail(&crate::error_text::chain(&err)));
        }
        self.written += 1;
        None
    }

    fn write_json(&mut self, cells: &[Cell]) -> Option<ExportOutcome> {
        let object = json_object(&self.names, cells);
        let text = if self.written == 0 { format!("[{object}") } else { format!(",{object}") };
        if let Err(err) = self.write_chunk(&text) {
            return Some(self.fail(&crate::error_text::chain(&err)));
        }
        self.written += 1;
        None
    }

    fn write_insert(&mut self, cells: &[Cell]) -> Option<ExportOutcome> {
        let line = insert_line(self.engine, &self.schema_name, &self.table_name, &self.columns, &self.names, cells);
        let text = if self.written == 0 { line } else { format!("\r\n{line}") };
        if let Err(err) = self.write_chunk(&text) {
            return Some(self.fail(&crate::error_text::chain(&err)));
        }
        self.written += 1;
        None
    }

    fn control(&mut self, flag: &AtomicBool, spec: &ExportSpec) -> Option<ExportOutcome> {
        if flag.load(Ordering::Relaxed) || spec.cancel_after_rows == Some(self.written) {
            return Some(self.cancel());
        }
        if spec.fail_after_rows == Some(self.written) {
            return Some(self.fail("写入失败"));
        }
        None
    }

    fn write_chunk(&mut self, text: &str) -> io::Result<()> {
        let file = self.file.as_mut().ok_or_else(|| io::Error::other("文件已关闭"))?;
        if !self.started {
            if self.format == "csv" && self.bom {
                file.write_all("\u{feff}".as_bytes())?;
            }
            self.started = true;
        }
        file.write_all(text.as_bytes())?;
        Ok(())
    }

    fn write_line(&mut self, line: &str) -> io::Result<()> {
        if self.started {
            self.write_chunk("\r\n")?;
        }
        self.write_chunk(line)
    }

    fn finish(mut self) -> ExportOutcome {
        if self.format == "json" {
            let tail = if self.written == 0 { "[]" } else { "]" };
            if let Err(err) = self.write_chunk(tail) {
                return residual(self.written, &self.partial_path, &crate::error_text::chain(&err));
            }
        }
        if self.format == "xlsx" {
            if let Err(err) = self.write_xlsx_package() {
                return residual(self.written, &self.partial_path, &crate::error_text::chain(&err));
            }
        }
        let flush = self.file.as_mut().map(|file| file.flush()).transpose();
        self.file.take();
        if let Err(err) = flush {
            return residual(self.written, &self.partial_path, &crate::error_text::chain(&err));
        }
        if self.final_path.exists() && fs::remove_file(&self.final_path).is_err() {
            return residual(self.written, &self.partial_path, "无法替换目标文件");
        }
        if let Err(err) = fs::rename(&self.partial_path, &self.final_path) {
            return residual(self.written, &self.partial_path, &crate::error_text::chain(&err));
        }
        let path = self.final_path.display().to_string();
        let message = if self.marked_partial {
            format!("已导出 {} 行到 {path}（仅已读取的行，不是完整结果）", self.written)
        } else {
            format!("已导出 {} 行。", self.written)
        };
        ExportOutcome {
            status: "done",
            success: true,
            written_rows: self.written,
            path: Some(path),
            residual_path: None,
            message,
            partial: self.marked_partial,
        }
    }

    fn fail(&mut self, message: &str) -> ExportOutcome {
        if self.format == "xlsx" && self.written > 0 {
            if let Err(err) = self.write_xlsx_package() {
                let message = format!("{message}；{err}");
                self.file.take();
                return residual(self.written, &self.partial_path, &message);
            }
        }
        self.file.take();
        residual(self.written, &self.partial_path, message)
    }

    fn write_xlsx_package(&mut self) -> io::Result<()> {
        let bytes = crate::export::write_xlsx(&self.names, &self.xlsx_rows).map_err(io::Error::other)?;
        let file = self.file.as_mut().ok_or_else(|| io::Error::other("文件已关闭"))?;
        file.write_all(&bytes)?;
        file.flush()?;
        Ok(())
    }

    fn cancel(&mut self) -> ExportOutcome {
        let written = self.written;
        self.file.take();
        if fs::remove_file(&self.partial_path).is_err() && self.partial_path.exists() {
            return residual(written, &self.partial_path, "取消后未能删除未完成的文件");
        }
        cancelled(written)
    }
}

fn xlsx_value(cell: &Cell) -> Option<String> {
    match cell {
        Cell::Null => None,
        Cell::Text(text) if text.is_empty() => None,
        Cell::Text(text) => Some(text.clone()),
    }
}

fn encode_field(value: Option<&str>, null_token: &str) -> String {
    match value {
        None => null_token.to_owned(),
        Some(text) => format!("\"{}\"", text.replace('"', "\"\"")),
    }
}

fn markdown_row<'a>(cells: impl Iterator<Item = Option<&'a str>>, null_token: &str) -> String {
    let fields = cells
        .map(|cell| match cell {
            None => null_token.to_owned(),
            Some(text) => text.replace('|', "\\|").replace(['\n', '\r'], " "),
        })
        .collect::<Vec<_>>();
    format!("| {} |", fields.join(" | "))
}

fn output_path(path: &str, partial: bool) -> PathBuf {
    if !partial || path.contains("（部分）") {
        return PathBuf::from(path);
    }
    for suffix in [".csv", ".CSV", ".json", ".JSON", ".sql", ".SQL", ".md", ".MD"] {
        if let Some(stripped) = path.strip_suffix(suffix) {
            return PathBuf::from(format!("{stripped}（部分）{}", suffix.to_ascii_lowercase()));
        }
    }
    PathBuf::from(format!("{path}（部分）"))
}

fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_object(names: &[String], cells: &[Cell]) -> String {
    let fields = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let value = match cells.get(index) {
                Some(Cell::Text(text)) => json_string(text),
                _ => "null".to_owned(),
            };
            format!("{}:{value}", json_string(name))
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{fields}}}")
}

/// 一行 Cell 写成一条带分号的 INSERT。全库导出（R7）的数据段复用同一套字面量规则（含二进制 hex）。
pub fn insert_line(engine: Engine, schema_name: &str, table: &str, columns: &[Column], names: &[String], cells: &[Cell]) -> String {
    let values = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let cell = cells.get(index).cloned().unwrap_or(Cell::Null);
            (name.clone(), cell)
        })
        .collect::<Vec<_>>();
    let schema = TableSchema {
        engine,
        schema: schema_name.to_owned(),
        name: table.to_owned(),
        kind: "table",
        columns: columns.to_vec(),
        key: Vec::new(),
    };
    format!("{};", sql::insert_sql(&schema, &values))
}

fn cancelled(written: u64) -> ExportOutcome {
    ExportOutcome {
        status: "cancelled",
        success: false,
        written_rows: written,
        path: None,
        residual_path: None,
        message: format!("已取消导出：写入 {written} 行后停止，未完成的文件已删除。"),
        partial: false,
    }
}

fn blocked(message: impl Into<String>) -> ExportOutcome {
    ExportOutcome {
        status: "blocked",
        success: false,
        written_rows: 0,
        path: None,
        residual_path: None,
        message: message.into(),
        partial: false,
    }
}

fn failed_message(message: impl Into<String>) -> ExportOutcome {
    let message = message.into();
    let message = if message.contains("没有生成完整结果") { message } else { format!("{message}。没有生成完整结果。") };
    ExportOutcome { status: "failed", success: false, written_rows: 0, path: None, residual_path: None, message, partial: false }
}

fn residual(written: u64, path: &Path, message: &str) -> ExportOutcome {
    ExportOutcome {
        status: "failed",
        success: false,
        written_rows: written,
        path: None,
        residual_path: Some(path.display().to_string()),
        message: format!("{message}。已写入 {written} 行，不完整的文件保留为 {}，不会冒充完整结果。", path.display()),
        partial: false,
    }
}

#[allow(clippy::too_many_arguments)]
fn page_sql(schema: &TableSchema, filters: &[Filter], search: Option<&str>, expression: Option<&str>, sort: Option<&Sort>, limit: u64, offset: u64, preview_bytes: usize) -> Result<String, String> {
    let (body, ordered) = page_body(schema, filters, search, expression, sort, false, preview_bytes)?;
    let tail = sql::page_tail(schema.engine, limit, offset, ordered);
    Ok(format!("{body}{tail}"))
}

#[allow(clippy::too_many_arguments)]
fn page_sql_full(schema: &TableSchema, filters: &[Filter], search: Option<&str>, expression: Option<&str>, sort: Option<&Sort>, limit: u64) -> Result<String, String> {
    let (body, ordered) = page_body(schema, filters, search, expression, sort, true, 1024)?;
    let tail = sql::page_tail(schema.engine, limit, 0, ordered);
    Ok(format!("{body}{tail}"))
}

fn page_sql_unlimited(schema: &TableSchema, filters: &[Filter], search: Option<&str>, expression: Option<&str>, sort: Option<&Sort>) -> Result<String, String> {
    page_body(schema, filters, search, expression, sort, true, 1024).map(|(sql, _)| sql)
}

#[allow(clippy::too_many_arguments)]
fn page_body(schema: &TableSchema, filters: &[Filter], search: Option<&str>, expression: Option<&str>, sort: Option<&Sort>, full: bool, preview_bytes: usize) -> Result<(String, bool), String> {
    let list = sql::select_list_preview(schema, full || sql::reads_full_values(schema, filters), preview_bytes);
    let where_sql = sql::compile_where_with_condition(schema, filters, search, expression)?;
    let clause = if where_sql.is_empty() { String::new() } else { format!(" WHERE {where_sql}") };
    let order = order_clause(schema, sort);
    let ordered = order.starts_with(" ORDER BY");
    Ok((format!("SELECT {list} FROM {}{clause}{}", sql::qualified_name(schema), order), ordered))
}

fn count_sql(schema: &TableSchema, filters: &[Filter], search: Option<&str>, expression: Option<&str>) -> Result<String, String> {
    let where_sql = sql::compile_where_with_condition(schema, filters, search, expression)?;
    let clause = if where_sql.is_empty() { String::new() } else { format!(" WHERE {where_sql}") };
    let count = crate::adapters::query::sql_syntax(schema.engine).count_expression();
    Ok(format!("SELECT {count} FROM {}{clause}", sql::qualified_name(schema)))
}

/// 排序列一律带上表名限定。选择列表里的列都被转成文本（PostgreSQL 的 `"id"::text`
/// 输出列名仍叫 id），而 ORDER BY 里的裸名字优先匹配输出列：不限定就会按文本排，
/// 整数主键成了 1、10、100、1000……；限定后指向表里的原始列，按真实类型排序。
fn order_column(schema: &TableSchema, column: &str) -> String {
    format!("{}.{}", sql::qualified_name(schema), sql::quote_ident(schema.engine, column))
}

fn order_clause(schema: &TableSchema, sort: Option<&Sort>) -> String {
    if let Some(sort) = sort {
        return format!(" ORDER BY {} {}", order_column(schema, &sort.column), if sort.descending { "DESC" } else { "ASC" });
    }
    if !schema.key.is_empty() {
        let columns = schema
            .key
            .iter()
            .map(|name| format!("{} ASC", order_column(schema, name)))
            .collect::<Vec<_>>()
            .join(", ");
        return format!(" ORDER BY {columns}");
    }
    if let Some(order) = crate::adapters::query::sql_syntax(schema.engine).implicit_order(schema.kind) {
        return format!(" ORDER BY {order}");
    }
    String::new()
}

fn live_row(schema: &TableSchema, cells: Vec<Cell>, offset: u64, index: usize) -> LiveRow {
    let mut map = BTreeMap::new();
    for (column, cell) in schema.columns.iter().zip(cells) {
        map.insert(column.name.clone(), cell);
    }
    let located = schema.kind == "table"
        && !schema.key.is_empty()
        && schema.key.iter().all(|name| matches!(map.get(name), Some(Cell::Text(_))));
    let locator = if located {
        schema
            .key
            .iter()
            .map(|name| (name.clone(), map.get(name).cloned().unwrap_or(Cell::Null)))
            .collect()
    } else {
        Vec::new()
    };
    let id = if located {
        let key = schema
            .key
            .iter()
            .map(|name| match map.get(name) {
                Some(Cell::Text(text)) => text.as_str(),
                _ => "",
            })
            .collect::<Vec<_>>()
            .join("\u{1f}");
        format!("k:{key}")
    } else {
        format!("o:{offset}:{index}")
    };
    LiveRow { id, cells: map, located, locator }
}

/// 按列数把一页行读成 Cell 矩阵。全库导出（R7）的流式分页读走这里，与网格同一套单元转换。
pub async fn fetch_cells(session: &CatalogSession, sql: &str, width: usize) -> Result<Vec<Vec<Cell>>, String> {
    session.grid_fetch_cells(sql, width).await
}

/// 单格完整原值读取：查看、复制、编辑、导出和 WHERE 需要完整值时共用这一次定位读取。
/// 只按唯一键定位一行；命中不到或不止一行都不猜值，也不把预览信封当原值返回。
pub async fn read_full_cell(session: &CatalogSession, schema: &TableSchema, locator: &[(String, Cell)], column: &str) -> Result<Cell, String> {
    if schema.key.is_empty() {
        return Err("这张表没有唯一键，无法定位到某一行读取完整值".to_owned());
    }
    if locator.len() != schema.key.len() || locator.iter().zip(&schema.key).any(|(item, key)| item.0 != *key) {
        return Err("定位值不完整，没有读取完整值".to_owned());
    }
    if locator.iter().any(|(_, value)| matches!(value, Cell::Null)) {
        return Err("定位值里有空值，无法唯一确定这一行".to_owned());
    }
    let index = schema
        .columns
        .iter()
        .position(|item| item.name == column)
        .ok_or_else(|| format!("列 {column} 不在当前表结构里"))?;
    // 完整投影：同一行其它列也一起读回，避免下一个格子再定位一次。
    let list = sql::select_list_preview(schema, true, 1024);
    let statement = format!(
        "SELECT {list} FROM {} WHERE {}{}",
        sql::qualified_name(schema),
        sql::locator_sql(schema, locator),
        sql::page_tail(schema.engine, 1, 0, false)
    );
    let mut rows = fetch_cells(session, &statement, schema.columns.len()).await?;
    match rows.len() {
        1 => Ok(rows.remove(0).into_iter().nth(index).unwrap_or(Cell::Null)),
        0 => Err("数据库里已经找不到这一行".to_owned()),
        count => Err(format!("定位命中 {count} 行，没有读取完整值")),
    }
}

async fn fetch_count(session: &CatalogSession, sql: &str) -> Result<i64, String> {
    session.grid_fetch_count(sql).await
}

async fn execute_sql(session: &CatalogSession, sql: &str) -> Result<u64, String> {
    session.grid_execute(sql).await
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

/// 把目录行收成表级外键。空名、空列和缺引用目标的行跳过；同名约束按行序合并成复合键。
pub(crate) fn assemble_foreign_keys(rows: &[Vec<Cell>]) -> Vec<ForeignKey> {
    let mut list = Vec::new();
    for row in rows {
        let cell = |index: usize| match row.get(index) {
            Some(Cell::Text(text)) => text.clone(),
            _ => String::new(),
        };
        let name = cell(0);
        let column = cell(1);
        if name.is_empty() || column.is_empty() {
            continue;
        }
        let ref_schema = cell(2);
        let ref_table = cell(3);
        let ref_column = cell(4);
        if ref_table.is_empty() || ref_column.is_empty() {
            continue;
        }
        push_foreign_key(&mut list, &name, &column, &ref_schema, &ref_table, &ref_column);
    }
    list
}

pub(crate) fn push_foreign_key(list: &mut Vec<ForeignKey>, name: &str, column: &str, ref_schema: &str, ref_table: &str, ref_column: &str) {
    if let Some(existing) = list.iter_mut().find(|item| item.name == name) {
        existing.columns.push(column.to_owned());
        existing.ref_columns.push(ref_column.to_owned());
        return;
    }
    list.push(ForeignKey {
        name: name.to_owned(),
        columns: vec![column.to_owned()],
        ref_schema: ref_schema.to_owned(),
        ref_table: ref_table.to_owned(),
        ref_columns: vec![ref_column.to_owned()],
    });
}

/// 表级出向外键；只读，失败不影响表结构本身的读取。
pub async fn foreign_keys(session: &CatalogSession, database: &str, schema_name: &str, table: &str, _engine_hint: &str) -> Result<Vec<ForeignKey>, String> {
    if table.is_empty() {
        return Ok(Vec::new());
    }
    session.grid_foreign_keys(database, schema_name, table).await
}

#[cfg(test)]
mod order_tests {
    use super::*;
    use crate::grid::store::{Column, Sort, TableSchema};

    fn schema(engine: Engine, schema: &str) -> TableSchema {
        let col = |name: &str, type_name: &str| Column { name: name.to_owned(), type_name: type_name.to_owned(), nullable: false, primary: name == "id", unique: false, auto: false };
        TableSchema {
            engine,
            schema: schema.to_owned(),
            name: "customers".to_owned(),
            kind: "table",
            columns: vec![col("id", "integer"), col("amount", "numeric")],
            key: vec!["id".to_owned()],
        }
    }

    #[test]
    fn order_by_targets_the_table_column_not_the_text_cast() {
        // PostgreSQL 的 `"id"::text` 输出列仍叫 id；ORDER BY "id" 会按文本排出 1、10、100……
        let pg = schema(Engine::Postgres, "shop");
        let default = page_sql(&pg, &[], None, None, None, 100, 0, 1024).unwrap();
        assert!(default.contains("id::text"), "{default}");
        assert!(default.contains(" ORDER BY shop.customers.id ASC LIMIT 100"), "{default}");
        let sorted = page_sql(&pg, &[], None, None, Some(&Sort { column: "amount".to_owned(), descending: true }), 100, 0, 1024).unwrap();
        assert!(sorted.contains(" ORDER BY shop.customers.amount DESC"), "{sorted}");
        let full = page_sql_full(&pg, &[], None, None, None, 10).unwrap();
        assert!(full.contains(" ORDER BY shop.customers.id ASC LIMIT 10"), "{full}");
        let mysql = page_sql(&schema(Engine::Mysql, "app"), &[], None, None, None, 5, 0, 1024).unwrap();
        assert!(mysql.contains(" ORDER BY app.customers.id ASC LIMIT 5"), "{mysql}");
        let mssql = page_sql(&schema(Engine::Sqlserver, "dbo"), &[], None, None, None, 5, 10, 1024).unwrap();
        assert!(mssql.contains(" ORDER BY dbo.customers.id ASC OFFSET 10 ROWS"), "{mssql}");
    }
}
