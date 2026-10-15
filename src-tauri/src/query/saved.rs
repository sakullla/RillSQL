//! 按名称保存的查询。只写入本机库的新表，不执行 SQL，也不写入执行历史。

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::history::HistoryStore;

const NAME_LIMIT: usize = 200;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SavedQuery {
    pub id: String,
    pub name: String,
    pub sql: String,
    pub connection_id: String,
    pub connection_name: String,
    pub database_name: String,
    pub engine: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SavedWrite {
    pub query: SavedQuery,
    pub created: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveRequest {
    pub name: String,
    pub sql: String,
    pub connection_id: String,
    pub connection_name: String,
    pub database_name: String,
    pub engine: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameRequest {
    pub id: String,
    pub name: String,
}

pub fn normalize_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("请填写名称".to_owned());
    }
    if trimmed.chars().count() > NAME_LIMIT {
        return Err("名称不能超过 200 个字".to_owned());
    }
    Ok(trimmed.to_owned())
}

pub async fn ensure_schema(pool: &sqlx::SqlitePool) -> Result<(), String> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS saved_queries (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            sql TEXT NOT NULL,
            connection_id TEXT NOT NULL,
            connection_name TEXT NOT NULL,
            database_name TEXT NOT NULL,
            engine TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await
    .map_err(|err| format!("建不了保存的查询：{err}"))?;
    Ok(())
}

impl HistoryStore {
    pub async fn list_saved(&self) -> Result<Vec<SavedQuery>, String> {
        let rows = sqlx::query(
            "SELECT id, name, sql, connection_id, connection_name, database_name, engine, updated_at
             FROM saved_queries ORDER BY name, id",
        )
        .fetch_all(self.pool())
        .await
        .map_err(|err| format!("读不了保存的查询：{err}"))?;
        rows.iter().map(saved_from_row).collect()
    }

    pub async fn save_named(&self, request: &SaveRequest) -> Result<SavedWrite, String> {
        let name = normalize_name(&request.name)?;
        let now = chrono::Utc::now().to_rfc3339();
        if let Some(existing) = self.find_saved_by_name(&name).await? {
            sqlx::query(
                "UPDATE saved_queries
                 SET sql = ?1, connection_id = ?2, connection_name = ?3, database_name = ?4, engine = ?5, updated_at = ?6
                 WHERE id = ?7",
            )
            .bind(&request.sql)
            .bind(&request.connection_id)
            .bind(&request.connection_name)
            .bind(&request.database_name)
            .bind(&request.engine)
            .bind(&now)
            .bind(&existing.id)
            .execute(self.pool())
            .await
            .map_err(|err| format!("更新不了保存的查询：{err}"))?;
            return Ok(SavedWrite {
                query: self.require_saved(&existing.id).await?,
                created: false,
            });
        }
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO saved_queries (
                id, name, sql, connection_id, connection_name, database_name, engine, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(&id)
        .bind(&name)
        .bind(&request.sql)
        .bind(&request.connection_id)
        .bind(&request.connection_name)
        .bind(&request.database_name)
        .bind(&request.engine)
        .bind(&now)
        .execute(self.pool())
        .await
        .map_err(|err| format!("存不了这条查询：{err}"))?;
        Ok(SavedWrite {
            query: self.require_saved(&id).await?,
            created: true,
        })
    }

    pub async fn rename_saved(&self, request: &RenameRequest) -> Result<SavedQuery, String> {
        let name = normalize_name(&request.name)?;
        let current = self.require_saved(&request.id).await?;
        if current.name == name {
            return Ok(current);
        }
        if let Some(clash) = self.find_saved_by_name(&name).await? {
            if clash.id != current.id {
                return Err("已经有同名的保存查询".to_owned());
            }
        }
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query("UPDATE saved_queries SET name = ?1, updated_at = ?2 WHERE id = ?3")
            .bind(&name)
            .bind(&now)
            .bind(&current.id)
            .execute(self.pool())
            .await
            .map_err(|err| format!("改不了这个名称：{err}"))?;
        self.require_saved(&current.id).await
    }

    pub async fn delete_saved(&self, id: &str) -> Result<(), String> {
        let result = sqlx::query("DELETE FROM saved_queries WHERE id = ?1")
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(|err| format!("删不了保存的查询：{err}"))?;
        if result.rows_affected() == 0 {
            return Err("找不到这个保存查询".to_owned());
        }
        Ok(())
    }

    async fn find_saved_by_name(&self, name: &str) -> Result<Option<SavedQuery>, String> {
        let row = sqlx::query(
            "SELECT id, name, sql, connection_id, connection_name, database_name, engine, updated_at
             FROM saved_queries WHERE name = ?1",
        )
        .bind(name)
        .fetch_optional(self.pool())
        .await
        .map_err(|err| format!("读不了保存的查询：{err}"))?;
        row.as_ref().map(saved_from_row).transpose()
    }

    async fn require_saved(&self, id: &str) -> Result<SavedQuery, String> {
        let row = sqlx::query(
            "SELECT id, name, sql, connection_id, connection_name, database_name, engine, updated_at
             FROM saved_queries WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await
        .map_err(|err| format!("读不了保存的查询：{err}"))?;
        row.as_ref().map(saved_from_row).transpose()?.ok_or_else(|| "找不到这个保存查询".to_owned())
    }

    #[cfg(test)]
    pub async fn saved_column_names(&self) -> Result<Vec<String>, String> {
        let rows = sqlx::query("PRAGMA table_info(saved_queries)")
            .fetch_all(self.pool())
            .await
            .map_err(|err| format!("读不了保存查询的表结构：{err}"))?;
        let mut names = Vec::new();
        for row in &rows {
            names.push(row.try_get::<String, _>(1).map_err(|err| err.to_string())?);
        }
        Ok(names)
    }
}

fn saved_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<SavedQuery, String> {
    Ok(SavedQuery {
        id: row.try_get(0).map_err(|err| err.to_string())?,
        name: row.try_get(1).map_err(|err| err.to_string())?,
        sql: row.try_get(2).map_err(|err| err.to_string())?,
        connection_id: row.try_get(3).map_err(|err| err.to_string())?,
        connection_name: row.try_get(4).map_err(|err| err.to_string())?,
        database_name: row.try_get(5).map_err(|err| err.to_string())?,
        engine: row.try_get(6).map_err(|err| err.to_string())?,
        updated_at: row.try_get(7).map_err(|err| err.to_string())?,
    })
}
