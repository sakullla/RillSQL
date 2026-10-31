//! 在已经打开的 catalog session 上执行 SQL。不新开执行用的数据库连接。

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use futures::TryStreamExt;
use mysql_async::prelude::Queryable;
use sqlx::Column;
use sqlx::Row;
use sqlx::TypeInfo;
use sqlx::ValueRef;

use crate::connections::CancelTransport;
use crate::connections::CatalogSession;

use super::cancel::{self, CancelView};
use super::split::{self, is_write, Piece};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementValue {
    pub columns: Vec<String>,
    pub column_types: Vec<String>,
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
    pub column_types: Vec<String>,
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
        "postgres" | "sqlserver" => DatabaseGate::Refuse,
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
    format!("当前连接打开的是 {current}，没有另开连接执行 {requested}")
}

pub fn classify_failure(sql: &str, message: String, cancel_requested: bool) -> StatementStop {
    if cancel_requested && cancel::is_cancel_error(&message) {
        return StatementStop::Cancelled { message };
    }
    if is_statement_timeout(&message) {
        return StatementStop::Error {
            message: format!("执行超时，语句已停止。{message}"),
        };
    }
    if is_disconnect(&message) && is_write(sql) {
        return StatementStop::Unknown {
            message: format!("连接中断，无法确认写入是否完成。不会自动重放。{message}"),
        };
    }
    StatementStop::Error { message }
}

/// 语句超时的报错文案。这些词同样带 timeout / interrupted，必须先于断连判定，否则写入会被误标成结果未知。
pub fn is_statement_timeout(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "statement timeout",
        "max_execution_time",
        "execution time exceeded",
        "max_statement_time",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// 生成本批要下发的会话级语句超时设置。目录连接是共享会话，整批结束由 `LiveDriver::finish` 复位。
/// MySQL 的 MAX_EXECUTION_TIME 只对 SELECT 生效；MariaDB 的单位是秒。
pub fn timeout_statements(engine: &str, timeout_ms: Option<u64>) -> Vec<String> {
    let ms = match timeout_ms {
        Some(ms) if ms > 0 => ms,
        _ => return Vec::new(),
    };
    match engine {
        "postgres" => vec![format!("SET statement_timeout = {ms}")],
        "mysql" => vec![format!("SET SESSION MAX_EXECUTION_TIME = {ms}")],
        "mariadb" => vec![format!("SET SESSION max_statement_time = {}", ms as f64 / 1000.0)],
        _ => Vec::new(),
    }
}

/// 整批结束后把会话级超时设置复位，避免泄漏到目录连接。
pub fn timeout_reset_statements(engine: &str) -> Vec<String> {
    match engine {
        "postgres" => vec!["SET statement_timeout = 0".to_owned()],
        "mysql" => vec!["SET SESSION MAX_EXECUTION_TIME = 0".to_owned()],
        "mariadb" => vec!["SET SESSION max_statement_time = 0".to_owned()],
        _ => Vec::new(),
    }
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
                    column_types: value.column_types,
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
                    column_types: Vec::new(),
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
        column_types: Vec::new(),
        rows: Vec::new(),
        truncated: false,
        affected: None,
    }
}

/// 把同一次结果流按块交给调用方。不负责再执行语句。
#[async_trait::async_trait]
pub trait RowSink: Send + Sync {
    async fn publish(&self, index: usize, value: &StatementValue);
    async fn pause(&self, cancel: &AtomicBool) -> Result<(), String>;
}

pub struct NopSink;

#[async_trait::async_trait]
impl RowSink for NopSink {
    async fn publish(&self, _index: usize, _value: &StatementValue) {}

    async fn pause(&self, _cancel: &AtomicBool) -> Result<(), String> {
        Ok(())
    }
}

/// 继续读取用的唤醒。先登记等待再看标志，避免通知发生在等待之前。
pub struct Pace {
    notify: tokio::sync::Notify,
    want: AtomicBool,
    generation: AtomicU64,
    done: AtomicBool,
}

impl Pace {
    pub fn new() -> Self {
        Self {
            notify: tokio::sync::Notify::new(),
            want: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            done: AtomicBool::new(false),
        }
    }

