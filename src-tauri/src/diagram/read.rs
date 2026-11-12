//! 在已经打开的会话上读取关系图。这里不新开数据库连接，也不改会话要连的库。

use mysql_async::prelude::Queryable;
use sqlx::Row;

use crate::connections::model::Engine;
use crate::connections::CatalogSession;
use crate::diagram::assemble::{assemble_diagram, group_key_parts};
use crate::diagram::model::{
    empty_mysql_database, empty_postgres_schema, empty_sqlserver_schema, other_postgres_database, Diagram, DiagramColumn, KeyPart, OutgoingKey, RangeScope, ReadColumns, TableAttempt,
};
use crate::diagram::sql::{
    MYSQL_COLUMNS, MYSQL_FOREIGN_KEYS, MYSQL_TABLES, POSTGRES_COLUMNS, POSTGRES_CURRENT_DATABASE, POSTGRES_FOREIGN_KEYS, POSTGRES_TABLES,
    SQLITE_COLUMNS, SQLITE_FOREIGN_KEYS, SQLITE_TABLES, SQLSERVER_COLUMNS, SQLSERVER_FOREIGN_KEYS, SQLSERVER_TABLES,
};
use crate::grid::{sqlserver_rows, Cell};

pub(crate) async fn read_diagram(engine: Engine, session: &CatalogSession, database: &str, schema: &str) -> Result<Diagram, String> {
    if engine == Engine::Postgres {
        if let Some(message) = empty_postgres_schema(schema) {
            return Err(message);
        }
    }
    if matches!(engine, Engine::Mysql | Engine::Mariadb) {
        if let Some(message) = empty_mysql_database(database) {
            return Err(message);
        }
    }
    if engine == Engine::Sqlserver {
        if let Some(message) = empty_sqlserver_schema(schema) {
            return Err(message);
        }
    }
    match (engine, session) {
        (Engine::Postgres, CatalogSession::Postgres(client)) => read_postgres(client, database, schema).await,
        (Engine::Mysql | Engine::Mariadb, CatalogSession::Mysql(pool)) => read_mysql(pool, database).await,
        (Engine::Sqlite, CatalogSession::Sqlite { pool, .. }) => read_sqlite(pool).await,
        (Engine::Sqlserver, CatalogSession::Sqlserver(client)) => read_sqlserver(client, database, schema).await,
        (_, CatalogSession::Scripted) => Err("这个连接没有连着可查询的数据库".to_owned()),
        _ => Err("连接类型和引擎不一致".to_owned()),
    }
}

fn failure(err: impl std::fmt::Display) -> String {
    format!("读取关系图失败：{err}")
}

async fn read_postgres(client: &tokio_postgres::Client, database: &str, schema: &str) -> Result<Diagram, String> {
    let current = current_database(client).await?;
    if let Some(message) = other_postgres_database(&current, database) {
        return Err(message);
    }
    let names = postgres_names(client, schema).await?;
    let mut attempts = Vec::with_capacity(names.len());
    for name in names {
        attempts.push(postgres_table(client, schema, name).await);
    }
    Ok(assemble_diagram(&RangeScope { database: database.to_owned(), schema: schema.to_owned(), fold_names: false }, attempts))
}

async fn current_database(client: &tokio_postgres::Client) -> Result<String, String> {
    let rows = client.query(POSTGRES_CURRENT_DATABASE, &[]).await.map_err(failure)?;
    let row = rows.first().ok_or_else(|| failure("没有当前数据库"))?;
    row.try_get(0).map_err(failure)
}

async fn postgres_names(client: &tokio_postgres::Client, schema: &str) -> Result<Vec<String>, String> {
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 1] = [&schema];
    let rows = client.query(POSTGRES_TABLES, &params).await.map_err(failure)?;
    let mut names = Vec::with_capacity(rows.len());
    for row in &rows {
        names.push(row.try_get(0).map_err(failure)?);
    }
    Ok(names)
}

