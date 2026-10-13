use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::connections::model::{
    CaMode, Engine, EnvLabel, SavedConnection, SshAuthKind, SshSettings, TlsMode, TlsSettings,
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
                ssh_password_saved INTEGER NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;
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
                password_saved, ssh_password_saved
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)
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
                ssh_password_saved = excluded.ssh_password_saved",
        )
        .bind(&connection.id)
        .bind(&connection.name)
        .bind(connection.engine.as_str())
        .bind(connection.env.as_str())
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
                    password_saved, ssh_password_saved
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
                    password_saved, ssh_password_saved
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
        self.checkpoint().await?;
        Ok(existing)
    }

    pub async fn host_key(&self, host: &str, port: u16) -> Result<Option<String>, sqlx::Error> {
        let row = sqlx::query("SELECT fingerprint FROM ssh_host_keys WHERE ssh_host = ?1 AND ssh_port = ?2")
            .bind(host)
            .bind(i64::from(port))
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|row| row.try_get(0)).transpose()?)
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

    async fn checkpoint(&self) -> Result<(), sqlx::Error> {
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&self.pool).await?;
        Ok(())
    }
}

fn row_to_connection(row: &sqlx::sqlite::SqliteRow) -> Result<SavedConnection, sqlx::Error> {
    let engine = Engine::parse(row.try_get::<String, _>(2)?.as_str()).ok_or_else(|| sqlx::Error::Protocol("未知引擎".into()))?;
    let env = EnvLabel::parse(row.try_get::<String, _>(3)?.as_str()).ok_or_else(|| sqlx::Error::Protocol("未知环境".into()))?;
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
        tls,
        ssh,
        password_saved: row.try_get::<i64, _>(20)? != 0,
        ssh_password_saved: row.try_get::<i64, _>(21)? != 0,
    })
}

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
