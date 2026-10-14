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

enum Attached {
    Sqlite(sqlx::SqlitePool),
    Mysql(mysql_async::Conn),
    Postgres(Arc<tokio_postgres::Client>),
    Scripted,
}

pub struct LiveDriver {
    session: tokio::sync::Mutex<Attached>,
    transport: CancelTransport,
}

impl LiveDriver {
    /// 整批只借出池里已有的那条连接。MySQL / MariaDB 归还时不发 COM_RESET_CONNECTION。
    pub async fn attach(session: CatalogSession, engine: &str, database: &str, transport: CancelTransport) -> Result<Self, String> {
        let attached = match session {
            CatalogSession::Scripted => Attached::Scripted,
            CatalogSession::Sqlite { pool, .. } => match database_gate("sqlite", "main", database) {
                DatabaseGate::Refuse => return Err(refuse_message("main", database)),
                _ => Attached::Sqlite(pool),
            },
            CatalogSession::Mysql(pool) => {
                let mut conn = pool.get_conn().await.map_err(|err| err.to_string())?;
                conn.reset_connection(false);
                if let Ok(Some(id)) = conn.query_first::<u64, _>("SELECT CONNECTION_ID()").await {
                    transport.set_mysql_connection_id(id);
                }
                align_mysql(&mut conn, engine, database).await?;
                Attached::Mysql(conn)
            }
            CatalogSession::Postgres(client) => {
                let current = postgres_current(&client).await?;
                match database_gate("postgres", &current, database) {
                    DatabaseGate::Refuse => return Err(refuse_message(&current, database)),
                    _ => Attached::Postgres(client),
                }
            }
        };
        Ok(Self {
            session: tokio::sync::Mutex::new(attached),
            transport,
        })
    }
}

#[async_trait::async_trait]
impl StatementDriver for LiveDriver {
    async fn run(&self, sql: &str, limit: usize, cancel: &Arc<AtomicBool>) -> Result<StatementValue, StatementStop> {
        let mut attached = self.session.lock().await;
        let result = match &mut *attached {
            Attached::Scripted => Err("这条已打开的会话没有可查询的数据库通道".to_owned()),
            Attached::Sqlite(pool) => sqlite_run(pool, sql, limit, cancel).await,
            Attached::Mysql(conn) => mysql_run(conn, &self.transport, sql, limit).await,
            Attached::Postgres(client) => postgres_run(client, sql, limit).await,
        };
        result.map_err(|message| classify_failure(sql, message, cancel.load(Ordering::SeqCst)))
    }
}

