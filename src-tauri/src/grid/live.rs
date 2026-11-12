//! 在已经打开的会话上查询、逐条提交和流式导出。不新开数据库连接。

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use futures::TryStreamExt;
use sqlx::Column as _;
use sqlx::Row as _;
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
    /// 整表关键字（R3）。table 模式的全量导出与网格读到同一组行，口径一致。
    pub search: Option<String>,
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
        // Redis 是键值专项引擎（R11）：不经 TableTarget→grid_describe 的 SQL 通路。
        CatalogSession::Redis(_) => Err("Redis 没有表数据网格；请使用键浏览器。".to_owned()),
        CatalogSession::Mongo(_) => Err("MongoDB 没有表数据网格；请使用集合文档浏览器。".to_owned()),
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
        CatalogSession::Sqlserver(client) => {
            if database.is_empty() {
                return Err("需要数据库名，没有另开连接。".to_owned());
            }
            ensure_sqlserver_database(client, database).await?;
            if schema.is_empty() {
                return Err("SQL Server 需要指定 schema（表所在的分组名），没有另开连接。".to_owned());
            }
            describe_sqlserver(client, schema, table).await
        }
    }
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
    query_page_search_preview(session, database, schema_name, table, engine_hint, filters, sort, search, offset, limit, 1024).await
}

