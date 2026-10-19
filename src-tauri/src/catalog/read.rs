//! 在已经打开的会话上读取库表。这里不新开数据库连接。

use mysql_async::prelude::Queryable;

use crate::catalog::model::{
    kind_sql, known_row_estimate, listed_kinds, metadata_sql, parse_kind, DatabaseNode, Engine, KindReport, ObjectKind,
    ObjectRow, SchemaNode, DEFAULT_OBJECT_PAGE,
};
use crate::catalog::model::sqlite_kind_count_sql;
use crate::connections::CatalogSession;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectWindow {
    pub database: String,
    pub schema: String,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionBody {
    pub error: Option<String>,
    pub foreign_keys: Option<bool>,
    pub databases: Vec<DatabaseNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectPageBody {
    pub objects: Vec<ObjectRow>,
    pub total: usize,
    pub error: Option<String>,
    pub kind_reports: Vec<KindReport>,
}

pub(crate) fn read_limit(requested: usize, page: usize) -> usize {
    let page = if page == 0 { DEFAULT_OBJECT_PAGE } else { page };
    if requested == 0 { page } else { requested }
}

fn requested_limit(database: &str, schema: &str, windows: &[ObjectWindow], page: usize) -> usize {
    let requested = windows
        .iter()
        .filter(|item| item.database == database && item.schema == schema)
        .map(|item| item.limit)
        .filter(|limit| *limit > 0)
        .max()
        .unwrap_or(0);
    read_limit(requested, page)
}

fn read_failure(err: impl std::fmt::Display) -> String {
    format!("读取库表失败：{err}")
}

fn definition_failure(err: impl std::fmt::Display) -> String {
    format!("读取定义失败：{err}")
}

fn scripted_message() -> String {
    read_failure("这个连接没有连着可查询的数据库")
}

pub(crate) fn other_database_message(current: &str, name: &str) -> String {
    format!("当前会话连在 {current}，没有另开连接读取 {name}")
}

fn empty_page(message: impl Into<String>) -> ObjectPageBody {
    ObjectPageBody {
        objects: Vec::new(),
        total: 0,
        error: Some(message.into()),
        kind_reports: Vec::new(),
    }
}

pub async fn read_catalog_session(engine: Engine, session: &CatalogSession, page: usize, windows: &[ObjectWindow]) -> ConnectionBody {
    match (engine, session) {
        (Engine::Sqlite, CatalogSession::Sqlite { pool, foreign_keys }) => read_sqlite(pool, *foreign_keys, page, windows).await,
        (Engine::Mysql | Engine::Mariadb, CatalogSession::Mysql(pool)) => match read_mysql(pool, engine, page, windows).await {
            Ok(databases) => ConnectionBody {
                error: None,
                foreign_keys: None,
                databases,
            },
            Err(message) => ConnectionBody {
                error: Some(message),
                foreign_keys: None,
                databases: Vec::new(),
            },
        },
        (Engine::Postgres, CatalogSession::Postgres(client)) => match read_postgres(client, page, windows).await {
            Ok(databases) => ConnectionBody {
                error: None,
                foreign_keys: None,
                databases,
            },
            Err(message) => ConnectionBody {
                error: Some(message),
                foreign_keys: None,
                databases: Vec::new(),
            },
        },
        (_, CatalogSession::Scripted) => ConnectionBody {
            error: Some(scripted_message()),
            foreign_keys: None,
            databases: Vec::new(),
        },
        _ => ConnectionBody {
            error: Some(read_failure("连接类型和引擎不一致")),
            foreign_keys: None,
            databases: Vec::new(),
        },
    }
}

/// 只读取库名；PostgreSQL 再读取 schema 名。对象留给 catalog_page。
pub async fn read_catalog_outline(engine: Engine, session: &CatalogSession) -> ConnectionBody {
    match (engine, session) {
        (Engine::Sqlite, CatalogSession::Sqlite { foreign_keys, .. }) => ConnectionBody {
            error: None,
            foreign_keys: *foreign_keys,
            databases: vec![DatabaseNode {
                name: "main".to_owned(),
                error: None,
                schemas: vec![SchemaNode::empty("")],
            }],
        },
        (Engine::Mysql | Engine::Mariadb, CatalogSession::Mysql(pool)) => match read_mysql_outline(pool, engine).await {
            Ok(databases) => ConnectionBody {
                error: None,
                foreign_keys: None,
                databases,
            },
            Err(message) => ConnectionBody {
                error: Some(message),
                foreign_keys: None,
                databases: Vec::new(),
            },
        },
        (Engine::Postgres, CatalogSession::Postgres(client)) => match read_postgres_outline(client).await {
            Ok(databases) => ConnectionBody {
                error: None,
                foreign_keys: None,
                databases,
            },
            Err(message) => ConnectionBody {
                error: Some(message),
                foreign_keys: None,
                databases: Vec::new(),
            },
        },
        (_, CatalogSession::Scripted) => ConnectionBody {
            error: Some(scripted_message()),
            foreign_keys: None,
            databases: Vec::new(),
        },
        _ => ConnectionBody {
            error: Some(read_failure("连接类型和引擎不一致")),
            foreign_keys: None,
            databases: Vec::new(),
        },
    }
}

pub async fn read_object_page(session: &CatalogSession, database: &str, schema: &str, offset: usize, limit: usize) -> ObjectPageBody {
    let limit = read_limit(limit, 0);
    match session {
        CatalogSession::Scripted => empty_page(scripted_message()),
        CatalogSession::Sqlite { pool, .. } => match read_sqlite_objects(pool, offset, limit).await {
            Ok(schema_node) => ObjectPageBody {
                objects: schema_node.objects,
                total: schema_node.total,
                error: schema_node.error,
                kind_reports: schema_node.kind_reports,
            },
            Err(message) => empty_page(message),
        },
        CatalogSession::Mysql(pool) => read_mysql_page(pool, database, offset, limit).await,
        CatalogSession::Postgres(client) => read_postgres_page(client, database, schema, offset, limit).await,
    }
}

pub async fn read_definition(
    session: &CatalogSession,
    database: &str,
    schema: &str,
    name: &str,
    kind: &str,
    signature: &str,
    relation: &str,
) -> Result<String, String> {
    if name.is_empty() {
        return Err(definition_failure("缺少对象名"));
    }
    match session {
        CatalogSession::Scripted => Err(definition_failure("这个连接没有连着可查询的数据库")),
        CatalogSession::Sqlite { pool, .. } => read_sqlite_definition(pool, name, kind).await,
        CatalogSession::Mysql(pool) => read_mysql_definition(pool, database, name, kind).await,
        CatalogSession::Postgres(client) => read_postgres_definition(client, schema, name, kind, signature, relation).await,
    }
}

pub(crate) fn assemble_postgres(names: &[String], current: &str, schemas: Result<Vec<SchemaNode>, String>) -> Vec<DatabaseNode> {
    names
        .iter()
        .map(|name| {
            if name == current {
                match &schemas {
                    Ok(nodes) => DatabaseNode {
                        name: name.clone(),
                        schemas: nodes.clone(),
                        error: None,
                    },
                    Err(message) => DatabaseNode {
                        name: name.clone(),
                        schemas: Vec::new(),
                        error: Some(message.clone()),
                    },
                }
            } else {
                DatabaseNode {
                    name: name.clone(),
                    schemas: Vec::new(),
                    error: Some(other_database_message(current, name)),
                }
            }
        })
        .collect()
}

fn finish_kinds(parts: Vec<(ObjectKind, Result<(usize, Vec<ObjectRow>), String>)>, offset: usize, limit: usize) -> (Vec<ObjectRow>, usize, Vec<KindReport>) {
    let mut reports = Vec::new();
    let mut rows = Vec::new();
    let mut total = 0usize;
    for (kind, result) in parts {
        match result {
            Ok((kind_total, objects)) => {
                total = total.saturating_add(kind_total);
                rows.extend(objects);
                reports.push(KindReport {
                    kind,
                    total: kind_total,
                    error: None,
                });
            }
            Err(message) => reports.push(KindReport {
                kind,
                total: 0,
                error: Some(message),
            }),
        }
    }
    rows.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.signature.cmp(&right.signature))
            .then_with(|| left.relation.cmp(&right.relation))
            .then_with(|| left.kind_rank().cmp(&right.kind_rank()))
    });
    let objects = rows.into_iter().skip(offset).take(limit).collect();
    (objects, total, reports)
}

