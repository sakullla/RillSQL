//! MySQL 家族的结构内省与执行：information_schema 与 SHOW CREATE 读表结构，逐条执行（每条结构语句立即提交）。

use crate::adapters::quote_ident;
use crate::connections::model::Engine;
use crate::ddl::exec::receipt;
use crate::ddl::introspect::keep_rows;
use crate::ddl::model::{Column, ForeignKey, IncomingForeignKey, Index, IndexColumn, Snapshot, Step, StepReceipt, StepStatus};
use crate::ddl::sql::qualified_table;

/// 改名前查新名字：MySQL 看同库的表与视图（名字逐字节比较，排除被改名的表自身）。
pub(super) async fn name_taken(pool: &mysql_async::Pool, database: &str, current: &str, name: &str) -> Result<bool, String> {
    use mysql_async::prelude::Queryable;
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    let count: Option<i64> = conn
        .exec_first(
            "SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND BINARY TABLE_NAME <> BINARY ?",
            (database, name, current),
        )
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    Ok(count.unwrap_or(0) > 0)
}

pub(super) async fn count(pool: &mysql_async::Pool, sql: &str) -> Result<i64, String> {
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    let row: Option<mysql_async::Row> =
        mysql_async::prelude::Queryable::query_first(&mut conn, sql.to_owned()).await.map_err(|err| crate::error_text::chain(&err))?;
    let row = row.ok_or_else(|| "没有读到 NULL 计数".to_owned())?;
    Ok(mysql_i64(&row, 0))
}

pub(super) async fn mysql_event_enabled(pool: &mysql_async::Pool, database: &str, event: &str) -> Result<Option<bool>, String> {
    if database.is_empty() {
        return Err("需要数据库名，没有另开连接。".to_owned());
    }
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    let row: Option<mysql_async::Row> = mysql_async::prelude::Queryable::exec_first(
        &mut conn,
        "SELECT STATUS FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ? AND EVENT_NAME = ?",
        (database.to_owned(), event.to_owned()),
    )
    .await
    .map_err(|err| format!("读不到事件 {event}：{err}"))?;
    let Some(row) = row else {
        return Err(format!("找不到事件 {database}.{event}。"));
    };
    Ok(Some(mysql_string(&row, 0).eq_ignore_ascii_case("ENABLED")))
}

