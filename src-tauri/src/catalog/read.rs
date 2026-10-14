//! 在已经打开的会话上读取库表。这里不新开数据库连接。

use mysql_async::prelude::Queryable;

use crate::catalog::model::{
    known_row_estimate, metadata_sql, object_count_sql, DatabaseNode, Engine, ObjectKind, ObjectRow, SchemaNode,
    DEFAULT_OBJECT_PAGE,
};
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

fn scripted_message() -> String {
    read_failure("这个连接没有连着可查询的数据库")
}

pub(crate) fn other_database_message(current: &str, name: &str) -> String {
    format!("当前会话连在 {current}，没有另开连接读取 {name}")
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

pub async fn read_object_page(session: &CatalogSession, database: &str, schema: &str, offset: usize, limit: usize) -> ObjectPageBody {
    let limit = read_limit(limit, 0);
    match session {
        CatalogSession::Scripted => ObjectPageBody {
            objects: Vec::new(),
            total: 0,
            error: Some(scripted_message()),
        },
        CatalogSession::Sqlite { pool, .. } => match read_sqlite_objects(pool, offset, limit).await {
            Ok((objects, total)) => ObjectPageBody {
                objects,
                total,
                error: None,
            },
            Err(message) => ObjectPageBody {
                objects: Vec::new(),
                total: 0,
                error: Some(message),
            },
        },
        CatalogSession::Mysql(pool) => read_mysql_page(pool, database, offset, limit).await,
        CatalogSession::Postgres(client) => read_postgres_page(client, database, schema, offset, limit).await,
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

async fn read_sqlite(pool: &sqlx::SqlitePool, foreign_keys: Option<bool>, page: usize, windows: &[ObjectWindow]) -> ConnectionBody {
    let limit = requested_limit("main", "", windows, page);
    match read_sqlite_objects(pool, 0, limit).await {
        Ok((objects, total)) => ConnectionBody {
            error: None,
            foreign_keys,
            databases: vec![DatabaseNode {
                name: "main".to_owned(),
                error: None,
                schemas: vec![SchemaNode {
                    name: String::new(),
                    objects,
                    total,
                    error: None,
                }],
            }],
        },
        Err(message) => ConnectionBody {
            error: Some(message),
            foreign_keys,
            databases: Vec::new(),
        },
    }
}

async fn read_sqlite_objects(pool: &sqlx::SqlitePool, offset: usize, limit: usize) -> Result<(Vec<ObjectRow>, usize), String> {
    let sql = metadata_sql(Engine::Sqlite);
    let total: i64 = sqlx::query_scalar(object_count_sql(Engine::Sqlite))
        .fetch_one(pool)
        .await
        .map_err(read_failure)?;
    let total = usize::try_from(total).map_err(|_| read_failure("对象数量无效"))?;
    let rows: Vec<(String, String)> = sqlx::query_as(sql.objects)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .bind(i64::try_from(offset).unwrap_or(i64::MAX))
        .fetch_all(pool)
        .await
        .map_err(read_failure)?;
    let objects = rows
        .into_iter()
        .map(|(name, kind)| ObjectRow {
            kind: object_kind(&kind),
            name,
            estimated_rows: None,
            stale: false,
        })
        .collect();
    Ok((objects, total))
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

async fn read_mysql_page(pool: &mysql_async::Pool, database: &str, offset: usize, limit: usize) -> ObjectPageBody {
    let mut conn = match pool.get_conn().await {
        Ok(conn) => conn,
        Err(err) => {
            return ObjectPageBody {
                objects: Vec::new(),
                total: 0,
                error: Some(read_failure(err)),
            }
        }
    };
    match read_mysql_objects(&mut conn, database, offset, limit).await {
        Ok((objects, total)) => ObjectPageBody {
            objects,
            total,
            error: None,
        },
        Err(message) => ObjectPageBody {
            objects: Vec::new(),
            total: 0,
            error: Some(message),
        },
    }
}

async fn read_mysql_database(conn: &mut mysql_async::Conn, database: &str, offset: usize, limit: usize) -> DatabaseNode {
    match read_mysql_objects(conn, database, offset, limit).await {
        Ok((objects, total)) => DatabaseNode {
            name: database.to_owned(),
            error: None,
            schemas: vec![SchemaNode {
                name: String::new(),
                objects,
                total,
                error: None,
            }],
        },
        Err(message) => DatabaseNode {
            name: database.to_owned(),
            schemas: Vec::new(),
            error: Some(message),
        },
    }
}

async fn read_mysql_objects(conn: &mut mysql_async::Conn, database: &str, offset: usize, limit: usize) -> Result<(Vec<ObjectRow>, usize), String> {
    let engine = Engine::Mysql;
    let sql = metadata_sql(engine);
    let count_row: Option<mysql_async::Row> = conn
        .exec_first(object_count_sql(engine), (database.to_owned(),))
        .await
        .map_err(read_failure)?;
    let total = match count_row.as_ref().and_then(|row| mysql_i64(row, 0)) {
        Some(value) => usize::try_from(value).map_err(|_| read_failure("对象数量无效"))?,
        None => return Err(read_failure("没有对象数量")),
    };
    let rows: Vec<mysql_async::Row> = conn
        .exec(
            sql.objects,
            (
                database.to_owned(),
                i64::try_from(limit).unwrap_or(i64::MAX),
                i64::try_from(offset).unwrap_or(i64::MAX),
            ),
        )
        .await
        .map_err(read_failure)?;
    let mut objects = Vec::new();
    for row in &rows {
        objects.push(mysql_object(row)?);
    }
    Ok((objects, total))
}

async fn read_postgres(client: &tokio_postgres::Client, page: usize, windows: &[ObjectWindow]) -> Result<Vec<DatabaseNode>, String> {
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
    let schemas_sql = sql.schemas.ok_or_else(|| read_failure("PostgreSQL 缺少 schema 查询"))?;
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

async fn read_postgres_page(client: &tokio_postgres::Client, database: &str, schema: &str, offset: usize, limit: usize) -> ObjectPageBody {
    let current = match current_database(client).await {
        Ok(name) => name,
        Err(message) => {
            return ObjectPageBody {
                objects: Vec::new(),
                total: 0,
                error: Some(message),
            }
        }
    };
    if database != current {
        return ObjectPageBody {
            objects: Vec::new(),
            total: 0,
            error: Some(other_database_message(&current, database)),
        };
    }
    let schema_node = read_postgres_schema(client, schema, offset, limit).await;
    match schema_node.error {
        Some(message) => ObjectPageBody {
            objects: Vec::new(),
            total: 0,
            error: Some(message),
        },
        None => ObjectPageBody {
            objects: schema_node.objects,
            total: schema_node.total,
            error: None,
        },
    }
}

async fn current_database(client: &tokio_postgres::Client) -> Result<String, String> {
    let rows = client.query("SELECT current_database()", &[]).await.map_err(read_failure)?;
    let row = rows.first().ok_or_else(|| read_failure("没有当前数据库"))?;
    pg_string(row, 0)
}

async fn read_postgres_schema(client: &tokio_postgres::Client, schema: &str, offset: usize, limit: usize) -> SchemaNode {
    let count_params: [&(dyn tokio_postgres::types::ToSql + Sync); 1] = [&schema];
    let count_rows = match client.query(object_count_sql(Engine::Postgres), &count_params).await {
        Ok(rows) => rows,
        Err(err) => return SchemaNode::failed(schema, read_failure(err)),
    };
    let total = match count_rows.first().map(|row| pg_i64(row, 0)) {
        Some(Ok(value)) => match usize::try_from(value) {
            Ok(total) => total,
            Err(_) => return SchemaNode::failed(schema, read_failure("对象数量无效")),
        },
        Some(Err(message)) => return SchemaNode::failed(schema, message),
        None => return SchemaNode::failed(schema, read_failure("没有对象数量")),
    };
    let limit_i = i64::try_from(limit).unwrap_or(i64::MAX);
    let offset_i = i64::try_from(offset).unwrap_or(i64::MAX);
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 3] = [&schema, &limit_i, &offset_i];
    let rows = match client.query(metadata_sql(Engine::Postgres).objects, &params).await {
        Ok(rows) => rows,
        Err(err) => return SchemaNode::failed(schema, read_failure(err)),
    };
    let mut objects = Vec::new();
    for row in &rows {
        match postgres_object(row) {
            Ok(object) => objects.push(object),
            Err(message) => return SchemaNode::failed(schema, message),
        }
    }
    SchemaNode {
        name: schema.to_owned(),
        objects,
        total,
        error: None,
    }
}

fn postgres_object(row: &tokio_postgres::Row) -> Result<ObjectRow, String> {
    let name = pg_string(row, 0)?;
    let kind = pg_string(row, 1)?;
    let estimated = pg_i64(row, 2)?;
    Ok(ObjectRow {
        name,
        kind: object_kind(&kind),
        estimated_rows: known_row_estimate(Some(estimated)),
        stale: false,
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

fn pg_i64(row: &tokio_postgres::Row, index: usize) -> Result<i64, String> {
    row.try_get(index).map_err(read_failure)
}

fn mysql_object(row: &mysql_async::Row) -> Result<ObjectRow, String> {
    Ok(ObjectRow {
        name: mysql_string(row, 0)?,
        kind: object_kind(&mysql_string(row, 1)?),
        estimated_rows: known_row_estimate(mysql_i64(row, 2)),
        stale: false,
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

fn object_kind(kind: &str) -> ObjectKind {
    if kind.eq_ignore_ascii_case("view") {
        ObjectKind::View
    } else {
        ObjectKind::Table
    }
}
