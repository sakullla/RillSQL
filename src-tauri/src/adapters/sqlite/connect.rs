//! SQLite（文件型）的打开、会话与中断。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::adapters::session::{Canceller, CatalogSession, Connector, Opened, Session};
use crate::connections::model::SavedConnection;
use crate::connections::route::Route;
use crate::connections::stages::{self, StageError};

/// 应用自己的连接配置库（也是 SQLite 文件）关闭前把 WAL 合并回主文件。
pub(crate) const LOCAL_STORE_CHECKPOINT: &str = "PRAGMA wal_checkpoint(TRUNCATE)";

pub(crate) struct SqliteOpen {
    pub pool: sqlx::SqlitePool,
    pub version: String,
    pub foreign_keys: bool,
    pub schema_objects: usize,
}

pub(crate) async fn open_sqlite(path: &Path) -> Result<SqliteOpen, StageError> {
    if path.as_os_str().is_empty() {
        return Err(stages::fail_code(crate::connections::error_codes::CONN_SQLITE_FILE_MISSING, "请选择数据库文件", "SQLite 只打开已有文件。"));
    }
    // 表编辑和目录元数据共用这一条已打开的会话，不再为写入另开只读连接。
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .busy_timeout(Duration::from_secs(5))
        .read_only(false);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_SQLITE_OPEN_FAILED, crate::error_text::chain(&err), "没有打开文件，后续阶段不会执行。"))?;
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type IN ('table', 'view') ORDER BY name")
        .fetch_all(&pool)
        .await
        .map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_SCHEMA_READ_FAILED, crate::error_text::chain(&err), "文件已打开，但没有读到 schema。"))?;
    let version: String = sqlx::query_scalar("SELECT sqlite_version()")
        .fetch_one(&pool)
        .await
        .map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_VERSION_READ_FAILED, crate::error_text::chain(&err), "文件已打开，但没有读到版本。"))?;
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&pool)
        .await
        .map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_FOREIGN_KEYS_READ_FAILED, crate::error_text::chain(&err), "没有读到 foreign_keys。"))?;
    Ok(SqliteOpen {
        pool,
        version,
        foreign_keys: foreign_keys != 0,
        schema_objects: names.len(),
    })
}

/// SQLite 连接器：只打开本地文件。
pub(in crate::adapters) struct SqliteConnector;

#[async_trait::async_trait]
impl Connector for SqliteConnector {
    async fn open_remote(&self, _config: &SavedConnection, _route: &Route, _password: &str) -> Result<Opened, StageError> {
        Err(stages::fail_code(crate::connections::error_codes::CONN_SQLITE_NOT_REMOTE, "SQLite 不走远程连接", "未执行"))
    }

    async fn open_file(&self, path: &Path) -> Result<Opened, StageError> {
        let opened = open_sqlite(path).await?;
        Ok(Opened {
            schema_note: Some(format!("SQLite {} · 读取到 {} 个表或视图", opened.version, opened.schema_objects)),
            version: opened.version,
            session: Arc::new(SqliteSession(opened.pool)),
            canceller: Arc::new(InterruptFlag(Arc::new(AtomicBool::new(false)))),
            foreign_keys: Some(opened.foreign_keys),
        })
    }
}

struct SqliteSession(sqlx::SqlitePool);

#[async_trait::async_trait]
impl Session for SqliteSession {
    fn catalog(&self, foreign_keys: Option<bool>) -> CatalogSession {
        CatalogSession::Sqlite { pool: self.0.clone(), foreign_keys }
    }

    fn local_file(&self) -> bool {
        true
    }

    /// 本地文件没有“断线”可言，免探活（这里到不了，防御返回成功）。
    async fn ping(&self) -> Result<(), String> {
        Ok(())
    }
}

/// 本地执行的中断：执行循环轮询这个标志。没有另开传输通道。
pub(crate) struct InterruptFlag(pub(crate) Arc<AtomicBool>);

#[async_trait::async_trait]
impl Canceller for InterruptFlag {
    fn interrupt_flag(&self) -> Option<Arc<AtomicBool>> {
        Some(Arc::clone(&self.0))
    }

    async fn cancel(&self) -> Result<bool, String> {
        self.0.store(true, Ordering::SeqCst);
        Ok(false)
    }
}