pub(super) async fn load_mysql(pool: &mysql_async::Pool, engine: Engine, database: &str, table: &str) -> Result<Snapshot, String> {
    if database.is_empty() {
        return Err("需要数据库名，没有另开连接。".to_owned());
    }
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    let kind: Option<mysql_async::Row> = mysql_async::prelude::Queryable::exec_first(
        &mut conn,
        "SELECT TABLE_TYPE, ENGINE FROM information_schema.tables WHERE table_schema = ? AND table_name = ?",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(|err| crate::error_text::chain(&err))?;
    let Some(kind_row) = kind else {
        return Err(format!("找不到表或视图 {database}.{table}"));
    };
    let mut snapshot = Snapshot::bare(engine, database, database, table);
    snapshot.kind = if mysql_string(&kind_row, 0).eq_ignore_ascii_case("VIEW") { "view" } else { "table" }.to_owned();
    snapshot.storage = Some(mysql_string(&kind_row, 1));
    let columns: Vec<mysql_async::Row> = mysql_async::prelude::Queryable::exec(
        &mut conn,
        "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, EXTRA, GENERATION_EXPRESSION FROM information_schema.columns WHERE table_schema = ? AND table_name = ? ORDER BY ORDINAL_POSITION",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(|err| crate::error_text::chain(&err))?;
    for row in &columns {
        let extra = mysql_string(row, 4).to_ascii_lowercase();
        let generated = !mysql_string(row, 5).is_empty() || extra.contains("generated");
        snapshot.columns.push(Column {
            name: mysql_string(row, 0),
            type_name: mysql_string(row, 1),
            nullable: mysql_string(row, 2).eq_ignore_ascii_case("YES"),
            default_sql: {
                let value = mysql_string(row, 3);
                if value.is_empty() { None } else { Some(value) }
            },
            primary: false,
            auto: extra.contains("auto_increment"),
            generated,
            default_constraint: None,
        });
    }
    let indexes: Vec<mysql_async::Row> = keep_rows(
        &mut snapshot.index_error,
        mysql_async::prelude::Queryable::exec(
            &mut conn,
            "SELECT INDEX_NAME, NON_UNIQUE, COLUMN_NAME, COLLATION FROM information_schema.statistics WHERE table_schema = ? AND table_name = ? ORDER BY INDEX_NAME, SEQ_IN_INDEX",
            (database.to_owned(), table.to_owned()),
        )
        .await
        .map_err(|err| crate::error_text::chain(&err)),
    );
    for row in &indexes {
        let name = mysql_string(row, 0);
        let column = IndexColumn { name: mysql_string(row, 2), descending: mysql_string(row, 3).eq_ignore_ascii_case("D") };
        if let Some(index) = snapshot.indexes.iter_mut().find(|index| index.name == name) {
            index.columns.push(column);
        } else {
            let primary = name.eq_ignore_ascii_case("PRIMARY");
            snapshot.indexes.push(Index {
                name,
                unique: mysql_string(row, 1) == "0",
                columns: vec![column],
                protected: primary,
                reason: if primary { "由主键约束维护".to_owned() } else { String::new() },
                partial: false,
                expression: false,
                sql: None,
            });
        }
    }
    for column in &mut snapshot.columns {
        column.primary = snapshot.indexes.iter().any(|index| index.name.eq_ignore_ascii_case("PRIMARY") && index.columns.iter().any(|item| item.name == column.name));
    }
    let foreign_keys: Vec<mysql_async::Row> = keep_rows(
        &mut snapshot.foreign_key_error,
        mysql_async::prelude::Queryable::exec(
            &mut conn,
            "SELECT k.CONSTRAINT_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_NAME, k.REFERENCED_COLUMN_NAME, r.DELETE_RULE, r.UPDATE_RULE FROM information_schema.KEY_COLUMN_USAGE k JOIN information_schema.REFERENTIAL_CONSTRAINTS r ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME WHERE k.TABLE_SCHEMA = ? AND k.TABLE_NAME = ? AND k.REFERENCED_TABLE_NAME IS NOT NULL ORDER BY k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
            (database.to_owned(), table.to_owned()),
        )
        .await
        .map_err(|err| crate::error_text::chain(&err)),
    );
    if snapshot.foreign_key_error.is_none() {
        push_mysql_fks(&mut snapshot.foreign_keys, &foreign_keys, false);
    }
    for key in &snapshot.foreign_keys {
        if let Some(index) = snapshot.indexes.iter_mut().find(|index| index.name == key.name) {
            index.protected = true;
            index.reason = format!("外键 {} 依赖此索引", key.name);
        }
    }
    let incoming: Vec<mysql_async::Row> = keep_rows(
        &mut snapshot.foreign_key_error,
        mysql_async::prelude::Queryable::exec(
            &mut conn,
            "SELECT k.CONSTRAINT_NAME, k.TABLE_NAME, k.COLUMN_NAME, k.REFERENCED_COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE k WHERE k.REFERENCED_TABLE_SCHEMA = ? AND k.REFERENCED_TABLE_NAME = ? AND k.REFERENCED_COLUMN_NAME IS NOT NULL ORDER BY k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
            (database.to_owned(), table.to_owned()),
        )
        .await
        .map_err(|err| crate::error_text::chain(&err)),
    );
    let mut incoming_keys = Vec::new();
    for row in &incoming {
        let name = mysql_string(row, 0);
        let child = mysql_string(row, 1);
        let column = mysql_string(row, 2);
        let referenced = mysql_string(row, 3);
        if let Some(key) = incoming_keys.iter_mut().find(|key: &&mut IncomingForeignKey| key.name == name && key.table == child) {
            key.columns.push(column);
            key.ref_columns.push(referenced);
        } else {
            incoming_keys.push(IncomingForeignKey { name, table: child, schema: database.to_owned(), columns: vec![column], ref_columns: vec![referenced] });
        }
    }
    snapshot.incoming = incoming_keys;
    snapshot.row_count = Some(count_nulls_mysql_count(&mut conn, database, table).await?);
    snapshot.object_exists = true;
    if snapshot.kind == "view" {
        match mysql_view_definition(&mut conn, database, table).await {
            Ok(sql) => snapshot.table_sql = sql,
            Err(message) => {
                snapshot.table_sql.clear();
                snapshot.definition_error = Some(message);
            }
        }
    } else {
        // 表的 DDL 用服务器自己的 SHOW CREATE TABLE 原文；读不到时退回占位，DDL 页签改用重建定义。
        snapshot.table_sql = match mysql_table_definition(&mut conn, database, table).await {
            Ok(sql) => sql,
            Err(_) => format!("-- {}.{table}", database),
        };
    }
    Ok(snapshot)
}

async fn mysql_table_definition(conn: &mut mysql_async::Conn, database: &str, table: &str) -> Result<String, String> {
    let sql = format!(
        "SHOW CREATE TABLE {}.{}",
        quote_ident(Engine::Mysql, database),
        quote_ident(Engine::Mysql, table)
    );
    let row: Option<mysql_async::Row> = mysql_async::prelude::Queryable::query_first(conn, sql).await.map_err(|err| crate::error_text::chain(&err))?;
    let row = row.ok_or_else(|| "没有读到建表语句".to_owned())?;
    let text = mysql_string(&row, 1);
    if !text.trim_start().to_ascii_uppercase().starts_with("CREATE") {
        return Err("没有读到建表语句".to_owned());
    }
    Ok(format!("{};", text.trim_end().trim_end_matches(';')))
}

async fn mysql_view_definition(conn: &mut mysql_async::Conn, database: &str, table: &str) -> Result<String, String> {
    let sql = format!(
        "SHOW CREATE VIEW {}.{}",
        quote_ident(Engine::Mysql, database),
        quote_ident(Engine::Mysql, table)
    );
    let row: Option<mysql_async::Row> = mysql_async::prelude::Queryable::query_first(conn, sql).await.map_err(|err| crate::error_text::chain(&err))?;
    let row = row.ok_or_else(|| "没有读到视图定义".to_owned())?;
    let text = mysql_string(&row, 1);
    if text.trim().is_empty() || text.trim().starts_with("--") {
        return Err("没有读到视图定义".to_owned());
    }
    Ok(text)
}

fn push_mysql_fks(target: &mut Vec<ForeignKey>, rows: &[mysql_async::Row], _incoming: bool) {
    for row in rows {
        let name = mysql_string(row, 0);
        let column = mysql_string(row, 1);
        let ref_table = mysql_string(row, 2);
        let ref_column = mysql_string(row, 3);
        let on_delete = mysql_string(row, 4);
        let on_update = mysql_string(row, 5);
        if let Some(key) = target.iter_mut().find(|key| key.name == name) {
            key.columns.push(column);
            key.ref_columns.push(ref_column);
        } else {
            target.push(ForeignKey {
                name,
                columns: vec![column],
                ref_schema: String::new(),
                ref_table,
                ref_columns: vec![ref_column],
                on_delete,
                on_update,
            });
        }
    }
}

/// 结构页只需要“约多少行”和“有没有行”：大表上的 COUNT(*) 会扫全表，
/// 占着连接几十秒甚至几分钟。这里用统计信息估算行数，再用 LIMIT 1 确认是否为空。
async fn count_nulls_mysql_count(conn: &mut mysql_async::Conn, database: &str, table: &str) -> Result<i64, String> {
    let qualified = qualified_table(Engine::Mysql, database, "", table)?;
    let probe = format!("SELECT 1 FROM {qualified} LIMIT 1");
    let any: Option<mysql_async::Row> = mysql_async::prelude::Queryable::query_first(conn, probe).await.map_err(|err| crate::error_text::chain(&err))?;
    if any.is_none() {
        return Ok(0);
    }
    let estimate: Option<mysql_async::Row> = mysql_async::prelude::Queryable::exec_first(
        conn,
        "SELECT TABLE_ROWS FROM information_schema.tables WHERE table_schema = ? AND table_name = ?",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(|err| crate::error_text::chain(&err))?;
    Ok(estimate.map(|row| mysql_i64(&row, 0)).unwrap_or(0).max(1))
}

fn mysql_string(row: &mysql_async::Row, index: usize) -> String {
    match row.as_ref(index).unwrap_or(&mysql_async::Value::NULL) {
        mysql_async::Value::NULL => String::new(),
        mysql_async::Value::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        mysql_async::Value::Int(number) => number.to_string(),
        mysql_async::Value::UInt(number) => number.to_string(),
        mysql_async::Value::Float(number) => number.to_string(),
        mysql_async::Value::Double(number) => number.to_string(),
        mysql_async::Value::Date(year, month, day, hour, minute, second, micros) => {
            format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{micros:06}")
        }
        mysql_async::Value::Time(negative, days, hour, minute, second, micros) => {
            format!("{}{days} {hour:02}:{minute:02}:{second:02}.{micros:06}", if *negative { "-" } else { "" })
        }
    }
}

fn mysql_i64(row: &mysql_async::Row, index: usize) -> i64 {
    mysql_string(row, index).parse().unwrap_or(0)
}

pub(in crate::adapters) async fn run_steps(pool: &mysql_async::Pool, steps: &[Step]) -> Result<Vec<StepReceipt>, String> {
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    let mut results = Vec::new();
    let mut stop = false;
    for step in steps {
        if stop {
            results.push(receipt(step, StepStatus::Skipped, "前面的语句没有成功，这条没有执行。"));
            continue;
        }
        match mysql_async::prelude::Queryable::query_drop(&mut conn, step.sql.clone()).await {
            Ok(()) => results.push(receipt(step, StepStatus::Ok, "")),
            Err(mysql_async::Error::Server(err)) => {
                stop = true;
                results.push(receipt(step, StepStatus::Failed, &crate::error_text::chain(&err)));
            }
            Err(err) => {
                stop = true;
                results.push(receipt(step, StepStatus::Unknown, &crate::error_text::chain(&err)));
            }
        }
    }
    Ok(results)
}