    pub fn request(&self) {
        self.want.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn kick(&self) {
        self.notify.notify_waiters();
    }

    pub fn finish(&self) {
        self.done.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn bump(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub async fn wait_changed(&self, before: u64) {
        loop {
            if self.generation() != before || self.done.load(Ordering::SeqCst) {
                return;
            }
            let notified = self.notify.notified();
            if self.generation() != before || self.done.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    pub async fn wait_continue(&self, cancel: &AtomicBool) -> Result<(), String> {
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err("已取消".to_owned());
            }
            let notified = self.notify.notified();
            if cancel.load(Ordering::SeqCst) {
                return Err("已取消".to_owned());
            }
            if self.want.swap(false, Ordering::SeqCst) {
                return Ok(());
            }
            if self.done.load(Ordering::SeqCst) {
                return Err("已取消".to_owned());
            }
            notified.await;
        }
    }
}

fn chunk_rows(limit: usize) -> usize {
    if limit <= 8 { limit.max(1) } else { (limit / 2).max(1) }
}

async fn reveal_rows(
    sink: &dyn RowSink,
    index: usize,
    columns: Vec<String>,
    column_types: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
    truncated: bool,
    affected: Option<u64>,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<StatementValue, String> {
    let step = chunk_rows(limit);
    if rows.is_empty() {
        let value = StatementValue {
            columns,
            column_types,
            rows,
            truncated: false,
            affected,
        };
        sink.publish(index, &value).await;
        return Ok(value);
    }
    let mut shown = 0usize;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消".to_owned());
        }
        let next = (shown + step).min(rows.len());
        let done = next == rows.len();
        let value = StatementValue {
            columns: columns.clone(),
            column_types: column_types.clone(),
            rows: rows[..next].to_vec(),
            truncated: truncated && done,
            affected: if done { affected } else { None },
        };
        sink.publish(index, &value).await;
        shown = next;
        if done {
            return Ok(value);
        }
        sink.pause(cancel).await?;
    }
}

enum Attached {
    Sqlite(sqlx::SqlitePool),
    Mysql(mysql_async::Conn),
    Postgres(Arc<tokio_postgres::Client>),
    Sqlserver(std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>),
    Scripted,
}

/// 批处理进行中压住探活：正在跑的语句不该被空闲计时误判成断线（R4）。
struct BusyGuard(Arc<crate::connections::SessionHealth>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.note_idle();
    }
}

pub struct LiveDriver {
    session: tokio::sync::Mutex<Attached>,
    transport: CancelTransport,
    engine: String,
    sink: Arc<dyn RowSink>,
    statement_index: AtomicUsize,
    timeout_applied: bool,
    /// 只为 Drop 语义保留：批处理结束后释放探活忙标记。
    _busy: Option<BusyGuard>,
}

/// attach 阶段的失败也走断线分类：池里借不出连接通常就是断线（R4）。
fn attach_failure(transport: &CancelTransport, message: String) -> String {
    if is_disconnect(&message) {
        transport.note_disconnect();
    }
    message
}

impl LiveDriver {
    /// 整批只借出池里已有的那条连接。MySQL / MariaDB 归还时不发 COM_RESET_CONNECTION。
    pub async fn attach(session: CatalogSession, engine: &str, database: &str, transport: CancelTransport, timeout_ms: Option<u64>) -> Result<Self, String> {
        let setup = timeout_statements(engine, timeout_ms);
        let attached = match session {
            CatalogSession::Scripted => Attached::Scripted,
            // Redis 没有 SQL 执行器（capability 门控）：命令行经 redis_exec 专项通道。
            CatalogSession::Redis(_) => return Err("Redis 不执行 SQL；命令在键浏览器里输入。".to_owned()),
            CatalogSession::Sqlite { pool, .. } => match database_gate("sqlite", "main", database) {
                DatabaseGate::Refuse => return Err(refuse_message("main", database)),
                _ => Attached::Sqlite(pool),
            },
            CatalogSession::Mysql(pool) => {
                let mut conn = pool
                    .get_conn()
                    .await
                    .map_err(|err| attach_failure(&transport, err.to_string()))?;
                conn.reset_connection(false);
                if let Ok(Some(id)) = conn.query_first::<u64, _>("SELECT CONNECTION_ID()").await {
                    transport.set_mysql_connection_id(id);
                }
                if let Err(err) = align_mysql(&mut conn, engine, database).await {
                    return Err(attach_failure(&transport, err));
                }
                if let Err(err) = run_mysql_statements(&mut conn, &setup).await {
                    return Err(attach_failure(&transport, err));
                }
                Attached::Mysql(conn)
            }
            CatalogSession::Postgres(client) => {
                let current = postgres_current(&client)
                    .await
                    .map_err(|err| attach_failure(&transport, err))?;
                match database_gate("postgres", &current, database) {
                    DatabaseGate::Refuse => return Err(refuse_message(&current, database)),
                    _ => {
                        if let Err(err) = run_postgres_statements(&client, &setup).await {
                            return Err(attach_failure(&transport, err));
                        }
                        Attached::Postgres(client)
                    }
                }
            }
            CatalogSession::Sqlserver(client) => {
                let current = sqlserver_current(&client)
                    .await
                    .map_err(|err| attach_failure(&transport, err))?;
                match database_gate("sqlserver", &current, database) {
                    DatabaseGate::Refuse => return Err(refuse_message(&current, database)),
                    _ => Attached::Sqlserver(client),
                }
            }
        };
        let busy = transport.health().map(|health| {
            health.note_busy();
            BusyGuard(Arc::clone(health))
        });
        Ok(Self {
            session: tokio::sync::Mutex::new(attached),
            transport,
            engine: engine.to_owned(),
            sink: Arc::new(NopSink),
            statement_index: AtomicUsize::new(0),
            timeout_applied: !setup.is_empty(),
            _busy: busy,
        })
    }

    /// 整批结束后复位会话级超时设置，避免这一批的限制泄漏到共享的目录连接。
    pub async fn finish(&self) -> Result<(), String> {
        if !self.timeout_applied {
            return Ok(());
        }
        let statements = timeout_reset_statements(&self.engine);
        if statements.is_empty() {
            return Ok(());
        }
        let mut attached = self.session.lock().await;
        match &mut *attached {
            Attached::Mysql(conn) => run_mysql_statements(conn, &statements).await,
            Attached::Postgres(client) => run_postgres_statements(client, &statements).await,
            _ => Ok(()),
        }
    }

