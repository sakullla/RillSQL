//! 在已经打开的 catalog session 上执行 SQL。不新开执行用的数据库连接。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::TryStreamExt;
use mysql_async::prelude::Queryable;
use sqlx::Column;
use sqlx::Row;

use crate::connections::CancelTransport;
use crate::connections::CatalogSession;

use super::cancel::{self, CancelView};
use super::split::{self, is_write, Piece};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementValue {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub truncated: bool,
    pub affected: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatementStop {
    Error { message: String },
    Unknown { message: String },
    Cancelled { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementReport {
    pub sql: String,
    pub status: &'static str,
    pub message: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub truncated: bool,
    pub affected: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchReport {
    pub status: &'static str,
    pub statements: Vec<StatementReport>,
    pub cancel: CancelView,
    pub calls: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseGate {
    Ready,
    Switch,
    Refuse,
}

pub fn database_gate(engine: &str, current: &str, requested: &str) -> DatabaseGate {
    if requested.is_empty() || requested == current {
        return DatabaseGate::Ready;
    }
    match engine {
        "mysql" | "mariadb" => DatabaseGate::Switch,
        "postgres" => DatabaseGate::Refuse,
        "sqlite" => {
            if requested == "main" || current == "main" && requested == "main" {
                DatabaseGate::Ready
            } else if current.is_empty() && requested == "main" {
                DatabaseGate::Ready
            } else {
                DatabaseGate::Refuse
            }
        }
        _ => DatabaseGate::Refuse,
    }
}

pub fn refuse_message(current: &str, requested: &str) -> String {
    format!("当前会话连在 {current}，没有另开连接执行 {requested}")
}

pub fn classify_failure(sql: &str, message: String, cancel_requested: bool) -> StatementStop {
    if cancel_requested && cancel::is_cancel_error(&message) {
        return StatementStop::Cancelled { message };
    }
    if is_disconnect(&message) && is_write(sql) {
        return StatementStop::Unknown {
            message: format!("连接中断，无法确认写入是否完成。不会自动重放。{message}"),
        };
    }
    StatementStop::Error { message }
}

pub fn is_disconnect(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "connection closed",
        "connection reset",
        "broken pipe",
        "server closed",
        "network is unreachable",
        "connection refused",
        "timed out",
        "timeout",
        "pool closed",
        "connection is closed",
        "server disconnected",
        "lost connection",
        "forcibly closed",
        "wsarecv",
        "eof",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || message.contains("连接中断")
        || message.contains("连接已断开")
        || message.contains("连接被关闭")
}

#[async_trait::async_trait]
pub trait StatementDriver: Send + Sync {
    async fn run(&self, sql: &str, limit: usize, cancel: &Arc<AtomicBool>) -> Result<StatementValue, StatementStop>;
}

pub async fn run_batch(parts: &[Piece], limit: usize, cancel_flag: &Arc<AtomicBool>, via_ssh: bool, driver: &dyn StatementDriver) -> BatchReport {
    let mut statements = Vec::new();
    let mut calls = 0usize;
    let mut cancel = CancelView {
        phase: "none",
        message: String::new(),
        fresh_channel: false,
    };
    let mut status = "done";
    for (index, piece) in parts.iter().enumerate() {
        if cancel_flag.load(Ordering::SeqCst) {
            if cancel.phase == "none" {
                cancel = cancel::on_request(via_ssh);
            }
            for rest in &parts[index..] {
                statements.push(skipped(rest.text.clone(), "未执行"));
            }
            if cancel.phase == "requested" {
                cancel.phase = "confirmed";
                cancel.message = "后续语句没有发送到数据库。".to_owned();
            }
            status = if cancel.phase == "failed" { "running" } else { "cancelled" };
            break;
        }
        calls += 1;
        match driver.run(&piece.text, limit, cancel_flag).await {
            Ok(value) => {
                statements.push(StatementReport {
                    sql: piece.text.clone(),
                    status: "ok",
                    message: String::new(),
                    columns: value.columns,
                    rows: value.rows,
                    truncated: value.truncated,
                    affected: value.affected,
                });
                if cancel_flag.load(Ordering::SeqCst) {
                    if cancel.phase == "none" {
                        cancel = cancel::on_request(via_ssh);
                    }
                    for rest in &parts[index + 1..] {
                        statements.push(skipped(rest.text.clone(), "未执行"));
                    }
                    if cancel.phase == "requested" {
                        cancel.message = "取消生效前，当前语句已经执行完成；后续语句未执行。".to_owned();
                    }
                    status = "done";
                    break;
                }
            }
            Err(stop) => {
                let (statement_status, message) = match stop {
                    StatementStop::Error { message } => ("error", message),
                    StatementStop::Unknown { message } => ("unknown", message),
                    StatementStop::Cancelled { message } => ("cancelled", message),
                };
                if statement_status == "cancelled" {
                    cancel::on_statement_end(&mut cancel, "cancelled");
                    if cancel.phase == "none" {
                        cancel = cancel::on_request(via_ssh);
                        cancel.phase = "confirmed";
                        cancel.message = "数据库已确认取消。".to_owned();
                    }
                }
                statements.push(StatementReport {
                    sql: piece.text.clone(),
                    status: statement_status,
                    message,
                    columns: Vec::new(),
                    rows: Vec::new(),
                    truncated: false,
                    affected: None,
                });
                for rest in &parts[index + 1..] {
                    statements.push(skipped(rest.text.clone(), "未执行"));
                }
                status = match statement_status {
                    "unknown" => "unknown",
                    "cancelled" => "cancelled",
                    _ => "error",
                };
                break;
            }
        }
    }
    if statements.is_empty() {
        status = "error";
    }
    BatchReport {
        status,
        statements,
        cancel,
        calls,
    }
}

fn skipped(sql: String, message: &str) -> StatementReport {
    StatementReport {
        sql,
        status: "skipped",
        message: message.to_owned(),
        columns: Vec::new(),
        rows: Vec::new(),
        truncated: false,
        affected: None,
    }
}

pub struct LiveDriver {
    pub session: CatalogSession,
    pub transport: CancelTransport,
}

#[async_trait::async_trait]
impl StatementDriver for LiveDriver {
    async fn run(&self, sql: &str, limit: usize, cancel: &Arc<AtomicBool>) -> Result<StatementValue, StatementStop> {
        let result = match &self.session {
            CatalogSession::Scripted => Err("这条已打开的会话没有可查询的数据库通道".to_owned()),
            CatalogSession::Sqlite { pool, .. } => sqlite_run(pool, sql, limit, cancel).await,
            CatalogSession::Mysql(pool) => mysql_run(pool, &self.transport, sql, limit).await,
            CatalogSession::Postgres(client) => postgres_run(client, sql, limit).await,
        };
        result.map_err(|message| classify_failure(sql, message, cancel.load(Ordering::SeqCst)))
    }
}

pub async fn align_database(session: &CatalogSession, engine: &str, database: &str) -> Result<(), String> {
    match session {
        CatalogSession::Scripted => Err("这条已打开的会话没有可查询的数据库通道".to_owned()),
        CatalogSession::Sqlite { .. } => match database_gate("sqlite", "main", database) {
            DatabaseGate::Refuse => Err(refuse_message("main", database)),
            _ => Ok(()),
        },
        CatalogSession::Mysql(pool) => {
            let current = mysql_current(pool).await?;
            match database_gate(engine, &current, database) {
                DatabaseGate::Refuse => Err(refuse_message(&current, database)),
                DatabaseGate::Switch => {
                    let mut conn = pool.get_conn().await.map_err(|err| err.to_string())?;
                    let statement = format!("USE {}", quote_mysql(database));
                    conn.query_drop(statement).await.map_err(|err| err.to_string())?;
                    Ok(())
                }
                DatabaseGate::Ready => Ok(()),
            }
        }
        CatalogSession::Postgres(client) => {
            let current = postgres_current(client).await?;
            match database_gate("postgres", &current, database) {
                DatabaseGate::Refuse => Err(refuse_message(&current, database)),
                _ => Ok(()),
            }
        }
    }
}

async fn mysql_current(pool: &mysql_async::Pool) -> Result<String, String> {
    let mut conn = pool.get_conn().await.map_err(|err| err.to_string())?;
    let value: Option<String> = conn.query_first("SELECT DATABASE()").await.map_err(|err| err.to_string())?;
    Ok(value.unwrap_or_default())
}

async fn postgres_current(client: &tokio_postgres::Client) -> Result<String, String> {
    let row = client.query_one("SELECT current_database()", &[]).await.map_err(|err| err.to_string())?;
    row.try_get(0).map_err(|err| err.to_string())
}

fn quote_mysql(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

async fn sqlite_run(pool: &sqlx::SqlitePool, sql: &str, limit: usize, cancel: &Arc<AtomicBool>) -> Result<StatementValue, String> {
    let mut conn = pool.acquire().await.map_err(|err| err.to_string())?;
    let watched = Arc::clone(cancel);
    conn.lock_handle()
        .await
        .map_err(|err| err.to_string())?
        .set_progress_handler(64, move || !watched.load(Ordering::SeqCst));
    let outcome = sqlite_dispatch(&mut conn, sql, limit).await;
    if let Ok(mut handle) = conn.lock_handle().await {
        handle.remove_progress_handler();
    }
    outcome
}

async fn sqlite_dispatch(conn: &mut sqlx::SqliteConnection, sql: &str, limit: usize) -> Result<StatementValue, String> {
    let safe = sqlx::AssertSqlSafe(sql.to_owned());
    if looks_like_rows(sql) {
        let mut stream = sqlx::query(safe).fetch(&mut *conn);
        let mut columns = Vec::new();
        let mut rows = Vec::new();
        let mut truncated = false;
        while let Some(row) = stream.try_next().await.map_err(|err| err.to_string())? {
            if columns.is_empty() {
                columns = row.columns().iter().map(|column| column.name().to_owned()).collect();
            }
            if rows.len() >= limit {
                truncated = true;
                break;
            }
            let cells = (0..columns.len()).map(|index| sqlite_text(&row, index)).collect();
            rows.push(cells);
        }
        drop(stream);
        return Ok(StatementValue {
            columns,
            rows,
            truncated,
            affected: None,
        });
    }
    let done = sqlx::query(safe).execute(&mut *conn).await.map_err(|err| err.to_string())?;
    Ok(StatementValue {
        columns: Vec::new(),
        rows: Vec::new(),
        truncated: false,
        affected: Some(done.rows_affected()),
    })
}

fn looks_like_rows(sql: &str) -> bool {
    matches!(
        split::first_keyword(sql).as_str(),
        "SELECT" | "WITH" | "SHOW" | "EXPLAIN" | "PRAGMA" | "VALUES" | "TABLE" | "DESC" | "DESCRIBE"
    )
}

fn sqlite_text(row: &sqlx::sqlite::SqliteRow, index: usize) -> Option<String> {
    if let Ok(value) = row.try_get::<Option<String>, _>(index) {
        return value;
    }
    if let Ok(value) = row.try_get::<Option<i64>, _>(index) {
        return value.map(|number| number.to_string());
    }
    if let Ok(value) = row.try_get::<Option<f64>, _>(index) {
        return value.map(|number| number.to_string());
    }
    if let Ok(value) = row.try_get::<Option<Vec<u8>>, _>(index) {
        return value.map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    }
    None
}

async fn mysql_run(pool: &mysql_async::Pool, transport: &CancelTransport, sql: &str, limit: usize) -> Result<StatementValue, String> {
    let mut conn = pool.get_conn().await.map_err(|err| err.to_string())?;
    if let Ok(Some(id)) = conn.query_first::<u64, _>("SELECT CONNECTION_ID()").await {
        transport.set_mysql_connection_id(id);
    }
    let mut result = conn.query_iter(sql).await.map_err(|err| err.to_string())?;
    let columns: Vec<String> = result.columns_ref().iter().map(|column| column.name_str().into_owned()).collect();
    if columns.is_empty() {
        let affected = result.affected_rows();
        while result.next().await.map_err(|err| err.to_string())?.is_some() {}
        return Ok(StatementValue {
            columns,
            rows: Vec::new(),
            truncated: false,
            affected: Some(affected),
        });
    }
    let mut rows = Vec::new();
    let mut truncated = false;
    while let Some(row) = result.next().await.map_err(|err| err.to_string())? {
        if rows.len() >= limit {
            truncated = true;
            break;
        }
        let cells = (0..columns.len())
            .map(|index| mysql_cell(row.as_ref(index).unwrap_or(&mysql_async::Value::NULL)))
            .collect();
        rows.push(cells);
    }
    if truncated {
        while result.next().await.map_err(|err| err.to_string())?.is_some() {}
    }
    Ok(StatementValue {
        columns,
        rows,
        truncated,
        affected: None,
    })
}

fn mysql_cell(value: &mysql_async::Value) -> Option<String> {
    match value {
        mysql_async::Value::NULL => None,
        mysql_async::Value::Bytes(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        mysql_async::Value::Int(number) => Some(number.to_string()),
        mysql_async::Value::UInt(number) => Some(number.to_string()),
        mysql_async::Value::Float(number) => Some(number.to_string()),
        mysql_async::Value::Double(number) => Some(number.to_string()),
        mysql_async::Value::Date(year, month, day, hour, minute, second, micros) => {
            Some(format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{micros:06}"))
        }
        mysql_async::Value::Time(negative, days, hour, minute, second, micros) => {
            Some(format!("{}{days} {hour:02}:{minute:02}:{second:02}.{micros:06}", if *negative { "-" } else { "" }))
        }
    }
}

async fn postgres_run(client: &tokio_postgres::Client, sql: &str, limit: usize) -> Result<StatementValue, String> {
    match client.prepare(sql).await {
        Ok(statement) => {
            let columns: Vec<String> = statement.columns().iter().map(|column| column.name().to_owned()).collect();
            if columns.is_empty() {
                let affected = client.execute(&statement, &[]).await.map_err(|err| err.to_string())?;
                return Ok(StatementValue {
                    columns,
                    rows: Vec::new(),
                    truncated: false,
                    affected: Some(affected),
                });
            }
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 0] = [];
            let stream = client.query_raw(&statement, params).await.map_err(|err| err.to_string())?;
            let mut stream = std::pin::pin!(stream);
            let mut rows = Vec::new();
            let mut truncated = false;
            while let Some(row) = stream.try_next().await.map_err(|err| err.to_string())? {
                if rows.len() >= limit {
                    truncated = true;
                    break;
                }
                let cells = (0..columns.len()).map(|index| pg_text(&row, index)).collect();
                rows.push(cells);
            }
            if truncated {
                while stream.try_next().await.map_err(|err| err.to_string())?.is_some() {}
            }
            Ok(StatementValue {
                columns,
                rows,
                truncated,
                affected: None,
            })
        }
        Err(_) => {
            let messages = client.simple_query(sql).await.map_err(|err| err.to_string())?;
            let mut columns = Vec::new();
            let mut rows = Vec::new();
            let mut affected = None;
            let mut truncated = false;
            for message in messages {
                match message {
                    tokio_postgres::SimpleQueryMessage::RowDescription(description) => {
                        columns = description.iter().map(|column| column.name().to_owned()).collect();
                    }
                    tokio_postgres::SimpleQueryMessage::Row(row) => {
                        if rows.len() >= limit {
                            truncated = true;
                            continue;
                        }
                        let width = if columns.is_empty() { row.len() } else { columns.len() };
                        if columns.is_empty() {
                            columns = (0..width).map(|index| format!("col{}", index + 1)).collect();
                        }
                        let cells = (0..width).map(|index| row.get(index).map(str::to_owned)).collect();
                        rows.push(cells);
                    }
                    tokio_postgres::SimpleQueryMessage::CommandComplete(count) => affected = Some(count),
                    _ => {}
                }
            }
            if columns.is_empty() {
                Ok(StatementValue {
                    columns,
                    rows,
                    truncated: false,
                    affected,
                })
            } else {
                Ok(StatementValue {
                    columns,
                    rows,
                    truncated,
                    affected: None,
                })
            }
        }
    }
}

fn pg_text(row: &tokio_postgres::Row, index: usize) -> Option<String> {
    if let Ok(value) = row.try_get::<_, Option<String>>(index) {
        return value;
    }
    if let Ok(value) = row.try_get::<_, Option<i64>>(index) {
        return value.map(|number| number.to_string());
    }
    if let Ok(value) = row.try_get::<_, Option<i32>>(index) {
        return value.map(|number| number.to_string());
    }
    if let Ok(value) = row.try_get::<_, Option<f64>>(index) {
        return value.map(|number| number.to_string());
    }
    if let Ok(value) = row.try_get::<_, Option<bool>>(index) {
        return value.map(|number| number.to_string());
    }
    if let Ok(value) = row.try_get::<_, Option<Vec<u8>>>(index) {
        return value.map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    }
    None
}

pub fn clamp_limit(limit: u64) -> usize {
    usize::try_from(limit.clamp(1, 10_000)).unwrap_or(10_000)
}
