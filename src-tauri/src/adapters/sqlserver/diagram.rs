//! SQL Server 的关系图读取：一个 schema 的基表、列与外键，只读会话当前库。

use crate::diagram::assemble::{assemble_diagram, group_key_parts};
use crate::diagram::model::{Diagram, DiagramColumn, KeyPart, OutgoingKey, RangeScope, ReadColumns, TableAttempt};
use super::grid_read::sqlserver_rows;
use crate::grid::Cell;

type TdsHandle = std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>;

/// SQL Server 只读基表。`{schema}` / `{table}` 在执行前换成已转义的字面量，不拼接标识符。
pub(crate) const SQLSERVER_TABLES: &str = "\
SELECT t.name
FROM sys.tables t
JOIN sys.schemas s ON s.schema_id = t.schema_id
WHERE s.name = N'{schema}'
ORDER BY t.name";

pub(crate) const SQLSERVER_COLUMNS: &str = "\
SELECT c.name,
       TYPE_NAME(c.user_type_id),
       CASE WHEN EXISTS (
         SELECT 1
         FROM sys.index_columns ic
         JOIN sys.indexes i ON i.object_id = ic.object_id AND i.index_id = ic.index_id
         WHERE i.is_primary_key = 1
           AND ic.object_id = c.object_id
           AND ic.column_id = c.column_id
       ) THEN 1 ELSE 0 END
FROM sys.columns c
JOIN sys.tables t ON t.object_id = c.object_id
JOIN sys.schemas s ON s.schema_id = t.schema_id
WHERE s.name = N'{schema}' AND t.name = N'{table}'
ORDER BY c.column_id";

pub(crate) const SQLSERVER_FOREIGN_KEYS: &str = "\
SELECT fk.name, pc.name, rs.name, rt.name, rc.name, fkc.constraint_column_id
FROM sys.foreign_keys fk
JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id
JOIN sys.tables pt ON pt.object_id = fk.parent_object_id
JOIN sys.schemas ps ON ps.schema_id = pt.schema_id
JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id
JOIN sys.tables rt ON rt.object_id = fk.referenced_object_id
JOIN sys.schemas rs ON rs.schema_id = rt.schema_id
JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id
WHERE ps.name = N'{schema}' AND pt.name = N'{table}'
ORDER BY fk.name, fkc.constraint_column_id";

pub(super) const STATEMENTS: &[&str] = &[SQLSERVER_TABLES, SQLSERVER_COLUMNS, SQLSERVER_FOREIGN_KEYS];

fn failure(err: impl Into<crate::error_text::Reason>) -> String {
    crate::diagram::read::failure(err)
}

pub(super) async fn read_sqlserver(client: &TdsHandle, database: &str, schema: &str) -> Result<Diagram, String> {
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

async fn sqlserver_names(client: &TdsHandle, schema: &str) -> Result<Vec<String>, String> {
    let sql = SQLSERVER_TABLES.replace("{schema}", &tsql_literal(schema));
    let (_names, rows) = sqlserver_rows(client, &sql).await.map_err(failure)?;
    Ok(rows.iter().map(|row| cell_string(row.first())).filter(|name| !name.is_empty()).collect())
}

async fn sqlserver_table(client: &TdsHandle, schema: &str, name: String) -> TableAttempt {
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
