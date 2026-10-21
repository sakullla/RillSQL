use std::path::Path;

use crate::catalog::model::{parse_kind, ObjectRow, DEFAULT_OBJECT_PAGE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlitePage {
    pub foreign_keys: bool,
    pub total: usize,
    pub objects: Vec<ObjectRow>,
}

pub async fn read_sqlite_pool(
    pool: &sqlx::SqlitePool,
    offset: usize,
    limit: usize,
) -> Result<SqlitePage, String> {
    let limit = if limit == 0 {
        DEFAULT_OBJECT_PAGE
    } else {
        limit
    };
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(pool)
        .await
        .map_err(|err| err.to_string())?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())?;
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT name, type, '', '' FROM sqlite_master WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?1 OFFSET ?2",
    )
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(pool)
    .await
    .map_err(|err| err.to_string())?;
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
    Ok(SqlitePage {
        foreign_keys: foreign_keys != 0,
        total: usize::try_from(total).unwrap_or(0),
        objects,
    })
}

pub async fn read_sqlite_file(
    path: &Path,
    offset: usize,
    limit: usize,
) -> Result<SqlitePage, String> {
    if path.as_os_str().is_empty() {
        return Err("请选择数据库文件".to_owned());
    }
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|err| format!("无法读取对象列表：{err}"))?;
    read_sqlite_pool(&pool, offset, limit).await
}
