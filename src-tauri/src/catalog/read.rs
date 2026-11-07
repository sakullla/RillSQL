//! 在已经打开的会话上读取库表。这里不新开数据库连接。

use mysql_async::prelude::Queryable;

use crate::catalog::model::{
    kind_sql, known_row_estimate, listed_kinds, metadata_sql, parse_kind, DatabaseNode, Engine, KindReport, ObjectKind,
    ObjectRow, SchemaNode, DEFAULT_OBJECT_PAGE,
};
use crate::catalog::model::{quote_sqlite, sqlite_kind_count_sql, sqlite_object_page_sql};
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
        (Engine::Sqlserver, CatalogSession::Sqlserver(client)) => match read_sqlserver(client, page, windows).await {
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
        // Redis 没有库/schema/对象层级：键空间由前端经 redis_scan_keys 分页读取。
        (Engine::Redis, CatalogSession::Redis(_)) => ConnectionBody {
            error: None,
            foreign_keys: None,
            databases: Vec::new(),
        },
        // MongoDB（R12）：库列表 + 每库集合清单（estimatedDocumentCount 作为行估算），复用 MySQL 的 databases/无 schema 形状。
        (Engine::Mongodb, CatalogSession::Mongo(client)) => match read_mongo(client, page, windows).await {
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
        // SQLite 的“库”就是 main 与每个 ATTACH 上来的库：ATTACH 之后它们同样出现在树上。
        (Engine::Sqlite, CatalogSession::Sqlite { pool, foreign_keys }) => match sqlite_database_names(pool).await {
            Ok(databases) => ConnectionBody {
                error: None,
                foreign_keys: *foreign_keys,
                databases,
            },
            Err(message) => ConnectionBody {
                error: Some(message),
                foreign_keys: *foreign_keys,
                databases: Vec::new(),
            },
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
        (Engine::Sqlserver, CatalogSession::Sqlserver(client)) => match read_sqlserver_outline(client).await {
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
        (Engine::Mongodb, CatalogSession::Mongo(client)) => match read_mongo_outline(client).await {
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

pub async fn read_object_page(session: &CatalogSession, engine: Engine, database: &str, schema: &str, offset: usize, limit: usize) -> ObjectPageBody {
    let limit = read_limit(limit, 0);
    match session {
        CatalogSession::Scripted => empty_page(scripted_message()),
        CatalogSession::Sqlite { pool, .. } => match read_sqlite_objects(pool, database, offset, limit).await {
            Ok(schema_node) => ObjectPageBody {
                objects: schema_node.objects,
                total: schema_node.total,
                error: schema_node.error,
                kind_reports: schema_node.kind_reports,
            },
            Err(message) => empty_page(message),
        },
        CatalogSession::Mysql(pool) => read_mysql_page(pool, engine, database, offset, limit).await,
        CatalogSession::Postgres(client) => read_postgres_page(client, database, schema, offset, limit).await,
        CatalogSession::Sqlserver(client) => read_sqlserver_page(client, database, schema, offset, limit).await,
        CatalogSession::Redis(_) => empty_page("Redis 键空间经键浏览器分页读取，不走对象分页。"),
        CatalogSession::Mongo(client) => read_mongo_page(client, database, offset, limit).await,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn read_definition(
    session: &CatalogSession,
    engine: Engine,
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
        CatalogSession::Sqlite { pool, .. } => read_sqlite_definition(pool, database, name, kind).await,
        CatalogSession::Mysql(pool) => read_mysql_definition(pool, engine, database, name, kind).await,
        CatalogSession::Postgres(client) => read_postgres_definition(client, schema, name, kind, signature, relation).await,
        CatalogSession::Sqlserver(client) => read_sqlserver_definition(client, schema, name, kind).await,
        CatalogSession::Redis(_) => Err(definition_failure("Redis 没有对象定义")),
        CatalogSession::Mongo(_) => Err(definition_failure("MongoDB 集合没有 SQL 定义；文档在集合浏览器查看")),
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

#[allow(clippy::type_complexity)]
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
            ObjectKind::Sequence => 5,
            ObjectKind::Enum => 6,
            ObjectKind::Domain => 7,
            ObjectKind::Extension => 8,
            ObjectKind::Event => 9,
        }
    }
}

/// SQLite 的库清单来自 PRAGMA database_list：main 之后是每个 ATTACH 上来的库。
/// temp 不是用户库，不进树。
async fn sqlite_database_names(pool: &sqlx::SqlitePool) -> Result<Vec<DatabaseNode>, String> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as("PRAGMA database_list")
        .fetch_all(pool)
        .await
        .map_err(read_failure)?;
    let databases: Vec<DatabaseNode> = rows
        .into_iter()
        .filter(|(_, name, _)| name != "temp")
        .map(|(_, name, _)| DatabaseNode {
            name,
            error: None,
            schemas: vec![SchemaNode::empty("")],
        })
        .collect();
    if databases.is_empty() {
        return Err(read_failure("没有读到 SQLite 数据库"));
    }
    Ok(databases)
}

async fn read_sqlite(pool: &sqlx::SqlitePool, foreign_keys: Option<bool>, page: usize, windows: &[ObjectWindow]) -> ConnectionBody {
    let names = match sqlite_database_names(pool).await {
        Ok(names) => names,
        Err(message) => {
            return ConnectionBody {
                error: Some(message),
                foreign_keys,
                databases: Vec::new(),
            };
        }
    };
    let mut databases = Vec::new();
    for node in names {
        let limit = requested_limit(&node.name, "", windows, page);
        // 单个附加库读失败只让这个库带原因，其它库照常显示。
        databases.push(match read_sqlite_objects(pool, &node.name, 0, limit).await {
            Ok(schema) => DatabaseNode {
                name: node.name,
                error: None,
                schemas: vec![schema],
            },
            Err(message) => DatabaseNode {
                name: node.name,
                error: Some(message),
                schemas: vec![SchemaNode::empty("")],
            },
        });
    }
    ConnectionBody {
        error: None,
        foreign_keys,
        databases,
    }
}

/// 空库名按 main 处理：调用方不带库名时读的就是这条连接自己的库。
fn sqlite_database_or_main(database: &str) -> &str {
    if database.is_empty() {
        "main"
    } else {
        database
    }
}

async fn read_sqlite_objects(pool: &sqlx::SqlitePool, database: &str, offset: usize, limit: usize) -> Result<SchemaNode, String> {
    let database = sqlite_database_or_main(database);
    // 库名只来自 PRAGMA database_list，并且按 SQL 标识符规则加引号；表名仍走绑定参数。
    let counts: Vec<(String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(sqlite_kind_count_sql(database)))
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
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(sqlite_object_page_sql(database)))
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
            materialized: false,
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

async fn read_sqlite_definition(pool: &sqlx::SqlitePool, database: &str, name: &str, kind: &str) -> Result<String, String> {
    let database = sqlite_database_or_main(database);
    if kind != "trigger" {
        return Err(definition_failure(match kind {
            "function" | "procedure" => "SQLite 没有函数或存储过程",
            "sequence" | "enum" | "domain" => "SQLite 没有序列、枚举或域",
            _ => "只有函数、存储过程和触发器可以读取只读定义",
        }));
    }
    // 附加库的触发器在它自己的 sqlite_master 里，必须带上库名限定。
    let statement = format!(
        "SELECT sql FROM {}.sqlite_master WHERE name = ?1 AND type = 'trigger'",
        quote_sqlite(database)
    );
    let sql: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
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
        databases.push(read_mysql_database(&mut conn, engine, &name, 0, limit).await);
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

async fn read_mysql_page(pool: &mysql_async::Pool, engine: Engine, database: &str, offset: usize, limit: usize) -> ObjectPageBody {
    let mut conn = match pool.get_conn().await {
        Ok(conn) => conn,
        Err(err) => return empty_page(read_failure(err)),
    };
    let schema = read_mysql_objects(&mut conn, engine, database, offset, limit).await;
    ObjectPageBody {
        objects: schema.objects,
        total: schema.total,
        error: schema.error,
        kind_reports: schema.kind_reports,
    }
}

async fn read_mysql_database(conn: &mut mysql_async::Conn, engine: Engine, database: &str, offset: usize, limit: usize) -> DatabaseNode {
    let schema = read_mysql_objects(conn, engine, database, offset, limit).await;
    DatabaseNode {
        name: database.to_owned(),
        error: None,
        schemas: vec![schema],
    }
}

async fn read_mysql_objects(conn: &mut mysql_async::Conn, engine: Engine, database: &str, offset: usize, limit: usize) -> SchemaNode {
    let window = offset.saturating_add(limit);
    let mut parts = Vec::new();
    for kind in listed_kinds(engine) {
        parts.push((*kind, read_mysql_kind(conn, engine, database, *kind, window).await));
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

async fn read_mysql_kind(conn: &mut mysql_async::Conn, engine: Engine, database: &str, kind: ObjectKind, window: usize) -> Result<(usize, Vec<ObjectRow>), String> {
    let sql = kind_sql(engine, kind);
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

async fn read_mysql_definition(pool: &mysql_async::Pool, engine: Engine, database: &str, name: &str, kind: &str) -> Result<String, String> {
    let statement = match kind {
        "function" => "FUNCTION",
        "procedure" => "PROCEDURE",
        "trigger" => "TRIGGER",
        // 事件的事件体只读：这里只给 SHOW CREATE EVENT 的原文。
        "event" => "EVENT",
        "sequence" if engine == Engine::Mariadb => "SEQUENCE",
        "sequence" | "enum" | "domain" => {
            return Err(definition_failure(if engine == Engine::Mariadb {
                "MariaDB 没有独立的枚举或域"
            } else {
                "MySQL 没有独立的序列、枚举或域"
            }));
        }
        _ => return Err(definition_failure("只有函数、存储过程、触发器、事件和视图可以读取只读定义")),
    };
    if database.is_empty() {
        return Err(definition_failure("需要数据库名"));
    }
    let sql = format!("SHOW CREATE {statement} {}.{}", quote_mysql(database), quote_mysql(name));
    let mut conn = pool.get_conn().await.map_err(definition_failure)?;
    let row: Option<mysql_async::Row> = conn.query_first(sql).await.map_err(definition_failure)?;
    let row = row.ok_or_else(|| definition_failure("没有读到定义"))?;
    // SHOW CREATE SEQUENCE 的语句在索引 1；函数、过程和触发器在索引 2。
    // SHOW CREATE EVENT 的列名是 Create Event（不同版本列序会变），按列名定位，读不到时退回索引 3。
    let column = match statement {
        "SEQUENCE" => 1,
        "EVENT" => mysql_column_index(&row, "Create Event").unwrap_or(3),
        _ => 2,
    };
    mysql_create_sql(&row, column)
}

/// 按列名定位 SHOW CREATE 的语句列；列名大小写按服务器返回值。
fn mysql_column_index(row: &mysql_async::Row, label: &str) -> Option<usize> {
    row.columns_ref()
        .iter()
        .position(|column| column.name_str().eq_ignore_ascii_case(label))
}

fn quote_mysql(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

async fn read_mongo(client: &mongodb::Client, page: usize, windows: &[ObjectWindow]) -> Result<Vec<DatabaseNode>, String> {
    let mut names = client.list_database_names().await.map_err(read_failure)?;
    names.retain(|name| !crate::mongo::docs::catalog_hidden(name));
    names.sort();
    let mut databases = Vec::new();
    for name in names {
        let limit = requested_limit(&name, "", windows, page);
        databases.push(read_mongo_database(client, &name, 0, limit).await);
    }
    Ok(databases)
}

async fn read_mongo_outline(client: &mongodb::Client) -> Result<Vec<DatabaseNode>, String> {
    let mut names = client.list_database_names().await.map_err(read_failure)?;
    names.retain(|name| !crate::mongo::docs::catalog_hidden(name));
    names.sort();
    Ok(names
        .into_iter()
        .map(|name| DatabaseNode {
            name,
            error: None,
            schemas: vec![SchemaNode::empty("")],
        })
        .collect())
}

async fn read_mongo_page(client: &mongodb::Client, database: &str, offset: usize, limit: usize) -> ObjectPageBody {
    let schema = read_mongo_objects(client, database, offset, limit).await;
    ObjectPageBody {
        objects: schema.objects,
        total: schema.total,
        error: schema.error,
        kind_reports: schema.kind_reports,
    }
}

async fn read_mongo_database(client: &mongodb::Client, database: &str, offset: usize, limit: usize) -> DatabaseNode {
    let schema = read_mongo_objects(client, database, offset, limit).await;
    DatabaseNode {
        name: database.to_owned(),
        error: None,
        schemas: vec![schema],
    }
}

/// 集合清单 + estimatedDocumentCount 计数（树上的行估算）。计数失败不拖垮清单，只让估算留空。
async fn read_mongo_objects(client: &mongodb::Client, database: &str, offset: usize, limit: usize) -> SchemaNode {
    let mut names = match client.database(database).list_collection_names().await {
        Ok(names) => names,
        Err(message) => {
            let error = read_failure(message);
            return SchemaNode {
                name: String::new(),
                objects: Vec::new(),
                total: 0,
                error: Some(error),
                kind_reports: Vec::new(),
            };
        }
    };
    names.sort();
    let total = names.len();
    let window = offset.saturating_add(limit);
    let mut objects = Vec::new();
    for name in names.into_iter().take(window).skip(offset) {
        let estimated = client
            .database(database)
            .collection::<mongodb::bson::Document>(&name)
            .estimated_document_count()
            .await
            .ok()
            .and_then(|count| i64::try_from(count).ok());
        objects.push(ObjectRow {
            name,
            kind: ObjectKind::Table,
            estimated_rows: known_row_estimate(estimated),
            stale: false,
            signature: String::new(),
            relation: String::new(),
            materialized: false,
        });
    }
    SchemaNode {
        name: String::new(),
        objects,
        total,
        error: None,
        kind_reports: vec![KindReport {
            kind: ObjectKind::Table,
            total,
            error: None,
        }],
    }
}

fn mysql_create_sql(row: &mysql_async::Row, index: usize) -> Result<String, String> {
    let statement = mysql_string(row, index).ok();
    let mut columns = vec![None; index + 1];
    columns[index] = statement.as_deref();
    mysql_show_create_at(&columns, index)
}

/// SHOW CREATE FUNCTION、PROCEDURE、TRIGGER 的语句都在索引 2。
/// 索引 0 是对象名，名称里的 CREATE 不是定义。
#[allow(dead_code)]
fn mysql_show_create_statement(columns: &[Option<&str>]) -> Result<String, String> {
    mysql_show_create_at(columns, 2)
}

fn mysql_show_create_at(columns: &[Option<&str>], index: usize) -> Result<String, String> {
    let text = columns.get(index).and_then(|item| *item).unwrap_or("");
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
            let text = pg_string(row, 0).map_err(definition_failure)?;
            if text.trim().is_empty() {
                return Err(definition_failure("没有读到定义"));
            }
            Ok(text)
        }
        "sequence" => read_postgres_text(
            client,
            "SELECT format('CREATE SEQUENCE %I.%I AS %s INCREMENT BY %s MINVALUE %s MAXVALUE %s START WITH %s CACHE %s %s;', schemaname, sequencename, data_type, increment_by, min_value, max_value, start_value, cache_size, CASE WHEN cycle THEN 'CYCLE' ELSE 'NO CYCLE' END) FROM pg_catalog.pg_sequences WHERE schemaname = $1 AND sequencename = $2",
            schema,
            name,
        )
        .await,
        "enum" => read_postgres_text(
            client,
            "SELECT format('CREATE TYPE %I.%I AS ENUM (%s);', n.nspname, t.typname, COALESCE((SELECT string_agg(quote_literal(e.enumlabel), ', ' ORDER BY e.enumsortorder) FROM pg_enum e WHERE e.enumtypid = t.oid), '')) FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname = $1 AND t.typname = $2 AND t.typtype = 'e'",
            schema,
            name,
        )
        .await,
        "domain" => read_postgres_text(
            client,
            "SELECT format('CREATE DOMAIN %I.%I AS %s%s%s%s;', n.nspname, t.typname, pg_catalog.format_type(t.typbasetype, t.typtypmod), CASE WHEN t.typnotnull THEN ' NOT NULL' ELSE '' END, CASE WHEN t.typdefault IS NOT NULL THEN ' DEFAULT ' || t.typdefault ELSE '' END, COALESCE((SELECT ' ' || string_agg(pg_get_constraintdef(c.oid), ' ' ORDER BY c.oid) FROM pg_constraint c WHERE c.contypid = t.oid), '')) FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname = $1 AND t.typname = $2 AND t.typtype = 'd'",
            schema,
            name,
        )
        .await,
        "trigger" => {
            let sql = "SELECT pg_get_triggerdef(t.oid, true) FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND t.tgname = $2 AND NOT t.tgisinternal AND ($3 = '' OR c.relname = $3)";
            let rows = client.query(sql, &[&schema, &name, &relation]).await.map_err(definition_failure)?;
            if rows.len() > 1 {
                return Err(definition_failure("有多个同名触发器，需要所属的表才能区分"));
            }
            let row = rows.first().ok_or_else(|| definition_failure("没有读到定义"))?;
            let text = pg_string(row, 0).map_err(definition_failure)?;
            if text.trim().is_empty() {
                return Err(definition_failure("没有读到定义"));
            }
            Ok(text)
        }
        _ => Err(definition_failure("只有函数、存储过程和触发器可以读取只读定义")),
    }
}

async fn read_postgres_text(client: &tokio_postgres::Client, sql: &str, schema: &str, name: &str) -> Result<String, String> {
    let rows = client.query(sql, &[&schema, &name]).await.map_err(definition_failure)?;
    let row = rows.first().ok_or_else(|| definition_failure("没有读到定义"))?;
    let text = pg_string(row, 0).map_err(definition_failure)?;
    if text.trim().is_empty() {
        Err(definition_failure("没有读到定义"))
    } else {
        Ok(text)
    }
}

fn postgres_object(row: &tokio_postgres::Row) -> Result<ObjectRow, String> {
    let name = pg_string(row, 0)?;
    let kind = pg_string(row, 1)?;
    let estimated = pg_i64_opt(row, 2)?;
    let signature = pg_text(row, 3);
    let materialized = signature == "materialized";
    Ok(ObjectRow {
        name,
        kind: parse_kind(&kind),
        estimated_rows: known_row_estimate(estimated),
        stale: false,
        signature: if materialized { String::new() } else { signature },
        relation: pg_text(row, 4),
        materialized,
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
        materialized: false,
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
#[allow(clippy::items_after_test_module)]
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

        let sequence = [Some("orders_id_seq"), Some("CREATE SEQUENCE `orders_id_seq` START WITH 1")];
        let sequence_sql = super::mysql_show_create_at(&sequence, 1).expect("sequence column");
        assert!(sequence_sql.starts_with("CREATE SEQUENCE"));
        assert_ne!(sequence_sql, "orders_id_seq");
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

type TdsHandle = std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>;

/// 执行带参数的查询并收齐所有行。分类查询都不大，一次性收完即可。
async fn tds_query_rows(client: &TdsHandle, sql: &str, params: &[&dyn tiberius::ToSql]) -> Result<Vec<tiberius::Row>, String> {
    use futures::TryStreamExt;
    let mut client = client.lock().await;
    let mut stream = client.query(sql, params).await.map_err(read_failure)?;
    let mut rows = Vec::new();
    while let Some(item) = stream.try_next().await.map_err(|err| read_failure(err.to_string()))? {
        if let Some(row) = item.into_row() {
            rows.push(row);
        }
    }
    Ok(rows)
}

async fn tds_query_scalar(client: &TdsHandle, sql: &str, params: &[&dyn tiberius::ToSql]) -> Result<String, String> {
    let rows = tds_query_rows(client, sql, params).await?;
    let row = rows.into_iter().next().ok_or_else(|| read_failure("没有结果"))?;
    tds_text(&row, 0).ok_or_else(|| read_failure("元数据列是空值"))
}

fn tds_text(row: &tiberius::Row, index: usize) -> Option<String> {
    row.try_get::<&str, _>(index).ok().flatten().map(str::to_owned)
}

fn tds_i64(row: &tiberius::Row, index: usize) -> Option<i64> {
    if let Ok(Some(value)) = row.try_get::<i64, _>(index) {
        return Some(value);
    }
    if let Ok(Some(value)) = row.try_get::<i32, _>(index) {
        return Some(i64::from(value));
    }
    tds_text(row, index).and_then(|text| text.parse().ok())
}

async fn sqlserver_current_database(client: &TdsHandle) -> Result<String, String> {
    tds_query_scalar(client, "SELECT DB_NAME()", &[]).await
}

fn assemble_sqlserver(names: &[String], current: &str, schemas: Result<Vec<SchemaNode>, String>) -> Vec<DatabaseNode> {
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

async fn sqlserver_database_names(client: &TdsHandle) -> Result<(Vec<String>, String), String> {
    let sql = metadata_sql(Engine::Sqlserver);
    let rows = tds_query_rows(client, sql.databases, &[]).await?;
    let mut names = Vec::new();
    for row in &rows {
        names.push(tds_text(row, 0).ok_or_else(|| read_failure("元数据列是空值"))?);
    }
    let current = sqlserver_current_database(client).await?;
    if !names.iter().any(|name| name == &current) {
        names.push(current.clone());
        names.sort();
    }
    Ok((names, current))
}

async fn read_sqlserver(client: &TdsHandle, page: usize, windows: &[ObjectWindow]) -> Result<Vec<DatabaseNode>, String> {
    let (names, current) = sqlserver_database_names(client).await?;
    let schemas_sql = metadata_sql(Engine::Sqlserver)
        .schemas
        .ok_or_else(|| read_failure("SQL Server 缺少 schema 查询"))?;
    let schema_result = match tds_query_rows(client, schemas_sql, &[]).await {
        Ok(rows) => {
            let mut schemas = Vec::new();
            for row in &rows {
                let name = tds_text(row, 0).ok_or_else(|| read_failure("元数据列是空值"))?;
                let limit = requested_limit(&current, &name, windows, page);
                schemas.push(read_sqlserver_schema(client, &name, 0, limit).await);
            }
            Ok(schemas)
        }
        Err(message) => Err(message),
    };
    Ok(assemble_sqlserver(&names, &current, schema_result))
}

async fn read_sqlserver_outline(client: &TdsHandle) -> Result<Vec<DatabaseNode>, String> {
    let (names, current) = sqlserver_database_names(client).await?;
    let schemas_sql = metadata_sql(Engine::Sqlserver)
        .schemas
        .ok_or_else(|| read_failure("SQL Server 缺少 schema 查询"))?;
    let schema_result = match tds_query_rows(client, schemas_sql, &[]).await {
        Ok(rows) => {
            let mut schemas = Vec::new();
            for row in &rows {
                schemas.push(SchemaNode::empty(tds_text(row, 0).ok_or_else(|| read_failure("元数据列是空值"))?));
            }
            Ok(schemas)
        }
        Err(message) => Err(message),
    };
    Ok(assemble_sqlserver(&names, &current, schema_result))
}

async fn read_sqlserver_page(client: &TdsHandle, database: &str, schema: &str, offset: usize, limit: usize) -> ObjectPageBody {
    let current = match sqlserver_current_database(client).await {
        Ok(name) => name,
        Err(message) => return empty_page(message),
    };
    if database != current {
        return empty_page(other_database_message(&current, database));
    }
    let schema_node = read_sqlserver_schema(client, schema, offset, limit).await;
    ObjectPageBody {
        objects: schema_node.objects,
        total: schema_node.total,
        error: schema_node.error,
        kind_reports: schema_node.kind_reports,
    }
}

async fn read_sqlserver_schema(client: &TdsHandle, schema: &str, offset: usize, limit: usize) -> SchemaNode {
    let window = offset.saturating_add(limit);
    let mut parts = Vec::new();
    for kind in listed_kinds(Engine::Sqlserver) {
        parts.push((*kind, read_sqlserver_kind(client, schema, *kind, window).await));
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

async fn read_sqlserver_kind(client: &TdsHandle, schema: &str, kind: ObjectKind, window: usize) -> Result<(usize, Vec<ObjectRow>), String> {
    let sql = kind_sql(Engine::Sqlserver, kind);
    let count_rows = tds_query_rows(client, sql.count, &[&schema]).await?;
    let total = tds_i64(count_rows.first().ok_or_else(|| read_failure("没有对象数量"))?, 0).unwrap_or(0) as usize;
    let window_i = i64::try_from(window).unwrap_or(i64::MAX);
    let rows = tds_query_rows(client, sql.list, &[&schema, &window_i]).await?;
    let mut objects = Vec::new();
    for row in &rows {
        objects.push(sqlserver_object(row)?);
    }
    Ok((total, objects))
}

fn sqlserver_object(row: &tiberius::Row) -> Result<ObjectRow, String> {
    let name = tds_text(row, 0).ok_or_else(|| read_failure("元数据列是空值"))?;
    let kind = tds_text(row, 1).ok_or_else(|| read_failure("元数据列是空值"))?;
    Ok(ObjectRow {
        name,
        kind: parse_kind(&kind),
        estimated_rows: known_row_estimate(tds_i64(row, 2)),
        stale: false,
        signature: tds_text(row, 3).unwrap_or_default(),
        relation: tds_text(row, 4).unwrap_or_default(),
        materialized: false,
    })
}

/// SQL Server 的只读定义：函数/过程/触发器/视图走 OBJECT_DEFINITION，序列按 sys.sequences 重建。
/// sys.sequences 的定义读取：start/increment/min/max 是 sql_variant、is_* 是位类型，
/// 都在 SQL 侧投影成文本/0-1；读不到就报错，不静默编造 START 1 / NO CYCLE 之类的默认值。
pub(crate) const SQLSERVER_SEQUENCE_SQL: &str = "SELECT TYPE_NAME(q.user_type_id), CAST(q.start_value AS NVARCHAR(40)), CAST(q.increment AS NVARCHAR(40)), CAST(q.minimum_value AS NVARCHAR(40)), CAST(q.maximum_value AS NVARCHAR(40)), CASE WHEN q.is_cycling THEN N'1' ELSE N'0' END, CASE WHEN q.is_cached THEN N'1' ELSE N'0' END, CAST(q.cache_size AS NVARCHAR(40)) FROM sys.sequences q JOIN sys.schemas s ON s.schema_id = q.schema_id WHERE s.name = @P1 AND q.name = @P2";

async fn read_sqlserver_definition(client: &TdsHandle, schema: &str, name: &str, kind: &str) -> Result<String, String> {
    if schema.is_empty() {
        return Err(definition_failure("SQL Server 需要 schema"));
    }
    match kind {
        "function" | "procedure" | "trigger" | "view" => {
            let sql = "SELECT OBJECT_DEFINITION(o.object_id) FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = @P1 AND o.name = @P2";
            let rows = tds_query_rows(client, sql, &[&schema, &name]).await.map_err(definition_failure)?;
            if rows.len() > 1 {
                return Err(definition_failure("有多个同名对象，需要更具体的名称"));
            }
            let row = rows.first().ok_or_else(|| definition_failure("没有读到定义"))?;
            let text = tds_text(row, 0).ok_or_else(|| definition_failure("没有读到定义"))?;
            if text.trim().is_empty() {
                return Err(definition_failure("没有读到定义"));
            }
            Ok(text)
        }
        "sequence" => {
            let sql = SQLSERVER_SEQUENCE_SQL;
            let rows = tds_query_rows(client, sql, &[&schema, &name]).await.map_err(definition_failure)?;
            let row = rows.first().ok_or_else(|| definition_failure("没有读到定义"))?;
            let type_name = tds_text(row, 0).ok_or_else(|| definition_failure("没有读到定义"))?;
            let start = tds_text(row, 1).ok_or_else(|| definition_failure("没有读到序列起点"))?;
            let increment = tds_text(row, 2).ok_or_else(|| definition_failure("没有读到序列步长"))?;
            let minimum = tds_text(row, 3).ok_or_else(|| definition_failure("没有读到序列下限"))?;
            let maximum = tds_text(row, 4).ok_or_else(|| definition_failure("没有读到序列上限"))?;
            let cycling = tds_text(row, 5).map(|value| value == "1").unwrap_or(false);
            let cached = tds_text(row, 6).map(|value| value == "1").unwrap_or(false);
            let cache_size = tds_text(row, 7).unwrap_or_default();
            let cycle_text = if cycling { " CYCLE" } else { " NO CYCLE" };
            let cache_text = if cached && !cache_size.is_empty() {
                format!(" CACHE {cache_size}")
            } else if cached {
                " CACHE".to_owned()
            } else {
                " NO CACHE".to_owned()
            };
            let bounds = format!(
                "{}{}",
                if minimum.is_empty() { String::new() } else { format!(" MINVALUE {minimum}") },
                if maximum.is_empty() { String::new() } else { format!(" MAXVALUE {maximum}") },
            );
            Ok(format!(
                "CREATE SEQUENCE [{schema}].[{name}] AS {type_name} START WITH {start} INCREMENT BY {increment}{bounds}{cycle_text}{cache_text};"
            ))
        }
        "enum" | "domain" => Err(definition_failure("SQL Server 没有独立的枚举或域")),
        _ => Err(definition_failure("只有函数、存储过程、触发器、视图和序列可以读取只读定义")),
    }
}
