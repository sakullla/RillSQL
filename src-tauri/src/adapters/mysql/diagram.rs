//! MySQL 家族的关系图读取：只读基表、列与外键，不新开连接也不改会话的库。

use mysql_async::prelude::Queryable;

use crate::diagram::assemble::{assemble_diagram, group_key_parts};
use crate::diagram::model::{Diagram, DiagramColumn, KeyPart, OutgoingKey, RangeScope, ReadColumns, TableAttempt};

pub(crate) const MYSQL_TABLES: &str = "\
SELECT TABLE_NAME
FROM information_schema.TABLES
WHERE TABLE_SCHEMA = ? AND TABLE_TYPE = 'BASE TABLE'
ORDER BY TABLE_NAME";

pub(crate) const MYSQL_COLUMNS: &str = "\
SELECT COLUMN_NAME, COLUMN_TYPE, COLUMN_KEY
FROM information_schema.COLUMNS
WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
ORDER BY ORDINAL_POSITION";

pub(crate) const MYSQL_FOREIGN_KEYS: &str = "\
SELECT CONSTRAINT_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME, ORDINAL_POSITION
FROM information_schema.KEY_COLUMN_USAGE
WHERE TABLE_SCHEMA = ?
  AND TABLE_NAME = ?
  AND REFERENCED_TABLE_NAME IS NOT NULL
ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION";

pub(super) const STATEMENTS: &[&str] = &[MYSQL_TABLES, MYSQL_COLUMNS, MYSQL_FOREIGN_KEYS];

fn failure(err: impl Into<crate::error_text::Reason>) -> String {
    crate::diagram::read::failure(err)
}

pub(super) async fn read_mysql(pool: &mysql_async::Pool, database: &str) -> Result<Diagram, String> {
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
                columns: Err(crate::error_text::chain(&err)),
                foreign_keys: Ok(Vec::new()),
            };
        }
    };
    let columns = match conn.exec(MYSQL_COLUMNS, (database.to_owned(), name.clone())).await {
        Ok(rows) => mysql_columns(&rows),
        Err(err) => Err(crate::error_text::chain(&err)),
    };
    if columns.is_err() {
        return TableAttempt { name, columns, foreign_keys: Ok(Vec::new()) };
    }
    let foreign_keys = match conn.exec(MYSQL_FOREIGN_KEYS, (database.to_owned(), name.clone())).await {
        Ok(rows) => mysql_keys(&rows),
        Err(err) => Err(crate::error_text::chain(&err)),
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
        mysql_async::Value::Bytes(bytes) => String::from_utf8(bytes.clone()).map_err(|err| crate::error_text::chain(&err)),
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
