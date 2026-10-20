//! 本机查询历史。只记录已经发起的 SQL，不保存密码，打开历史也不会执行。

use std::path::Path;

use serde::Serialize;
use sqlx::Row;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    pub sql: String,
    pub connection_id: String,
    pub connection_name: String,
    pub database_name: String,
    pub engine: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub elapsed_ms: Option<i64>,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct HistoryDraft {
    pub id: String,
    pub sql: String,
    pub connection_id: String,
    pub connection_name: String,
    pub database_name: String,
    pub engine: String,
    pub started_at: String,
    pub summary: String,
}

/// 默认保留的历史条数。写入后按 started_at 只留最新的这些。
const DEFAULT_HISTORY_CAP: i64 = 2000;

#[derive(Clone)]
pub struct HistoryStore {
    pool: sqlx::SqlitePool,
    cap: i64,
}

impl HistoryStore {
    pub async fn open(path: &Path) -> Result<Self, String> {
        Self::open_with_cap(path, DEFAULT_HISTORY_CAP).await
    }

    pub async fn open_with_cap(path: &Path, cap: i64) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|err| format!("建不了历史目录：{err}"))?;
            }
        }
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(|err| format!("打不开查询历史：{err}"))?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS query_history (
                id TEXT PRIMARY KEY,
                sql TEXT NOT NULL,
                connection_id TEXT NOT NULL,
                connection_name TEXT NOT NULL,
                database_name TEXT NOT NULL,
                engine TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                status TEXT NOT NULL,
                elapsed_ms INTEGER,
                summary TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .map_err(|err| format!("建不了查询历史：{err}"))?;
        super::saved::ensure_schema(&pool).await?;
        Ok(Self { pool, cap })
    }

    pub(super) fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    pub async fn insert(&self, draft: &HistoryDraft) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO query_history (
                id, sql, connection_id, connection_name, database_name, engine, started_at, finished_at, status, elapsed_ms, summary
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, 'running', NULL, ?8)",
        )
        .bind(&draft.id)
        .bind(&draft.sql)
        .bind(&draft.connection_id)
        .bind(&draft.connection_name)
        .bind(&draft.database_name)
        .bind(&draft.engine)
        .bind(&draft.started_at)
        .bind(&draft.summary)
        .execute(&self.pool)
        .await
        .map_err(|err| format!("写不了查询历史：{err}"))?;
        sqlx::query("DELETE FROM query_history WHERE id NOT IN (SELECT id FROM query_history ORDER BY started_at DESC, id DESC LIMIT ?1)")
            .bind(self.cap)
            .execute(&self.pool)
            .await
            .map_err(|err| format!("修剪不了查询历史：{err}"))?;
        Ok(())
    }

    pub async fn finish(&self, id: &str, finished_at: &str, status: &str, elapsed_ms: i64, summary: &str) -> Result<(), String> {
        sqlx::query("UPDATE query_history SET finished_at = ?1, status = ?2, elapsed_ms = ?3, summary = ?4 WHERE id = ?5")
            .bind(finished_at)
            .bind(status)
            .bind(elapsed_ms)
            .bind(summary)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|err| format!("更新不了查询历史：{err}"))?;
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<HistoryEntry>, String> {
        let rows = sqlx::query(
            "SELECT id, sql, connection_id, connection_name, database_name, engine, started_at, finished_at, status, elapsed_ms, summary
             FROM query_history ORDER BY started_at DESC, id DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| format!("读不了查询历史：{err}"))?;
        rows.iter().map(entry_from_row).collect()
    }

    pub async fn delete(&self, id: &str) -> Result<(), String> {
        sqlx::query("DELETE FROM query_history WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|err| format!("删不了这条查询历史：{err}"))?;
        Ok(())
    }

    pub async fn clear(&self) -> Result<(), String> {
        sqlx::query("DELETE FROM query_history")
            .execute(&self.pool)
            .await
            .map_err(|err| format!("清空不了查询历史：{err}"))?;
        Ok(())
    }

    #[cfg(test)]
    pub async fn column_names(&self) -> Result<Vec<String>, String> {
        let rows = sqlx::query("PRAGMA table_info(query_history)")
            .fetch_all(&self.pool)
            .await
            .map_err(|err| format!("读不了历史表结构：{err}"))?;
        let mut names = Vec::new();
        for row in &rows {
            names.push(row.try_get::<String, _>(1).map_err(|err| err.to_string())?);
        }
        Ok(names)
    }
}

fn entry_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<HistoryEntry, String> {
    Ok(HistoryEntry {
        id: row.try_get(0).map_err(|err| err.to_string())?,
        sql: row.try_get(1).map_err(|err| err.to_string())?,
        connection_id: row.try_get(2).map_err(|err| err.to_string())?,
        connection_name: row.try_get(3).map_err(|err| err.to_string())?,
        database_name: row.try_get(4).map_err(|err| err.to_string())?,
        engine: row.try_get(5).map_err(|err| err.to_string())?,
        started_at: row.try_get(6).map_err(|err| err.to_string())?,
        finished_at: row.try_get(7).map_err(|err| err.to_string())?,
        status: row.try_get(8).map_err(|err| err.to_string())?,
        elapsed_ms: row.try_get(9).map_err(|err| err.to_string())?,
        summary: row.try_get(10).map_err(|err| err.to_string())?,
    })
}

/// 从历史恢复时只产生草稿。调用方不能据此执行。
#[cfg(test)]
pub fn restored_draft(entry: &HistoryEntry, connection_exists: bool) -> RestoredDraft {
    RestoredDraft {
        sql: entry.sql.clone(),
        connection_id: if connection_exists { Some(entry.connection_id.clone()) } else { None },
        database_name: if connection_exists { Some(entry.database_name.clone()) } else { None },
        needs_target: !connection_exists,
        execute: false,
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredDraft {
    pub sql: String,
    pub connection_id: Option<String>,
    pub database_name: Option<String>,
    pub needs_target: bool,
    pub execute: bool,
}