trait KindRank {
    fn kind_rank(&self) -> u8;
}

impl KindRank for ObjectRow {
    fn kind_rank(&self) -> u8 {
        match self.kind {
            ObjectKind::Table => 0,
            ObjectKind::View => 1,
            ObjectKind::Function => 2,
            ObjectKind::Procedure => 3,
            ObjectKind::Trigger => 4,
        }
    }
}

async fn read_sqlite(pool: &sqlx::SqlitePool, foreign_keys: Option<bool>, page: usize, windows: &[ObjectWindow]) -> ConnectionBody {
    let limit = requested_limit("main", "", windows, page);
    match read_sqlite_objects(pool, 0, limit).await {
        Ok(schema) => ConnectionBody {
            error: None,
            foreign_keys,
            databases: vec![DatabaseNode {
                name: "main".to_owned(),
                error: None,
                schemas: vec![schema],
            }],
        },
        Err(message) => ConnectionBody {
            error: Some(message),
            foreign_keys,
            databases: Vec::new(),
        },
    }
}

async fn read_sqlite_objects(pool: &sqlx::SqlitePool, offset: usize, limit: usize) -> Result<SchemaNode, String> {
    let sql = metadata_sql(Engine::Sqlite);
    let counts: Vec<(String, i64)> = sqlx::query_as(sqlite_kind_count_sql())
        .fetch_all(pool)
        .await
        .map_err(read_failure)?;
    let mut reports: Vec<KindReport> = listed_kinds(Engine::Sqlite)
        .iter()
        .copied()
        .map(|kind| KindReport {
            kind,
            total: 0,
            error: None,
        })
        .collect();
    for (kind_name, count) in counts {
        let kind = parse_kind(&kind_name);
        if let Some(report) = reports.iter_mut().find(|item| item.kind == kind) {
            report.total = usize::try_from(count).unwrap_or(0);
        }
    }
    let total = reports.iter().map(|item| item.total).sum();
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(sql.objects)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .bind(i64::try_from(offset).unwrap_or(i64::MAX))
        .fetch_all(pool)
        .await
        .map_err(read_failure)?;
    let objects = rows
        .into_iter()
        .map(|(name, kind, signature, relation)| ObjectRow {
            kind: parse_kind(&kind),
            name,
            estimated_rows: None,
            stale: false,
            signature,
            relation,
        })
        .collect();
    Ok(SchemaNode {
        name: String::new(),
        objects,
        total,
        error: None,
        kind_reports: reports,
    })
}