async fn align_mysql(conn: &mut mysql_async::Conn, engine: &str, database: &str) -> Result<(), String> {
    let value: Option<String> = conn.query_first("SELECT DATABASE()").await.map_err(|err| err.to_string())?;
    let current = value.unwrap_or_default();
    match database_gate(engine, &current, database) {
        DatabaseGate::Refuse => Err(refuse_message(&current, database)),
        DatabaseGate::Switch => {
            let statement = format!("USE {}", quote_mysql(database));
            conn.query_drop(statement).await.map_err(|err| err.to_string())?;
            Ok(())
        }
        DatabaseGate::Ready => Ok(()),
    }
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

async fn mysql_run(conn: &mut mysql_async::Conn, transport: &CancelTransport, sql: &str, limit: usize) -> Result<StatementValue, String> {
    if let Ok(Some(id)) = conn.query_first::<u64, _>("SELECT CONNECTION_ID()").await {
        transport.set_mysql_connection_id(id);
    }
    let bounded = cap_result_sql(sql, limit);
    let mut result = conn.query_iter(bounded).await.map_err(|err| err.to_string())?;
    let columns: Vec<String> = result.columns_ref().iter().map(|column| column.name_str().into_owned()).collect();
    if columns.is_empty() {
        let affected = result.affected_rows();
        // OK 包没有行。收掉这个结果集，同一条连接才能执行下一条。
        result.drop_result().await.map_err(|err| err.to_string())?;
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
        // LIMIT 已让服务器停住；再读一次只是收结束包，不是把剩余行读完。
        let _ = result.next().await.map_err(|err| err.to_string())?;
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
    let bounded = cap_result_sql(sql, limit);
    match client.prepare(&bounded).await {
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
                // 语句上的 LIMIT 已经让服务器停住，这里只把这个有限结果的协议尾读完。
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
            let messages = client.simple_query(&bounded).await.map_err(|err| err.to_string())?;
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
                            break;
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

/// 给会返回行的语句加上不超过 `cap + 1` 的 LIMIT。多出来的一行只用来判断截断。
pub fn cap_result_sql(sql: &str, cap: usize) -> String {
    let probe = cap.saturating_add(1);
    let (body, suffix) = peel_trailing_semi(sql);
    if !limit_target(body) {
        return sql.to_owned();
    }
    let scan = scan_top(body);
    if scan.unsafe_limit || scan.has_into {
        return sql.to_owned();
    }
    if let Some(count) = scan.limit {
        if count.value <= probe as u64 {
            return sql.to_owned();
        }
        let mut rewritten = String::new();
        rewritten.push_str(&body[..count.span.0]);
        rewritten.push_str(&probe.to_string());
        rewritten.push_str(&body[count.span.1..]);
        rewritten.push_str(suffix);
        return rewritten;
    }
    if let Some(fetch) = scan.fetch {
        if fetch.value <= probe as u64 {
            return sql.to_owned();
        }
        let mut rewritten = String::new();
        rewritten.push_str(&body[..fetch.span.0]);
        rewritten.push_str(&probe.to_string());
        rewritten.push_str(&body[fetch.span.1..]);
        rewritten.push_str(suffix);
        return rewritten;
    }
    let insert_at = scan.insert_at.unwrap_or(scan.end_of_code);
    let mut rewritten = String::new();
    rewritten.push_str(&body[..insert_at]);
    if insert_at > 0 && !ends_with_whitespace(&body[..insert_at]) {
        rewritten.push(' ');
    }
    rewritten.push_str("LIMIT ");
    rewritten.push_str(&probe.to_string());
    if insert_at < body.len() && !starts_with_whitespace(&body[insert_at..]) {
        rewritten.push(' ');
    }
    rewritten.push_str(&body[insert_at..]);
    rewritten.push_str(suffix);
    rewritten
}

struct CountSpan {
    value: u64,
    span: (usize, usize),
}

struct TopScan {
    limit: Option<CountSpan>,
    fetch: Option<CountSpan>,
    insert_at: Option<usize>,
    end_of_code: usize,
    unsafe_limit: bool,
    has_into: bool,
}

fn ends_with_whitespace(text: &str) -> bool {
    text.chars().next_back().map(|ch| ch.is_whitespace()).unwrap_or(false)
}

fn starts_with_whitespace(text: &str) -> bool {
    text.chars().next().map(|ch| ch.is_whitespace()).unwrap_or(false)
}

fn limit_target(sql: &str) -> bool {
    matches!(split::first_keyword(sql).as_str(), "SELECT" | "WITH" | "VALUES" | "TABLE")
}

fn peel_trailing_semi(sql: &str) -> (&str, &str) {
    let bytes = sql.as_bytes();
    let mut i = 0usize;
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut dollar: Option<String> = None;
    let mut semi_at = None;
    while i < bytes.len() {
        if let Some(tag) = dollar.clone() {
            let marker = format!("${tag}$");
            if sql[i..].starts_with(&marker) {
                semi_at = None;
                i += marker.len();
                dollar = None;
                continue;
            }
            semi_at = None;
            i += utf8_len(bytes[i]);
            continue;
        }
        if let Some(q) = quote {
            if bytes[i] == q {
                if q == b'\'' && i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                quote = None;
            } else if q == b'\'' && bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            semi_at = None;
            i += 1;
            continue;
        }
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        if bytes[i] == b'\'' || bytes[i] == b'"' || bytes[i] == b'`' {
            quote = Some(bytes[i]);
            semi_at = None;
            i += 1;
            continue;
        }
        if bytes[i] == b'$' {
            if let Some(tag) = dollar_tag(sql, i) {
                dollar = Some(tag.clone());
                semi_at = None;
                i += tag.len() + 2;
                continue;
            }
        }
        if bytes[i] == b'(' {
            depth += 1;
        } else if bytes[i] == b')' {
            depth -= 1;
        }
        if bytes[i] == b';' && depth == 0 {
            semi_at = Some(i);
        } else {
            semi_at = None;
        }
        i += utf8_len(bytes[i]);
    }
    match semi_at {
        Some(at) => (&sql[..at], &sql[at..]),
        None => (sql, ""),
    }
}

fn scan_top(sql: &str) -> TopScan {
    let bytes = sql.as_bytes();
    let mut i = 0usize;
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut dollar: Option<String> = None;
    let mut words: Vec<(String, usize, usize)> = Vec::new();
    let mut end_of_code = 0usize;
    while i < bytes.len() {
        if let Some(tag) = dollar.clone() {
            let marker = format!("${tag}$");
            if sql[i..].starts_with(&marker) {
                end_of_code = i + marker.len();
                i += marker.len();
                dollar = None;
                continue;
            }
            let len = utf8_len(bytes[i]);
            i += len;
            end_of_code = i;
            continue;
        }
        if let Some(q) = quote {
            if bytes[i] == q {
                if q == b'\'' && i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    end_of_code = i;
                    continue;
                }
                quote = None;
                i += 1;
                end_of_code = i;
                continue;
            }
            if q == b'\'' && bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
                end_of_code = i;
                continue;
            }
            i += utf8_len(bytes[i]);
            end_of_code = i;
            continue;
        }
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        if bytes[i] == b'\'' || bytes[i] == b'"' || bytes[i] == b'`' {
            quote = Some(bytes[i]);
            i += 1;
            end_of_code = i;
            continue;
        }
        if bytes[i] == b'$' {
            if let Some(tag) = dollar_tag(sql, i) {
                dollar = Some(tag.clone());
                i += tag.len() + 2;
                end_of_code = i;
                continue;
            }
        }
        if bytes[i] == b'(' {
            depth += 1;
            i += 1;
            end_of_code = i;
            continue;
        }
        if bytes[i] == b')' {
            depth -= 1;
            i += 1;
            end_of_code = i;
            continue;
        }
        if depth == 0 && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            words.push((sql[start..i].to_ascii_uppercase(), start, i));
            end_of_code = i;
            continue;
        }
        let len = utf8_len(bytes[i]);
        i += len;
        end_of_code = i;
    }
    interpret_words(sql, words, end_of_code)
}

fn note_insert(slot: &mut Option<usize>, at: usize) {
    if slot.map(|current| at < current).unwrap_or(true) {
        *slot = Some(at);
    }
}

fn interpret_words(sql: &str, words: Vec<(String, usize, usize)>, end_of_code: usize) -> TopScan {
    let mut limit = None;
    let mut fetch = None;
    let mut insert_at = None;
    let mut unsafe_limit = false;
    let mut has_into = false;
    let mut index = 0usize;
    while index < words.len() {
        let (word, start, end) = &words[index];
        if word == "INTO" {
            has_into = true;
        }
        if word == "LIMIT" {
            match parse_limit_count(sql, *end) {
                Some(count) => limit = Some(count),
                None if limit_expression(sql, *end) => unsafe_limit = true,
                None => {}
            }
        }
        if word == "FETCH" {
            if let Some(count) = parse_fetch_count(sql, &words, index) {
                fetch = Some(count);
            }
        }
        if word == "OFFSET" {
            note_insert(&mut insert_at, *start);
        }
        if word == "FOR" {
            let next = words.get(index + 1).map(|item| item.0.as_str());
            if matches!(next, Some("UPDATE") | Some("SHARE") | Some("NO") | Some("KEY")) {
                note_insert(&mut insert_at, *start);
            }
        }
        if word == "LOCK" {
            let next = words.get(index + 1).map(|item| item.0.as_str());
            if next == Some("IN") {
                note_insert(&mut insert_at, *start);
            }
        }
        index += 1;
    }
    if limit.is_some() || fetch.is_some() {
        insert_at = None;
    }
    TopScan {
        limit,
        fetch,
        insert_at,
        end_of_code,
        unsafe_limit,
        has_into,
    }
}

fn parse_limit_count(sql: &str, mut i: usize) -> Option<CountSpan> {
    i = skip_noise(sql, i);
    if sql[i..].to_ascii_uppercase().starts_with("ALL") && ident_boundary(sql, i + 3) {
        return Some(CountSpan {
            value: u64::MAX,
            span: (i, i + 3),
        });
    }
    let (first, next) = read_u64(sql, i)?;
    let mut after = skip_noise(sql, next);
    if after < sql.len() && sql.as_bytes()[after] == b',' {
        after = skip_noise(sql, after + 1);
        let (count, count_end) = read_u64(sql, after)?;
        return Some(CountSpan {
            value: count,
            span: (after, count_end),
        });
    }
    Some(CountSpan {
        value: first,
        span: (i, next),
    })
}

fn limit_expression(sql: &str, end: usize) -> bool {
    let i = skip_noise(sql, end);
    matches!(sql.as_bytes().get(i).copied(), Some(b'@') | Some(b'?') | Some(b':') | Some(b'('))
}

fn parse_fetch_count(sql: &str, words: &[(String, usize, usize)], index: usize) -> Option<CountSpan> {
    let kind = words.get(index + 1)?;
    if kind.0 != "FIRST" && kind.0 != "NEXT" {
        return None;
    }
    let start = skip_noise(sql, kind.2);
    let (value, end) = read_u64(sql, start)?;
    Some(CountSpan { value, span: (start, end) })
}

fn ident_boundary(sql: &str, index: usize) -> bool {
    sql.as_bytes().get(index).map(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_').unwrap_or(true)
}

fn read_u64(sql: &str, i: usize) -> Option<(u64, usize)> {
    let bytes = sql.as_bytes();
    if i >= bytes.len() || !bytes[i].is_ascii_digit() {
        return None;
    }
    let mut end = i + 1;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    let value = sql[i..end].parse::<u64>().ok()?;
    Some((value, end))
}

fn skip_noise(sql: &str, mut i: usize) -> usize {
    let bytes = sql.as_bytes();
    loop {
        if i >= bytes.len() {
            return i;
        }
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        return i;
    }
}

fn dollar_tag(sql: &str, start: usize) -> Option<String> {
    let rest = &sql[start + 1..];
    let end = rest.find('$')?;
    let tag = &rest[..end];
    if tag.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        Some(tag.to_owned())
    } else {
        None
    }
}

fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first < 0xE0 {
        2
    } else if first < 0xF0 {
        3
    } else {
        4
    }
}
