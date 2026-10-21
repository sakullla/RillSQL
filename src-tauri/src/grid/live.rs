//! 在已经打开的会话上查询、逐条提交和流式导出。不新开数据库连接。

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use futures::TryStreamExt;
use sqlx::Column as _;
use sqlx::Executor;
use sqlx::SqlSafeStr;
use sqlx::Statement as _;

use crate::connections::CatalogSession;

use super::sql;
use super::store::{Applied, Cell, Column, CommitReport, Engine, Failed, Filter, Sort, TableSchema, COMMIT_RULE};

const MAX_PAGE: u64 = 10_000;

fn db_err(err: impl std::fmt::Display) -> String {
    err.to_string()
}

/// 标识符已由结构化编译器加引号，字面量也已转义。这里只执行那条 SQL，不接收手写片段。
fn audited(statement: &str) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(statement.to_owned())
}

fn no_channel() -> String {
    "这个连接没有连着可查询的数据库".to_owned()
}

fn other_database(current: &str, name: &str) -> String {
    format!("当前连接打开的是 {current}，没有另开连接读取 {name}")
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
    match session {
        CatalogSession::Scripted => Err(no_channel()),
        CatalogSession::Sqlite { pool, .. } => {
            if !database.is_empty() && database != "main" {
                return Err(other_database("main", database));
            }
            describe_sqlite(pool, table).await
        }
        CatalogSession::Mysql(pool) => {
            if database.is_empty() {
                return Err("需要数据库名，没有另开连接。".to_owned());
            }
            let engine = if engine_hint == "mariadb" { Engine::Mariadb } else { Engine::Mysql };
            describe_mysql(pool, engine, database, table).await
        }
        CatalogSession::Postgres(client) => {
            ensure_postgres_database(client, database).await?;
            if schema.is_empty() {
                return Err("PostgreSQL 需要指定 schema（表所在的分组名），没有另开连接。".to_owned());
            }
            describe_postgres(client, schema, table).await
        }
    }
}

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
    let schema = describe(session, database, schema_name, table, engine_hint).await?;
    let limit = limit.min(MAX_PAGE);
    if let Some(sort) = sort {
        if !schema.columns.iter().any(|column| column.name == sort.column) {
            return Err(format!("未知排序列 {}", sort.column));
        }
    }
    let display = page_sql(&schema, filters, sort, limit, offset)?;
    if limit == 0 {
        return Ok(LivePage { rows: Vec::new(), truncated: false, sql: display });
    }
    let executed = page_sql(&schema, filters, sort, limit.saturating_add(1), offset)?;
    let width = schema.columns.len();
    let mut fetched = fetch_cells(session, &executed, width).await?;
    let truncated = fetched.len() as u64 > limit;
    if truncated {
        fetched.truncate(limit as usize);
    }
    let rows = fetched
        .into_iter()
        .enumerate()
        .map(|(index, cells)| live_row(&schema, cells, offset, index))
        .collect();
    Ok(LivePage { rows, truncated, sql: display })
}

pub async fn count_rows(
    session: &CatalogSession,
    database: &str,
    schema_name: &str,
    table: &str,
    engine_hint: &str,
    filters: &[Filter],
) -> Result<i64, String> {
    let schema = describe(session, database, schema_name, table, engine_hint).await?;
    let sql = count_sql(&schema, filters)?;
    fetch_count(session, &sql).await
}