async fn read_sqlite_definition(pool: &sqlx::SqlitePool, name: &str, kind: &str) -> Result<String, String> {
    if kind != "trigger" {
        return Err(definition_failure(if kind == "function" || kind == "procedure" {
            "SQLite 没有函数或存储过程"
        } else {
            "只有函数、存储过程和触发器可以读取只读定义"
        }));
    }
    let sql: Option<String> = sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = ?1 AND type = 'trigger'")
        .bind(name)
        .fetch_optional(pool)
        .await
        .map_err(definition_failure)?;
    match sql {
        Some(text) if !text.trim().is_empty() => Ok(text),
        _ => Err(definition_failure("没有读到定义")),
    }
}

async fn read_mysql(pool: &mysql_async::Pool, engine: Engine, page: usize, windows: &[ObjectWindow]) -> Result<Vec<DatabaseNode>, String> {
    let sql = metadata_sql(engine);
    let mut conn = pool.get_conn().await.map_err(read_failure)?;
    let rows: Vec<mysql_async::Row> = conn.query(sql.databases).await.map_err(read_failure)?;
    let mut names = Vec::new();
    for row in &rows {
        names.push(mysql_string(row, 0)?);
    }
    let mut databases = Vec::new();
    for name in names {
        let limit = requested_limit(&name, "", windows, page);
        databases.push(read_mysql_database(&mut conn, &name, 0, limit).await);
    }
    Ok(databases)
}

