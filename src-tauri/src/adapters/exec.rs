//! 查询执行的会话一侧（ADR-0 / R22，A4）：把已打开的会话接成整批执行用的句柄、逐条执行、复位会话设置。
//!
//! 主代码（`query/exec.rs` 的 `LiveDriver`）只持有 [`Attached`] 并调用这里的方法；各引擎的取连接、
//! 切库、超时设置、逐行读取与错误位置写在 `adapters/<engine>/exec.rs`。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use crate::connections::{CancelTransport, CatalogSession};
use crate::query::exec::{attach_failure, refuse_message, database_gate, DatabaseGate, ErrorAt, Failure, ResultExtras, RowSink, StatementValue};
use crate::query::split;

use super::{mysql, postgres, redis, sqlite, sqlserver};

/// 整批执行借用的会话句柄。
pub enum Attached {
    Sqlite(sqlx::SqlitePool),
    Mysql(mysql_async::Conn),
    Postgres(Arc<tokio_postgres::Client>),
    Sqlserver(Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>),
    Redis(Arc<tokio::sync::Mutex<redis::conn::RedisConn>>),
    Scripted,
}

/// 会返回行的语句（按首个关键字判断），执行器据此决定收行集还是只拿受影响行数。
pub(crate) fn looks_like_rows(sql: &str) -> bool {
    matches!(
        split::first_keyword(sql).as_str(),
        "SELECT" | "WITH" | "SHOW" | "EXPLAIN" | "PRAGMA" | "VALUES" | "TABLE" | "DESC" | "DESCRIBE"
    )
}

pub(crate) fn to_hex(bytes: Vec<u8>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

impl CatalogSession {
    /// 接成整批执行的句柄：按引擎切库或拒绝跨库、下发会话级超时 `setup`。
    /// 返回句柄，以及超时设置是否真的生效（整批结束要复位）。
    ///
    /// 关系型用户查询固定使用业务会话。不再回退复用 MySQL 元数据池：事务归属不能随资源情况改变，
    /// 专用业务连接建立失败时明确失败（ADR-1）。
    pub async fn attach_query(
        self,
        engine: &str,
        database: &str,
        transport: &CancelTransport,
        setup: &[String],
    ) -> Result<(Attached, bool), String> {
        let mut timeout_applied = !setup.is_empty();
        let attached = match self {
            Self::Scripted => Attached::Scripted,
            Self::Redis(conn) => {
                if !database.trim().is_empty() {
                    let db = database.trim().strip_prefix("db").unwrap_or(database.trim()).parse::<u32>()
                        .map_err(|_| format!("Redis 库号无效：{database}"))?;
                    let mut guard = conn.lock().await;
                    // 集群只有 0 号库，SELECT 会被随机路由到某个节点；0 号库无需切换，其余库号明确拒绝。
                    if guard.is_cluster() {
                        if db != 0 {
                            return Err(format!("Redis 集群只有 0 号库，不能切换到 {database}。"));
                        }
                        drop(guard);
                        timeout_applied = false;
                        return Ok((Attached::Redis(conn), timeout_applied));
                    }
                    let _: () = ::redis::cmd("SELECT")
                        .arg(db)
                        .query_async(&mut *guard)
                        .await
                        .map_err(|err| attach_failure(transport, err.to_string()))?;
                }
                timeout_applied = false;
                Attached::Redis(conn)
            }
            // MongoDB 不做 SQL 面（R12）：文档在集合浏览器里查看与编辑。
            Self::Mongo(_) => return Err("MongoDB 不执行 SQL；文档在集合浏览器里查看。".to_owned()),
            Self::Sqlite { pool, .. } => match database_gate("sqlite", "main", database) {
                DatabaseGate::Refuse => return Err(refuse_message("main", database)),
                _ => Attached::Sqlite(pool),
            },
            Self::Mysql(pool) => {
                // 用户查询固定只借业务执行池：不再回退元数据会话，也不再有第二套超时分支。
                let mut conn = mysql::exec::acquire_mysql_conn(&pool)
                    .await
                    .map_err(|err| attach_failure(transport, err))?;
                conn.reset_connection(false);
                // 握手已经提供真实连接 id，无需再花一次网络往返查询。
                transport.set_mysql_connection_id(u64::from(conn.id()));
                match tokio::time::timeout(Duration::from_secs(12), mysql::exec::align_mysql(&mut conn, engine, database)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => return Err(attach_failure(transport, err)),
                    Err(_) => return Err(attach_failure(transport, "切换数据库响应超时，连接可能已中断。".to_owned())),
                }
                if !setup.is_empty() {
                    match tokio::time::timeout(Duration::from_secs(8), mysql::exec::run_mysql_statements(&mut conn, setup)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(err)) if mysql::exec::is_unsupported_timeout_var(&err) => {
                            // 旧版 MySQL / TiDB / OceanBase / 代理不支持会话级超时变量时不阻断 SQL 执行。
                            timeout_applied = false;
                        }
                        Ok(Err(err)) => return Err(attach_failure(transport, err)),
                        Err(_) => return Err(attach_failure(transport, "初始化查询会话超时。".to_owned())),
                    }
                }
                Attached::Mysql(conn)
            }
            Self::Postgres(client) => {
                let current = postgres::exec::postgres_current(&client)
                    .await
                    .map_err(|err| attach_failure(transport, err))?;
                match database_gate("postgres", &current, database) {
                    DatabaseGate::Refuse => return Err(refuse_message(&current, database)),
                    _ => {
                        if let Err(err) = postgres::exec::run_postgres_statements(&client, setup).await {
                            return Err(attach_failure(transport, err));
                        }
                        Attached::Postgres(client)
                    }
                }
            }
            Self::Sqlserver(client) => {
                let current = sqlserver::exec::sqlserver_current(&client)
                    .await
                    .map_err(|err| attach_failure(transport, err))?;
                match database_gate("sqlserver", &current, database) {
                    DatabaseGate::Refuse => return Err(refuse_message(&current, database)),
                    _ => Attached::Sqlserver(client),
                }
            }
        };
        Ok((attached, timeout_applied))
    }
}