    pub fn set_sink(&mut self, sink: Arc<dyn RowSink>) {
        self.sink = sink;
    }
}

#[async_trait::async_trait]
impl StatementDriver for LiveDriver {
    async fn run(&self, sql: &str, limit: usize, cancel: &Arc<AtomicBool>) -> Result<StatementValue, StatementStop> {
        let index = self.statement_index.fetch_add(1, Ordering::SeqCst);
        let mut attached = self.session.lock().await;
        let result = match &mut *attached {
            Attached::Scripted => Err("这个连接没有连着可查询的数据库".to_owned()),
            Attached::Sqlite(pool) => sqlite_run(pool, &self.engine, sql, limit, cancel, self.sink.as_ref(), index).await,
            Attached::Mysql(conn) => mysql_run(conn, &self.transport, &self.engine, sql, limit, cancel, self.sink.as_ref(), index).await,
            Attached::Postgres(client) => postgres_run(client, &self.engine, sql, limit, cancel, self.sink.as_ref(), index).await,
            Attached::Sqlserver(client) => sqlserver_run(client, &self.engine, sql, limit, cancel, self.sink.as_ref(), index).await,
        };
        // 成功语句刷新空闲计时；断线类错误挂起 suspect 标记，交轮询探活确认（R4）。
        // 语句超时的报错同样带 timeout 字样，不算断线。
        match &result {
            Ok(_) => self.transport.note_activity(),
            Err(message) => {
                if is_disconnect(message) && !is_statement_timeout(message) {
                    self.transport.note_disconnect();
                }
            }
        }
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

async fn run_mysql_statements(conn: &mut mysql_async::Conn, statements: &[String]) -> Result<(), String> {
    for statement in statements {
        conn.query_drop(statement).await.map_err(|err| err.to_string())?;
    }
    Ok(())
}

async fn run_postgres_statements(client: &tokio_postgres::Client, statements: &[String]) -> Result<(), String> {
    for statement in statements {
        client.simple_query(statement).await.map_err(|err| err.to_string())?;
    }
    Ok(())
}

async fn postgres_current(client: &tokio_postgres::Client) -> Result<String, String> {
    let row = client.query_one("SELECT current_database()", &[]).await.map_err(|err| err.to_string())?;
    row.try_get(0).map_err(|err| err.to_string())
}

async fn sqlserver_current(client: &std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>) -> Result<String, String> {
    let (_names, rows) = crate::grid::sqlserver_rows(client, "SELECT DB_NAME()").await?;
    match rows.first().and_then(|row| row.first()) {
        Some(crate::grid::Cell::Text(text)) => Ok(text.clone()),
        _ => Err("没有当前数据库".to_owned()),
    }
}

/// TDS 执行：会返回行的语句走 simple_query 收行集，其余走 execute 拿受影响行数。
/// SET 类语句（含 SET SHOWPLAN_ALL）一律走 simple_query：execute 是 sp_executesql，
/// 其中的 SET 只作用于内层批次、结束后即还原，外层会话上的 SHOWPLAN_ALL 不会生效。
async fn sqlserver_run(
    client: &std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>,
    engine: &str,
    sql: &str,
    limit: usize,
    cancel: &Arc<AtomicBool>,
    sink: &dyn RowSink,
    index: usize,
) -> Result<StatementValue, String> {
    let bounded = cap_result_sql(engine, sql, limit);
    let mut locked = client.lock().await;
    if !sqlserver_wants_rows(&bounded) {
        let done = locked.execute(bounded, &[]).await.map_err(|err| err.to_string())?;
        let value = StatementValue {
            columns: Vec::new(),
            column_types: Vec::new(),
            rows: Vec::new(),
            truncated: false,
            affected: Some(done.total()),
        };
        sink.publish(index, &value).await;
        return Ok(value);
    }
    let mut stream = locked.simple_query(bounded).await.map_err(|err| err.to_string())?;
    let mut columns: Vec<String> = Vec::new();
    let mut column_types: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut truncated = false;
    {
        use futures::TryStreamExt;
        while let Some(item) = stream.try_next().await.map_err(|err| err.to_string())? {
            if let Some(meta) = item.as_metadata() {
                if columns.is_empty() {
                    columns = meta.columns().iter().map(|column| column.name().to_owned()).collect();
                    column_types = meta.columns().iter().map(|column| tds_type_name(column.column_type()).to_owned()).collect();
                }
                continue;
            }
            if let Some(row) = item.into_row() {
                if columns.is_empty() {
                    columns = (0..row.len()).map(|column| format!("col{column}")).collect();
                    column_types = vec!["类型未知".to_owned(); columns.len()];
                }
                if rows.len() >= limit {
                    truncated = true;
                    break;
                }
                let cells = (0..columns.len()).map(|cell| tds_string_cell(&row, cell)).collect();
                rows.push(cells);
            }
        }
    }
    drop(stream);
    drop(locked);
    // SHOWPLAN_ALL 行集按 NodeId 排成稳定的父先子后顺序（层次有效性经 showplan_outline 验证）。
    let rows = order_showplan_results(&columns, rows);
    if columns.is_empty() {
        let value = StatementValue {
            columns,
            column_types,
            rows,
            truncated: false,
            affected: None,
        };
        sink.publish(index, &value).await;
        return Ok(value);
    }
    reveal_rows(sink, index, columns, column_types, rows, truncated, None, limit, cancel).await
}

/// SHOWPLAN 批次判定：以 SET SHOWPLAN_ALL 开头的语句即使整批到达也按收行路径执行。
pub fn is_showplan_statement(sql: &str) -> bool {
    let mut words = sql.trim_start().split_whitespace();
    matches!(
        (words.next(), words.next()),
        (Some(first), Some(second)) if first.eq_ignore_ascii_case("SET") && second.eq_ignore_ascii_case("SHOWPLAN_ALL")
    )
}

/// TDS 的收行路由：普通 SELECT 族、以及全部 SET 开头的会话设置语句。
fn sqlserver_wants_rows(sql: &str) -> bool {
    is_showplan_statement(sql) || split::first_keyword(sql) == "SET" || looks_like_rows(sql)
}

/// 把已收到的 SHOWPLAN_ALL 行集按 NodeId 升序排成父先子后。
/// 先把行集还原成 (NodeId, Parent, 操作) 三元组走 showplan_outline 验证层次完整
/// （父节点缺失或 NodeId 非数字时按原序返回，不猜）；有效层次才重排。
pub fn order_showplan_results(columns: &[String], rows: Vec<Vec<Option<String>>>) -> Vec<Vec<Option<String>>> {
    let names: Vec<String> = columns.iter().map(|column| column.trim().to_ascii_lowercase()).collect();
    // 只对 SHOWPLAN_ALL 形状应用：除 NodeId/Parent 外还须有 PhysicalOp/LogicalOp/StmtId 等
    // SHOWPLAN 专属列同现——普通查询恰好有 NodeId+Parent 数字列时不重排、保持原序。
    if !names.iter().any(|name| matches!(name.as_str(), "physicalop" | "logicalop" | "stmtid" | "estimaterows" | "totalsubtreecost")) {
        return rows;
    }
    let (Some(node_at), Some(parent_at)) = (names.iter().position(|name| name == "nodeid"), names.iter().position(|name| name == "parent")) else {
        return rows;
    };
    let text = |row: &Vec<Option<String>>, index: usize| -> String {
        row.get(index).and_then(|cell| cell.as_deref()).unwrap_or("").trim().to_owned()
    };
    let mut triples = Vec::with_capacity(rows.len());
    for (position, row) in rows.iter().enumerate() {
        let (Some(node), Some(parent)) = (text(row, node_at).parse::<i64>().ok(), text(row, parent_at).parse::<i64>().ok()) else {
            return rows;
        };
        triples.push((node, parent, position));
    }
    if showplan_outline(&triples.iter().map(|(node, parent, _)| (*node, *parent, String::new(), String::new())).collect::<Vec<_>>()).len() != rows.len() {
        return rows;
    }
    let mut indexed = triples;
    indexed.sort_by_key(|(node, _, _)| *node);
    indexed.into_iter().map(|(_, _, position)| rows[position].clone()).collect()
}

fn tds_string_cell(row: &tiberius::Row, index: usize) -> Option<String> {
    if let Ok(Some(text)) = row.try_get::<&str, _>(index) {
        return Some(text.to_owned());
    }
    if let Ok(Some(value)) = row.try_get::<i64, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<i32, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<i16, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<u8, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<f64, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(Some(value)) = row.try_get::<bool, _>(index) {
        return Some(if value { "1" } else { "0" }.to_owned());
    }
    if let Ok(Some(bytes)) = row.try_get::<&[u8], _>(index) {
        return Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect());
    }
    // decimal / numeric / money：Numeric 自带定点文本（含小数位）。
    if let Ok(Some(value)) = row.try_get::<tiberius::numeric::Numeric, _>(index) {
        return Some(tds_numeric_text(value));
    }
    // uniqueidentifier。
    if let Ok(Some(value)) = row.try_get::<uuid::Uuid, _>(index) {
        return Some(tds_uuid_text(value));
    }
    // xml（文档原文）：XmlData 只实现 FromSqlOwned，经 cells() 按 ColumnData 直接取。
    if let Some((_, data)) = row.cells().nth(index) {
        if let tiberius::ColumnData::Xml(Some(xml)) = data {
            return Some(xml.to_string());
        }
    }
    // datetime / datetime2 / smalldatetime → 本地日期时间文本。
    if let Ok(Some(value)) = row.try_get::<chrono::NaiveDateTime, _>(index) {
        return Some(tds_naive_datetime_text(value));
    }
    // date。
    if let Ok(Some(value)) = row.try_get::<chrono::NaiveDate, _>(index) {
        return Some(tds_naive_date_text(value));
    }
    // time(n)。
    if let Ok(Some(value)) = row.try_get::<chrono::NaiveTime, _>(index) {
        return Some(tds_naive_time_text(value));
    }
    // datetimeoffset（带时区偏移）。
    if let Ok(Some(value)) = row.try_get::<chrono::DateTime<chrono::Utc>, _>(index) {
        return Some(tds_datetime_offset_text(value));
    }
    None
}

/// 以下格式化器把 TDS 专用类型落成与其它引擎一致的网格文本；
/// 单独成函数是为了不经数据库就能单测格式口径。
pub fn tds_numeric_text(value: tiberius::numeric::Numeric) -> String {
    value.to_string()
}

pub fn tds_uuid_text(value: uuid::Uuid) -> String {
    value.to_string()
}

pub fn tds_naive_datetime_text(value: chrono::NaiveDateTime) -> String {
    // chrono Display：无小数 "2024-01-02 15:04:05"，有则补到 3/6/9 位。
    value.to_string()
}

pub fn tds_naive_date_text(value: chrono::NaiveDate) -> String {
    value.format("%Y-%m-%d").to_string()
}

pub fn tds_naive_time_text(value: chrono::NaiveTime) -> String {
    value.to_string()
}

pub fn tds_datetime_offset_text(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339()
}

pub fn tds_type_name(kind: tiberius::ColumnType) -> &'static str {
    use tiberius::ColumnType::*;
    match kind {
        Bit | Bitn => "bit",
        Int1 => "tinyint",
        Int2 => "smallint",
        Int4 => "int",
        Int8 | Intn => "bigint",
        Datetime4 | Datetime | Datetimen | Datetime2 => "datetime",
        DatetimeOffsetn => "datetimeoffset",
        Daten => "date",
        Timen => "time",
        Float4 | Floatn => "real",
        Float8 => "float",
        Money | Money4 | Decimaln | Numericn => "decimal",
        Guid => "uniqueidentifier",
        BigVarBin | BigBinary | Image => "varbinary",
        BigVarChar | BigChar | Text => "varchar",
        NVarchar | NChar | NText => "nvarchar",
        Xml => "xml",
        Udt | SSVariant | Null => "类型未知",
    }
}

fn quote_mysql(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

async fn sqlite_run(
    pool: &sqlx::SqlitePool,
    engine: &str,
    sql: &str,
    limit: usize,
    cancel: &Arc<AtomicBool>,
    sink: &dyn RowSink,
    index: usize,
) -> Result<StatementValue, String> {
    let mut conn = pool.acquire().await.map_err(|err| err.to_string())?;
    let watched = Arc::clone(cancel);
    conn.lock_handle()
        .await
        .map_err(|err| err.to_string())?
        .set_progress_handler(64, move || !watched.load(Ordering::SeqCst));
    let outcome = sqlite_dispatch(&mut conn, engine, sql, limit, cancel, sink, index).await;
    if let Ok(mut handle) = conn.lock_handle().await {
        handle.remove_progress_handler();
    }
    outcome
}

async fn sqlite_dispatch(
    conn: &mut sqlx::SqliteConnection,
    engine: &str,
    sql: &str,
    limit: usize,
    cancel: &Arc<AtomicBool>,
    sink: &dyn RowSink,
    index: usize,
) -> Result<StatementValue, String> {
    let safe = sqlx::AssertSqlSafe(cap_result_sql(engine, sql, limit));
    if looks_like_rows(sql) {
        let mut stream = sqlx::query(safe).fetch(&mut *conn);
        let mut columns = Vec::new();
        let mut column_types = Vec::new();
        let mut rows = Vec::new();
        let mut truncated = false;
        let mut pending: Option<sqlx::sqlite::SqliteRow> = None;
        let step = chunk_rows(limit);
        loop {
            if cancel.load(Ordering::SeqCst) {
                drop(stream);
                return Err("已取消".to_owned());
            }
            let row = if let Some(row) = pending.take() {
                row
            } else {
                match stream.try_next().await.map_err(|err| err.to_string())? {
                    Some(row) => row,
                    None => break,
                }
            };
            if columns.is_empty() {
                columns = row.columns().iter().map(|column| column.name().to_owned()).collect();
                column_types = sqlite_type_names(&row);
            }
            if rows.len() >= limit {
                truncated = true;
                break;
            }
            let width = columns.len();
            rows.push((0..width).map(|cell| sqlite_text(&row, cell)).collect());
            if rows.len() < limit && rows.len() % step == 0 {
                let peek = stream.try_next().await.map_err(|err| err.to_string())?;
                let partial = StatementValue {
                    columns: columns.clone(),
                    column_types: column_types.clone(),
                    rows: rows.clone(),
                    truncated: false,
                    affected: None,
                };
                sink.publish(index, &partial).await;
                match peek {
                    None => {
                        drop(stream);
                        return Ok(partial);
                    }
                    Some(next) => {
                        sink.pause(cancel).await?;
                        pending = Some(next);
                    }
                }
            }
        }
        drop(stream);
        let value = StatementValue {
            columns,
            column_types,
            rows,
            truncated,
            affected: None,
        };
        sink.publish(index, &value).await;
        return Ok(value);
    }
    let done = sqlx::query(safe).execute(&mut *conn).await.map_err(|err| err.to_string())?;
    let value = StatementValue {
        columns: Vec::new(),
        column_types: Vec::new(),
        rows: Vec::new(),
        truncated: false,
        affected: Some(done.rows_affected()),
    };
    sink.publish(index, &value).await;
    Ok(value)
}

fn looks_like_rows(sql: &str) -> bool {
    matches!(
        split::first_keyword(sql).as_str(),
        "SELECT" | "WITH" | "SHOW" | "EXPLAIN" | "PRAGMA" | "VALUES" | "TABLE" | "DESC" | "DESCRIBE"
    )
}

fn sqlite_type_names(row: &sqlx::sqlite::SqliteRow) -> Vec<String> {
    row.columns()
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let declared = column.type_info().name();
            if declared.is_empty() || declared == "NULL" {
                sqlite_storage_name(row, index).unwrap_or_else(|| "类型未知".to_owned())
            } else {
                declared.to_owned()
            }
        })
        .collect()
}