pub async fn commit(
    session: &CatalogSession,
    database: &str,
    schema_name: &str,
    table: &str,
    engine_hint: &str,
    changes: &[Mutation],
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
                if let Some(message) = reject_sets(&schema, &change.sets, &change.resolved) {
                    return Ok(fail(applied, &change.id, String::new(), message, pending));
                }
                let sql = match located_sql(&schema, &change.locator, true) {
                    Ok(locator) => sql::update_sql(&schema, &change.sets, &locator),
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
                    Ok(locator) => sql::delete_sql(&schema, &locator),
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
                if let Some(message) = reject_sets(&schema, &change.values, &[]) {
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

fn reject_sets(schema: &TableSchema, sets: &[(String, Cell)], resolved: &[bool]) -> Option<String> {
    sets.iter().enumerate().find_map(|(index, (name, value))| {
        let column = schema.columns.iter().find(|column| column.name == *name)?;
        sql::reject_literal_with(column, value, resolved.get(index).copied().unwrap_or(false))
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

fn engine_from(hint: &str) -> Engine {
    match hint {
        "mysql" => Engine::Mysql,
        "mariadb" => Engine::Mariadb,
        "sqlite" => Engine::Sqlite,
        _ => Engine::Postgres,
    }
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
    if matches!(session, CatalogSession::Scripted) {
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
            stream_statement(session, &statement, spec, flag, false, None, engine_from(&spec.engine_hint), &[]).await
        }
        ExportMode::TableLoaded | ExportMode::TableFull => {
            let schema = match describe(session, &spec.database, &spec.schema, &spec.table, &spec.engine_hint).await {
                Ok(schema) => schema,
                Err(message) => return failed_message(message),
            };
            let partial = spec.mode == ExportMode::TableLoaded && spec.truncated;
            let sql = match if spec.mode == ExportMode::TableFull {
                page_sql_unlimited(&schema, &spec.filters, spec.sort.as_ref())
            } else {
                page_sql_full(&schema, &spec.filters, spec.sort.as_ref(), spec.limit)
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
    match session {
        CatalogSession::Scripted => failed_message(no_channel()),
        CatalogSession::Sqlite { pool, .. } => {
            let names = match known_names {
                Some(names) => names,
                None => {
                    let mut conn = match pool.acquire().await {
                        Ok(conn) => conn,
                        Err(err) => return failed_message(err.to_string()),
                    };
                    let statement = match conn.prepare(audited(sql).into_sql_str()).await {
                        Ok(statement) => statement,
                        Err(err) => return failed_message(err.to_string()),
                    };
                    statement.columns().iter().map(|column| column.name().to_owned()).collect()
                }
            };
            let typed = plain_columns(&names, columns);
            let mut csv = match CsvFile::create(&spec.path, partial, spec, &typed, engine) {
                Ok(csv) => csv,
                Err(outcome) => return outcome,
            };
            csv.set_names(&names);
            if let Some(outcome) = csv.control(flag, spec) {
                return outcome;
            }
            if spec.header {
                if let Some(outcome) = csv.write_header(&names) {
                    return outcome;
                }
            }
            let mut stream = sqlx::query(audited(sql)).fetch(pool);
            loop {
                if let Some(outcome) = csv.control(flag, spec) {
                    return outcome;
                }
                match stream.try_next().await {
                    Ok(Some(row)) => {
                        let cells = match read_width(&row, names.len(), sqlite_cell) {
                            Ok(cells) => cells,
                            Err(message) => return csv.fail(&message),
                        };
                        if let Some(outcome) = csv.write_cells(&cells) {
                            return outcome;
                        }
                    }
                    Ok(None) => return csv.finish(),
                    Err(err) => return csv.fail(&err.to_string()),
                }
            }
        }
        CatalogSession::Mysql(pool) => {
            let mut conn = match pool.get_conn().await {
                Ok(conn) => conn,
                Err(err) => return failed_message(err.to_string()),
            };
            let mut result = match mysql_async::prelude::Queryable::query_iter(&mut conn, sql).await {
                Ok(result) => result,
                Err(err) => return failed_message(err.to_string()),
            };
            let names = known_names.unwrap_or_else(|| result.columns_ref().iter().map(|column| column.name_str().into_owned()).collect());
            let typed = plain_columns(&names, columns);
            let mut csv = match CsvFile::create(&spec.path, partial, spec, &typed, engine) {
                Ok(csv) => csv,
                Err(outcome) => return outcome,
            };
            csv.set_names(&names);
            if let Some(outcome) = csv.control(flag, spec) {
                return outcome;
            }
            if spec.header {
                if let Some(outcome) = csv.write_header(&names) {
                    return outcome;
                }
            }
            loop {
                if let Some(outcome) = csv.control(flag, spec) {
                    return outcome;
                }
                match result.next().await {
                    Ok(Some(row)) => {
                        let cells = (0..names.len()).map(|index| mysql_cell(row.as_ref(index).unwrap_or(&mysql_async::Value::NULL))).collect::<Vec<_>>();
                        if let Some(outcome) = csv.write_cells(&cells) {
                            return outcome;
                        }
                    }
                    Ok(None) => return csv.finish(),
                    Err(err) => return csv.fail(&err.to_string()),
                }
            }
        }
        CatalogSession::Postgres(client) => {
            let names = match known_names {
                Some(names) => names,
                None => {
                    let statement = match client.prepare(sql).await {
                        Ok(statement) => statement,
                        Err(err) => return failed_message(err.to_string()),
                    };
                    statement.columns().iter().map(|column| column.name().to_owned()).collect()
                }
            };
            let statement = match client.prepare(sql).await {
                Ok(statement) => statement,
                Err(err) => return failed_message(err.to_string()),
            };
            let typed = plain_columns(&names, columns);
            let mut csv = match CsvFile::create(&spec.path, partial, spec, &typed, engine) {
                Ok(csv) => csv,
                Err(outcome) => return outcome,
            };
            csv.set_names(&names);
            if let Some(outcome) = csv.control(flag, spec) {
                return outcome;
            }
            if spec.header {
                if let Some(outcome) = csv.write_header(&names) {
                    return outcome;
                }
            }
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 0] = [];
            let stream = match client.query_raw(&statement, params).await {
                Ok(stream) => stream,
                Err(err) => return csv.fail(&err.to_string()),
            };
            futures::pin_mut!(stream);
            loop {
                if let Some(outcome) = csv.control(flag, spec) {
                    return outcome;
                }
                match stream.try_next().await {
                    Ok(Some(row)) => {
                        let cells = match (0..names.len()).map(|index| pg_cell(&row, index)).collect::<Result<Vec<_>, _>>() {
                            Ok(cells) => cells,
                            Err(message) => return csv.fail(&message),
                        };
                        if let Some(outcome) = csv.write_cells(&cells) {
                            return outcome;
                        }
                    }
                    Ok(None) => return csv.finish(),
                    Err(err) => return csv.fail(&err.to_string()),
                }
            }
        }
    }
}

fn read_width(row: &sqlx::sqlite::SqliteRow, width: usize, read: fn(&sqlx::sqlite::SqliteRow, usize) -> Result<Cell, String>) -> Result<Vec<Cell>, String> {
    (0..width).map(|index| read(row, index)).collect()
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
                return Some(self.fail(&err.to_string()));
            }
            return None;
        }
        if self.format != "csv" {
            return None;
        }
        let fields = names.iter().map(|name| encode_field(Some(name), &self.null_token)).collect::<Vec<_>>();
        self.write_line(&fields.join(&self.delimiter)).err().map(|err| self.fail(&err.to_string()))
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
            return Some(self.fail(&err.to_string()));
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
            return Some(self.fail(&err.to_string()));
        }
        self.written += 1;
        None
    }

    fn write_json(&mut self, cells: &[Cell]) -> Option<ExportOutcome> {
        let object = json_object(&self.names, cells);
        let text = if self.written == 0 { format!("[{object}") } else { format!(",{object}") };
        if let Err(err) = self.write_chunk(&text) {
            return Some(self.fail(&err.to_string()));
        }
        self.written += 1;
        None
    }

    fn write_insert(&mut self, cells: &[Cell]) -> Option<ExportOutcome> {
        let line = insert_line(self.engine, &self.schema_name, &self.table_name, &self.columns, &self.names, cells);
        let text = if self.written == 0 { line } else { format!("\r\n{line}") };
        if let Err(err) = self.write_chunk(&text) {
            return Some(self.fail(&err.to_string()));
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
                return residual(self.written, &self.partial_path, &err.to_string());
            }
        }
        if self.format == "xlsx" {
            if let Err(err) = self.write_xlsx_package() {
                return residual(self.written, &self.partial_path, &err.to_string());
            }
        }
        let flush = self.file.as_mut().map(|file| file.flush()).transpose();
        self.file.take();
        if let Err(err) = flush {
            return residual(self.written, &self.partial_path, &err.to_string());
        }
        if self.final_path.exists() && fs::remove_file(&self.final_path).is_err() {
            return residual(self.written, &self.partial_path, "无法替换目标文件");
        }
        if let Err(err) = fs::rename(&self.partial_path, &self.final_path) {
            return residual(self.written, &self.partial_path, &err.to_string());
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
        let bytes = xlsx_package(&self.names, &self.xlsx_rows);
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

fn column_letters(index: usize) -> String {
    let mut n = index + 1;
    let mut chars = Vec::new();
    while n > 0 {
        n -= 1;
        chars.push(b'A' + (n % 26) as u8);
        n /= 26;
    }
    chars.reverse();
    String::from_utf8(chars).unwrap_or_else(|_| "A".to_owned())
}

fn xml_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c == '\u{9}' || c == '\n' || c == '\r' || (c >= '\u{20}' && c != '\u{FFFE}' && c != '\u{FFFF}') => out.push(c),
            _ => {}
        }
    }
    out
}

fn xlsx_row<'a>(row_number: usize, cells: impl Iterator<Item = Option<&'a str>>) -> String {
    let mut out = format!("<row r=\"{row_number}\">");
    for (index, cell) in cells.enumerate() {
        let Some(text) = cell else { continue };
        let refer = format!("{}{row_number}", column_letters(index));
        out.push_str(&format!(
            "<c r=\"{refer}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
            xml_text(text)
        ));
    }
    out.push_str("</row>");
    out
}

fn worksheet_xml(names: &[String], rows: &[Vec<Option<String>>]) -> String {
    let cols = names.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
    let height = rows.len() + usize::from(!names.is_empty());
    let dim = if cols == 0 {
        "A1".to_owned()
    } else {
        format!("A1:{}{}", column_letters(cols - 1), height.max(1))
    };
    let mut body = String::new();
    let mut row_number = 1usize;
    if !names.is_empty() {
        body.push_str(&xlsx_row(row_number, names.iter().map(|name| Some(name.as_str()))));
        row_number += 1;
    }
    for row in rows {
        let cells = row.iter().map(|cell| cell.as_deref());
        body.push_str(&xlsx_row(row_number, cells));
        row_number += 1;
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><dimension ref=\"{dim}\"/><sheetData>{body}</sheetData></worksheet>"
    )
}

fn xlsx_package(names: &[String], rows: &[Vec<Option<String>>]) -> Vec<u8> {
    let sheet = worksheet_xml(names, rows);
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
    let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
    let workbook_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
    zip_store(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        ("xl/_rels/workbook.xml.rels", workbook_rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
    ])
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn zip_store(items: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in items {
        let offset = u32::try_from(out.len()).unwrap_or(u32::MAX);
        let crc = crc32(data);
        let name_bytes = name.as_bytes();
        let size = u32::try_from(data.len()).unwrap_or(u32::MAX);
        write_u32(&mut out, 0x0403_4B50);
        write_u16(&mut out, 20);
        write_u16(&mut out, 0);
        write_u16(&mut out, 0);
        write_u16(&mut out, 0);
        write_u16(&mut out, 0x0021);
        write_u32(&mut out, crc);
        write_u32(&mut out, size);
        write_u32(&mut out, size);
        write_u16(&mut out, u16::try_from(name_bytes.len()).unwrap_or(u16::MAX));
        write_u16(&mut out, 0);
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);

        write_u32(&mut central, 0x0201_4B50);
        write_u16(&mut central, 20);
        write_u16(&mut central, 20);
        write_u16(&mut central, 0);
        write_u16(&mut central, 0);
        write_u16(&mut central, 0);
        write_u16(&mut central, 0x0021);
        write_u32(&mut central, crc);
        write_u32(&mut central, size);
        write_u32(&mut central, size);
        write_u16(&mut central, u16::try_from(name_bytes.len()).unwrap_or(u16::MAX));
        write_u16(&mut central, 0);
        write_u16(&mut central, 0);
        write_u16(&mut central, 0);
        write_u16(&mut central, 0);
        write_u32(&mut central, 0);
        write_u32(&mut central, offset);
        central.extend_from_slice(name_bytes);
    }
    let cd_offset = u32::try_from(out.len()).unwrap_or(u32::MAX);
    let cd_size = u32::try_from(central.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&central);
    write_u32(&mut out, 0x0605_4B50);
    write_u16(&mut out, 0);
    write_u16(&mut out, 0);
    write_u16(&mut out, u16::try_from(items.len()).unwrap_or(u16::MAX));
    write_u16(&mut out, u16::try_from(items.len()).unwrap_or(u16::MAX));
    write_u32(&mut out, cd_size);
    write_u32(&mut out, cd_offset);
    write_u16(&mut out, 0);
    out
}

fn write_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
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

fn insert_line(engine: Engine, schema_name: &str, table: &str, columns: &[Column], names: &[String], cells: &[Cell]) -> String {
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

fn page_sql(schema: &TableSchema, filters: &[Filter], sort: Option<&Sort>, limit: u64, offset: u64) -> Result<String, String> {
    let body = page_body(schema, filters, sort, false)?;
    let tail = if offset == 0 { format!(" LIMIT {limit}") } else { format!(" LIMIT {limit} OFFSET {offset}") };
    Ok(format!("{body}{tail}"))
}

fn page_sql_full(schema: &TableSchema, filters: &[Filter], sort: Option<&Sort>, limit: u64) -> Result<String, String> {
    let body = page_body(schema, filters, sort, true)?;
    Ok(format!("{body} LIMIT {limit}"))
}

fn page_sql_unlimited(schema: &TableSchema, filters: &[Filter], sort: Option<&Sort>) -> Result<String, String> {
    page_body(schema, filters, sort, true)
}

fn page_body(schema: &TableSchema, filters: &[Filter], sort: Option<&Sort>, full: bool) -> Result<String, String> {
    let list = sql::select_list(schema, full || sql::reads_full_values(schema, filters));
    let where_sql = sql::compile_where(schema, filters)?;
    let clause = if where_sql.is_empty() { String::new() } else { format!(" WHERE {where_sql}") };
    Ok(format!("SELECT {list} FROM {}{clause}{}", sql::qualified_name(schema), order_clause(schema, sort)))
}

fn count_sql(schema: &TableSchema, filters: &[Filter]) -> Result<String, String> {
    let where_sql = sql::compile_where(schema, filters)?;
    let clause = if where_sql.is_empty() { String::new() } else { format!(" WHERE {where_sql}") };
    let count = match schema.engine {
        Engine::Postgres => "COUNT(*)::bigint",
        Engine::Sqlite | Engine::Mysql | Engine::Mariadb => "COUNT(*)",
    };
    Ok(format!("SELECT {count} FROM {}{clause}", sql::qualified_name(schema)))
}

fn order_clause(schema: &TableSchema, sort: Option<&Sort>) -> String {
    if let Some(sort) = sort {
        return format!(" ORDER BY {} {}", sql::quote_ident(schema.engine, &sort.column), if sort.descending { "DESC" } else { "ASC" });
    }
    if !schema.key.is_empty() {
        let columns = schema
            .key
            .iter()
            .map(|name| format!("{} ASC", sql::quote_ident(schema.engine, name)))
            .collect::<Vec<_>>()
            .join(", ");
        return format!(" ORDER BY {columns}");
    }
    if schema.engine == Engine::Sqlite && schema.kind == "table" {
        return " ORDER BY rowid".to_owned();
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

async fn fetch_cells(session: &CatalogSession, sql: &str, width: usize) -> Result<Vec<Vec<Cell>>, String> {
    match session {
        CatalogSession::Scripted => Err(no_channel()),
        CatalogSession::Sqlite { pool, .. } => {
            let rows = sqlx::query(audited(sql)).fetch_all(pool).await.map_err(db_err)?;
            rows.iter().map(|row| read_width(row, width, sqlite_cell)).collect()
        }
        CatalogSession::Mysql(pool) => {
            let (_names, rows) = mysql_rows(pool, sql).await?;
            Ok(rows)
        }
        CatalogSession::Postgres(client) => {
            let rows = client.query(sql, &[]).await.map_err(db_err)?;
            rows.iter()
                .map(|row| (0..width).map(|index| pg_cell(row, index)).collect::<Result<Vec<_>, _>>())
                .collect()
        }
    }
}

async fn fetch_count(session: &CatalogSession, sql: &str) -> Result<i64, String> {
    match session {
        CatalogSession::Scripted => Err(no_channel()),
        CatalogSession::Sqlite { pool, .. } => sqlx::query_scalar(audited(sql)).fetch_one(pool).await.map_err(db_err),
        CatalogSession::Mysql(pool) => {
            let (_names, rows) = mysql_rows(pool, sql).await?;
            match rows.first().and_then(|row| row.first()) {
                Some(Cell::Text(text)) => text.parse::<i64>().map_err(|_| format!("计数无效 {text}")),
                _ => Err("没有计数".to_owned()),
            }
        }
        CatalogSession::Postgres(client) => {
            let row = client.query_one(sql, &[]).await.map_err(db_err)?;
            row.try_get(0).map_err(db_err)
        }
    }
}

async fn execute_sql(session: &CatalogSession, sql: &str) -> Result<u64, String> {
    match session {
        CatalogSession::Scripted => Err(no_channel()),
        CatalogSession::Sqlite { pool, .. } => sqlx::query(audited(sql)).execute(pool).await.map(|done| done.rows_affected()).map_err(db_err),
        CatalogSession::Mysql(pool) => {
            let mut conn = pool.get_conn().await.map_err(db_err)?;
            let mut result = mysql_async::prelude::Queryable::query_iter(&mut conn, sql).await.map_err(db_err)?;
            let affected = result.affected_rows();
            while result.next().await.map_err(db_err)?.is_some() {}
            Ok(affected)
        }
        CatalogSession::Postgres(client) => client.execute(sql, &[]).await.map_err(db_err),
    }
}

fn sqlite_cell(row: &sqlx::sqlite::SqliteRow, index: usize) -> Result<Cell, String> {
    use sqlx::Row;
    if let Ok(value) = row.try_get::<Option<String>, _>(index) {
        return Ok(cell_text(value));
    }
    if let Ok(value) = row.try_get::<Option<i64>, _>(index) {
        return Ok(cell_text(value.map(|number| number.to_string())));
    }
    if let Ok(value) = row.try_get::<Option<f64>, _>(index) {
        return Ok(cell_text(value.map(|number| number.to_string())));
    }
    if let Ok(value) = row.try_get::<Option<Vec<u8>>, _>(index) {
        return Ok(cell_text(value.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())));
    }
    Err(format!("读不到第 {index} 列"))
}

fn cell_text(value: Option<String>) -> Cell {
    match value {
        Some(text) => Cell::Text(text),
        None => Cell::Null,
    }
}

fn pg_cell(row: &tokio_postgres::Row, index: usize) -> Result<Cell, String> {
    if let Ok(value) = row.try_get::<_, Option<String>>(index) {
        return Ok(cell_text(value));
    }
    if let Ok(value) = row.try_get::<_, Option<i64>>(index) {
        return Ok(cell_text(value.map(|number| number.to_string())));
    }
    if let Ok(value) = row.try_get::<_, Option<i32>>(index) {
        return Ok(cell_text(value.map(|number| number.to_string())));
    }
    if let Ok(value) = row.try_get::<_, Option<i16>>(index) {
        return Ok(cell_text(value.map(|number| number.to_string())));
    }
    if let Ok(value) = row.try_get::<_, Option<f64>>(index) {
        return Ok(cell_text(value.map(|number| number.to_string())));
    }
    if let Ok(value) = row.try_get::<_, Option<bool>>(index) {
        return Ok(cell_text(value.map(|number| number.to_string())));
    }
    if let Ok(value) = row.try_get::<_, Option<Vec<u8>>>(index) {
        return Ok(cell_text(value.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())));
    }
    Err(format!("读不到第 {index} 列"))
}

fn mysql_cell(value: &mysql_async::Value) -> Cell {
    match value {
        mysql_async::Value::NULL => Cell::Null,
        mysql_async::Value::Bytes(bytes) => Cell::Text(String::from_utf8_lossy(bytes).into_owned()),
        mysql_async::Value::Int(number) => Cell::Text(number.to_string()),
        mysql_async::Value::UInt(number) => Cell::Text(number.to_string()),
        mysql_async::Value::Float(number) => Cell::Text(number.to_string()),
        mysql_async::Value::Double(number) => Cell::Text(number.to_string()),
        mysql_async::Value::Date(year, month, day, hour, minute, second, micros) => {
            Cell::Text(format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{micros:06}"))
        }
        mysql_async::Value::Time(negative, days, hour, minute, second, micros) => {
            Cell::Text(format!("{}{days} {hour:02}:{minute:02}:{second:02}.{micros:06}", if *negative { "-" } else { "" }))
        }
    }
}

async fn mysql_rows(pool: &mysql_async::Pool, sql: &str) -> Result<(Vec<String>, Vec<Vec<Cell>>), String> {
    let mut conn = pool.get_conn().await.map_err(db_err)?;
    let mut result = mysql_async::prelude::Queryable::query_iter(&mut conn, sql).await.map_err(db_err)?;
    let names = result.columns_ref().iter().map(|column| column.name_str().into_owned()).collect::<Vec<_>>();
    let mut rows = Vec::new();
    while let Some(row) = result.next().await.map_err(db_err)? {
        let cells = (0..names.len()).map(|index| mysql_cell(row.as_ref(index).unwrap_or(&mysql_async::Value::NULL))).collect();
        rows.push(cells);
    }
    Ok((names, rows))
}

async fn describe_sqlite(pool: &sqlx::SqlitePool, table: &str) -> Result<TableSchema, String> {
    let kind: Option<String> = sqlx::query_scalar("SELECT type FROM sqlite_master WHERE name = ?1 AND type IN ('table', 'view')")
        .bind(table)
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;
    let kind = kind.ok_or_else(|| format!("找不到表或视图 {table}"))?;
    let kind = if kind.eq_ignore_ascii_case("view") { "view" } else { "table" };
    let quoted = sql::quote_ident(Engine::Sqlite, table);
    let info = sqlx::query(audited(&format!("PRAGMA table_info({quoted})"))).fetch_all(pool).await.map_err(db_err)?;
    if info.is_empty() {
        return Err(format!("找不到表或视图 {table}"));
    }
    let mut pk_order: Vec<(i64, String)> = Vec::new();
    let mut columns = Vec::new();
    for row in &info {
        let name = match sqlite_cell(row, 1)? {
            Cell::Text(text) => text,
            Cell::Null => return Err("列名是空的".to_owned()),
        };
        let type_name = match sqlite_cell(row, 2)? {
            Cell::Text(text) => text,
            Cell::Null => String::new(),
        };
        let not_null = sqlite_number(row, 3)? != 0;
        let pk = sqlite_number(row, 5)?;
        if pk > 0 {
            pk_order.push((pk, name.clone()));
        }
        columns.push(Column {
            name,
            type_name,
            nullable: !not_null,
            primary: pk > 0,
            unique: false,
            auto: false,
        });
    }
    pk_order.sort_by_key(|(order, _)| *order);
    let mut key: Vec<String> = pk_order.into_iter().map(|(_, name)| name).collect();
    let indexes = sqlx::query(audited(&format!("PRAGMA index_list({quoted})"))).fetch_all(pool).await.map_err(db_err)?;
    let mut unique_columns: Vec<Vec<String>> = Vec::new();
    for index in &indexes {
        let unique = sqlite_number(index, 2)? != 0;
        let partial = sqlite_number(index, 4).unwrap_or(0) != 0;
        if !unique || partial {
            continue;
        }
        let index_name = match sqlite_cell(index, 1)? {
            Cell::Text(text) => text,
            Cell::Null => continue,
        };
        let origin = match sqlite_cell(index, 3) {
            Ok(Cell::Text(text)) => text,
            _ => String::new(),
        };
        let index_rows = sqlx::query(audited(&format!("PRAGMA index_info({})", sql::quote_ident(Engine::Sqlite, &index_name))))
            .fetch_all(pool)
            .await
            .map_err(db_err)?;
        let mut pairs = Vec::new();
        for index_row in &index_rows {
            let seq = sqlite_number(index_row, 0)?;
            let name = match sqlite_cell(index_row, 2)? {
                Cell::Text(text) => text,
                Cell::Null => continue,
            };
            pairs.push((seq, name));
        }
        pairs.sort_by_key(|(seq, _)| *seq);
        let names = pairs.into_iter().map(|(_, name)| name).collect::<Vec<_>>();
        if origin == "pk" && key.is_empty() {
            key = names.clone();
        }
        unique_columns.push(names);
    }
    if key.is_empty() {
        if let Some(first) = unique_columns.first() {
            key = first.clone();
        }
    }
    let single_unique = unique_columns.iter().filter(|group| group.len() == 1).map(|group| group[0].clone()).collect::<Vec<_>>();
    let key_for_flags = key.clone();
    for column in &mut columns {
        column.unique = column.primary || single_unique.iter().any(|name| name == &column.name);
        column.auto = column.primary && key_for_flags.len() == 1 && column.type_name.to_ascii_lowercase().contains("int");
    }
    Ok(TableSchema { engine: Engine::Sqlite, schema: String::new(), name: table.to_owned(), kind, columns, key })
}

fn sqlite_number(row: &sqlx::sqlite::SqliteRow, index: usize) -> Result<i64, String> {
    match sqlite_cell(row, index)? {
        Cell::Text(text) => text.parse::<i64>().map_err(|_| format!("不是整数 {text}")),
        Cell::Null => Ok(0),
    }
}

async fn ensure_postgres_database(client: &tokio_postgres::Client, database: &str) -> Result<(), String> {
    if database.is_empty() {
        return Ok(());
    }
    let row = client.query_one("SELECT current_database()", &[]).await.map_err(db_err)?;
    let current: Option<String> = row.try_get(0).map_err(db_err)?;
    let current = current.unwrap_or_default();
    if current != database {
        return Err(other_database(&current, database));
    }
    Ok(())
}

async fn describe_postgres(client: &tokio_postgres::Client, schema: &str, table: &str) -> Result<TableSchema, String> {
    let kind_rows = client
        .query(
            "SELECT c.relkind::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r','p','v','m')",
            &[&schema, &table],
        )
        .await
        .map_err(db_err)?;
    let kind_text: String = kind_rows.first().ok_or_else(|| format!("找不到表或视图 {schema}.{table}"))?.get(0);
    let kind = if kind_text == "r" || kind_text == "p" { "table" } else { "view" };
    let columns_rows = client
        .query(
            "SELECT a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod), a.attnotnull, a.attidentity::text, pg_get_expr(ad.adbin, ad.adrelid) FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace LEFT JOIN pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum WHERE n.nspname = $1 AND c.relname = $2 AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
            &[&schema, &table],
        )
        .await
        .map_err(db_err)?;
    let pk_rows = client
        .query(
            "SELECT a.attname FROM pg_index i JOIN pg_class c ON c.oid = i.indrelid JOIN pg_namespace n ON n.oid = c.relnamespace JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = ANY (i.indkey) WHERE n.nspname = $1 AND c.relname = $2 AND i.indisprimary ORDER BY array_position(i.indkey, a.attnum)",
            &[&schema, &table],
        )
        .await
        .map_err(db_err)?;
    let pk = pk_rows.iter().map(|row| row.get::<_, String>(0)).collect::<Vec<_>>();
    let unique_rows = client
        .query(
            "SELECT i.indexrelid::text, a.attname FROM pg_index i JOIN pg_class c ON c.oid = i.indrelid JOIN pg_namespace n ON n.oid = c.relnamespace JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = ANY (i.indkey) WHERE n.nspname = $1 AND c.relname = $2 AND i.indisunique AND NOT i.indisprimary AND i.indpred IS NULL AND i.indexprs IS NULL AND NOT (0 = ANY (i.indkey)) ORDER BY i.indexrelid, array_position(i.indkey, a.attnum)",
            &[&schema, &table],
        )
        .await
        .map_err(db_err)?;
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for row in &unique_rows {
        let id: String = row.get(0);
        let name: String = row.get(1);
        if let Some(group) = groups.iter_mut().find(|group| group.0 == id) {
            group.1.push(name);
        } else {
            groups.push((id, vec![name]));
        }
    }
    let key = if !pk.is_empty() { pk.clone() } else { groups.first().map(|group| group.1.clone()).unwrap_or_default() };
    let mut columns = Vec::new();
    for row in &columns_rows {
        let name: String = row.get(0);
        let type_name: String = row.get(1);
        let not_null: bool = row.get(2);
        let identity: String = row.get(3);
        let default: Option<String> = row.get(4);
        let auto = identity == "a" || identity == "d" || default.as_deref().unwrap_or("").starts_with("nextval(");
        let primary = pk.iter().any(|item| item == &name);
        let unique = primary || groups.iter().any(|group| group.1.len() == 1 && group.1[0] == name);
        columns.push(Column { name, type_name, nullable: !not_null, primary, unique, auto });
    }
    Ok(TableSchema { engine: Engine::Postgres, schema: schema.to_owned(), name: table.to_owned(), kind, columns, key })
}

async fn describe_mysql(pool: &mysql_async::Pool, engine: Engine, database: &str, table: &str) -> Result<TableSchema, String> {
    let mut conn = pool.get_conn().await.map_err(db_err)?;
    let kind_row: Option<mysql_async::Row> = mysql_async::prelude::Queryable::exec_first(
        &mut conn,
        "SELECT TABLE_TYPE FROM information_schema.tables WHERE table_schema = ? AND table_name = ?",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(db_err)?;
    let kind_text = mysql_text(kind_row.as_ref().ok_or_else(|| format!("找不到表或视图 {database}.{table}"))?, 0)?;
    let kind = if kind_text.eq_ignore_ascii_case("VIEW") { "view" } else { "table" };
    let column_rows: Vec<mysql_async::Row> = mysql_async::prelude::Queryable::exec(
        &mut conn,
        "SELECT COLUMN_NAME, DATA_TYPE, IS_NULLABLE, COLUMN_KEY, EXTRA FROM information_schema.columns WHERE table_schema = ? AND table_name = ? ORDER BY ORDINAL_POSITION",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(db_err)?;
    let index_rows: Vec<mysql_async::Row> = mysql_async::prelude::Queryable::exec(
        &mut conn,
        "SELECT INDEX_NAME, NON_UNIQUE, SEQ_IN_INDEX, COLUMN_NAME FROM information_schema.statistics WHERE table_schema = ? AND table_name = ? ORDER BY INDEX_NAME, SEQ_IN_INDEX",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(db_err)?;
    let mut groups: Vec<(String, bool, Vec<String>)> = Vec::new();
    for row in &index_rows {
        let name = mysql_text(row, 0)?;
        let non_unique = mysql_text(row, 1)?;
        let column = mysql_text(row, 3)?;
        let unique = non_unique == "0";
        if let Some(group) = groups.iter_mut().find(|group| group.0 == name) {
            group.2.push(column);
        } else {
            groups.push((name, unique, vec![column]));
        }
    }
    let pk = groups.iter().find(|group| group.0.eq_ignore_ascii_case("PRIMARY")).map(|group| group.2.clone()).unwrap_or_default();
    let key = if !pk.is_empty() {
        pk.clone()
    } else {
        groups.iter().find(|group| group.1).map(|group| group.2.clone()).unwrap_or_default()
    };
    let mut columns = Vec::new();
    for row in &column_rows {
        let name = mysql_text(row, 0)?;
        let type_name = mysql_text(row, 1)?;
        let nullable = mysql_text(row, 2)?.eq_ignore_ascii_case("YES");
        let extra = mysql_text(row, 4)?;
        let primary = pk.iter().any(|item| item == &name);
        let unique = primary || groups.iter().any(|group| group.1 && group.2.len() == 1 && group.2[0] == name);
        let auto = extra.to_ascii_lowercase().contains("auto_increment");
        columns.push(Column { name, type_name, nullable, primary, unique, auto });
    }
    Ok(TableSchema { engine, schema: database.to_owned(), name: table.to_owned(), kind, columns, key })
}

fn mysql_text(row: &mysql_async::Row, index: usize) -> Result<String, String> {
    match mysql_cell(row.as_ref(index).unwrap_or(&mysql_async::Value::NULL)) {
        Cell::Text(text) => Ok(text),
        Cell::Null => Err("元数据列是空值".to_owned()),
    }
}
