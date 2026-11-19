//! PostgreSQL 的关系图读取：一个 schema 的基表、列与外键。会话只连着自己的库，别的库直接报原因。

use crate::diagram::assemble::{assemble_diagram, group_key_parts};
use crate::diagram::model::{Diagram, DiagramColumn, KeyPart, OutgoingKey, RangeScope, ReadColumns, TableAttempt};

pub(crate) const POSTGRES_CURRENT_DATABASE: &str = "SELECT current_database()::text";

pub(crate) const POSTGRES_TABLES: &str = "\
SELECT c.relname::text
FROM pg_catalog.pg_class c
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = $1
  AND c.relkind IN ('r', 'p')
ORDER BY c.relname";

pub(crate) const POSTGRES_COLUMNS: &str = "\
SELECT a.attname::text,
       pg_catalog.format_type(a.atttypid, a.atttypmod)::text,
       EXISTS (
         SELECT 1
         FROM pg_catalog.pg_index i
         WHERE i.indrelid = c.oid
           AND i.indisprimary
           AND a.attnum = ANY (i.indkey)
       )
FROM pg_catalog.pg_attribute a
JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = $1
  AND c.relname = $2
  AND c.relkind IN ('r', 'p')
  AND a.attnum > 0
  AND NOT a.attisdropped
ORDER BY a.attnum";

pub(crate) const POSTGRES_FOREIGN_KEYS: &str = "\
SELECT con.conname::text,
       src_attr.attname::text,
       nref.nspname::text,
       tgt.relname::text,
       tgt_attr.attname::text,
       gs.ord::bigint
FROM pg_catalog.pg_constraint con
JOIN pg_catalog.pg_class src ON src.oid = con.conrelid
JOIN pg_catalog.pg_namespace nsrc ON nsrc.oid = src.relnamespace
JOIN pg_catalog.pg_class tgt ON tgt.oid = con.confrelid
JOIN pg_catalog.pg_namespace nref ON nref.oid = tgt.relnamespace
JOIN LATERAL generate_subscripts(con.conkey, 1) AS gs(ord) ON true
JOIN pg_catalog.pg_attribute src_attr
  ON src_attr.attrelid = con.conrelid AND src_attr.attnum = con.conkey[gs.ord]
JOIN pg_catalog.pg_attribute tgt_attr
  ON tgt_attr.attrelid = con.confrelid AND tgt_attr.attnum = con.confkey[gs.ord]
WHERE con.contype = 'f'
  AND nsrc.nspname = $1
  AND src.relname = $2
ORDER BY con.conname, gs.ord";

pub(super) const STATEMENTS: &[&str] = &[POSTGRES_CURRENT_DATABASE, POSTGRES_TABLES, POSTGRES_COLUMNS, POSTGRES_FOREIGN_KEYS];

fn failure(err: impl Into<crate::error_text::Reason>) -> String {
    crate::diagram::read::failure(err)
}

pub(super) async fn read_postgres(client: &tokio_postgres::Client, database: &str, schema: &str) -> Result<Diagram, String> {
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
        Err(err) => Err(crate::error_text::chain(&err)),
    };
    if columns.is_err() {
        return TableAttempt { name, columns, foreign_keys: Ok(Vec::new()) };
    }
    let key_params: [&(dyn tokio_postgres::types::ToSql + Sync); 2] = [&schema, &name];
    let foreign_keys = match client.query(POSTGRES_FOREIGN_KEYS, &key_params).await {
        Ok(rows) => postgres_keys(&rows),
        Err(err) => Err(crate::error_text::chain(&err)),
    };
    TableAttempt { name, columns, foreign_keys }
}

fn postgres_columns(rows: &[tokio_postgres::Row]) -> Result<ReadColumns, String> {
    let mut columns = Vec::with_capacity(rows.len());
    for row in rows {
        let type_name: Option<String> = row.try_get(1).map_err(|err| crate::error_text::chain(&err))?;
        columns.push(DiagramColumn {
            name: row.try_get(0).map_err(|err| crate::error_text::chain(&err))?,
            type_name: type_name.unwrap_or_default(),
            primary: row.try_get(2).map_err(|err| crate::error_text::chain(&err))?,
        });
    }
    Ok(ReadColumns(columns))
}

fn postgres_keys(rows: &[tokio_postgres::Row]) -> Result<Vec<OutgoingKey>, String> {
    let mut parts = Vec::with_capacity(rows.len());
    for row in rows {
        parts.push(KeyPart {
            name: row.try_get(0).map_err(|err| crate::error_text::chain(&err))?,
            column: row.try_get(1).map_err(|err| crate::error_text::chain(&err))?,
            ref_schema: row.try_get(2).map_err(|err| crate::error_text::chain(&err))?,
            ref_table: row.try_get(3).map_err(|err| crate::error_text::chain(&err))?,
            ref_column: row.try_get(4).map_err(|err| crate::error_text::chain(&err))?,
            ordinal: row.try_get(5).map_err(|err| crate::error_text::chain(&err))?,
        });
    }
    Ok(group_key_parts(parts))
}

pub(crate) fn empty_postgres_schema(schema: &str) -> Option<String> {
    if schema.trim().is_empty() {
        Some("PostgreSQL 关系图需要一个 schema。".to_owned())
    } else {
        None
    }
}

pub(crate) fn other_postgres_database(current: &str, database: &str) -> Option<String> {
    if database == current {
        None
    } else {
        Some(format!("当前会话连在 {current}，没有另开连接读取 {database}"))
    }
}