fn sqlite_storage_name(row: &sqlx::sqlite::SqliteRow, index: usize) -> Option<String> {
    let raw = row.try_get_raw(index).ok()?;
    let name = raw.type_info().name().to_owned();
    drop(raw);
    if name.is_empty() || name == "NULL" { None } else { Some(name) }
}

fn sqlite_text(row: &sqlx::sqlite::SqliteRow, index: usize) -> Option<String> {
    let storage = {
        let raw = row.try_get_raw(index).ok()?;
        let name = raw.type_info().name().to_owned();
        drop(raw);
        name
    };
    match storage.as_str() {
        "NULL" => None,
        "INTEGER" | "INT4" | "BOOLEAN" => {
            if let Ok(value) = row.try_get::<Option<i64>, _>(index) {
                return value.map(|number| number.to_string());
            }
            row.try_get::<Option<String>, _>(index).ok().flatten()
        }
        "BLOB" => row.try_get::<Option<Vec<u8>>, _>(index).ok().flatten().map(to_hex),
        _ => row.try_get::<Option<String>, _>(index).ok().flatten(),
    }
}

fn to_hex(bytes: Vec<u8>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

async fn mysql_run(
    conn: &mut mysql_async::Conn,
    transport: &CancelTransport,
    engine: &str,
    sql: &str,
    limit: usize,
    cancel: &Arc<AtomicBool>,
    sink: &dyn RowSink,
    index: usize,
) -> Result<StatementValue, String> {
    if let Ok(Some(id)) = conn.query_first::<u64, _>("SELECT CONNECTION_ID()").await {
        transport.set_mysql_connection_id(id);
    }
    let bounded = cap_result_sql(engine, sql, limit);
    let mut result = conn.query_iter(bounded).await.map_err(|err| err.to_string())?;
    let column_meta: Vec<(String, mysql_async::consts::ColumnType, u16)> = result
        .columns_ref()
        .iter()
        .map(|column| (column.name_str().into_owned(), column.column_type(), column.character_set()))
        .collect();
    let columns: Vec<String> = column_meta.iter().map(|column| column.0.clone()).collect();
    let column_types: Vec<String> = column_meta.iter().map(|column| mysql_type_name(column.1, column.2).to_owned()).collect();
    if columns.is_empty() {
        let affected = result.affected_rows();
        // OK 包没有行。收掉这个结果集，同一条连接才能执行下一条。
        result.drop_result().await.map_err(|err| err.to_string())?;
        let value = StatementValue {
            columns,
            column_types,
            rows: Vec::new(),
            truncated: false,
            affected: Some(affected),
        };
        sink.publish(index, &value).await;
        return Ok(value);
    }
    let mut rows = Vec::new();
    let mut truncated = false;
    let mut pending: Option<mysql_async::Row> = None;
    let step = chunk_rows(limit);
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = result.drop_result().await;
            return Err("已取消".to_owned());
        }
        let row = if let Some(row) = pending.take() {
            row
        } else {
            match result.next().await.map_err(|err| err.to_string())? {
                Some(row) => row,
                None => break,
            }
        };
        if rows.len() >= limit {
            truncated = true;
            break;
        }
        let cells = (0..columns.len())
            .map(|cell| mysql_cell(row.as_ref(cell).unwrap_or(&mysql_async::Value::NULL), column_meta[cell].1, column_meta[cell].2))
            .collect();
        rows.push(cells);
        if rows.len() < limit && rows.len() % step == 0 {
            let peek = result.next().await.map_err(|err| err.to_string())?;
            let partial = StatementValue {
                columns: columns.clone(),
                column_types: column_types.clone(),
                rows: rows.clone(),
                truncated: false,
                affected: None,
            };
            sink.publish(index, &partial).await;
            match peek {
                None => return Ok(partial),
                Some(next) => {
                    sink.pause(cancel).await?;
                    pending = Some(next);
                }
            }
        }
    }
    if truncated {
        // LIMIT 已让服务器停住；再读一次只是收结束包，不是把剩余行读完。
        let _ = result.next().await.map_err(|err| err.to_string())?;
    }
    let value = StatementValue {
        columns,
        column_types,
        rows,
        truncated,
        affected: None,
    };
    sink.publish(index, &value).await;
    Ok(value)
}

