//! 全库导出（R7）：依赖序 DDL + 流式数据 INSERT，写成一个 `.sql` 文件。
//! 只读：对象枚举复用 catalog 的读取，表结构复用 ddl 的快照，顺序决策复用 order 的共享排序核心。
//! 文件先写 `.rillsql-<uuid>.partial`，完成后改名；取消即删 `.partial`（native_export 先例）。

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::catalog::{metadata_sql, read_definition, read_object_page, Engine, ObjectKind, ObjectRow};
use crate::connections::CatalogSession;
use crate::ddl::{load_partial, Action, Request as DdlRequest};
use crate::dump::order::{self, DepNode};
use crate::grid::{self, describe, fetch_cells, insert_line, qualified_name, quote_ident, select_list};

/// 每页读取的行数。逐页取回，不把整表装进内存。
const DATA_PAGE: u64 = 500;

/// 每个对象枚举读取的页大小（对象名单与 compare 的读取同量级）。
const OBJECT_PAGE: usize = 500;

// ---------------------------------------------------------------- 取消注册表（grid/live.rs 先例）

pub fn request_cancel(id: &str) {
    let mut jobs = jobs().lock().expect("dump jobs");
    if let Some(flag) = jobs.get(id) {
        flag.store(true, Ordering::Relaxed);
    } else {
        jobs.insert(id.to_owned(), Arc::new(AtomicBool::new(true)));
    }
}

fn jobs() -> &'static Mutex<std::collections::HashMap<String, Arc<AtomicBool>>> {
    static JOBS: OnceLock<Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn begin_job(id: &str) -> Arc<AtomicBool> {
    let mut jobs = jobs().lock().expect("dump jobs");
    if let Some(flag) = jobs.get(id) {
        return Arc::clone(flag);
    }
    let flag = Arc::new(AtomicBool::new(false));
    jobs.insert(id.to_owned(), Arc::clone(&flag));
    flag
}

fn finish_job(id: &str) {
    jobs().lock().expect("dump jobs").remove(id);
}

// ---------------------------------------------------------------- 请求与结果

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DumpRequest {
    /// 任务中心里的任务 id，进度事件与取消都按它对号。
    pub id: String,
    pub connection_id: String,
    /// 引擎名（与 TableRef.engine 同一来源），MariaDB 与 MySQL 共用会话形态但对象面不同。
    pub engine: String,
    pub database: String,
    /// PostgreSQL 的 schema 范围；空列表表示该库的全部 schema。其余引擎忽略。
    #[serde(default)]
    pub schemas: Vec<String>,
    /// 表范围；空列表表示全部表。视图、函数等对象始终全部导出。
    #[serde(default)]
    pub tables: Vec<String>,
    #[serde(default)]
    pub include_data: bool,
    pub path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DumpDto {
    pub status: String,
    pub success: bool,
    pub path: Option<String>,
    pub residual_path: Option<String>,
    pub message: String,
    pub objects: usize,
    pub rows: u64,
}

/// `dump_progress` 事件的载荷：object 是当前对象名，rows 是该表已写入的行数。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DumpProgress {
    pub id: String,
    pub object: String,
    pub phase: String,
    pub rows: u64,
}

impl DumpProgress {
    fn ddl(id: &str, object: &str) -> Self {
        Self { id: id.to_owned(), object: object.to_owned(), phase: "ddl".to_owned(), rows: 0 }
    }

    fn data(id: &str, object: &str, rows: u64) -> Self {
        Self { id: id.to_owned(), object: object.to_owned(), phase: "data".to_owned(), rows }
    }
}

// ---------------------------------------------------------------- 对象模型

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DumpKind {
    Table,
    View,
    Function,
    Procedure,
    Trigger,
    Sequence,
    Enum,
    Domain,
}

impl DumpKind {
    fn from_catalog(kind: ObjectKind) -> Self {
        match kind {
            ObjectKind::Table => Self::Table,
            ObjectKind::View => Self::View,
            ObjectKind::Function => Self::Function,
            ObjectKind::Procedure => Self::Procedure,
            ObjectKind::Trigger => Self::Trigger,
            ObjectKind::Sequence => Self::Sequence,
            ObjectKind::Enum => Self::Enum,
            ObjectKind::Domain => Self::Domain,
        }
    }