async fn read_mysql_outline(pool: &mysql_async::Pool, engine: Engine) -> Result<Vec<DatabaseNode>, String> {
    let sql = metadata_sql(engine);
    let mut conn = pool.get_conn().await.map_err(read_failure)?;
    let rows: Vec<mysql_async::Row> = conn.query(sql.databases).await.map_err(read_failure)?;
    let mut databases = Vec::new();
    for row in &rows {
        databases.push(DatabaseNode {
            name: mysql_string(row, 0)?,
            error: None,
            schemas: vec![SchemaNode::empty("")],
        });
    }
    Ok(databases)
}

async fn read_mysql_page(pool: &mysql_async::Pool, database: &str, offset: usize, limit: usize) -> ObjectPageBody {
    let mut conn = match pool.get_conn().await {
        Ok(conn) => conn,
        Err(err) => return empty_page(read_failure(err)),
    };
    let schema = read_mysql_objects(&mut conn, database, offset, limit).await;
    ObjectPageBody {
        objects: schema.objects,
        total: schema.total,
        error: schema.error,
        kind_reports: schema.kind_reports,
    }
}

async fn read_mysql_database(conn: &mut mysql_async::Conn, database: &str, offset: usize, limit: usize) -> DatabaseNode {
    let schema = read_mysql_objects(conn, database, offset, limit).await;
    DatabaseNode {
        name: database.to_owned(),
        error: None,
        schemas: vec![schema],
    }
}

async fn read_mysql_objects(conn: &mut mysql_async::Conn, database: &str, offset: usize, limit: usize) -> SchemaNode {
    let window = offset.saturating_add(limit);
    let mut parts = Vec::new();
    for kind in listed_kinds(Engine::Mysql) {
        parts.push((*kind, read_mysql_kind(conn, database, *kind, window).await));
    }
    let (objects, total, reports) = finish_kinds(parts, offset, limit);
    SchemaNode {
        name: String::new(),
        objects,
        total,
        error: None,
        kind_reports: reports,
    }
}

async fn read_mysql_kind(conn: &mut mysql_async::Conn, database: &str, kind: ObjectKind, window: usize) -> Result<(usize, Vec<ObjectRow>), String> {
    let sql = kind_sql(Engine::Mysql, kind);
    let count_row: Option<mysql_async::Row> = conn
        .exec_first(sql.count, (database.to_owned(),))
        .await
        .map_err(read_failure)?;
    let total = match count_row.as_ref().and_then(|row| mysql_i64(row, 0)) {
        Some(value) => usize::try_from(value).map_err(|_| read_failure("对象数量无效"))?,
        None => return Err(read_failure("没有对象数量")),
    };
    let rows: Vec<mysql_async::Row> = conn
        .exec(sql.list, (database.to_owned(), i64::try_from(window).unwrap_or(i64::MAX)))
        .await
        .map_err(read_failure)?;
    let mut objects = Vec::new();
    for row in &rows {
        objects.push(mysql_object(row)?);
    }
    Ok((total, objects))
}

async fn read_mysql_definition(pool: &mysql_async::Pool, database: &str, name: &str, kind: &str) -> Result<String, String> {
    let statement = match kind {
        "function" => "FUNCTION",
        "procedure" => "PROCEDURE",
        "trigger" => "TRIGGER",
        _ => return Err(definition_failure("只有函数、存储过程和触发器可以读取只读定义")),
    };
    if database.is_empty() {
        return Err(definition_failure("需要数据库名"));
    }
    let sql = format!("SHOW CREATE {statement} {}.{}", quote_mysql(database), quote_mysql(name));
    let mut conn = pool.get_conn().await.map_err(definition_failure)?;
    let row: Option<mysql_async::Row> = conn.query_first(sql).await.map_err(definition_failure)?;
    let row = row.ok_or_else(|| definition_failure("没有读到定义"))?;
    mysql_create_sql(&row)
}