async fn postgres_table(client: &tokio_postgres::Client, schema: &str, name: String) -> TableAttempt {
    let column_params: [&(dyn tokio_postgres::types::ToSql + Sync); 2] = [&schema, &name];
    let columns = match client.query(POSTGRES_COLUMNS, &column_params).await {
        Ok(rows) => postgres_columns(&rows),
        Err(err) => Err(err.to_string()),
    };
    if columns.is_err() {
        return TableAttempt { name, columns, foreign_keys: Ok(Vec::new()) };
    }
    let key_params: [&(dyn tokio_postgres::types::ToSql + Sync); 2] = [&schema, &name];
    let foreign_keys = match client.query(POSTGRES_FOREIGN_KEYS, &key_params).await {
        Ok(rows) => postgres_keys(&rows),
        Err(err) => Err(err.to_string()),
    };
    TableAttempt { name, columns, foreign_keys }
}

fn postgres_columns(rows: &[tokio_postgres::Row]) -> Result<ReadColumns, String> {
    let mut columns = Vec::with_capacity(rows.len());
    for row in rows {
        let type_name: Option<String> = row.try_get(1).map_err(|err| err.to_string())?;
        columns.push(DiagramColumn {
            name: row.try_get(0).map_err(|err| err.to_string())?,
            type_name: type_name.unwrap_or_default(),
            primary: row.try_get(2).map_err(|err| err.to_string())?,
        });
    }
    Ok(ReadColumns(columns))
}

fn postgres_keys(rows: &[tokio_postgres::Row]) -> Result<Vec<OutgoingKey>, String> {
    let mut parts = Vec::with_capacity(rows.len());
    for row in rows {
        parts.push(KeyPart {
            name: row.try_get(0).map_err(|err| err.to_string())?,
            column: row.try_get(1).map_err(|err| err.to_string())?,
            ref_schema: row.try_get(2).map_err(|err| err.to_string())?,
            ref_table: row.try_get(3).map_err(|err| err.to_string())?,
            ref_column: row.try_get(4).map_err(|err| err.to_string())?,
            ordinal: row.try_get(5).map_err(|err| err.to_string())?,
        });
    }
    Ok(group_key_parts(parts))
}

async fn read_mysql(pool: &mysql_async::Pool, database: &str) -> Result<Diagram, String> {
    let names = mysql_names(pool, database).await?;
    let mut attempts = Vec::with_capacity(names.len());
    for name in names {
        attempts.push(mysql_table(pool, database, name).await);
    }
    Ok(assemble_diagram(
        &RangeScope {
            database: database.to_owned(),
            schema: String::new(),
            fold_names: true,
        },
        attempts,
    ))
}

async fn mysql_names(pool: &mysql_async::Pool, database: &str) -> Result<Vec<String>, String> {
    let mut conn = pool.get_conn().await.map_err(failure)?;
    let rows: Vec<mysql_async::Row> = conn.exec(MYSQL_TABLES, (database.to_owned(),)).await.map_err(failure)?;
    let mut names = Vec::with_capacity(rows.len());
    for row in &rows {
        names.push(mysql_string(row, 0).map_err(failure)?);
    }
    Ok(names)
}

async fn mysql_table(pool: &mysql_async::Pool, database: &str, name: String) -> TableAttempt {
    let mut conn = match pool.get_conn().await {
        Ok(conn) => conn,
        Err(err) => {
            return TableAttempt {
                name,
                columns: Err(err.to_string()),
                foreign_keys: Ok(Vec::new()),
            };
        }
    };
    let columns = match conn.exec(MYSQL_COLUMNS, (database.to_owned(), name.clone())).await {
        Ok(rows) => mysql_columns(&rows),
        Err(err) => Err(err.to_string()),
    };
    if columns.is_err() {
        return TableAttempt { name, columns, foreign_keys: Ok(Vec::new()) };
    }
    let foreign_keys = match conn.exec(MYSQL_FOREIGN_KEYS, (database.to_owned(), name.clone())).await {
        Ok(rows) => mysql_keys(&rows),
        Err(err) => Err(err.to_string()),
    };
    TableAttempt { name, columns, foreign_keys }
}

fn mysql_columns(rows: &[mysql_async::Row]) -> Result<ReadColumns, String> {
    let mut columns = Vec::with_capacity(rows.len());
    for row in rows {
        let key = mysql_string(row, 2).unwrap_or_default();
        columns.push(DiagramColumn {
            name: mysql_string(row, 0)?,
            type_name: mysql_string(row, 1).unwrap_or_default(),
            primary: key.eq_ignore_ascii_case("PRI"),
        });
    }
    Ok(ReadColumns(columns))
}