impl Attached {
    /// 这个句柄会把语句当 SQL 发给数据库（探针连接与 Redis 命令不会，读取上限不改写）。
    pub fn sends_sql(&self) -> bool {
        !matches!(self, Self::Scripted | Self::Redis(_))
    }

    /// 执行一条语句。`sent` 是实际发出的文本（可能带读取上限），`sql` 是用户写的那条（错误位置映射回它）。
    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        &mut self,
        transport: &CancelTransport,
        sent: &str,
        sql: &str,
        limit: usize,
        cancel: &Arc<AtomicBool>,
        sink: &dyn RowSink,
        index: usize,
    ) -> Result<StatementValue, Failure> {
        self.run_full(transport, sent, sql, limit, cancel, sink, index).await.map(|(value, _)| value).map_err(|(failure, _)| failure)
    }

    /// 与 run 相同，另外带回第一个结果集之外的东西（存储过程的其余结果集、警告等，见 ResultExtras）。
    /// 失败时同样带回 ResultExtras：存储过程中途失败时，已经读到的结果集在里面（partial / more）。
    #[allow(clippy::too_many_arguments)]
    pub async fn run_full(
        &mut self,
        transport: &CancelTransport,
        sent: &str,
        sql: &str,
        limit: usize,
        cancel: &Arc<AtomicBool>,
        sink: &dyn RowSink,
        index: usize,
    ) -> Result<(StatementValue, ResultExtras), (Failure, Box<ResultExtras>)> {
        let plain = |value: StatementValue| (value, ResultExtras::default());
        let bare = |failure: Failure| (failure, Box::default());
        match self {
            Self::Scripted => Err(bare(Failure::from("这个连接没有连着可查询的数据库".to_owned()))),
            Self::Sqlite(pool) => sqlite::exec::sqlite_run(pool, sent, limit, cancel, sink, index).await.map(plain).map_err(|message| bare(Failure::from(message))),
            Self::Mysql(conn) => mysql::exec::mysql_run(conn, transport, sent, limit, cancel, sink, index)
                .await
                .map_err(|(message, extras)| (Failure { at: mysql::exec::mysql_error_line(&message).map(ErrorAt::Line), message, code: None }, extras)),
            Self::Postgres(client) => postgres::exec::postgres_run(client, sent, sql, limit, cancel, sink, index).await.map(plain).map_err(bare),
            Self::Sqlserver(client) => sqlserver::exec::sqlserver_run(client, sent, limit, cancel, sink, index).await.map(plain).map_err(bare),
            Self::Redis(conn) => redis::console::redis_query_run(conn, sent, limit, cancel, sink, index).await.map(plain).map_err(|message| bare(Failure::from(message))),
        }
    }

    /// 整批结束后执行复位语句（会话级超时），避免泄漏到这一批之外的业务会话。
    pub async fn reset(&mut self, statements: &[String]) -> Result<(), String> {
        match self {
            Self::Mysql(conn) => mysql::exec::run_mysql_statements(conn, statements).await,
            Self::Postgres(client) => postgres::exec::run_postgres_statements(client, statements).await,
            _ => Ok(()),
        }
    }
}