fn quote_mysql(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

fn mysql_create_sql(row: &mysql_async::Row) -> Result<String, String> {
    let statement = mysql_string(row, 2).ok();
    mysql_show_create_statement(&[None, None, statement.as_deref()])
}

/// SHOW CREATE FUNCTION、PROCEDURE、TRIGGER 的语句都在索引 2。
/// 索引 0 是对象名，名称里的 CREATE 不是定义。
fn mysql_show_create_statement(columns: &[Option<&str>]) -> Result<String, String> {
    let text = columns.get(2).and_then(|item| *item).unwrap_or("");
    if text.trim().is_empty() {
        Err(definition_failure("没有读到定义"))
    } else {
        Ok(text.to_owned())
    }
}

async fn read_postgres(client: &tokio_postgres::Client, page: usize, windows: &[ObjectWindow]) -> Result<Vec<DatabaseNode>, String> {
    let (names, current) = postgres_database_names(client).await?;
    let schemas_sql = metadata_sql(Engine::Postgres).schemas.ok_or_else(|| read_failure("PostgreSQL 缺少 schema 查询"))?;
    let schema_result = match client.query(schemas_sql, &[]).await {
        Ok(rows) => {
            let mut schemas = Vec::new();
            for row in &rows {
                let name = pg_string(row, 0)?;
                let limit = requested_limit(&current, &name, windows, page);
                schemas.push(read_postgres_schema(client, &name, 0, limit).await);
            }
            Ok(schemas)
        }
        Err(err) => Err(read_failure(err)),
    };
    Ok(assemble_postgres(&names, &current, schema_result))
}

async fn read_postgres_outline(client: &tokio_postgres::Client) -> Result<Vec<DatabaseNode>, String> {
    let (names, current) = postgres_database_names(client).await?;
    let schemas_sql = metadata_sql(Engine::Postgres).schemas.ok_or_else(|| read_failure("PostgreSQL 缺少 schema 查询"))?;
    let schema_result = match client.query(schemas_sql, &[]).await {
        Ok(rows) => {
            let mut schemas = Vec::new();
            for row in &rows {
                schemas.push(SchemaNode::empty(pg_string(row, 0)?));
            }
            Ok(schemas)
        }
        Err(err) => Err(read_failure(err)),
    };
    Ok(assemble_postgres(&names, &current, schema_result))
}

async fn postgres_database_names(client: &tokio_postgres::Client) -> Result<(Vec<String>, String), String> {
    let sql = metadata_sql(Engine::Postgres);
    let db_rows = client.query(sql.databases, &[]).await.map_err(read_failure)?;
    let mut names = Vec::new();
    for row in &db_rows {
        names.push(pg_string(row, 0)?);
    }
    let current = current_database(client).await?;
    if !names.iter().any(|name| name == &current) {
        names.push(current.clone());
        names.sort();
    }
    Ok((names, current))
}

async fn read_postgres_page(client: &tokio_postgres::Client, database: &str, schema: &str, offset: usize, limit: usize) -> ObjectPageBody {
    let current = match current_database(client).await {
        Ok(name) => name,
        Err(message) => return empty_page(message),
    };
    if database != current {
        return empty_page(other_database_message(&current, database));
    }
    let schema_node = read_postgres_schema(client, schema, offset, limit).await;
    ObjectPageBody {
        objects: schema_node.objects,
        total: schema_node.total,
        error: schema_node.error,
        kind_reports: schema_node.kind_reports,
    }
}

async fn current_database(client: &tokio_postgres::Client) -> Result<String, String> {
    let rows = client.query("SELECT current_database()", &[]).await.map_err(read_failure)?;
    let row = rows.first().ok_or_else(|| read_failure("没有当前数据库"))?;
    pg_string(row, 0)
}

async fn read_postgres_schema(client: &tokio_postgres::Client, schema: &str, offset: usize, limit: usize) -> SchemaNode {
    let window = offset.saturating_add(limit);
    let mut parts = Vec::new();
    for kind in listed_kinds(Engine::Postgres) {
        parts.push((*kind, read_postgres_kind(client, schema, *kind, window).await));
    }
    let (objects, total, reports) = finish_kinds(parts, offset, limit);
    SchemaNode {
        name: schema.to_owned(),
        objects,
        total,
        error: None,
        kind_reports: reports,
    }
}

async fn read_postgres_kind(client: &tokio_postgres::Client, schema: &str, kind: ObjectKind, window: usize) -> Result<(usize, Vec<ObjectRow>), String> {
    let sql = kind_sql(Engine::Postgres, kind);
    let count_params: [&(dyn tokio_postgres::types::ToSql + Sync); 1] = [&schema];
    let count_rows = client.query(sql.count, &count_params).await.map_err(read_failure)?;
    let total = match count_rows.first().map(|row| pg_i64(row, 0)) {
        Some(Ok(value)) => usize::try_from(value).map_err(|_| read_failure("对象数量无效"))?,
        Some(Err(message)) => return Err(message),
        None => return Err(read_failure("没有对象数量")),
    };
    let window_i = i64::try_from(window).unwrap_or(i64::MAX);
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 2] = [&schema, &window_i];
    let rows = client.query(sql.list, &params).await.map_err(read_failure)?;
    let mut objects = Vec::new();
    for row in &rows {
        objects.push(postgres_object(row)?);
    }
    Ok((total, objects))
}

