use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::connections::groups::ConnectionGroup;
use crate::connections::model::{
    CaMode, Engine, EnvTag, SavedConnection, SshAuthKind, SshSettings, TlsMode, TlsSettings,
};

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, sqlx::Error> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(sqlx::Error::Io)?;
        }
        let options = SqliteConnectOptions::new().filename(&path).create_if_missing(true);
        let pool = SqlitePoolOptions::new().max_connections(1).connect_with(options).await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> Result<(), sqlx::Error> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS connections (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                engine TEXT NOT NULL,
                env TEXT NOT NULL,
                host TEXT,
                port INTEGER,
                database_name TEXT,
                username TEXT,
                file_path TEXT,
                tls_mode TEXT,
                tls_server_name TEXT,
                ca_mode TEXT,
                ca_file TEXT,
                client_cert TEXT,
                client_key TEXT,
                ssh_host TEXT,
                ssh_port INTEGER,
                ssh_user TEXT,
                ssh_auth TEXT,
                ssh_key_path TEXT,
                password_saved INTEGER NOT NULL,
                ssh_password_saved INTEGER NOT NULL,
                replica_set TEXT
            )",
        )
        .execute(&self.pool)
        .await?;
        // 旧配置库补列（MongoDB 副本集参数，R12）；列已存在时报错直接忽略。
        if let Err(err) = sqlx::query("ALTER TABLE connections ADD COLUMN replica_set TEXT").execute(&self.pool).await {
            let text = err.to_string();
            if !text.contains("duplicate column") {
                return Err(err);
            }
        }
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS ssh_host_keys (
                ssh_host TEXT NOT NULL,
                ssh_port INTEGER NOT NULL,
                algorithm TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                PRIMARY KEY (ssh_host, ssh_port)
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS env_tags (
                id TEXT PRIMARY KEY,
                label TEXT NOT NULL,
                tone TEXT NOT NULL,
                position INTEGER NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS app_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;
        // 最近成功打开时间单独一张表：连接定义（含导出/导入）不因“用过”而改变形状，
        // 存量库只多一张空表，未使用过的连接保持原排序。
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS connection_usage (
                id TEXT PRIMARY KEY,
                last_used_at INTEGER NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;
        // 用户分组与成员关系各一张表：分组只是组织元数据，连接定义（含导出/导入形状）
        // 一个字段都不改；没有分组时两张表是空的，存量连接天然是“未分组”。
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS connection_groups (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                position INTEGER NOT NULL,
                collapsed INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS connection_group_members (
                connection_id TEXT PRIMARY KEY,
                group_id TEXT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;
        // 迁移只清理悬空引用，不删任何连接行：删过连接、导入过别的库之后仍能自洽。
        sqlx::query(
            "DELETE FROM connection_group_members
             WHERE connection_id NOT IN (SELECT id FROM connections)
                OR group_id NOT IN (SELECT id FROM connection_groups)",
        )
        .execute(&self.pool)
        .await?;
        self.seed_env_tags().await?;
        Ok(())
    }

    /// 只在第一次打开配置库时放入原来的四个标签。用户删光之后不再补回去。
    async fn seed_env_tags(&self) -> Result<(), sqlx::Error> {
        let row = sqlx::query("SELECT value FROM app_meta WHERE key = 'env_tags_seeded'")
            .fetch_optional(&self.pool)
            .await?;
        let seeded: Option<String> = row.as_ref().map(|item| item.try_get(0)).transpose()?;
        if seeded.as_deref() == Some("1") {
            return Ok(());
        }
        for (position, (id, label, tone)) in [
            ("dev", "开发", "dev"),
            ("test", "测试", "test"),
            ("prod", "生产", "prod"),
            ("local", "本地", "local"),
        ]
        .into_iter()
        .enumerate()
        {
            sqlx::query("INSERT OR IGNORE INTO env_tags (id, label, tone, position) VALUES (?1, ?2, ?3, ?4)")
                .bind(id)
                .bind(label)
                .bind(tone)
                .bind(i64::try_from(position).unwrap_or(0))
                .execute(&self.pool)
                .await?;
        }
        sqlx::query(
            "INSERT INTO app_meta (key, value) VALUES ('env_tags_seeded', '1')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert(&self, connection: &SavedConnection) -> Result<(), sqlx::Error> {
        let tls = connection.tls.as_ref();
        let ssh = connection.ssh.as_ref();
        sqlx::query(
            "INSERT INTO connections (
                id, name, engine, env, host, port, database_name, username, file_path,
                tls_mode, tls_server_name, ca_mode, ca_file, client_cert, client_key,
                ssh_host, ssh_port, ssh_user, ssh_auth, ssh_key_path,
                password_saved, ssh_password_saved, replica_set
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                engine = excluded.engine,
                env = excluded.env,
                host = excluded.host,
                port = excluded.port,
                database_name = excluded.database_name,
                username = excluded.username,
                file_path = excluded.file_path,
                tls_mode = excluded.tls_mode,
                tls_server_name = excluded.tls_server_name,
                ca_mode = excluded.ca_mode,
                ca_file = excluded.ca_file,
                client_cert = excluded.client_cert,
                client_key = excluded.client_key,
                ssh_host = excluded.ssh_host,
                ssh_port = excluded.ssh_port,
                ssh_user = excluded.ssh_user,
                ssh_auth = excluded.ssh_auth,
                ssh_key_path = excluded.ssh_key_path,
                password_saved = excluded.password_saved,
                ssh_password_saved = excluded.ssh_password_saved,
                replica_set = excluded.replica_set",
        )
        .bind(&connection.id)
        .bind(&connection.name)
        .bind(connection.engine.as_str())
        .bind(&connection.env)
        .bind(&connection.host)
        .bind(connection.port.map(i64::from))
        .bind(&connection.database)
        .bind(&connection.username)
        .bind(&connection.file_path)
        .bind(tls.map(|item| item.mode.as_str()))
        .bind(tls.map(|item| item.server_name.clone()))
        .bind(tls.map(|item| item.ca_mode.as_str()))
        .bind(tls.and_then(|item| item.ca_file.clone()))
        .bind(tls.and_then(|item| item.client_cert.clone()))
        .bind(tls.and_then(|item| item.client_key.clone()))
        .bind(ssh.map(|item| item.host.clone()))
        .bind(ssh.map(|item| i64::from(item.port)))
        .bind(ssh.map(|item| item.username.clone()))
        .bind(ssh.map(|item| item.auth.as_str()))
        .bind(ssh.and_then(|item| item.key_path.clone()))
        .bind(i64::from(u8::from(connection.password_saved)))
        .bind(i64::from(u8::from(connection.ssh_password_saved)))
        .bind(&connection.replica_set)
        .execute(&self.pool)
        .await?;
        self.checkpoint().await?;
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<SavedConnection>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, name, engine, env, host, port, database_name, username, file_path,
                    tls_mode, tls_server_name, ca_mode, ca_file, client_cert, client_key,
                    ssh_host, ssh_port, ssh_user, ssh_auth, ssh_key_path,
                    password_saved, ssh_password_saved, replica_set
             FROM connections ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_connection).collect()
    }

    pub async fn get(&self, id: &str) -> Result<Option<SavedConnection>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, name, engine, env, host, port, database_name, username, file_path,
                    tls_mode, tls_server_name, ca_mode, ca_file, client_cert, client_key,
                    ssh_host, ssh_port, ssh_user, ssh_auth, ssh_key_path,
                    password_saved, ssh_password_saved, replica_set
             FROM connections WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(row_to_connection).transpose()
    }

    pub async fn delete(&self, id: &str) -> Result<Option<SavedConnection>, sqlx::Error> {
        let existing = self.get(id).await?;
        sqlx::query("DELETE FROM connections WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM connection_usage WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        // 分组是组织元数据：连接删掉就一起清掉它的成员关系，分组本身留下。
        sqlx::query("DELETE FROM connection_group_members WHERE connection_id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.checkpoint().await?;
        Ok(existing)
    }

    /// 记一次成功打开。只写使用时间，不碰连接定义；密码等机密仍只在系统凭据库。
    /// 时间戳严格递增（同一毫秒内的连续打开也能分出先后），排序不依赖 id 或插入顺序。
    pub async fn touch_last_used(&self, id: &str, at: i64) -> Result<(), sqlx::Error> {
        let row = sqlx::query("SELECT COALESCE(MAX(last_used_at), 0) FROM connection_usage")
            .fetch_one(&self.pool)
            .await?;
        let highest: i64 = row.try_get(0)?;
        let at = at.max(highest.saturating_add(1));
        sqlx::query(
            "INSERT INTO connection_usage (id, last_used_at) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET last_used_at = excluded.last_used_at",
        )
        .bind(id)
        .bind(at)
        .execute(&self.pool)
        .await?;
        self.checkpoint().await?;
        Ok(())
    }

    /// 最近使用时间，最新在前。时间相同的按 id 稳定排序，不靠 SQLite 的行序。
    pub async fn last_used(&self) -> Result<Vec<(String, i64)>, sqlx::Error> {
        let rows = sqlx::query("SELECT id, last_used_at FROM connection_usage ORDER BY last_used_at DESC, id")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| Ok((row.try_get::<String, _>(0)?, row.try_get::<i64, _>(1)?)))
            .collect()
    }

    pub async fn host_key(&self, host: &str, port: u16) -> Result<Option<String>, sqlx::Error> {
        let row = sqlx::query("SELECT fingerprint FROM ssh_host_keys WHERE ssh_host = ?1 AND ssh_port = ?2")
            .bind(host)
            .bind(i64::from(port))
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| row.try_get(0)).transpose()
    }

    pub async fn save_host_key(&self, host: &str, port: u16, algorithm: &str, fingerprint: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO ssh_host_keys (ssh_host, ssh_port, algorithm, fingerprint)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(ssh_host, ssh_port) DO UPDATE SET algorithm = excluded.algorithm, fingerprint = excluded.fingerprint",
        )
        .bind(host)
        .bind(i64::from(port))
        .bind(algorithm)
        .bind(fingerprint)
        .execute(&self.pool)
        .await?;
        self.checkpoint().await?;
        Ok(())
    }

    pub async fn delete_host_key(&self, host: &str, port: u16) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM ssh_host_keys WHERE ssh_host = ?1 AND ssh_port = ?2")
            .bind(host)
            .bind(i64::from(port))
            .execute(&self.pool)
            .await?;
        self.checkpoint().await?;
        Ok(())
    }

    pub async fn ssh_host_still_used(&self, host: &str, port: u16, except_id: &str) -> Result<bool, sqlx::Error> {
        let row = sqlx::query(
            "SELECT COUNT(*) FROM connections WHERE ssh_host = ?1 AND ssh_port = ?2 AND id <> ?3",
        )
        .bind(host)
        .bind(i64::from(port))
        .bind(except_id)
        .fetch_one(&self.pool)
        .await?;
        let count: i64 = row.try_get(0)?;
        Ok(count > 0)
    }

    pub async fn list_tags(&self) -> Result<Vec<EnvTag>, sqlx::Error> {
        let rows = sqlx::query("SELECT id, label, tone, position FROM env_tags ORDER BY position, label")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_tag).collect()
    }

    pub async fn tag(&self, id: &str) -> Result<Option<EnvTag>, sqlx::Error> {
        let row = sqlx::query("SELECT id, label, tone, position FROM env_tags WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(row_to_tag).transpose()
    }

    pub async fn label_taken(&self, label: &str, except_id: &str) -> Result<bool, sqlx::Error> {
        let row = sqlx::query("SELECT COUNT(*) FROM env_tags WHERE label = ?1 AND id <> ?2")
            .bind(label)
            .bind(except_id)
            .fetch_one(&self.pool)
            .await?;
        let count: i64 = row.try_get(0)?;
        Ok(count > 0)
    }

    pub async fn next_tag_position(&self) -> Result<i64, sqlx::Error> {
        let row = sqlx::query("SELECT COALESCE(MAX(position), -1) + 1 FROM env_tags")
            .fetch_one(&self.pool)
            .await?;
        row.try_get(0)
    }

    pub async fn upsert_tag(&self, tag: &EnvTag) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO env_tags (id, label, tone, position) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET label = excluded.label, tone = excluded.tone, position = excluded.position",
        )
        .bind(&tag.id)
        .bind(&tag.label)
        .bind(&tag.tone)
        .bind(tag.position)
        .execute(&self.pool)
        .await?;
        self.checkpoint().await?;
        Ok(())
    }

    pub async fn delete_tag(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE connections SET env = 'none' WHERE env = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM env_tags WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.checkpoint().await?;
        Ok(())
    }

    pub async fn list_groups(&self) -> Result<Vec<ConnectionGroup>, sqlx::Error> {
        let rows = sqlx::query("SELECT id, name, position, collapsed FROM connection_groups ORDER BY position, name, id")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_group).collect()
    }

    pub async fn group(&self, id: &str) -> Result<Option<ConnectionGroup>, sqlx::Error> {
        let row = sqlx::query("SELECT id, name, position, collapsed FROM connection_groups WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(row_to_group).transpose()
    }

    pub async fn next_group_position(&self) -> Result<i64, sqlx::Error> {
        let row = sqlx::query("SELECT COALESCE(MAX(position), -1) + 1 FROM connection_groups")
            .fetch_one(&self.pool)
            .await?;
        row.try_get(0)
    }

    pub async fn upsert_group(&self, group: &ConnectionGroup) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO connection_groups (id, name, position, collapsed) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, position = excluded.position, collapsed = excluded.collapsed",
        )
        .bind(&group.id)
        .bind(&group.name)
        .bind(group.position)
        .bind(i64::from(u8::from(group.collapsed)))
        .execute(&self.pool)
        .await?;
        self.checkpoint().await?;
        Ok(())
    }

    /// 删分组只删分组和它的成员关系，连接行一个都不动：失败的删除也不会丢连接。
    pub async fn delete_group(&self, id: &str) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM connection_group_members WHERE group_id = ?1").bind(id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM connection_groups WHERE id = ?1").bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        self.checkpoint().await?;
        Ok(())
    }

    pub async fn set_group_collapsed(&self, id: &str, collapsed: bool) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE connection_groups SET collapsed = ?2 WHERE id = ?1")
            .bind(id)
            .bind(i64::from(u8::from(collapsed)))
            .execute(&self.pool)
            .await?;
        self.checkpoint().await?;
        Ok(())
    }

    /// 连接 id → 分组 id。没有行的连接就是未分组，不写哨兵值。
    pub async fn memberships(&self) -> Result<Vec<(String, String)>, sqlx::Error> {
        let rows = sqlx::query("SELECT connection_id, group_id FROM connection_group_members")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| Ok((row.try_get::<String, _>(0)?, row.try_get::<String, _>(1)?)))
            .collect()
    }

    pub async fn assign_group(&self, connection_id: &str, group: Option<&str>) -> Result<(), sqlx::Error> {
        match group {
            Some(group) => {
                sqlx::query(
                    "INSERT INTO connection_group_members (connection_id, group_id) VALUES (?1, ?2)
                     ON CONFLICT(connection_id) DO UPDATE SET group_id = excluded.group_id",
                )
                .bind(connection_id)
                .bind(group)
                .execute(&self.pool)
                .await?;
            }
            None => {
                sqlx::query("DELETE FROM connection_group_members WHERE connection_id = ?1")
                    .bind(connection_id)
                    .execute(&self.pool)
                    .await?;
            }
        }
        self.checkpoint().await?;
        Ok(())
    }

    #[cfg(test)]
    pub async fn testing_insert_membership(&self, connection_id: &str, group_id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT OR REPLACE INTO connection_group_members (connection_id, group_id) VALUES (?1, ?2)")
            .bind(connection_id)
            .bind(group_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    #[cfg(test)]
    pub async fn testing_set_env(&self, id: &str, env: &str) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE connections SET env = ?1 WHERE id = ?2")
            .bind(env)
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.checkpoint().await?;
        Ok(())
    }

    async fn checkpoint(&self) -> Result<(), sqlx::Error> {
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&self.pool).await?;
        Ok(())
    }
}

