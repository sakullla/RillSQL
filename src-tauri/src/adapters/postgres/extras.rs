//! PostgreSQL 的连接级对象详情（R5）：可用扩展与序列参数。每节独立读取、独立报错。

use crate::catalog::objects::{extras_failure, ExtensionRow, ExtrasBody, SequenceRow};

pub(crate) const POSTGRES_EXTENSIONS: &str = "SELECT a.name, a.default_version, a.installed_version, a.comment FROM pg_catalog.pg_available_extensions a ORDER BY a.name";

pub(crate) const POSTGRES_SEQUENCES: &str = "SELECT s.schemaname, s.sequencename, s.data_type::text, s.start_value, s.min_value, s.max_value, s.increment_by, s.cache_size, s.cycle, s.last_value FROM pg_catalog.pg_sequences s WHERE s.schemaname = $1 ORDER BY s.sequencename";

pub(crate) const POSTGRES_SEQUENCES_ALL: &str = "SELECT s.schemaname, s.sequencename, s.data_type::text, s.start_value, s.min_value, s.max_value, s.increment_by, s.cache_size, s.cycle, s.last_value FROM pg_catalog.pg_sequences s WHERE s.schemaname NOT IN ('pg_catalog', 'information_schema') ORDER BY s.schemaname, s.sequencename";

pub(super) async fn read_postgres_extras(client: &tokio_postgres::Client, schema: &str) -> ExtrasBody {
    let mut body = ExtrasBody::default();
    match client.query(POSTGRES_EXTENSIONS, &[]).await {
        Ok(rows) => {
            body.extensions.items = rows
                .iter()
                .map(|row| ExtensionRow {
                    name: pg_text(row, 0),
                    default_version: pg_text(row, 1),
                    installed_version: pg_text_opt(row, 2),
                    comment: pg_text(row, 3),
                })
                .collect();
        }
        Err(err) => body.extensions.error = Some(extras_failure("扩展", err)),
    }
    let result = if schema.is_empty() {
        client.query(POSTGRES_SEQUENCES_ALL, &[]).await
    } else {
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 1] = [&schema];
        client.query(POSTGRES_SEQUENCES, &params).await
    };
    match result {
        Ok(rows) => {
            body.sequences.items = rows
                .iter()
                .map(|row| SequenceRow {
                    schema: pg_text(row, 0),
                    name: pg_text(row, 1),
                    data_type: pg_text(row, 2),
                    start_value: row.try_get::<_, i64>(3).unwrap_or_default(),
                    min_value: row.try_get::<_, i64>(4).unwrap_or_default(),
                    max_value: row.try_get::<_, i64>(5).unwrap_or_default(),
                    increment: row.try_get::<_, i64>(6).unwrap_or_default(),
                    cache_size: row.try_get::<_, i64>(7).unwrap_or_default(),
                    cycle: row.try_get::<_, bool>(8).unwrap_or(false),
                    last_value: row.try_get::<_, Option<i64>>(9).unwrap_or(None),
                })
                .collect();
        }
        Err(err) => body.sequences.error = Some(extras_failure("序列", err)),
    }
    body
}

fn pg_text(row: &tokio_postgres::Row, index: usize) -> String {
    row.try_get::<_, String>(index)
        .or_else(|_| row.try_get::<_, Option<String>>(index).map(|value| value.unwrap_or_default()))
        .unwrap_or_default()
}

fn pg_text_opt(row: &tokio_postgres::Row, index: usize) -> Option<String> {
    row.try_get::<_, Option<String>>(index).ok().flatten()
}