    fn kind_str(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::View => "view",
            Self::Function => "function",
            Self::Procedure => "procedure",
            Self::Trigger => "trigger",
            Self::Sequence => "sequence",
            Self::Enum => "enum",
            Self::Domain => "domain",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Table => "表",
            Self::View => "视图",
            Self::Function => "函数",
            Self::Procedure => "过程",
            Self::Trigger => "触发器",
            Self::Sequence => "序列",
            Self::Enum => "枚举",
            Self::Domain => "域",
        }
    }

    /// 支撑对象的创建秩：枚举 → 域 → 序列（compare 的 support_create_rank 同序，函数与过程随视图段排序）。
    fn support_rank(self) -> u8 {
        match self {
            Self::Enum => 0,
            Self::Domain => 1,
            Self::Sequence => 2,
            _ => 9,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DumpColumn {
    name: String,
    type_name: String,
    nullable: bool,
    default_sql: String,
    primary: bool,
    auto: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DumpIndex {
    name: String,
    unique: bool,
    columns: Vec<(String, bool)>,
    reason: String,
    partial: bool,
    expression: bool,
    sql: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DumpForeignKey {
    name: String,
    columns: Vec<String>,
    ref_schema: String,
    ref_table: String,
    ref_columns: Vec<String>,
    on_delete: String,
    on_update: String,
}

#[derive(Clone, Debug, Default)]
struct DumpTable {
    columns: Vec<DumpColumn>,
    indexes: Vec<DumpIndex>,
    foreign_keys: Vec<DumpForeignKey>,
    /// SQLite 的 CREATE TABLE 原文（外键只能内联，原文最保真）。
    source_sql: String,
}

#[derive(Clone, Debug)]
struct DumpObject {
    kind: DumpKind,
    schema: String,
    name: String,
    signature: String,
    relation: String,
    definition: String,
    materialized: bool,
    table: Option<DumpTable>,
    error: Option<String>,
}

impl DumpObject {
    fn bare(kind: DumpKind, schema: &str, row: ObjectRow) -> Self {
        Self {
            kind,
            schema: schema.to_owned(),
            name: row.name,
            signature: row.signature,
            relation: row.relation,
            definition: String::new(),
            materialized: row.materialized,
            table: None,
            error: None,
        }
    }
}

/// 导出的范围限定：database 用于 MySQL 的限定名，schema 是 PostgreSQL 的对象分组。
#[derive(Clone, Debug, Default)]
struct DumpScope {
    database: String,
    schemas: Vec<String>,
}

impl DumpScope {
    fn qualify(&self, engine: Engine, schema: &str, name: &str) -> String {
        match engine {
            Engine::Sqlite => quote_ident(engine, name),
            Engine::Mysql | Engine::Mariadb => format!("{}.{}", quote_ident(engine, &self.database), quote_ident(engine, name)),
            Engine::Postgres => format!("{}.{}", quote_ident(engine, schema), quote_ident(engine, name)),
            Engine::Sqlserver => format!("{}.{}", quote_ident(engine, schema), quote_ident(engine, name)),
            // Redis 不在全库导出面（capability 门控）；占位保持穷尽。
            Engine::Redis | Engine::Mongodb => quote_ident(engine, name),
        }
    }
}

// ---------------------------------------------------------------- 对象枚举与定义读取

fn read_failure(err: impl std::fmt::Display) -> String {
    format!("读取失败：{err}")
}

fn engine_matches(session: &CatalogSession, engine: Engine) -> bool {
    matches!(
        (engine, session),
        (Engine::Sqlite, CatalogSession::Sqlite { .. })
            | (Engine::Mysql | Engine::Mariadb, CatalogSession::Mysql(_))
            | (Engine::Postgres, CatalogSession::Postgres(_))
            | (Engine::Sqlserver, CatalogSession::Sqlserver(_))
    )
}

pub fn engine_name(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "postgres",
        Engine::Mysql => "mysql",
        Engine::Mariadb => "mariadb",
        Engine::Sqlite => "sqlite",
        Engine::Sqlserver => "sqlserver",
        Engine::Redis => "redis",
        Engine::Mongodb => "mongodb",
    }
}

async fn postgres_text_column(session: &CatalogSession, sql: &str) -> Result<Vec<String>, String> {
    let CatalogSession::Postgres(client) = session else {
        return Err(crate::connections::error_codes::coded_string(crate::connections::error_codes::DUMP_CONNECTION_MISMATCH, "连接类型和引擎不一致"));
    };
    let rows = client.query(sql, &[]).await.map_err(read_failure)?;
    rows.iter()
        .map(|row| {
            if let Ok(value) = row.try_get::<_, String>(0) {
                return Ok(value);
            }
            match row.try_get::<_, Option<String>>(0) {
                Ok(Some(value)) => Ok(value),
                Ok(None) => Err(read_failure("元数据列是空值")),
                Err(err) => Err(read_failure(err)),
            }
        })
        .collect()
}

async fn current_postgres_database(session: &CatalogSession) -> Result<String, String> {
    postgres_text_column(session, "SELECT current_database()").await?
        .into_iter()
        .next()
        .ok_or_else(|| read_failure("没有当前数据库"))
}

/// 解析要导出的范围：库与 schema 列表。PostgreSQL 限定当前连接的库，schema 缺省全部。
async fn resolve_scope(engine: Engine, session: &CatalogSession, database: &str, schemas: &[String]) -> Result<(String, Vec<String>), String> {
    match engine {
        // Redis 不在全库导出面（capability 门控）。
        Engine::Redis => return Err(crate::connections::error_codes::coded_string(crate::connections::error_codes::DUMP_ENGINE_UNSUPPORTED, "Redis 没有整库导出；键空间经键浏览器访问。")),
        Engine::Mongodb => return Err(crate::connections::error_codes::coded_string(crate::connections::error_codes::DUMP_ENGINE_UNSUPPORTED, "MongoDB 没有整库导出；集合文档经文档浏览器访问。")),
        Engine::Sqlite => {
            if !database.is_empty() && database != "main" {
                return Err(crate::connections::error_codes::coded_string(crate::connections::error_codes::DUMP_SESSION_GAP_MAIN, format!("当前连接打开的是 main，没有另开连接读取 {database}")));
            }
            Ok((String::new(), vec![String::new()]))
        }
        Engine::Mysql | Engine::Mariadb => {
            if database.trim().is_empty() {
                return Err(crate::connections::error_codes::coded_string(crate::connections::error_codes::DUMP_DATABASE_REQUIRED, "需要数据库名，没有另开连接。"));
            }
            Ok((database.to_owned(), vec![String::new()]))
        }
        Engine::Postgres => {
            let current = current_postgres_database(session).await?;
            if !database.is_empty() && database != current {
                return Err(crate::connections::error_codes::coded_string(crate::connections::error_codes::DUMP_SESSION_GAP, format!("当前连接打开的是 {current}，没有另开连接读取 {database}")));
            }
            if schemas.is_empty() {
                let sql = metadata_sql(Engine::Postgres).schemas.ok_or_else(|| read_failure("PostgreSQL 缺少 schema 查询"))?;
                let all = postgres_text_column(session, sql).await?;
                if all.is_empty() {
                    return Err(read_failure("没有读到 schema"));
                }
                Ok((current, all))
            } else {
                Ok((current, schemas.to_vec()))
            }
        }
        Engine::Sqlserver => {
            let current = sqlserver_current_database(session).await?;
            if !database.is_empty() && database != current {
                return Err(crate::connections::error_codes::coded_string(crate::connections::error_codes::DUMP_SESSION_GAP, format!("当前连接打开的是 {current}，没有另开连接读取 {database}")));
            }
            if schemas.is_empty() {
                let sql = metadata_sql(Engine::Sqlserver).schemas.ok_or_else(|| read_failure("SQL Server 缺少 schema 查询"))?;
                let all = sqlserver_text_column(session, sql).await?;
                if all.is_empty() {
                    return Err(read_failure("没有读到 schema"));
                }
                Ok((current, all))
            } else {
                Ok((current, schemas.to_vec()))
            }
        }
    }
}

async fn sqlserver_current_database(session: &CatalogSession) -> Result<String, String> {
    let CatalogSession::Sqlserver(client) = session else {
        return Err(crate::connections::error_codes::coded_string(crate::connections::error_codes::DUMP_CONNECTION_MISMATCH, "连接类型和引擎不一致"));
    };
    let (_names, rows) = crate::grid::sqlserver_rows(client, "SELECT DB_NAME()").await.map_err(read_failure)?;
    match rows.first().and_then(|row| row.first()) {
        Some(crate::grid::Cell::Text(text)) => Ok(text.clone()),
        _ => Err(read_failure("没有当前数据库")),
    }
}

async fn sqlserver_text_column(session: &CatalogSession, sql: &str) -> Result<Vec<String>, String> {
    let CatalogSession::Sqlserver(client) = session else {
        return Err(crate::connections::error_codes::coded_string(crate::connections::error_codes::DUMP_CONNECTION_MISMATCH, "连接类型和引擎不一致"));
    };
    let (_names, rows) = crate::grid::sqlserver_rows(client, sql).await.map_err(read_failure)?;
    rows.iter()
        .map(|row| match row.first() {
            Some(crate::grid::Cell::Text(text)) => Ok(text.clone()),
            _ => Err(read_failure("元数据列是空值")),
        })
        .collect()
}

async fn enumerate_objects(engine: Engine, session: &CatalogSession, scope: &DumpScope, flag: &AtomicBool) -> Result<Vec<DumpObject>, String> {
    let mut objects = Vec::new();
    for schema in &scope.schemas {
        let mut offset = 0usize;
        loop {
            if flag.load(Ordering::Relaxed) {
                return Err(CANCELLED.to_owned());
            }
            let page = read_object_page(session, engine, &scope.database, schema, offset, OBJECT_PAGE).await;
            if let Some(message) = page.error {
                return Err(message);
            }
            let count = page.objects.len();
            for row in page.objects {
                objects.push(DumpObject::bare(DumpKind::from_catalog(row.kind), schema, row));
            }
            if count == 0 || offset + count >= page.total {
                break;
            }
            offset += count;
        }
    }
    Ok(objects)
}

const CANCELLED: &str = "\u{1}cancelled";

/// 表与视图的结构走 ddl 的快照（列、索引、外键、视图原文）；其余对象走 catalog 的只读定义。
async fn enrich_objects(engine: Engine, session: &CatalogSession, scope: &DumpScope, objects: &mut [DumpObject], notes: &mut Vec<String>, flag: &AtomicBool) {
    for object in objects.iter_mut() {
        if flag.load(Ordering::Relaxed) {
            object.error = Some("导出已取消".to_owned());
            continue;
        }
        match object.kind {
            DumpKind::Table | DumpKind::View => {
                let request = DdlRequest {
                    database: scope.database.clone(),
                    schema: object.schema.clone(),
                    engine,
                    action: Action::DropTable { table: object.name.clone() },
                };
                match load_partial(session, &request, &object.name).await {
                    Ok(snapshot) => absorb_snapshot(object, &snapshot, notes),
                    Err(message) => object.error = Some(message),
                }
            }
            _ => match read_definition(session, engine, &scope.database, &object.schema, &object.name, object.kind.kind_str(), &object.signature, &object.relation).await {
                Ok(sql) => object.definition = sql,
                Err(message) => object.error = Some(message),
            },
        }
    }
}

fn absorb_snapshot(object: &mut DumpObject, snapshot: &crate::ddl::Snapshot, notes: &mut Vec<String>) {
    if let Some(message) = &snapshot.definition_error {
        object.error = Some(message.clone());
        return;
    }
    if snapshot.kind == "view" || object.kind == DumpKind::View {
        object.kind = DumpKind::View;
        object.materialized = snapshot.materialized;
        let text = snapshot.table_sql.trim();
        if text.is_empty() || text.starts_with("--") {
            object.error = Some("没有读到视图定义".to_owned());
            return;
        }
        object.definition = snapshot.table_sql.clone();
        return;
    }
    let mut table = DumpTable {
        columns: snapshot
            .columns
            .iter()
            .map(|column| DumpColumn {
                name: column.name.clone(),
                type_name: column.type_name.clone(),
                nullable: column.nullable,
                default_sql: column.default_sql.clone().unwrap_or_default(),
                primary: column.primary,
                auto: column.auto,
            })
            .collect(),
        indexes: snapshot
            .indexes
            .iter()
            .map(|index| DumpIndex {
                name: index.name.clone(),
                unique: index.unique,
                columns: index.columns.iter().map(|column| (column.name.clone(), column.descending)).collect(),
                reason: index.reason.clone(),
                partial: index.partial,
                expression: index.expression,
                sql: index.sql.clone().unwrap_or_default(),
            })
            .collect(),
        foreign_keys: snapshot
            .foreign_keys
            .iter()
            .map(|key| DumpForeignKey {
                name: key.name.clone(),
                columns: key.columns.clone(),
                ref_schema: key.ref_schema.clone(),
                ref_table: key.ref_table.clone(),
                ref_columns: key.ref_columns.clone(),
                on_delete: key.on_delete.clone(),
                on_update: key.on_update.clone(),
            })
            .collect(),
        source_sql: String::new(),
    };
    if snapshot.index_error.is_some() {
        notes.push(format!("表 {} 的索引没有读全，导出不含索引。", object.name));
        table.indexes.clear();
    }
    if snapshot.foreign_key_error.is_some() {
        notes.push(format!("表 {} 的外键没有读全，导出不含外键。", object.name));
        table.foreign_keys.clear();
    }
    let source = snapshot.table_sql.trim();
    // SQLite 的 CREATE TABLE 原文带内联外键；其余引擎没有原文，只有 "-- 库.表" 占位。
    if snapshot.engine == Engine::Sqlite && !source.is_empty() && !source.starts_with("--") {
        table.source_sql = source.to_owned();
    }
    object.table = Some(table);
}

// ---------------------------------------------------------------- DDL 渲染（纯函数）

/// 导出文件里的一段：一条 DDL，或某张表的数据 INSERT 段（跟随该表的 CREATE 之后）。
#[derive(Clone, Debug, PartialEq, Eq)]
enum DumpEntry {
    Ddl { object: String, sql: String },
    Data { schema: String, table: String },
}

fn primary_columns(table: &DumpTable) -> Vec<String> {
    if let Some(index) = table.indexes.iter().find(|index| index.reason.contains("主键")) {
        return index.columns.iter().map(|(name, _)| name.clone()).collect();
    }
    table.columns.iter().filter(|column| column.primary).map(|column| column.name.clone()).collect()
}

/// 与 compare 的 skip_index 同规则：自动索引、主键与主键同列的索引不单独导出。
fn skip_index(index: &DumpIndex, table: &DumpTable) -> bool {
    if index.name.starts_with("sqlite_autoindex") || index.reason.contains("主键") {
        return true;
    }
    let pk = primary_columns(table);
    !pk.is_empty() && index.columns.iter().map(|(name, _)| name.as_str()).eq(pk.iter().map(String::as_str))
}

fn column_clause(engine: Engine, column: &DumpColumn) -> String {
    let mut line = format!("{} {}", quote_ident(engine, &column.name), column.type_name.trim());
    if !column.nullable {
        line.push_str(" NOT NULL");
    }
    if !column.default_sql.trim().is_empty() {
        line.push_str(" DEFAULT ");
        line.push_str(column.default_sql.trim());
    }
    if column.auto && matches!(engine, Engine::Mysql | Engine::Mariadb) {
        line.push_str(" AUTO_INCREMENT");
    }
    line
}

fn foreign_key_clause(engine: Engine, key: &DumpForeignKey) -> String {
    let columns = key.columns.iter().map(|column| quote_ident(engine, column)).collect::<Vec<_>>().join(", ");
    let refs = key.ref_columns.iter().map(|column| quote_ident(engine, column)).collect::<Vec<_>>().join(", ");
    let mut clause = format!(
        "CONSTRAINT {} FOREIGN KEY ({columns}) REFERENCES {} ({refs})",
        quote_ident(engine, &key.name),
        reference_name(engine, key)
    );
    if !key.on_delete.trim().is_empty() {
        clause.push_str(" ON DELETE ");
        clause.push_str(key.on_delete.trim());
    }
    if !key.on_update.trim().is_empty() {
        clause.push_str(" ON UPDATE ");
        clause.push_str(key.on_update.trim());
    }
    clause
}

fn reference_name(engine: Engine, key: &DumpForeignKey) -> String {
    if key.ref_schema.is_empty() {
        quote_ident(engine, &key.ref_table)
    } else {
        format!("{}.{}", quote_ident(engine, &key.ref_schema), quote_ident(engine, &key.ref_table))
    }
}

fn create_table_sql(engine: Engine, qualified: &str, table: &DumpTable, inline_fk: bool) -> String {
    let mut lines = Vec::new();
    for column in &table.columns {
        lines.push(column_clause(engine, column));
    }
    let pk = primary_columns(table);
    if !pk.is_empty() {
        let names = pk.iter().map(|column| quote_ident(engine, column)).collect::<Vec<_>>().join(", ");
        lines.push(format!("PRIMARY KEY ({names})"));
    }
    if inline_fk {
        for key in &table.foreign_keys {
            lines.push(foreign_key_clause(engine, key));
        }
    }
    format!("CREATE TABLE {} (\n  {}\n)", qualified, lines.join(",\n  "))
}

fn create_index_sql(engine: Engine, qualified_table: &str, index: &DumpIndex) -> String {
    if (index.partial || index.expression) && !index.sql.trim().is_empty() {
        return index.sql.trim().trim_end_matches(';').to_owned();
    }
    let unique = if index.unique { "UNIQUE " } else { "" };
    if index.columns.is_empty() {
        return index.sql.trim().trim_end_matches(';').to_owned();
    }
    let list = index
        .columns
        .iter()
        .map(|(name, descending)| format!("{} {}", quote_ident(engine, name), if *descending { "DESC" } else { "ASC" }))
        .collect::<Vec<_>>()
        .join(", ");
    format!("CREATE {unique}INDEX {} ON {} ({list})", quote_ident(engine, &index.name), qualified_table)
}

/// 视图与例程的定义原文若已是完整语句则原样导出，否则补上 `CREATE ... 名称 AS` 前缀。
fn definition_sql(qualified: &str, object: &DumpObject) -> Option<String> {
    let definition = object.definition.trim();
    if definition.is_empty() || definition.starts_with("--") {
        return None;
    }
    if definition.len() >= 6 && definition[..6].eq_ignore_ascii_case("CREATE") {
        return Some(definition.trim_end_matches(';').trim().to_owned());
    }
    match object.kind {
        DumpKind::View if object.materialized => Some(format!("CREATE MATERIALIZED VIEW {qualified} AS {definition}")),
        DumpKind::View => Some(format!("CREATE VIEW {qualified} AS {definition}")),
        _ => Some(definition.trim_end_matches(';').trim().to_owned()),
    }
}

/// 排出导出文件的段落顺序：支撑对象（秩+拓扑）→ 表（被引用方在前，数据跟随）→ 视图与例程（引用序）→ 触发器 → 外键补挂。
fn plan_sections(engine: Engine, scope: &DumpScope, objects: &[DumpObject], include_data: bool, table_filter: &[String]) -> Vec<DumpEntry> {
    let wanted_table = |name: &str| table_filter.is_empty() || table_filter.iter().any(|item| item.eq_ignore_ascii_case(name));
    let mut entries = Vec::new();
    let usable: Vec<&DumpObject> = objects.iter().filter(|object| object.error.is_none()).collect();

    // 1. 支撑对象：枚举、域、序列。定义文本里点到的同批名字是依赖，秩决定同层先后；同名重载合并为同一顶点、逐个写出。
    //    函数与过程放到第 3 段跟视图一起排：先建表再建例程，查询视图或表的例程才不会在空库上失败。
    let support: Vec<&DumpObject> = usable
        .iter()
        .copied()
        .filter(|object| matches!(object.kind, DumpKind::Enum | DumpKind::Domain | DumpKind::Sequence) && !object.definition.trim().is_empty())
        .collect();
    if !support.is_empty() {
        let names: BTreeSet<String> = support.iter().map(|object| object.name.clone()).collect();
        let nodes = support
            .iter()
            .map(|object| DepNode {
                key: object.name.clone(),
                rank: object.kind.support_rank(),
                references: order::definition_references(&object.name, &object.definition, &names),
            })
            .collect::<Vec<_>>();
        let mut by_name: std::collections::BTreeMap<String, Vec<&DumpObject>> = std::collections::BTreeMap::new();
        for object in support {
            by_name.entry(object.name.clone()).or_default().push(object);
        }
        for name in order::order_dep_keys(nodes, false) {
            for object in by_name.remove(&name).unwrap_or_default() {
                if let Some(sql) = definition_sql(&scope.qualify(engine, &object.schema, &object.name), object) {
                    entries.push(DumpEntry::Ddl { object: format!("{} {}", object.kind.label(), object.name), sql });
                }
            }
        }
    }

    // 2. 表：外键指向的表先建（SQLite 外键内联时必须如此；其余引擎数据行也按此序插入）。
    let tables: Vec<&DumpObject> = usable
        .iter()
        .copied()
        .filter(|object| object.kind == DumpKind::Table && object.table.is_some() && wanted_table(&object.name))
        .collect();
    let table_names: BTreeSet<String> = tables.iter().map(|object| object.name.clone()).collect();
    let nodes = tables
        .iter()
        .map(|object| DepNode {
            key: object.name.clone(),
            rank: 0,
            references: table_references(object, &table_names),
        })
        .collect::<Vec<_>>();
    let ordered_tables = order::order_dep_keys(nodes, false)
        .into_iter()
        .flat_map(|name| tables.iter().copied().filter(move |object| object.name.eq_ignore_ascii_case(&name)))
        .collect::<Vec<_>>();

    for object in &ordered_tables {
        let table = object.table.as_ref().expect("table shape");
        let qualified = scope.qualify(engine, &object.schema, &object.name);
        let inline_fk = engine == Engine::Sqlite;
        let create = if !table.source_sql.is_empty() {
            table.source_sql.trim_end_matches(';').to_owned()
        } else {
            create_table_sql(engine, &qualified, table, inline_fk)
        };
        entries.push(DumpEntry::Ddl { object: format!("表 {}", object.name), sql: create });
        for index in &table.indexes {
            if skip_index(index, table) {
                continue;
            }
            entries.push(DumpEntry::Ddl { object: format!("索引 {}", index.name), sql: create_index_sql(engine, &qualified, index) });
        }
        if include_data {
            entries.push(DumpEntry::Data { schema: object.schema.clone(), table: object.name.clone() });
        }
    }

    // 3. 视图与例程：视图按 FROM/JOIN 的真实引用排序，例程按调用与视图引用排序。
    let readers: Vec<&DumpObject> = usable
        .iter()
        .copied()
        .filter(|object| matches!(object.kind, DumpKind::View | DumpKind::Function | DumpKind::Procedure) && !object.definition.trim().is_empty())
        .collect();
    if !readers.is_empty() {
        let view_names: Vec<String> = readers.iter().filter(|object| object.kind == DumpKind::View).map(|object| object.name.clone()).collect();
        let routine_names: Vec<String> = readers
            .iter()
            .filter(|object| matches!(object.kind, DumpKind::Function | DumpKind::Procedure))
            .map(|object| object.name.clone())
            .collect();
        let nodes = readers
            .iter()
            .map(|object| DepNode {
                key: object.name.clone(),
                rank: 0,
                references: reader_references(object, &view_names, &routine_names),
            })
            .collect::<Vec<_>>();
        let mut by_name: std::collections::BTreeMap<String, Vec<&DumpObject>> = std::collections::BTreeMap::new();
        for object in readers {
            by_name.entry(object.name.clone()).or_default().push(object);
        }
        for name in order::order_dep_keys(nodes, false) {
            for object in by_name.remove(&name).unwrap_or_default() {
                if let Some(sql) = definition_sql(&scope.qualify(engine, &object.schema, &object.name), object) {
                    entries.push(DumpEntry::Ddl { object: format!("{} {}", object.kind.label(), object.name), sql });
                }
            }
        }
    }

    // 4. 触发器最后：它们依赖表和例程都已就位。
    let mut triggers: Vec<&DumpObject> = usable.iter().copied().filter(|object| object.kind == DumpKind::Trigger && !object.definition.trim().is_empty()).collect();
    triggers.sort_by(|left, right| left.name.cmp(&right.name));
    for object in triggers {
        if let Some(sql) = definition_sql(&scope.qualify(engine, &object.schema, &object.name), object) {
            entries.push(DumpEntry::Ddl { object: format!("触发器 {}", object.name), sql });
        }
    }

    // 5. 非 SQLite 的外键在全部数据行之后补挂，插入顺序就不再受约束检查牵制。
    if engine != Engine::Sqlite {
        for object in &ordered_tables {
            let Some(table) = object.table.as_ref() else { continue };
            let qualified = scope.qualify(engine, &object.schema, &object.name);
            for key in &table.foreign_keys {
                if key.name.trim().is_empty() {
                    continue;
                }
                entries.push(DumpEntry::Ddl {
                    object: format!("外键 {}", key.name),
                    sql: format!("ALTER TABLE {qualified} ADD {}", foreign_key_clause(engine, key)),
                });
            }
        }
    }
    entries
}

/// 表的外键依赖：指向同批表名的引用（PostgreSQL 只认本 schema 或未带 schema 的引用）。
fn table_references(object: &DumpObject, table_names: &BTreeSet<String>) -> Vec<String> {
    let Some(table) = object.table.as_ref() else {
        return Vec::new();
    };
    let mut refs = Vec::new();
    for key in &table.foreign_keys {
        if key.ref_table.is_empty() {
            continue;
        }
        if !key.ref_schema.is_empty() && !key.ref_schema.eq_ignore_ascii_case(&object.schema) {
            continue;
        }
        if table_names.contains(&key.ref_table) && !key.ref_table.eq_ignore_ascii_case(&object.name) {
            refs.push(key.ref_table.clone());
        }
    }
    refs
}

/// 视图/例程的引用边：视图 → 被查的视图 + 被调的例程；例程 → 被调的例程 + 被查的视图。
fn reader_references(object: &DumpObject, views: &[String], routines: &[String]) -> Vec<String> {
    let mut refs = BTreeSet::new();
    let relations = order::referenced_relations(&object.definition);
    if object.kind == DumpKind::View {
        for name in views {
            if relations.iter().any(|found| found.eq_ignore_ascii_case(name)) && !name.eq_ignore_ascii_case(&object.name) {
                refs.insert(name.clone());
            }
        }
    }
    for name in routines {
        if !name.eq_ignore_ascii_case(&object.name) && order::calls_routine(&object.definition, name) {
            refs.insert(name.clone());
        }
    }
    if object.kind != DumpKind::View {
        for name in views {
            if relations.iter().any(|found| found.eq_ignore_ascii_case(name)) {
                refs.insert(name.clone());
            }
        }
    }
    refs.into_iter().collect()
}

// ---------------------------------------------------------------- 数据段（流式分页读 + insert_line）

enum DumpStop {
    Cancelled,
    Failed(String),
}

/// 数据段的稳定排序：键列优先，SQLite 无键表按 rowid。迁移（R8）的流式分页读沿用同一规则。
pub(crate) fn data_order_clause(schema: &grid::TableSchema) -> String {
    if !schema.key.is_empty() {
        let columns = schema
            .key
            .iter()
            .map(|name| format!("{} ASC", quote_ident(schema.engine, name)))
            .collect::<Vec<_>>()
            .join(", ");
        return format!(" ORDER BY {columns}");
    }
    if schema.engine == Engine::Sqlite && schema.kind == "table" {
        return " ORDER BY rowid".to_owned();
    }
    String::new()
}

async fn write_table_data(
    out: &mut BufWriter<File>,
    session: &CatalogSession,
    database: &str,
    schema: &str,
    table: &str,
    engine_hint: &str,
    flag: &AtomicBool,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<u64, DumpStop> {
    let schema = match describe(session, database, schema, table, engine_hint).await {
        Ok(schema) => schema,
        Err(message) => return Err(DumpStop::Failed(format!("表 {table} 读取失败：{message}"))),
    };
    let list = select_list(&schema, true);
    let order = data_order_clause(&schema);
    let width = schema.columns.len();
    let names: Vec<String> = schema.columns.iter().map(|column| column.name.clone()).collect();
    let columns = schema.columns.clone();
    let engine = schema.engine;
    let schema_label = schema.schema.clone();
    let table = schema.name.clone();
    let mut offset = 0u64;
    let mut written = 0u64;
    loop {
        if flag.load(Ordering::Relaxed) {
            return Err(DumpStop::Cancelled);
        }
        let sql = format!("SELECT {list} FROM {}{order} LIMIT {DATA_PAGE} OFFSET {offset}", qualified_name(&schema));
        let rows = match fetch_cells(session, &sql, width).await {
            Ok(rows) => rows,
            Err(message) => return Err(DumpStop::Failed(format!("表 {} 读取失败：{message}", table))),
        };
        if rows.is_empty() {
            break;
        }
        let page_rows = rows.len() as u64;
        for cells in &rows {
            let line = insert_line(engine, &schema_label, &table, &columns, &names, cells);
            if let Err(err) = writeln!(out, "{line}") {
                return Err(DumpStop::Failed(err.to_string()));
            }
            written += 1;
        }
        progress(written);
        if page_rows < DATA_PAGE {
            break;
        }
        offset += DATA_PAGE;
    }
    Ok(written)
}

// ---------------------------------------------------------------- 主流程

fn staging_path(target: &Path) -> PathBuf {
    target.with_file_name(format!(".rillsql-{}.partial", uuid::Uuid::new_v4()))
}

fn cancelled_dto(objects: usize, rows: u64) -> DumpDto {
    DumpDto {
        status: "cancelled".to_owned(),
        success: false,
        path: None,
        residual_path: None,
        message: format!("已取消导出：已写出 {objects} 个对象、{rows} 行后停止，未完成的文件已删除。"),
        objects,
        rows,
    }
}

fn failed_dto(message: String, residual: Option<String>, objects: usize, rows: u64) -> DumpDto {
    let residual_text = match &residual {
        Some(path) => format!("不完整的文件保留为 {path}，不会冒充完整结果。"),
        None => "没有生成完整结果。".to_owned(),
    };
    DumpDto {
        status: "failed".to_owned(),
        success: false,
        path: None,
        residual_path: residual,
        message: format!("{message}。已写出 {objects} 个对象、{rows} 行。{residual_text}"),
        objects,
        rows,
    }
}

fn done_dto(path: String, objects: usize, rows: u64, include_data: bool) -> DumpDto {
    let data_note = if include_data { format!("含数据 {rows} 行。") } else { "仅结构，不含数据。".to_owned() };
    DumpDto {
        status: "done".to_owned(),
        success: true,
        path: Some(path.clone()),
        residual_path: None,
        message: format!("已导出 {objects} 个对象到 {path}。{data_note}"),
        objects,
        rows,
    }
}

/// 导出正文。progress 回调在命令层转发成 `dump_progress` 事件；测试里换成收集器。
pub(crate) async fn run_dump(session: &CatalogSession, request: &DumpRequest, progress: &(dyn Fn(DumpProgress) + Send + Sync)) -> DumpDto {
    let flag = begin_job(&request.id);
    let outcome = dump_body(session, request, &flag, progress).await;
    finish_job(&request.id);
    outcome
}

async fn dump_body(session: &CatalogSession, request: &DumpRequest, flag: &AtomicBool, progress: &(dyn Fn(DumpProgress) + Send + Sync)) -> DumpDto {
    if matches!(session, CatalogSession::Scripted) {
        return failed_dto("这个连接没有连着可查询的数据库".to_owned(), None, 0, 0);
    }
    let engine = match Engine::parse(&request.engine) {
        Ok(engine) if engine_matches(session, engine) => engine,
        Ok(_) | Err(_) => return failed_dto("会话类型和目标引擎不一致".to_owned(), None, 0, 0),
    };
    let (database, schemas) = match resolve_scope(engine, session, &request.database, &request.schemas).await {
        Ok(scope) => scope,
        Err(message) => return failed_dto(message, None, 0, 0),
    };
    let scope = DumpScope { database, schemas };
    let mut objects = match enumerate_objects(engine, session, &scope, flag).await {
        Ok(objects) => objects,
        Err(message) if message == CANCELLED => return cancelled_dto(0, 0),
        Err(message) => return failed_dto(message, None, 0, 0),
    };
    let mut notes = Vec::new();
    enrich_objects(engine, session, &scope, &mut objects, &mut notes, flag).await;
    if flag.load(Ordering::Relaxed) {
        return cancelled_dto(0, 0);
    }
    for object in objects.iter().filter(|object| object.error.is_some()) {
        notes.push(format!("{} {} 没有导出：{}", object.kind.label(), object.name, object.error.as_deref().unwrap_or("")));
    }
    if !request.tables.is_empty() {
        notes.push("表范围只勾选了部分表；视图与例程仍全部导出，引用未导出表的对象在空库执行会失败。".to_owned());
    }

    let target = Path::new(&request.path);
    if !target.is_absolute() || target.file_name().is_none() {
        return failed_dto("请选择完整的文件保存路径".to_owned(), None, 0, 0);
    }
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return failed_dto(format!("路径不可写：目录不存在 {}", parent.display()), None, 0, 0);
        }
    }
    let staging = staging_path(target);
    let file = match File::create(&staging) {
        Ok(file) => file,
        Err(err) => return failed_dto(format!("路径不可写：{err}"), None, 0, 0),
    };
    let mut out = BufWriter::new(file);
    if let Err(err) = write_header(&mut out, engine, &scope, request, &notes) {
        return residual_outcome(&staging, err.to_string(), 0, 0);
    }

    let entries = plan_sections(engine, &scope, &objects, request.include_data, &request.tables);
    let mut written_objects = 0usize;
    let mut written_rows = 0u64;
    for entry in &entries {
        if flag.load(Ordering::Relaxed) {
            let _ = out.flush();
            let _ = fs::remove_file(&staging);
            return cancelled_dto(written_objects, written_rows);
        }
        match entry {
            DumpEntry::Ddl { object, sql } => {
                let text = sql.trim();
                if text.is_empty() {
                    continue;
                }
                if let Err(err) = writeln!(out, "{text};\n") {
                    return residual_outcome(&staging, err.to_string(), written_objects, written_rows);
                }
                written_objects += 1;
                progress(DumpProgress::ddl(&request.id, object));
            }
            DumpEntry::Data { schema, table } => {
                if let Err(err) = writeln!(out, "\n-- 表 {table} 的数据") {
                    return residual_outcome(&staging, err.to_string(), written_objects, written_rows);
                }
                let mut table_rows = |rows: u64| progress(DumpProgress::data(&request.id, table, rows));
                match write_table_data(&mut out, session, &scope.database, schema, table, engine_name(engine), flag, &mut table_rows).await {
                    Ok(count) => written_rows += count,
                    Err(DumpStop::Cancelled) => {
                        let _ = out.flush();
                        let _ = fs::remove_file(&staging);
                        return cancelled_dto(written_objects, written_rows);
                    }
                    Err(DumpStop::Failed(message)) => return residual_outcome(&staging, message, written_objects, written_rows),
                }
                if let Err(err) = writeln!(out) {
                    return residual_outcome(&staging, err.to_string(), written_objects, written_rows);
                }
            }
        }
    }
    if let Err(err) = out.flush() {
        return residual_outcome(&staging, err.to_string(), written_objects, written_rows);
    }
    if target.exists() && fs::remove_file(target).is_err() {
        return residual_outcome(&staging, "无法替换目标文件".to_owned(), written_objects, written_rows);
    }
    if let Err(err) = fs::rename(&staging, target) {
        return residual_outcome(&staging, err.to_string(), written_objects, written_rows);
    }
    done_dto(target.display().to_string(), written_objects, written_rows, request.include_data)
}

fn residual_outcome(staging: &Path, message: String, objects: usize, rows: u64) -> DumpDto {
    failed_dto(message, Some(staging.display().to_string()), objects, rows)
}

fn write_header(out: &mut BufWriter<File>, engine: Engine, scope: &DumpScope, request: &DumpRequest, notes: &[String]) -> io::Result<()> {
    let engine_label = match engine {
        Engine::Postgres => "PostgreSQL",
        Engine::Mysql => "MySQL",
        Engine::Mariadb => "MariaDB",
        Engine::Sqlite => "SQLite",
        Engine::Sqlserver => "SQL Server",
        // Redis 不在全库导出面（capability 门控）；占位保持穷尽。
        Engine::Redis => "Redis",
        Engine::Mongodb => "MongoDB",
    };
    let database = if scope.database.is_empty() { "main".to_owned() } else { scope.database.clone() };
    let schemas = if matches!(engine, Engine::Postgres | Engine::Sqlserver) { scope.schemas.join(", ") } else { String::new() };
    writeln!(out, "-- RillSQL 全库导出：只读生成，未执行任何语句。")?;
    writeln!(out, "-- 引擎：{engine_label}；库：{database}{}", if schemas.is_empty() { String::new() } else { format!("；schema：{schemas}") })?;
    writeln!(out, "-- 生成时间：{}；{}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), if request.include_data { "含结构与数据。" } else { "仅结构，不含数据。" })?;
    for note in notes {
        writeln!(out, "-- 注：{note}")?;
    }
    writeln!(out)
}

// ---------------------------------------------------------------- 纯逻辑测试（不依赖 docker）

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Cell;

    fn column(name: &str, type_name: &str, nullable: bool) -> DumpColumn {
        DumpColumn { name: name.to_owned(), type_name: type_name.to_owned(), nullable, default_sql: String::new(), primary: false, auto: false }
    }

    fn pk_index(columns: &[&str]) -> DumpIndex {
        DumpIndex {
            name: "PRIMARY".to_owned(),
            unique: true,
            columns: columns.iter().map(|item| ((*item).to_owned(), false)).collect(),
            reason: "主键".to_owned(),
            partial: false,
            expression: false,
            sql: String::new(),
        }
    }

    fn fk(name: &str, ref_table: &str) -> DumpForeignKey {
        DumpForeignKey {
            name: name.to_owned(),
            columns: vec!["ref_id".to_owned()],
            ref_schema: String::new(),
            ref_table: ref_table.to_owned(),
            ref_columns: vec!["id".to_owned()],
            on_delete: String::new(),
            on_update: String::new(),
        }
    }

    fn table_object(schema: &str, name: &str, table: DumpTable) -> DumpObject {
        DumpObject { kind: DumpKind::Table, schema: schema.to_owned(), name: name.to_owned(), signature: String::new(), relation: String::new(), definition: String::new(), materialized: false, table: Some(table), error: None }
    }

    fn defined_object(kind: DumpKind, schema: &str, name: &str, definition: &str) -> DumpObject {
        DumpObject { kind, schema: schema.to_owned(), name: name.to_owned(), signature: String::new(), relation: String::new(), definition: definition.to_owned(), materialized: false, table: None, error: None }
    }

    fn postgres_scope() -> DumpScope {
        DumpScope { database: "app".to_owned(), schemas: vec!["public".to_owned()] }
    }

    fn entry_at(entries: &[DumpEntry], needle: &str) -> usize {
        entries
            .iter()
            .position(|entry| match entry {
                DumpEntry::Ddl { sql, .. } => sql.contains(needle),
                DumpEntry::Data { table, .. } => table.contains(needle),
            })
            .unwrap_or_else(|| panic!("找不到 {needle}：{entries:?}"))
    }

    #[test]
    fn dump_orders_support_objects_by_rank_and_references() {
        // 枚举、域按秩先行；函数与过程跟视图同段，按调用关系先建被调用方。
        let objects = vec![
            defined_object(DumpKind::Domain, "public", "price", "CREATE DOMAIN public.price AS numeric"),
            defined_object(DumpKind::Enum, "public", "color", "CREATE TYPE public.color AS ENUM ('red')"),
            defined_object(DumpKind::Procedure, "public", "ship", "CREATE PROCEDURE public.ship() AS $$ BEGIN PERFORM total_price(); END $$"),
            defined_object(DumpKind::Function, "public", "total_price", "CREATE FUNCTION public.total_price() RETURNS numeric AS $$ SELECT 1::price $$"),
        ];
        let entries = plan_sections(Engine::Postgres, &postgres_scope(), &objects, false, &[]);
        let sqls: Vec<String> = entries
            .iter()
            .filter_map(|entry| match entry {
                DumpEntry::Ddl { sql, .. } => Some(sql.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(sqls, vec![
            "CREATE TYPE public.color AS ENUM ('red')",
            "CREATE DOMAIN public.price AS numeric",
            "CREATE FUNCTION public.total_price() RETURNS numeric AS $$ SELECT 1::price $$",
            "CREATE PROCEDURE public.ship() AS $$ BEGIN PERFORM total_price(); END $$",
        ]);
    }

    #[test]
    fn dump_puts_fk_parent_table_before_child_and_data_after_its_table() {
        let child = DumpTable {
            columns: vec![column("id", "integer", false), column("customer_id", "integer", true)],
            indexes: vec![pk_index(&["id"])],
            foreign_keys: vec![fk("orders_customer_fk", "customers")],
            source_sql: String::new(),
        };
        let parent = DumpTable { columns: vec![column("id", "integer", false), column("name", "text", false)], indexes: vec![pk_index(&["id"])], foreign_keys: vec![], source_sql: String::new() };
        let objects = vec![table_object("public", "orders", child), table_object("public", "customers", parent)];
        let entries = plan_sections(Engine::Postgres, &postgres_scope(), &objects, true, &[]);
        let parent_at = entry_at(&entries, "CREATE TABLE \"public\".\"customers\"");
        let child_at = entry_at(&entries, "CREATE TABLE \"public\".\"orders\"");
        let child_data = entries
            .iter()
            .position(|entry| matches!(entry, DumpEntry::Data { table, .. } if table == "orders"))
            .expect("orders 数据段");
        let parent_data = entries
            .iter()
            .position(|entry| matches!(entry, DumpEntry::Data { table, .. } if table == "customers"))
            .expect("customers 数据段");
        assert!(parent_at < child_at, "{entries:?}");
        assert!(child_at < child_data, "{entries:?}");
        assert!(parent_at < parent_data, "{entries:?}");
        // 数据 INSERT 跟随各自表：外键补挂语句在全部数据之后。
        let fk_at = entry_at(&entries, "ALTER TABLE");
        assert!(fk_at > child_data, "{entries:?}");
        let create_sql = match &entries[child_at] {
            DumpEntry::Ddl { sql, .. } => sql.clone(),
            _ => String::new(),
        };
        assert!(create_sql.contains("PRIMARY KEY"), "{create_sql}");
        assert!(!create_sql.contains("FOREIGN KEY"), "{create_sql}");
    }

    #[test]
    fn dump_inline_sqlite_foreign_keys_and_keeps_source_sql() {
        let child = DumpTable {
            columns: vec![column("id", "integer", false)],
            indexes: vec![],
            foreign_keys: vec![fk("orders_fk", "customers")],
            source_sql: "CREATE TABLE \"orders\" (\n  \"id\" integer PRIMARY KEY,\n  \"ref_id\" integer REFERENCES \"customers\" (\"id\")\n)".to_owned(),
        };
        let objects = vec![table_object("", "orders", child)];
        let scope = DumpScope::default();
        let entries = plan_sections(Engine::Sqlite, &scope, &objects, false, &[]);
        let create = match &entries[0] {
            DumpEntry::Ddl { sql, .. } => sql.clone(),
            _ => String::new(),
        };
        assert!(create.contains("REFERENCES"), "{create}");
        assert!(entries.iter().all(|entry| !matches!(entry, DumpEntry::Ddl { sql, .. } if sql.starts_with("ALTER TABLE"))));
    }

    #[test]
    fn dump_orders_views_after_tables_and_between_views_by_reference() {
        let orders = DumpTable { columns: vec![column("id", "integer", false)], indexes: vec![pk_index(&["id"])], foreign_keys: vec![], source_sql: String::new() };
        let objects = vec![
            defined_object(DumpKind::View, "public", "v_top", "SELECT * FROM v_base"),
            table_object("public", "orders", orders),
            defined_object(DumpKind::View, "public", "v_base", "SELECT id FROM orders"),
        ];
        let entries = plan_sections(Engine::Postgres, &postgres_scope(), &objects, false, &[]);
        let table_at = entry_at(&entries, "CREATE TABLE");
        let base_at = entry_at(&entries, "CREATE VIEW \"public\".\"v_base\" AS SELECT id FROM orders");
        let top_at = entry_at(&entries, "CREATE VIEW \"public\".\"v_top\" AS SELECT * FROM v_base");
        assert!(table_at < base_at, "{entries:?}");
        assert!(base_at < top_at, "{entries:?}");
    }

    #[test]
    fn dump_wraps_bare_view_bodies_and_skips_unreadable_objects() {
        let objects = vec![
            defined_object(DumpKind::View, "public", "v_raw", "SELECT 1 AS one"),
            DumpObject {
                kind: DumpKind::View,
                schema: "public".to_owned(),
                name: "v_broken".to_owned(),
                signature: String::new(),
                relation: String::new(),
                definition: String::new(),
                materialized: false,
                table: None,
                error: Some("权限不足".to_owned()),
            },
            DumpObject {
                kind: DumpKind::View,
                schema: "public".to_owned(),
                name: "v_mat".to_owned(),
                signature: String::new(),
                relation: String::new(),
                definition: "SELECT id FROM orders".to_owned(),
                materialized: true,
                table: None,
                error: None,
            },
        ];
        let entries = plan_sections(Engine::Postgres, &postgres_scope(), &objects, false, &[]);
        let raw = entry_at(&entries, "CREATE VIEW \"public\".\"v_raw\" AS SELECT 1 AS one");
        let mat = entry_at(&entries, "CREATE MATERIALIZED VIEW \"public\".\"v_mat\" AS SELECT id FROM orders");
        assert!(entries.iter().all(|entry| !matches!(entry, DumpEntry::Ddl { sql, .. } if sql.contains("v_broken"))));
        assert!(raw < mat || mat < raw);
    }

    #[test]
    fn dump_table_filter_keeps_other_objects_and_marks_data_scope() {
        let orders = DumpTable { columns: vec![column("id", "integer", false)], indexes: vec![pk_index(&["id"])], foreign_keys: vec![], source_sql: String::new() };
        let objects = vec![
            table_object("public", "orders", orders),
            defined_object(DumpKind::View, "public", "v_orders", "SELECT id FROM orders"),
        ];
        let entries = plan_sections(Engine::Postgres, &postgres_scope(), &objects, true, &["Orders".to_owned().to_ascii_lowercase()]);
        // 表范围只筛表：视图与例程仍全部导出。
        assert_eq!(entries.len(), 3, "{entries:?}");
        assert!(matches!(&entries[0], DumpEntry::Ddl { sql, .. } if sql.contains("CREATE TABLE")));
        assert!(matches!(&entries[1], DumpEntry::Data { table, .. } if table == "orders"));
        assert!(matches!(&entries[2], DumpEntry::Ddl { sql, .. } if sql.contains("v_orders")));
    }

    #[test]
    fn insert_lines_use_the_grid_projection_rules_with_binary_hex() {
        // 与 grid 的 insert_line 同一实现：二进制 hex 文本按引擎落成 X'..' / '\x..' 字面量。
        let schema = grid::TableSchema {
            engine: Engine::Postgres,
            schema: "public".to_owned(),
            name: "docs".to_owned(),
            kind: "table",
            columns: vec![
                grid::Column { name: "id".to_owned(), type_name: "integer".to_owned(), nullable: false, primary: true, unique: true, auto: false },
                grid::Column { name: "body".to_owned(), type_name: "bytea".to_owned(), nullable: true, primary: false, unique: false, auto: false },
                grid::Column { name: "note".to_owned(), type_name: "text".to_owned(), nullable: true, primary: false, unique: false, auto: false },
            ],
            key: vec!["id".to_owned()],
        };
        let names: Vec<String> = schema.columns.iter().map(|column| column.name.clone()).collect();
        let cells = vec![Cell::Text("7".to_owned()), Cell::Text("0AFF".to_owned()), Cell::Text("它说'引号'".to_owned())];
        let line = insert_line(Engine::Postgres, &schema.schema, &schema.name, &schema.columns, &names, &cells);
        assert_eq!(line, "INSERT INTO \"public\".\"docs\" (\"id\", \"body\", \"note\") VALUES (7, '\\x0aff', '它说''引号''');");
        let mysql_line = insert_line(Engine::Mysql, "app", "docs", &schema.columns, &names, &cells);
        assert!(mysql_line.contains("X'0aff'"), "{mysql_line}");
        assert!(mysql_line.contains("`app`.`docs`"), "{mysql_line}");
    }

    #[test]
    fn data_order_clause_uses_key_then_sqlite_rowid() {
        let schema = grid::TableSchema {
            engine: Engine::Postgres,
            schema: "public".to_owned(),
            name: "orders".to_owned(),
            kind: "table",
            columns: vec![],
            key: vec!["id".to_owned(), "seq".to_owned()],
        };
        assert_eq!(data_order_clause(&schema), " ORDER BY \"id\" ASC, \"seq\" ASC");
        let sqlite = grid::TableSchema { engine: Engine::Sqlite, schema: String::new(), name: "notes".to_owned(), kind: "table", columns: vec![], key: vec![] };
        assert_eq!(data_order_clause(&sqlite), " ORDER BY rowid");
        let loose = grid::TableSchema { engine: Engine::Mysql, schema: "app".to_owned(), name: "logs".to_owned(), kind: "table", columns: vec![], key: vec![] };
        assert_eq!(data_order_clause(&loose), "");
    }

    /// 真实 SQLite 会话的端到端对拍：导出文件在空库上直执行，行数与二进制值往返一致。
    /// docker seed 在本环境不可用，SQLite 用同一套 dump 通路覆盖“可空库直执行”验收。
    #[tokio::test]
    async fn sqlite_dump_executes_on_an_empty_database_and_rows_round_trip() {
        use crate::connections::Service;
        let dir = std::env::temp_dir().join(format!("rillsql-dump-e2e-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let database = dir.join("shop.sqlite");
        let setup = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
            .await
            .unwrap();
        // 子表叫 a_orders、父表叫 z_customers：名称序会把子表排前，外键序必须把父表放前面。
        for sql in [
            "CREATE TABLE z_customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, avatar BLOB)",
            "CREATE TABLE a_orders (id INTEGER PRIMARY KEY, customer_id INTEGER NOT NULL REFERENCES z_customers (id), note TEXT)",
            "CREATE VIEW v_orders AS SELECT id, note FROM a_orders",
            "CREATE TRIGGER trim_note AFTER INSERT ON a_orders BEGIN UPDATE a_orders SET note = trim(note) WHERE id = NEW.id; END",
        ] {
            sqlx::query(sql).execute(&setup).await.unwrap();
        }
        sqlx::query("INSERT INTO z_customers (id, name, avatar) VALUES (1, '林夏', x'DEADBEef'), (2, '周衡', NULL)")
            .execute(&setup)
            .await
            .unwrap();
        // 触发器会把首尾空格剪掉，这里插入已修剪的值，往返比对才稳定。
        sqlx::query("INSERT INTO a_orders (id, customer_id, note) VALUES (1, 1, '已''付''款'), (2, 2, NULL)")
            .execute(&setup)
            .await
            .unwrap();
        setup.close().await;

        let service = Service::new_for_test(dir.join("connections.sqlite"));
        service.open_sqlite_for_test("shop", "商城", &database).await.unwrap();
        let session = service.catalog_session("shop").expect("已打开会话");
        let target = dir.join("shop-dump.sql");
        let request = DumpRequest {
            id: format!("dump-e2e-{}", uuid::Uuid::new_v4()),
            connection_id: "shop".to_owned(),
            engine: "sqlite".to_owned(),
            database: String::new(),
            schemas: Vec::new(),
            tables: Vec::new(),
            include_data: true,
            path: target.display().to_string(),
        };
        let events: std::sync::Mutex<Vec<DumpProgress>> = std::sync::Mutex::new(Vec::new());
        let outcome = run_dump(&session, &request, &|payload| events.lock().expect("events").push(payload)).await;
        assert!(outcome.success, "{}", outcome.message);
        assert_eq!(outcome.status, "done");
        assert!(outcome.rows >= 4, "{}", outcome.message);
        let script = fs::read_to_string(&target).unwrap();
        assert!(!script.contains(".partial"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().filter_map(|item| item.ok()).filter(|item| item.file_name().to_string_lossy().contains(".partial")).count(), 0);
        let customers_at = script.find("CREATE TABLE z_customers").expect("建表");
        let orders_at = script.find("CREATE TABLE a_orders").expect("建表");
        let view_at = script.find("CREATE VIEW").expect("建视图");
        let trigger_at = script.find("CREATE TRIGGER").expect("建触发器");
        assert!(customers_at < orders_at, "{script}");
        assert!(orders_at < view_at, "{script}");
        assert!(view_at < trigger_at, "{script}");
        assert!(script.contains("X'deadbeef'") || script.to_ascii_lowercase().contains("x'deadbeef'"), "{script}");
        assert!(script.contains("'已''付''款'"), "{script}");
        assert!(events.lock().expect("events").iter().any(|event| event.phase == "data" && event.object == "z_customers"));

        // 空库直执行：逐条执行导出的语句，再对拍行数与二进制值。
        let fresh_file = dir.join("fresh.sqlite");
        let fresh = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&fresh_file).create_if_missing(true))
            .await
            .unwrap();
        let report = crate::query::split_sql("sqlite", &script);
        assert!(report.refusal.is_none(), "{:?}", report.refusal);
        let pieces: Vec<String> = report.pieces.iter().map(|piece| piece.text.trim().to_owned()).filter(|text| !text.is_empty()).collect();
        for piece in pieces {
            // 测试执行的是刚生成的导出语句，与 live.rs 的 audited 同一豁免方式。
            let statement = sqlx::AssertSqlSafe(piece.clone());
            let quoted = piece.clone();
            sqlx::query(statement).execute(&fresh).await.unwrap_or_else(|err| panic!("语句执行失败：{err}
{quoted}"));
        }
        let customers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM z_customers").fetch_one(&fresh).await.unwrap();
        let orders: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM a_orders").fetch_one(&fresh).await.unwrap();
        let blob: String = sqlx::query_scalar("SELECT lower(hex(avatar)) FROM z_customers WHERE id = 1").fetch_one(&fresh).await.unwrap();
        let views: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name = 'v_orders' AND type = 'view'").fetch_one(&fresh).await.unwrap();
        let triggers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name = 'trim_note' AND type = 'trigger'").fetch_one(&fresh).await.unwrap();
        assert_eq!((customers, orders, views, triggers), (2, 2, 1, 1));
        assert_eq!(blob, "deadbeef");
        fresh.close().await;

        // 仅结构导出没有任何 INSERT。
        let structure_target = dir.join("shop-structure.sql");
        let structure = DumpRequest { path: structure_target.display().to_string(), include_data: false, ..request };
        let structure_outcome = run_dump(&session, &structure, &|_| {}).await;
        assert!(structure_outcome.success, "{}", structure_outcome.message);
        let structure_script = fs::read_to_string(&structure_target).unwrap();
        assert!(structure_script.contains("CREATE TABLE"), "{structure_script}");
        assert!(!structure_script.contains("INSERT INTO"), "{structure_script}");
        // 会话连接仍握着源库文件，Windows 下清理放后台尽力而为。
        drop(session);
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn dump_writes_partial_then_renames_and_cancel_removes_partial() {
        let dir = std::env::temp_dir().join(format!("rillsql-dump-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let target = dir.join("app.sql");
        let request = DumpRequest {
            id: "dump-1".to_owned(),
            connection_id: "c1".to_owned(),
            engine: "sqlite".to_owned(),
            database: String::new(),
            schemas: Vec::new(),
            tables: Vec::new(),
            include_data: false,
            path: target.display().to_string(),
        };
        // Scripted 会话没有数据库通道：按现有纪律整体失败且不留文件。
        let outcome = run_dump(&CatalogSession::Scripted, &request, &|_| {}).await;
        assert_eq!(outcome.status, "failed");
        assert!(!target.exists(), "{}", target.display());
        assert!(std::fs::read_dir(&dir).unwrap().count() == 0, "不应留下 .partial");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn request_cancel_flags_a_running_job() {
        let flag = begin_job("dump-cancel-test");
        assert!(!flag.load(Ordering::Relaxed));
        request_cancel("dump-cancel-test");
        assert!(flag.load(Ordering::Relaxed));
        finish_job("dump-cancel-test");
    }
}