const MYSQL_BINARY_CHARSET: u16 = 63;

fn mysql_type_name(kind: mysql_async::consts::ColumnType, charset: u16) -> &'static str {
    use mysql_async::consts::ColumnType::*;
    let binary = charset == MYSQL_BINARY_CHARSET;
    match kind {
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "DECIMAL",
        MYSQL_TYPE_TINY => "TINYINT",
        MYSQL_TYPE_SHORT => "SMALLINT",
        MYSQL_TYPE_LONG => "INT",
        MYSQL_TYPE_FLOAT => "FLOAT",
        MYSQL_TYPE_DOUBLE => "DOUBLE",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "TIMESTAMP",
        MYSQL_TYPE_LONGLONG => "BIGINT",
        MYSQL_TYPE_INT24 => "MEDIUMINT",
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "DATE",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "TIME",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "DATETIME",
        MYSQL_TYPE_YEAR => "YEAR",
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING => {
            if binary {
                "VARBINARY"
            } else {
                "VARCHAR"
            }
        }
        MYSQL_TYPE_BIT => "BIT",
        MYSQL_TYPE_JSON => "JSON",
        MYSQL_TYPE_ENUM => "ENUM",
        MYSQL_TYPE_SET => "SET",
        MYSQL_TYPE_TINY_BLOB => {
            if binary {
                "TINYBLOB"
            } else {
                "TINYTEXT"
            }
        }
        MYSQL_TYPE_BLOB => {
            if binary {
                "BLOB"
            } else {
                "TEXT"
            }
        }
        MYSQL_TYPE_MEDIUM_BLOB => {
            if binary {
                "MEDIUMBLOB"
            } else {
                "MEDIUMTEXT"
            }
        }
        MYSQL_TYPE_LONG_BLOB => {
            if binary {
                "LONGBLOB"
            } else {
                "LONGTEXT"
            }
        }
        MYSQL_TYPE_STRING => {
            if binary {
                "BINARY"
            } else {
                "CHAR"
            }
        }
        MYSQL_TYPE_GEOMETRY => "GEOMETRY",
        MYSQL_TYPE_NULL => "NULL",
        MYSQL_TYPE_VECTOR => "VECTOR",
        MYSQL_TYPE_UNKNOWN | MYSQL_TYPE_TYPED_ARRAY => "类型未知",
    }
}