fn row_to_tag(row: &sqlx::sqlite::SqliteRow) -> Result<EnvTag, sqlx::Error> {
    Ok(EnvTag {
        id: row.try_get(0)?,
        label: row.try_get(1)?,
        tone: row.try_get(2)?,
        position: row.try_get(3)?,
    })
}

fn row_to_group(row: &sqlx::sqlite::SqliteRow) -> Result<ConnectionGroup, sqlx::Error> {
    Ok(ConnectionGroup {
        id: row.try_get(0)?,
        name: row.try_get(1)?,
        position: row.try_get(2)?,
        collapsed: row.try_get::<i64, _>(3)? != 0,
    })
}

fn row_to_connection(row: &sqlx::sqlite::SqliteRow) -> Result<SavedConnection, sqlx::Error> {
    let engine = Engine::parse(row.try_get::<String, _>(2)?.as_str()).map_err(|_| sqlx::Error::Protocol("未知引擎".into()))?;
    let env = {
        let value: String = row.try_get(3)?;
        let trimmed = value.trim();
        if trimmed.is_empty() {
            "none".to_owned()
        } else {
            trimmed.to_owned()
        }
    };
    let tls_mode: Option<String> = row.try_get(9)?;
    let tls = match tls_mode.as_deref().and_then(TlsMode::parse) {
        Some(mode) => Some(TlsSettings {
            mode,
            server_name: row.try_get::<Option<String>, _>(10)?.unwrap_or_default(),
            ca_mode: row
                .try_get::<Option<String>, _>(11)?
                .as_deref()
                .and_then(CaMode::parse)
                .unwrap_or(CaMode::System),
            ca_file: row.try_get(12)?,
            client_cert: row.try_get(13)?,
            client_key: row.try_get(14)?,
        }),
        None => None,
    };
    let ssh_host: Option<String> = row.try_get(15)?;
    let ssh = match ssh_host {
        Some(host) => Some(SshSettings {
            host,
            port: u16::try_from(row.try_get::<Option<i64>, _>(16)?.unwrap_or(22)).unwrap_or(22),
            username: row.try_get::<Option<String>, _>(17)?.unwrap_or_default(),
            auth: row
                .try_get::<Option<String>, _>(18)?
                .as_deref()
                .and_then(SshAuthKind::parse)
                .unwrap_or(SshAuthKind::Key),
            key_path: row.try_get(19)?,
        }),
        None => None,
    };
    Ok(SavedConnection {
        id: row.try_get(0)?,
        name: row.try_get(1)?,
        engine,
        env,
        host: row.try_get(4)?,
        port: row.try_get::<Option<i64>, _>(5)?.and_then(|port| u16::try_from(port).ok()),
        database: row.try_get(6)?,
        username: row.try_get(7)?,
        file_path: row.try_get(8)?,
        replica_set: row.try_get(22)?,
        tls,
        ssh,
        password_saved: row.try_get::<i64, _>(20)? != 0,
        ssh_password_saved: row.try_get::<i64, _>(21)? != 0,
    })
}

#[cfg(test)]
pub fn persisted_bytes(path: &Path) -> Vec<u8> {
    let mut bytes = std::fs::read(path).unwrap_or_default();
    for suffix in ["-wal", "-shm"] {
        let extra = PathBuf::from(format!("{}{suffix}", path.display()));
        if let Ok(more) = std::fs::read(extra) {
            bytes.extend(more);
        }
    }
    bytes
}