fn mysql_keys(rows: &[mysql_async::Row]) -> Result<Vec<OutgoingKey>, String> {
    let mut parts = Vec::with_capacity(rows.len());
    for row in rows {
        parts.push(KeyPart {
            name: mysql_string(row, 0)?,
            column: mysql_string(row, 1)?,
            ref_schema: mysql_string(row, 2).unwrap_or_default(),
            ref_table: mysql_string(row, 3)?,
            ref_column: mysql_string(row, 4)?,
            ordinal: mysql_i64(row, 5).unwrap_or(0),
        });
    }
    Ok(group_key_parts(parts))
}

fn mysql_string(row: &mysql_async::Row, index: usize) -> Result<String, String> {
    let value = row.as_ref(index).ok_or_else(|| "元数据列缺失".to_owned())?;
    match value {
        mysql_async::Value::Bytes(bytes) => String::from_utf8(bytes.clone()).map_err(|err| err.to_string()),
        mysql_async::Value::NULL => Err("元数据列是空值".to_owned()),
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

async fn read_sqlserver(client: &std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>, database: &str, schema: &str) -> Result<Diagram, String> {
    let (_names, rows) = sqlserver_rows(client, "SELECT DB_NAME()").await.map_err(failure)?;
    let current = cell_string(rows.first().and_then(|row| row.first()));
    if current != database {
        return Err(format!("当前会话连在 {current}，没有另开连接读取 {database}"));
    }
    let schema = schema.trim();
    let names = sqlserver_names(client, schema).await?;
    let mut attempts = Vec::with_capacity(names.len());
    for name in names {
        attempts.push(sqlserver_table(client, schema, name).await);
    }
    Ok(assemble_diagram(
        &RangeScope {
            database: database.to_owned(),
            schema: schema.to_owned(),
            fold_names: true,
        },
        attempts,
    ))
}

async fn sqlserver_names(client: &std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>, schema: &str) -> Result<Vec<String>, String> {
    let sql = SQLSERVER_TABLES.replace("{schema}", &tsql_literal(schema));
    let (_names, rows) = sqlserver_rows(client, &sql).await.map_err(failure)?;
    Ok(rows.iter().map(|row| cell_string(row.first())).filter(|name| !name.is_empty()).collect())
}

async fn sqlserver_table(client: &std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>, schema: &str, name: String) -> TableAttempt {
    let columns_sql = SQLSERVER_COLUMNS.replace("{schema}", &tsql_literal(schema)).replace("{table}", &tsql_literal(&name));
    let columns = match sqlserver_rows(client, &columns_sql).await {
        Ok((_names, rows)) => Ok(sqlserver_columns(&rows)),
        Err(err) => Err(err),
    };
    if columns.is_err() {
        return TableAttempt { name, columns, foreign_keys: Ok(Vec::new()) };
    }
    let keys_sql = SQLSERVER_FOREIGN_KEYS.replace("{schema}", &tsql_literal(schema)).replace("{table}", &tsql_literal(&name));
    let foreign_keys = match sqlserver_rows(client, &keys_sql).await {
        Ok((_names, rows)) => sqlserver_keys(&rows),
        Err(err) => Err(err),
    };
    TableAttempt { name, columns, foreign_keys }
}

fn sqlserver_keys(rows: &[Vec<Cell>]) -> Result<Vec<OutgoingKey>, String> {
    let mut parts = Vec::with_capacity(rows.len());
    for row in rows {
        let ordinal = cell_string(row.get(5)).parse::<i64>().unwrap_or(0);
        parts.push(KeyPart {
            name: cell_string(row.first()),
            column: cell_string(row.get(1)),
            ref_schema: cell_string(row.get(2)),
            ref_table: cell_string(row.get(3)),
            ref_column: cell_string(row.get(4)),
            ordinal,
        });
    }
    Ok(group_key_parts(parts))
}

fn sqlserver_columns(rows: &[Vec<Cell>]) -> ReadColumns {
    let mut columns = Vec::with_capacity(rows.len());
    for row in rows {
        let name = cell_string(row.first());
        if name.is_empty() {
            continue;
        }
        let flag = cell_string(row.get(2));
        columns.push(DiagramColumn {
            name,
            type_name: cell_string(row.get(1)),
            primary: flag == "1" || flag.eq_ignore_ascii_case("true"),
        });
    }
    ReadColumns(columns)
}

fn cell_string(cell: Option<&Cell>) -> String {
    match cell {
        Some(Cell::Text(text)) => text.clone(),
        _ => String::new(),
    }
}

fn tsql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

async fn read_sqlite(pool: &sqlx::SqlitePool) -> Result<Diagram, String> {
    let names: Vec<String> = sqlx::query_scalar(SQLITE_TABLES).fetch_all(pool).await.map_err(failure)?;
    let mut attempts = Vec::with_capacity(names.len());
    for name in names {
        attempts.push(sqlite_table(pool, name).await);
    }
    Ok(assemble_diagram(
        &RangeScope {
            database: "main".to_owned(),
            schema: String::new(),
            fold_names: true,
        },
        attempts,
    ))
}

async fn sqlite_table(pool: &sqlx::SqlitePool, name: String) -> TableAttempt {
    let columns = match sqlx::query(SQLITE_COLUMNS).bind(&name).fetch_all(pool).await {
        Ok(rows) => sqlite_columns(&rows),
        Err(err) => Err(err.to_string()),
    };
    if columns.is_err() {
        return TableAttempt { name, columns, foreign_keys: Ok(Vec::new()) };
    }
    let foreign_keys = match sqlx::query(SQLITE_FOREIGN_KEYS).bind(&name).fetch_all(pool).await {
        Ok(rows) => sqlite_keys(&rows),
        Err(err) => Err(err.to_string()),
    };
    TableAttempt { name, columns, foreign_keys }
}

fn sqlite_columns(rows: &[sqlx::sqlite::SqliteRow]) -> Result<ReadColumns, String> {
    let mut columns = Vec::with_capacity(rows.len());
    for row in rows {
        let name: String = row.try_get(0).map_err(|err| err.to_string())?;
        let type_name: String = row.try_get::<Option<String>, _>(1).map_err(|err| err.to_string())?.unwrap_or_default();
        let primary = sqlite_pk(&row)?;
        columns.push(DiagramColumn { name, type_name, primary });
    }
    Ok(ReadColumns(columns))
}

fn sqlite_pk(row: &sqlx::sqlite::SqliteRow) -> Result<bool, String> {
    if let Ok(value) = row.try_get::<i64, _>(2) {
        return Ok(value > 0);
    }
    if let Ok(value) = row.try_get::<i32, _>(2) {
        return Ok(value > 0);
    }
    let text: Option<String> = row.try_get(2).map_err(|err| err.to_string())?;
    Ok(text.as_deref().is_some_and(|value| value != "0" && !value.is_empty()))
}

fn sqlite_keys(rows: &[sqlx::sqlite::SqliteRow]) -> Result<Vec<OutgoingKey>, String> {
    let mut parts = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.try_get(0).map_err(|err| err.to_string())?;
        let seq: i64 = row.try_get(1).map_err(|err| err.to_string())?;
        let (ref_schema, ref_table) = split_sqlite_target(&sqlite_text(row, 3)?);
        parts.push(KeyPart {
            name: id.to_string(),
            column: sqlite_text(row, 2)?,
            ref_schema,
            ref_table,
            ref_column: sqlite_text(row, 4)?,
            ordinal: seq,
        });
    }
    let mut keys = group_key_parts(parts);
    for key in &mut keys {
        key.name.clear();
    }
    Ok(keys)
}

fn sqlite_text(row: &sqlx::sqlite::SqliteRow, index: usize) -> Result<String, String> {
    let value: Result<String, _> = row.try_get(index);
    if let Ok(value) = value {
        return Ok(value);
    }
    let optional: Result<Option<String>, _> = row.try_get(index);
    optional.map(|value| value.unwrap_or_default()).map_err(|err| err.to_string())
}

/// `main.customers` 仍在当前文件里。其他前缀是附加库，不在这张图的范围。
fn split_sqlite_target(raw: &str) -> (String, String) {
    match raw.rsplit_once('.') {
        Some((schema, table)) if schema.eq_ignore_ascii_case("main") || schema.eq_ignore_ascii_case("temp") => (String::new(), table.to_owned()),
        Some((schema, table)) => (schema.to_owned(), table.to_owned()),
        None => (String::new(), raw.to_owned()),
    }
}
