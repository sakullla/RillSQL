//! SQLite 的关系图读取：当前文件（main）的基表、列与外键，只读、不改 PRAGMA。

use sqlx::Row;

use crate::diagram::assemble::{assemble_diagram, group_key_parts};
use crate::diagram::model::{Diagram, DiagramColumn, KeyPart, OutgoingKey, RangeScope, ReadColumns, TableAttempt};

pub(crate) const SQLITE_TABLES: &str = "\
SELECT name
FROM sqlite_master
WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'
ORDER BY name";

pub(crate) const SQLITE_COLUMNS: &str = "SELECT name, type, pk FROM pragma_table_info(?1) ORDER BY cid";

pub(crate) const SQLITE_FOREIGN_KEYS: &str = "\
SELECT id, seq, \"from\", \"table\", \"to\"
FROM pragma_foreign_key_list(?1)
ORDER BY id, seq";

pub(super) const STATEMENTS: &[&str] = &[SQLITE_TABLES, SQLITE_COLUMNS, SQLITE_FOREIGN_KEYS];

fn failure(err: impl Into<crate::error_text::Reason>) -> String {
    crate::diagram::read::failure(err)
}

pub(super) async fn read_sqlite(pool: &sqlx::SqlitePool) -> Result<Diagram, String> {
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
        Err(err) => Err(crate::error_text::chain(&err)),
    };
    if columns.is_err() {
        return TableAttempt { name, columns, foreign_keys: Ok(Vec::new()) };
    }
    let foreign_keys = match sqlx::query(SQLITE_FOREIGN_KEYS).bind(&name).fetch_all(pool).await {
        Ok(rows) => sqlite_keys(&rows),
        Err(err) => Err(crate::error_text::chain(&err)),
    };
    TableAttempt { name, columns, foreign_keys }
}

fn sqlite_columns(rows: &[sqlx::sqlite::SqliteRow]) -> Result<ReadColumns, String> {
    let mut columns = Vec::with_capacity(rows.len());
    for row in rows {
        let name: String = row.try_get(0).map_err(|err| crate::error_text::chain(&err))?;
        let type_name: String = row.try_get::<Option<String>, _>(1).map_err(|err| crate::error_text::chain(&err))?.unwrap_or_default();
        let primary = sqlite_pk(row)?;
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
    let text: Option<String> = row.try_get(2).map_err(|err| crate::error_text::chain(&err))?;
    Ok(text.as_deref().is_some_and(|value| value != "0" && !value.is_empty()))
}

fn sqlite_keys(rows: &[sqlx::sqlite::SqliteRow]) -> Result<Vec<OutgoingKey>, String> {
    let mut parts = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.try_get(0).map_err(|err| crate::error_text::chain(&err))?;
        let seq: i64 = row.try_get(1).map_err(|err| crate::error_text::chain(&err))?;
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
    optional.map(|value| value.unwrap_or_default()).map_err(|err| crate::error_text::chain(&err))
}

/// `main.customers` 仍在当前文件里。其他前缀是附加库，不在这张图的范围。
fn split_sqlite_target(raw: &str) -> (String, String) {
    match raw.rsplit_once('.') {
        Some((schema, table)) if schema.eq_ignore_ascii_case("main") || schema.eq_ignore_ascii_case("temp") => (String::new(), table.to_owned()),
        Some((schema, table)) => (schema.to_owned(), table.to_owned()),
        None => (String::new(), raw.to_owned()),
    }
}