/// 与 query_page_search 相同，但大字段预览的上限来自设置 preview_kb。
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
    offset: u64,
    limit: u64,
    preview_bytes: usize,
) -> Result<LivePage, String> {
    let schema = describe(session, database, schema_name, table, engine_hint).await?;
    let limit = limit.min(MAX_PAGE);
    if let Some(sort) = sort {
        if !schema.columns.iter().any(|column| column.name == sort.column) {
            return Err(format!("未知排序列 {}", sort.column));
        }
    }
    let display = page_sql(&schema, filters, search, sort, limit, offset, preview_bytes)?;
    if limit == 0 {
        return Ok(LivePage { rows: Vec::new(), truncated: false, sql: display });
    }
    let executed = page_sql(&schema, filters, search, sort, limit.saturating_add(1), offset, preview_bytes)?;
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

#[allow(dead_code)]
pub async fn count_rows(
    session: &CatalogSession,
    database: &str,
    schema_name: &str,
    table: &str,
    engine_hint: &str,
    filters: &[Filter],
) -> Result<i64, String> {
    count_rows_search(session, database, schema_name, table, engine_hint, filters, None).await
}

/// count_rows 的整表关键字形态（R3）：统计行数与分页读到同一组行。
pub async fn count_rows_search(
    session: &CatalogSession,
    database: &str,
    schema_name: &str,
    table: &str,
    engine_hint: &str,
    filters: &[Filter],
    search: Option<&str>,
) -> Result<i64, String> {
    let schema = describe(session, database, schema_name, table, engine_hint).await?;
    let sql = count_sql(&schema, filters, search)?;
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

fn engine_from(hint: &str) -> Engine {
    match hint {
        "mysql" => Engine::Mysql,
        "mariadb" => Engine::Mariadb,
        "sqlite" => Engine::Sqlite,
        "sqlserver" => Engine::Sqlserver,
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
                page_sql_unlimited(&schema, &spec.filters, spec.search.as_deref(), spec.sort.as_ref())
            } else {
                page_sql_full(&schema, &spec.filters, spec.search.as_deref(), spec.sort.as_ref(), spec.limit)
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
    match session {
        CatalogSession::Scripted => failed_message(no_channel()),
        // Redis 不参与五格式导出（capability 门控）；误入时明确拒绝。
        CatalogSession::Redis(_) => failed_message("Redis 没有表数据导出；请使用键浏览器。".to_owned()),
        CatalogSession::Mongo(_) => failed_message("MongoDB 没有表数据导出；请使用集合文档浏览器。".to_owned()),
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
        CatalogSession::Sqlserver(client) => {
            let mut locked = client.lock().await;
            let mut stream = match locked.simple_query(sql).await {
                Ok(stream) => stream,
                Err(err) => return failed_message(err.to_string()),
            };
            let mut names = known_names;
            let mut rows: Vec<tiberius::Row> = Vec::new();
            {
                use futures::TryStreamExt;
                // QueryItem 不公开导出，经 as_metadata/into_row 取内容。
                while let Ok(Some(item)) = stream.try_next().await {
                    if let Some(meta) = item.as_metadata() {
                        if names.is_none() {
                            names = Some(meta.columns().iter().map(|column| column.name().to_owned()).collect());
                        }
                        continue;
                    }
                    if let Some(row) = item.into_row() {
                        rows.push(row);
                    }
                }
            }
            drop(stream);
            drop(locked);
            let names = names.unwrap_or_default();
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
            for row in rows {
                if let Some(outcome) = csv.control(flag, spec) {
                    return outcome;
                }
                let cells = (0..names.len()).map(|index| tds_cell(&row, index)).collect::<Vec<_>>();
                if let Some(outcome) = csv.write_cells(&cells) {
                    return outcome;
                }
            }
            csv.finish()
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

fn page_sql(schema: &TableSchema, filters: &[Filter], search: Option<&str>, sort: Option<&Sort>, limit: u64, offset: u64, preview_bytes: usize) -> Result<String, String> {
    let (body, ordered) = page_body(schema, filters, search, sort, false, preview_bytes)?;
    let tail = sql::page_tail(schema.engine, limit, offset, ordered);
    Ok(format!("{body}{tail}"))
}

fn page_sql_full(schema: &TableSchema, filters: &[Filter], search: Option<&str>, sort: Option<&Sort>, limit: u64) -> Result<String, String> {
    let (body, ordered) = page_body(schema, filters, search, sort, true, 1024)?;
    let tail = if schema.engine == Engine::Sqlserver {
        sql::page_tail(schema.engine, limit, 0, ordered)
    } else {
        format!(" LIMIT {limit}")
    };
    Ok(format!("{body}{tail}"))
}

fn page_sql_unlimited(schema: &TableSchema, filters: &[Filter], search: Option<&str>, sort: Option<&Sort>) -> Result<String, String> {
    page_body(schema, filters, search, sort, true, 1024).map(|(sql, _)| sql)
}

fn page_body(schema: &TableSchema, filters: &[Filter], search: Option<&str>, sort: Option<&Sort>, full: bool, preview_bytes: usize) -> Result<(String, bool), String> {
    let list = sql::select_list_preview(schema, full || sql::reads_full_values(schema, filters), preview_bytes);
    let where_sql = sql::compile_where_with_search(schema, filters, search)?;
    let clause = if where_sql.is_empty() { String::new() } else { format!(" WHERE {where_sql}") };
    let order = order_clause(schema, sort);
    let ordered = order.starts_with(" ORDER BY");
    Ok((format!("SELECT {list} FROM {}{clause}{}", sql::qualified_name(schema), order), ordered))
}

fn count_sql(schema: &TableSchema, filters: &[Filter], search: Option<&str>) -> Result<String, String> {
    let where_sql = sql::compile_where_with_search(schema, filters, search)?;
    let clause = if where_sql.is_empty() { String::new() } else { format!(" WHERE {where_sql}") };
    let count = match schema.engine {
        Engine::Postgres => "COUNT(*)::bigint",
        Engine::Sqlite | Engine::Mysql | Engine::Mariadb => "COUNT(*)",
        Engine::Sqlserver => "CAST(COUNT_BIG(*) AS BIGINT)",
        // Redis 没有表网格；TableSchema 不会带这个引擎（describe 已拒绝）。
        Engine::Redis | Engine::Mongodb => "COUNT(*)",
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

/// 按列数把一页行读成 Cell 矩阵。全库导出（R7）的流式分页读走这里，与网格同一套单元转换。
pub async fn fetch_cells(session: &CatalogSession, sql: &str, width: usize) -> Result<Vec<Vec<Cell>>, String> {
    match session {
        CatalogSession::Scripted => Err(no_channel()),
        CatalogSession::Redis(_) => Err("Redis 没有 SQL 行读取；数据经键浏览专项通路。".to_owned()),
        CatalogSession::Mongo(_) => Err("MongoDB 没有 SQL 行读取；文档经集合浏览器分页。".to_owned()),
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
        CatalogSession::Sqlserver(client) => {
            let (_names, rows) = sqlserver_rows(client, sql).await?;
            let _ = width;
            Ok(rows)
        }
    }
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
    match session {
        CatalogSession::Scripted => Err(no_channel()),
        CatalogSession::Redis(_) => Err("Redis 没有 SQL 计数；键空间计数走专项通路。".to_owned()),
        CatalogSession::Mongo(_) => Err("MongoDB 没有 SQL 计数；文档计数走 estimatedDocumentCount。".to_owned()),
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
        CatalogSession::Sqlserver(client) => {
            let (_names, rows) = sqlserver_rows(client, sql).await?;
            match rows.first().and_then(|row| row.first()) {
                Some(Cell::Text(text)) => text.parse::<i64>().map_err(|_| format!("计数无效 {text}")),
                _ => Err("没有计数".to_owned()),
            }
        }
    }
}

async fn execute_sql(session: &CatalogSession, sql: &str) -> Result<u64, String> {
    match session {
        CatalogSession::Scripted => Err(no_channel()),
        CatalogSession::Redis(_) => Err("Redis 不执行 SQL；命令经 redis_exec 专项通道。".to_owned()),
        CatalogSession::Mongo(_) => Err("MongoDB 不执行 SQL；文档操作经 mongo_* 专项通道。".to_owned()),
        CatalogSession::Sqlite { pool, .. } => sqlx::query(audited(sql)).execute(pool).await.map(|done| done.rows_affected()).map_err(db_err),
        CatalogSession::Mysql(pool) => {
            let mut conn = pool.get_conn().await.map_err(db_err)?;
            let mut result = mysql_async::prelude::Queryable::query_iter(&mut conn, sql).await.map_err(db_err)?;
            let affected = result.affected_rows();
            while result.next().await.map_err(db_err)?.is_some() {}
            Ok(affected)
        }
        CatalogSession::Postgres(client) => client.execute(sql, &[]).await.map_err(db_err),
        // execute 走 sp_executesql，返回受影响行数；结果流在下一条语句前会被 tiberius 排空。
        CatalogSession::Sqlserver(client) => {
            let mut client = client.lock().await;
            client.execute(sql, &[]).await.map(|done| done.total()).map_err(db_err)
        }
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

fn tds_text_cell(row: &tiberius::Row, index: usize) -> Option<String> {
    row.try_get::<&str, _>(index).ok().flatten().map(str::to_owned)
}

/// TDS 行到 Cell：文本优先，数字/浮点/布尔逐类兜底，二进制转十六进制（与前端投影协议一致）。
fn tds_cell(row: &tiberius::Row, index: usize) -> Cell {
    if let Some(text) = tds_text_cell(row, index) {
        return Cell::Text(text);
    }
    if let Ok(Some(value)) = row.try_get::<i64, _>(index) {
        return Cell::Text(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<i32, _>(index) {
        return Cell::Text(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<i16, _>(index) {
        return Cell::Text(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<u8, _>(index) {
        return Cell::Text(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<f64, _>(index) {
        return Cell::Text(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<f32, _>(index) {
        return Cell::Text(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<bool, _>(index) {
        return Cell::Text(if value { "1" } else { "0" }.to_owned());
    }
    if let Ok(Some(value)) = row.try_get::<&[u8], _>(index) {
        return Cell::Text(to_hex_text(value));
    }
    Cell::Null
}

fn to_hex_text(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

type TdsHandle = std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>;

pub(crate) async fn sqlserver_rows(client: &TdsHandle, sql: &str) -> Result<(Vec<String>, Vec<Vec<Cell>>), String> {
    let mut locked = client.lock().await;
    let mut stream = locked.simple_query(sql).await.map_err(db_err)?;
    let mut names: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    {
        use futures::TryStreamExt;
        // QueryItem 不公开导出，经 as_metadata/into_row 取内容。
        while let Some(item) = stream.try_next().await.map_err(|err| db_err(err.to_string()))? {
            if let Some(meta) = item.as_metadata() {
                if names.is_empty() {
                    names = meta.columns().iter().map(|column| column.name().to_owned()).collect();
                }
                continue;
            }
            if let Some(row) = item.into_row() {
                if names.is_empty() {
                    names = (0..row.len()).map(|index| format!("col{}", index + 1)).collect();
                }
                let cells = (0..names.len()).map(|index| tds_cell(&row, index)).collect();
                rows.push(cells);
            }
        }
    }
    Ok((names, rows))
}

async fn ensure_sqlserver_database(client: &TdsHandle, database: &str) -> Result<(), String> {
    let (_names, rows) = sqlserver_rows(client, "SELECT DB_NAME()").await?;
    let current = match rows.first().and_then(|row| row.first()) {
        Some(Cell::Text(text)) => text.clone(),
        _ => String::new(),
    };
    if current != database {
        return Err(other_database(&current, database));
    }
    Ok(())
}

async fn describe_sqlserver(client: &TdsHandle, schema: &str, table: &str) -> Result<TableSchema, String> {
    let kind_rows = sqlserver_rows(
        client,
        &format!(
            "SELECT CASE WHEN o.type = 'V' THEN 'VIEW' ELSE 'BASE TABLE' END FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = N'{}' AND o.name = N'{}' AND o.type IN ('U', 'V')",
            escape_tds_text(schema),
            escape_tds_text(table)
        ),
    )
    .await?;
    let kind_text = match kind_rows.1.first().and_then(|row| row.first()) {
        Some(Cell::Text(text)) => text.clone(),
        _ => return Err(format!("找不到表或视图 {schema}.{table}")),
    };
    let kind = if kind_text.eq_ignore_ascii_case("VIEW") { "view" } else { "table" };
    let column_rows = sqlserver_rows(
        client,
        &format!(
            "SELECT c.name, TYPE_NAME(c.user_type_id), c.is_nullable, c.is_identity, dc.definition FROM sys.columns c JOIN sys.objects o ON o.object_id = c.object_id JOIN sys.schemas s ON s.schema_id = o.schema_id LEFT JOIN sys.default_constraints dc ON dc.parent_object_id = c.object_id AND dc.parent_column_id = c.column_id WHERE s.name = N'{}' AND o.name = N'{}' ORDER BY c.column_id",
            escape_tds_text(schema),
            escape_tds_text(table)
        ),
    )
    .await?;
    let pk_rows = sqlserver_rows(
        client,
        &format!(
            "SELECT c.name FROM sys.index_columns ic JOIN sys.indexes i ON i.object_id = ic.object_id AND i.index_id = ic.index_id JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id JOIN sys.objects o ON o.object_id = i.object_id JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE i.is_primary_key = 1 AND s.name = N'{}' AND o.name = N'{}' ORDER BY ic.key_ordinal",
            escape_tds_text(schema),
            escape_tds_text(table)
        ),
    )
    .await?;
    let unique_rows = sqlserver_rows(
        client,
        &format!(
            "SELECT i.name, i.is_unique, ic.key_ordinal, c.name FROM sys.indexes i JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id JOIN sys.objects o ON o.object_id = i.object_id JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE i.is_unique = 1 AND i.is_primary_key = 0 AND s.name = N'{}' AND o.name = N'{}' ORDER BY i.name, ic.key_ordinal",
            escape_tds_text(schema),
            escape_tds_text(table)
        ),
    )
    .await?;
    let pk: Vec<String> = pk_rows
        .1
        .iter()
        .filter_map(|row| match row.first() {
            Some(Cell::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .collect();
    let mut groups: Vec<(String, bool, Vec<String>)> = Vec::new();
    for row in &unique_rows.1 {
        let cells: Vec<&Cell> = row.iter().collect();
        let name = match cells.first().and_then(|cell| match cell {
            Cell::Text(text) => Some(text.as_str()),
            _ => None,
        }) {
            Some(name) => name.to_owned(),
            None => continue,
        };
        let unique = cells.get(1).map(|cell| matches!(cell, Cell::Text(text) if text == "1")).unwrap_or(false);
        let column = cells
            .get(3)
            .and_then(|cell| match cell {
                Cell::Text(text) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        if let Some(group) = groups.iter_mut().find(|group| group.0 == name) {
            group.2.push(column);
        } else {
            groups.push((name, unique, vec![column]));
        }
    }
    let key = if !pk.is_empty() {
        pk.clone()
    } else {
        groups.iter().find(|group| group.1).map(|group| group.2.clone()).unwrap_or_default()
    };
    let mut columns = Vec::new();
    for row in &column_rows.1 {
        let cells: Vec<&Cell> = row.iter().collect();
        let name = match cells.first().and_then(|cell| match cell {
            Cell::Text(text) => Some(text.clone()),
            _ => None,
        }) {
            Some(name) => name,
            None => return Err("列名是空的".to_owned()),
        };
        let type_name = cells.get(1).and_then(|cell| match cell {
            Cell::Text(text) => Some(text.clone()),
            _ => None,
        }).unwrap_or_default();
        let nullable = cells.get(2).map(|cell| matches!(cell, Cell::Text(text) if text == "1")).unwrap_or(true);
        let identity = cells.get(3).map(|cell| matches!(cell, Cell::Text(text) if text == "1")).unwrap_or(false);
        let primary = pk.iter().any(|item| item == &name);
        let unique = primary || groups.iter().any(|group| group.1 && group.2.len() == 1 && group.2[0] == name);
        // IDENTITY 列即网格的自增列：插入时省略、UPDATE 不进 SET。
        columns.push(Column { name, type_name, nullable, primary, unique, auto: identity });
    }
    Ok(TableSchema { engine: Engine::Sqlserver, schema: schema.to_owned(), name: table.to_owned(), kind, columns, key })
}

/// 元数据查询里内联的 N'...' 文本转义。标识符与表名来自用户输入。
fn escape_tds_text(value: &str) -> String {
    value.replace('\'', "''")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

/// SQL Server 出向外键查询。只读 sys 目录，标识符按 N'...' 转义。
pub(crate) fn sqlserver_foreign_key_sql(schema: &str, table: &str) -> String {
    let mut sql = String::from(
        "SELECT fk.name, pc.name, rs.name, rt.name, rc.name \
         FROM sys.foreign_key_columns fkc \
         JOIN sys.foreign_keys fk ON fk.object_id = fkc.constraint_object_id \
         JOIN sys.tables pt ON pt.object_id = fkc.parent_object_id \
         JOIN sys.schemas ps ON ps.schema_id = pt.schema_id \
         JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id \
         JOIN sys.tables rt ON rt.object_id = fkc.referenced_object_id \
         JOIN sys.schemas rs ON rs.schema_id = rt.schema_id \
         JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
         WHERE pt.name = ",
    );
    sql.push_str(&format!("N'{}'", escape_tds_text(table)));
    if !schema.is_empty() {
        sql.push_str(" AND ps.name = ");
        sql.push_str(&format!("N'{}'", escape_tds_text(schema)));
    }
    sql.push_str(" ORDER BY fk.name, fkc.constraint_column_id");
    sql
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
        push_key(&mut list, &name, &column, &ref_schema, &ref_table, &ref_column);
    }
    list
}

fn push_key(list: &mut Vec<ForeignKey>, name: &str, column: &str, ref_schema: &str, ref_table: &str, ref_column: &str) {
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
pub async fn foreign_keys(session: &CatalogSession, database: &str, schema_name: &str, table: &str, engine_hint: &str) -> Result<Vec<ForeignKey>, String> {
    if table.is_empty() {
        return Ok(Vec::new());
    }
    match session {
        CatalogSession::Scripted => Ok(Vec::new()),
        CatalogSession::Redis(_) | CatalogSession::Mongo(_) => Ok(Vec::new()),
        CatalogSession::Sqlite { pool, .. } => {
            if !database.is_empty() && database != "main" {
                return Err(other_database("main", database));
            }
            let quoted = sql::quote_ident(Engine::Sqlite, table);
            let rows = sqlx::query(audited(&format!("PRAGMA foreign_key_list({quoted})")))
                .fetch_all(pool)
                .await
                .map_err(db_err)?;
            let mut list: Vec<ForeignKey> = Vec::new();
            let mut pk_cache: HashMap<String, Vec<String>> = HashMap::new();
            for row in rows {
                let id: i64 = row.try_get("id").map_err(db_err)?;
                let seq: i64 = row.try_get("seq").unwrap_or(0);
                let column: String = row.try_get("from").map_err(db_err)?;
                let ref_table: String = row.try_get("table").map_err(db_err)?;
                let explicit: Option<String> = row.try_get("to").ok().flatten();
                let ref_column = match explicit {
                    Some(name) if !name.is_empty() => name,
                    // 引用父表主键时 PRAGMA 的 to 为 NULL：按 seq 取父表主键序号，找不到就跳过。
                    _ => {
                        if !pk_cache.contains_key(&ref_table) {
                            let parent = sql::quote_ident(Engine::Sqlite, &ref_table);
                            let info = sqlx::query(audited(&format!("PRAGMA table_info({parent})")))
                                .fetch_all(pool)
                                .await
                                .map_err(db_err)?;
                            let mut keys: Vec<(i64, String)> = info
                                .iter()
                                .filter_map(|item| {
                                    let pk = item.try_get::<i64, _>("pk").unwrap_or(0);
                                    if pk > 0 { item.try_get::<String, _>("name").ok().map(|name| (pk, name)) } else { None }
                                })
                                .collect();
                            keys.sort_by_key(|(pk, _)| *pk);
                            pk_cache.insert(ref_table.clone(), keys.into_iter().map(|(_, name)| name).collect());
                        }
                        match pk_cache.get(&ref_table).and_then(|keys| keys.get(seq as usize)) {
                            Some(name) => name.clone(),
                            None => continue,
                        }
                    }
                };
                push_key(&mut list, &format!("fk_{id}"), &column, "main", &ref_table, &ref_column);
            }


            Ok(list)
        }
        CatalogSession::Mysql(pool) => {
            if database.is_empty() {
                return Ok(Vec::new());
            }
            let _ = engine_hint;
            let mut conn = pool.get_conn().await.map_err(db_err)?;
            let rows: Vec<mysql_async::Row> = mysql_async::prelude::Queryable::exec(
                &mut conn,
                "SELECT CONSTRAINT_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND REFERENCED_TABLE_NAME IS NOT NULL ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION",
                (database.to_owned(), table.to_owned()),
            )
            .await
            .map_err(db_err)?;
            let mut list: Vec<ForeignKey> = Vec::new();
            for row in &rows {
                let name = mysql_text(row, 0)?;
                let column = mysql_text(row, 1)?;
                let ref_schema = mysql_text(row, 2)?;
                let ref_table = mysql_text(row, 3)?;
                let ref_column = mysql_text(row, 4)?;
                let scope = if ref_schema.is_empty() { database.to_owned() } else { ref_schema };
                push_key(&mut list, &name, &column, &scope, &ref_table, &ref_column);
            }
            Ok(list)
        }

        CatalogSession::Postgres(client) => {
            ensure_postgres_database(client, database).await?;
            if schema_name.is_empty() {
                return Ok(Vec::new());
            }
            let rows = client
                .query(
                    "SELECT c.conname AS name, a.attname AS column_name, fs.nspname AS ref_schema, ft.relname AS ref_table, af.attname AS ref_column \
                     FROM pg_constraint c \
                     JOIN pg_class t ON t.oid = c.conrelid \
                     JOIN pg_namespace s ON s.oid = t.relnamespace \
                     JOIN pg_class ft ON ft.oid = c.confrelid \
                     JOIN pg_namespace fs ON fs.oid = ft.relnamespace \
                     JOIN LATERAL unnest(c.conkey, c.confkey) WITH ORDINALITY AS k(attnum, fattnum, ord) ON true \
                     JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
                     JOIN pg_attribute af ON af.attrelid = ft.oid AND af.attnum = k.fattnum \
                     WHERE s.nspname = $1 AND t.relname = $2 AND c.contype = 'f' \
                     ORDER BY c.conname, k.ord",
                    &[&schema_name, &table],
                )
                .await
                .map_err(db_err)?;
            let mut list: Vec<ForeignKey> = Vec::new();
            for row in &rows {
                let name: String = row.get("name");
                let column: String = row.get("column_name");
                let ref_schema: String = row.get("ref_schema");
                let ref_table: String = row.get("ref_table");
                let ref_column: String = row.get("ref_column");
                push_key(&mut list, &name, &column, &ref_schema, &ref_table, &ref_column);
            }
            Ok(list)
        }
        CatalogSession::Sqlserver(client) => {
            if database.is_empty() || table.is_empty() {
                return Ok(Vec::new());
            }
            ensure_sqlserver_database(client, database).await?;
            // 与查询补全同一条 sys 目录语句，避免网格和外键提示各写一套。
            let sql = sqlserver_foreign_key_sql(schema_name, table);
            let (_names, rows) = sqlserver_rows(client, &sql).await?;
            Ok(assemble_foreign_keys(&rows))
        }
    }
}