async fn read_postgres_definition(
    client: &tokio_postgres::Client,
    schema: &str,
    name: &str,
    kind: &str,
    signature: &str,
    relation: &str,
) -> Result<String, String> {
    if schema.is_empty() {
        return Err(definition_failure("PostgreSQL 需要 schema"));
    }
    match kind {
        "function" | "procedure" => {
            let prokind = if kind == "procedure" { "p" } else { "f" };
            let sql = "SELECT pg_get_functiondef(p.oid) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 AND p.proname = $2 AND p.prokind = $3 AND ($4 = '' OR pg_get_function_identity_arguments(p.oid) = $4)";
            let rows = client
                .query(sql, &[&schema, &name, &prokind, &signature])
                .await
                .map_err(definition_failure)?;
            if rows.len() > 1 {
                return Err(definition_failure("有多个同名函数，需要用参数类型区分"));
            }
            let row = rows.first().ok_or_else(|| definition_failure("没有读到定义"))?;
            let text = pg_string(row, 0).map_err(|err| definition_failure(err))?;
            if text.trim().is_empty() {
                return Err(definition_failure("没有读到定义"));
            }
            Ok(text)
        }
        "trigger" => {
            let sql = "SELECT pg_get_triggerdef(t.oid, true) FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND t.tgname = $2 AND NOT t.tgisinternal AND ($3 = '' OR c.relname = $3)";
            let rows = client.query(sql, &[&schema, &name, &relation]).await.map_err(definition_failure)?;
            if rows.len() > 1 {
                return Err(definition_failure("有多个同名触发器，需要所属的表才能区分"));
            }
            let row = rows.first().ok_or_else(|| definition_failure("没有读到定义"))?;
            let text = pg_string(row, 0).map_err(|err| definition_failure(err))?;
            if text.trim().is_empty() {
                return Err(definition_failure("没有读到定义"));
            }
            Ok(text)
        }
        _ => Err(definition_failure("只有函数、存储过程和触发器可以读取只读定义")),
    }
}