fn mysql_cell(value: &mysql_async::Value, kind: mysql_async::consts::ColumnType, charset: u16) -> Option<String> {
    use mysql_async::consts::ColumnType::*;
    if matches!(kind, MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL) {
        return match value {
            mysql_async::Value::NULL => None,
            mysql_async::Value::Bytes(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
            mysql_async::Value::Int(number) => Some(number.to_string()),
            mysql_async::Value::UInt(number) => Some(number.to_string()),
            mysql_async::Value::Float(_) | mysql_async::Value::Double(_) => None,
            mysql_async::Value::Date(_, _, _, _, _, _, _) | mysql_async::Value::Time(_, _, _, _, _, _) => None,
        };
    }
    let binary = charset == MYSQL_BINARY_CHARSET
        && matches!(
            kind,
            MYSQL_TYPE_TINY_BLOB
                | MYSQL_TYPE_BLOB
                | MYSQL_TYPE_MEDIUM_BLOB
                | MYSQL_TYPE_LONG_BLOB
                | MYSQL_TYPE_STRING
                | MYSQL_TYPE_VARCHAR
                | MYSQL_TYPE_VAR_STRING
        );
    match value {
        mysql_async::Value::NULL => None,
        mysql_async::Value::Bytes(bytes) => {
            if binary {
                Some(to_hex(bytes.clone()))
            } else {
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
        }
        mysql_async::Value::Int(number) => Some(number.to_string()),
        mysql_async::Value::UInt(number) => Some(number.to_string()),
        mysql_async::Value::Float(number) => Some(number.to_string()),
        mysql_async::Value::Double(number) => Some(number.to_string()),
        mysql_async::Value::Date(year, month, day, hour, minute, second, micros) => Some(mysql_date_text(kind, *year, *month, *day, *hour, *minute, *second, *micros)),
        mysql_async::Value::Time(negative, days, hour, minute, second, micros) => {
            Some(format!("{}{days} {hour:02}:{minute:02}:{second:02}.{micros:06}", if *negative { "-" } else { "" }))
        }
    }
}

fn mysql_date_text(kind: mysql_async::consts::ColumnType, year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8, micros: u32) -> String {
    use mysql_async::consts::ColumnType::*;
    let date = format!("{year:04}-{month:02}-{day:02}");
    let time = if micros == 0 {
        format!("{hour:02}:{minute:02}:{second:02}")
    } else {
        format!("{hour:02}:{minute:02}:{second:02}.{micros:06}")
    };
    match kind {
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => date,
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => time,
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 | MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => format!("{date} {time}"),
        _ if hour == 0 && minute == 0 && second == 0 && micros == 0 => date,
        _ => format!("{date} {time}"),
    }
}

async fn postgres_run(
    client: &tokio_postgres::Client,
    engine: &str,
    sql: &str,
    limit: usize,
    cancel: &Arc<AtomicBool>,
    sink: &dyn RowSink,
    index: usize,
) -> Result<StatementValue, String> {
    let bounded = cap_result_sql(engine, sql, limit);
    let prepared = match client.prepare(&bounded).await {
        Ok(statement) => {
            let columns = statement.columns().iter().map(|column| column.name().to_owned()).collect::<Vec<_>>();
            let types = statement.columns().iter().map(|column| column.type_().name().to_owned()).collect::<Vec<_>>();
            drop(statement);
            Some((columns, types))
        }
        Err(_) => None,
    };
    let messages = client.simple_query(&bounded).await.map_err(|err| err.to_string())?;
    let mut columns = prepared.as_ref().map(|item| item.0.clone()).unwrap_or_default();
    let mut types = prepared.as_ref().map(|item| item.1.clone()).unwrap_or_default();
    let mut rows = Vec::new();
    let mut affected = None;
    let mut truncated = false;
    for message in messages {
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消".to_owned());
        }
        match message {
            tokio_postgres::SimpleQueryMessage::RowDescription(description) => {
                if columns.is_empty() {
                    columns = description.iter().map(|column| column.name().to_owned()).collect();
                }
            }
            tokio_postgres::SimpleQueryMessage::Row(row) => {
                if rows.len() >= limit {
                    truncated = true;
                    break;
                }
                let width = row.len();
                if columns.is_empty() {
                    columns = (0..width).map(|column| format!("col{}", column + 1)).collect();
                }
                if types.is_empty() {
                    types = vec!["类型未知".to_owned(); columns.len().max(width)];
                } else if types.len() < width {
                    types.resize(width, "类型未知".to_owned());
                }
                let cells = (0..width).map(|cell| row.get(cell).map(str::to_owned)).collect();
                rows.push(cells);
            }
            tokio_postgres::SimpleQueryMessage::CommandComplete(count) => affected = Some(count),
            _ => {}
        }
    }
    if columns.is_empty() {
        let value = StatementValue {
            columns,
            column_types: Vec::new(),
            rows,
            truncated: false,
            affected,
        };
        sink.publish(index, &value).await;
        return Ok(value);
    }
    if types.is_empty() {
        types = vec!["类型未知".to_owned(); columns.len()];
    }
    reveal_rows(sink, index, columns, types, rows, truncated, None, limit, cancel).await
}

/// SQL Server 的执行计划批次：SET SHOWPLAN_ALL ON 后服务器只编译不执行，读完行集再关掉。
/// 与前端 src/query/plan.ts 的 sqlserver 形态互为镜像；这里钉住批次文本的契约。
pub fn showplan_batch(statement: &str) -> String {
    let body = statement.trim().trim_end_matches(';');
    format!("SET SHOWPLAN_ALL ON;\n{body};\nSET SHOWPLAN_ALL OFF")
}

/// SHOWPLAN_ALL 行集的层次展开（纯函数）：(NodeId, Parent, 操作, 估算行数) 按父节点深度排成缩进大纲。
/// 根节点的 Parent 不在结果里时深度为 0；父节点缺失按根处理，成环按根处理。
pub fn showplan_outline(rows: &[(i64, i64, String, String)]) -> Vec<(usize, String, String)> {
    let ids: Vec<i64> = rows.iter().map(|(node, _, _, _)| *node).collect();
    let depth_of = |node: i64| -> usize {
        let mut seen = std::collections::HashSet::new();
        let mut depth = 0usize;
        let mut current = node;
        loop {
            if !seen.insert(current) {
                return depth;
            }
            let Some((_, parent, _, _)) = rows.iter().find(|(candidate, _, _, _)| *candidate == current) else {
                return depth;
            };
            if *parent == current || !ids.contains(parent) {
                return depth;
            }
            depth += 1;
            current = *parent;
        }
    };
    rows.iter()
        .map(|(node, _, operation, est_rows)| (depth_of(*node), operation.clone(), est_rows.clone()))
        .collect()
}

pub fn clamp_limit(limit: u64) -> usize {
    usize::try_from(limit.clamp(1, 10_000)).unwrap_or(10_000)
}

/// 给会返回行的语句加上不超过 `cap + 1` 的 LIMIT。多出来的一行只用来判断截断。
/// SQL Server 走 TOP：语句以 SELECT 开头时改写第一个 SELECT，其余（WITH、FETCH 已有）不改写或按计数改写。
pub fn cap_result_sql(engine: &str, sql: &str, cap: usize) -> String {
    if engine == "sqlserver" {
        return cap_result_sql_sqlserver(sql, cap);
    }
    let probe = cap.saturating_add(1);
    let (body, suffix) = peel_trailing_semi(sql);
    if !limit_target(body) {
        return sql.to_owned();
    }
    let scan = scan_top(engine, body);
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

/// T-SQL 的行数上限：SELECT 直接改写为 SELECT TOP (n)；FETCH NEXT 已有时只改数字；
/// TOP 已有时只改数字；WITH 开头的语句不改写（TOP 需要落在内层 SELECT，盲改会破坏语义）。
fn cap_result_sql_sqlserver(sql: &str, cap: usize) -> String {
    let probe = cap.saturating_add(1);
    let (body, suffix) = peel_trailing_semi(sql);
    if !limit_target(body) {
        return sql.to_owned();
    }
    let scan = scan_top("sqlserver", body);
    if scan.has_into {
        return sql.to_owned();
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
    if let Some(top) = scan_top_n(body) {
        if top.value <= probe as u64 {
            return sql.to_owned();
        }
        let mut rewritten = String::new();
        rewritten.push_str(&body[..top.span.0]);
        rewritten.push_str(&probe.to_string());
        rewritten.push_str(&body[top.span.1..]);
        rewritten.push_str(suffix);
        return rewritten;
    }
    if split::first_keyword(body) != "SELECT" {
        return sql.to_owned();
    }
    let mut rewritten = String::new();
    let cut = body.to_ascii_uppercase().find("SELECT").unwrap_or(0) + "SELECT".len();
    rewritten.push_str(&body[..cut]);
    rewritten.push_str(" TOP (");
    rewritten.push_str(&probe.to_string());
    rewritten.push(')');
    rewritten.push_str(&body[cut..]);
    rewritten.push_str(suffix);
    rewritten
}

/// 顶层 TOP n 的数量区间。TOP 与数量之间允许括号。
fn scan_top_n(sql: &str) -> Option<CountSpan> {
    let (words, _) = split::top_level_words("sqlserver", sql);
    let mut index = 0usize;
    while index < words.len() {
        if words[index].0 == "TOP" {
            let mut at = skip_noise(sql, words[index].2);
            let bytes = sql.as_bytes();
            if at < bytes.len() && bytes[at] == b'(' {
                at = skip_noise(sql, at + 1);
            }
            let (value, end) = read_u64(sql, at)?;
            return Some(CountSpan { value, span: (at, end) });
        }
        index += 1;
    }
    None
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

fn scan_top(engine: &str, sql: &str) -> TopScan {
    let (words, end_of_code) = split::top_level_words(engine, sql);
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

#[cfg(test)]
mod mysql_value_tests {
    use super::{mysql_cell, mysql_type_name};
    use mysql_async::consts::ColumnType::*;

    #[test]
    fn charset_splits_text_from_binary() {
        let text = mysql_async::Value::Bytes("你好".as_bytes().to_vec());
        let binary = mysql_async::Value::Bytes(vec![0xff, 0x00, 0x41]);
        assert_eq!(mysql_type_name(MYSQL_TYPE_BLOB, 45), "TEXT");
        assert_eq!(mysql_cell(&text, MYSQL_TYPE_BLOB, 45).as_deref(), Some("你好"));
        assert_eq!(mysql_type_name(MYSQL_TYPE_TINY_BLOB, 33), "TINYTEXT");
        assert_eq!(mysql_type_name(MYSQL_TYPE_MEDIUM_BLOB, 45), "MEDIUMTEXT");
        assert_eq!(mysql_type_name(MYSQL_TYPE_LONG_BLOB, 45), "LONGTEXT");
        assert_eq!(mysql_type_name(MYSQL_TYPE_BLOB, 63), "BLOB");
        assert_eq!(mysql_type_name(MYSQL_TYPE_TINY_BLOB, 63), "TINYBLOB");
        assert_eq!(mysql_type_name(MYSQL_TYPE_MEDIUM_BLOB, 63), "MEDIUMBLOB");
        assert_eq!(mysql_type_name(MYSQL_TYPE_LONG_BLOB, 63), "LONGBLOB");
        assert_eq!(mysql_cell(&binary, MYSQL_TYPE_BLOB, 63).as_deref(), Some("ff0041"));
        assert_eq!(mysql_type_name(MYSQL_TYPE_VAR_STRING, 63), "VARBINARY");
        assert_eq!(mysql_type_name(MYSQL_TYPE_VARCHAR, 63), "VARBINARY");
        assert_eq!(mysql_type_name(MYSQL_TYPE_STRING, 63), "BINARY");
        assert_eq!(mysql_cell(&binary, MYSQL_TYPE_VAR_STRING, 63).as_deref(), Some("ff0041"));
        assert_eq!(mysql_cell(&binary, MYSQL_TYPE_STRING, 63).as_deref(), Some("ff0041"));
        assert_eq!(mysql_type_name(MYSQL_TYPE_VAR_STRING, 45), "VARCHAR");
        assert_eq!(mysql_type_name(MYSQL_TYPE_STRING, 45), "CHAR");
        assert_eq!(mysql_cell(&mysql_async::Value::Bytes(b"abc".to_vec()), MYSQL_TYPE_VAR_STRING, 45).as_deref(), Some("abc"));
        assert_eq!(mysql_cell(&mysql_async::Value::Bytes(b"12.50".to_vec()), MYSQL_TYPE_NEWDECIMAL, 63).as_deref(), Some("12.50"));
        assert_eq!(mysql_cell(&mysql_async::Value::Date(2024, 1, 2, 0, 0, 0, 0), MYSQL_TYPE_DATE, 63).as_deref(), Some("2024-01-02"));
    }
}
