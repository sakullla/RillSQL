//! MySQL / MariaDB 的表网格读写：描述表结构、读一页、计数、逐条执行、外键与流式导出。都在已打开的连接池上进行。

use crate::adapters::grid::{db_err, ExportSink};
use crate::connections::model::Engine;
use crate::grid::{push_foreign_key, Cell, Column, ForeignKey, TableSchema};

/// `engine_hint` 区分 MySQL 与 MariaDB（两者共用连接池类型）。
pub(in crate::adapters) async fn describe(pool: &mysql_async::Pool, database: &str, table: &str, engine_hint: &str) -> Result<TableSchema, String> {
    if database.is_empty() {
        return Err("需要数据库名，没有另开连接。".to_owned());
    }
    let engine = if engine_hint == "mariadb" { Engine::Mariadb } else { Engine::Mysql };
    describe_mysql(pool, engine, database, table).await
}

pub(in crate::adapters) async fn fetch_cells(pool: &mysql_async::Pool, sql: &str) -> Result<Vec<Vec<Cell>>, String> {
    let (_names, rows) = mysql_rows(pool, sql).await?;
    Ok(rows)
}

pub(in crate::adapters) async fn fetch_count(pool: &mysql_async::Pool, sql: &str) -> Result<i64, String> {
    let (_names, rows) = mysql_rows(pool, sql).await?;
    match rows.first().and_then(|row| row.first()) {
        Some(Cell::Text(text)) => text.parse::<i64>().map_err(|_| format!("计数无效 {text}")),
        _ => Err("没有计数".to_owned()),
    }
}

pub(in crate::adapters) async fn execute(pool: &mysql_async::Pool, sql: &str) -> Result<u64, String> {
    let mut conn = pool.get_conn().await.map_err(db_err)?;
    let mut result = mysql_async::prelude::Queryable::query_iter(&mut conn, sql).await.map_err(db_err)?;
    let affected = result.affected_rows();
    while result.next().await.map_err(db_err)?.is_some() {}
    Ok(affected)
}

pub(in crate::adapters) async fn foreign_keys(pool: &mysql_async::Pool, database: &str, table: &str) -> Result<Vec<ForeignKey>, String> {
    if database.is_empty() {
        return Ok(Vec::new());
    }
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
        push_foreign_key(&mut list, &name, &column, &scope, &ref_table, &ref_column);
    }
    Ok(list)
}

/// 流式导出：逐行从服务器取，列名已知时直接用。
pub(in crate::adapters) async fn stream(pool: &mysql_async::Pool, sql: &str, known_names: Option<Vec<String>>, sink: &mut dyn ExportSink) {
    let mut conn = match pool.get_conn().await {
        Ok(conn) => conn,
        Err(err) => return sink.fail(&crate::error_text::chain(&err)),
    };
    let mut result = match mysql_async::prelude::Queryable::query_iter(&mut conn, sql).await {
        Ok(result) => result,
        Err(err) => return sink.fail(&crate::error_text::chain(&err)),
    };
    let names = known_names.unwrap_or_else(|| result.columns_ref().iter().map(|column| column.name_str().into_owned()).collect());
    if !sink.start(&names) {
        return;
    }
    loop {
        if !sink.proceed() {
            return;
        }
        match result.next().await {
            Ok(Some(row)) => {
                let cells = (0..names.len()).map(|index| mysql_cell(row.as_ref(index).unwrap_or(&mysql_async::Value::NULL))).collect::<Vec<_>>();
                if !sink.row(&cells) {
                    return;
                }
            }
            Ok(None) => return sink.finish(),
            Err(err) => return sink.fail(&crate::error_text::chain(&err)),
        }
    }
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
        "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY, EXTRA FROM information_schema.columns WHERE table_schema = ? AND table_name = ? ORDER BY ORDINAL_POSITION",
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