fn postgres_object(row: &tokio_postgres::Row) -> Result<ObjectRow, String> {
    let name = pg_string(row, 0)?;
    let kind = pg_string(row, 1)?;
    let estimated = pg_i64_opt(row, 2)?;
    Ok(ObjectRow {
        name,
        kind: parse_kind(&kind),
        estimated_rows: known_row_estimate(estimated),
        stale: false,
        signature: pg_text(row, 3),
        relation: pg_text(row, 4),
    })
}

fn pg_string(row: &tokio_postgres::Row, index: usize) -> Result<String, String> {
    if let Ok(value) = row.try_get::<_, String>(index) {
        return Ok(value);
    }
    match row.try_get::<_, Option<String>>(index) {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err(read_failure("元数据列是空值")),
        Err(err) => Err(read_failure(err)),
    }
}

fn pg_text(row: &tokio_postgres::Row, index: usize) -> String {
    row.try_get::<_, String>(index)
        .or_else(|_| row.try_get::<_, Option<String>>(index).map(|value| value.unwrap_or_default()))
        .unwrap_or_default()
}

fn pg_i64(row: &tokio_postgres::Row, index: usize) -> Result<i64, String> {
    row.try_get(index).map_err(read_failure)
}

fn pg_i64_opt(row: &tokio_postgres::Row, index: usize) -> Result<Option<i64>, String> {
    if let Ok(value) = row.try_get::<_, Option<i64>>(index) {
        return Ok(value);
    }
    row.try_get::<_, i64>(index).map(Some).map_err(read_failure)
}

fn mysql_object(row: &mysql_async::Row) -> Result<ObjectRow, String> {
    Ok(ObjectRow {
        name: mysql_string(row, 0)?,
        kind: parse_kind(&mysql_string(row, 1)?),
        estimated_rows: known_row_estimate(mysql_i64(row, 2)),
        stale: false,
        signature: mysql_text(row, 3),
        relation: mysql_text(row, 4),
    })
}

fn mysql_string(row: &mysql_async::Row, index: usize) -> Result<String, String> {
    let value = row.as_ref(index).ok_or_else(|| read_failure("元数据列缺失"))?;
    match value {
        mysql_async::Value::Bytes(bytes) => String::from_utf8(bytes.clone()).map_err(read_failure),
        mysql_async::Value::NULL => Err(read_failure("元数据列是空值")),
        other => Ok(other.as_sql(true)),
    }
}

fn mysql_text(row: &mysql_async::Row, index: usize) -> String {
    mysql_string(row, index).unwrap_or_default()
}

#[cfg(test)]
mod definition_column_tests {
    use super::mysql_show_create_statement;

    #[test]
    fn show_create_uses_the_statement_column_when_the_name_contains_create() {
        let function = [
            Some("create_order"),
            Some("ONLY_FULL_GROUP_BY"),
            Some("CREATE FUNCTION `create_order`() RETURNS int\nBEGIN\n  RETURN 1;\nEND"),
        ];
        let sql = mysql_show_create_statement(&function).expect("statement column");
        assert!(sql.starts_with("CREATE FUNCTION"));
        assert_ne!(sql, "create_order");

        let trigger = [
            Some("created_at"),
            Some(""),
            Some("CREATE TRIGGER created_at BEFORE INSERT ON t FOR EACH ROW SET NEW.a = 1"),
        ];
        assert!(mysql_show_create_statement(&trigger).unwrap().starts_with("CREATE TRIGGER"));
        assert!(mysql_show_create_statement(&[Some("create_order"), Some("mode")]).is_err());
        assert!(mysql_show_create_statement(&[Some("create_order"), Some("mode"), Some("   ")]).is_err());
    }
}

fn mysql_i64(row: &mysql_async::Row, index: usize) -> Option<i64> {
    let value = row.as_ref(index)?;
    match value {
        mysql_async::Value::NULL => None,
        mysql_async::Value::Int(number) => Some(*number),
        mysql_async::Value::UInt(number) => i64::try_from(*number).ok(),
        mysql_async::Value::Bytes(bytes) => std::str::from_utf8(bytes).ok().and_then(|text| text.parse().ok()),
        _ => None,
    }
}
