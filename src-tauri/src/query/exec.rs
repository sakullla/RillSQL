//! 在已经打开的 catalog session 上执行 SQL。不新开执行用的数据库连接。

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::connections::transaction::TransactionCoordinator;
use crate::connections::CancelTransport;
use crate::connections::CatalogSession;
use crate::transaction::{TransactionEffect, TransactionSnapshot};

use super::cancel::{self, CancelView};
use super::split::{self, is_write, Piece};
use super::transaction::StatementTrial;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementValue {
    pub columns: Vec<String>,
    pub column_types: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub truncated: bool,
    pub affected: Option<u64>,
}

/// 驱动报出的错误位置，相对于出错的那一条语句。
/// Char 是语句内从 0 开始的字符偏移（PostgreSQL 的 position 换算而来）；
/// Line 是语句内从 1 开始的行号（MySQL 的 "at line N"、SQL Server 的行号）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum ErrorAt {
    Char(usize),
    Line(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatementStop {
    /// at 只在驱动确实给出位置时才有；没有位置（如 SQLite）就是 None，不推测。
    /// code 是驱动给出的结构化错误码（PostgreSQL 的 SQLSTATE 等）；驱动不给时为 None，界面再从文本里找。
    Error { message: String, at: Option<ErrorAt>, code: Option<String> },
    Unknown { message: String },
    Cancelled { message: String },
}

/// 执行前的读取上限改写：驱动实际发出的语句比用户写的那条多了什么。
/// 只描述这一处改写，不改写规则本身（LIMIT/TOP/FETCH 的判定仍是 cap_result_sql_detail）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRewrite {
    /// limit（追加 LIMIT 或改大 LIMIT 数字）、fetch（改大 FETCH 数字）、top（追加 TOP 或改大 TOP 数字）。
    pub kind: String,
    /// 原语句里的字节位置：追加时是插入点（它之前的内容不变），替换时是被替换数字的起点。
    pub at: usize,
    /// 原语句里被替换掉的字节长度；追加时为 0。
    pub replaced: usize,
    /// 实际插入或替换进去的文本（含补的空格）。
    pub added: String,
}

impl AutoRewrite {
    /// 改写让实际语句比原语句多出的字节数。
    pub fn inserted_len(&self) -> usize {
        self.added.len().saturating_sub(self.replaced)
    }
}

/// 一条语句实际上会发出去的文本，以及它被自动改写过的地方。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CappedSql {
    pub sql: String,
    pub rewrite: Option<AutoRewrite>,
}

impl CappedSql {
    pub(crate) fn unchanged(sql: &str) -> Self {
        Self { sql: sql.to_owned(), rewrite: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementReport {
    /// 用户写的那条语句（草稿里的原文）；界面展示和定位都以它为准。
    pub sql: String,
    /// 驱动实际发出的语句：等于 sql，或带上了自动追加/放大的读取上限。
    pub sent_sql: String,
    /// 自动改写信息；没有改写时为 None（手写 LIMIT 且不超过上限也属于没有改写）。
    pub rewrite: Option<AutoRewrite>,
    pub status: &'static str,
    pub message: String,
    pub columns: Vec<String>,
    pub column_types: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub truncated: bool,
    pub affected: Option<u64>,
    /// 这一条语句自己的耗时（毫秒，向上取整）。被取消或没有执行的语句没有耗时。
    pub elapsed_ms: Option<u64>,
    /// 失败语句的驱动错误位置；只有 status 为 error 时可能有值。
    pub error_at: Option<ErrorAt>,
    /// 失败语句的驱动错误码（结构化取得的）；只有 status 为 error 时可能有值。
    pub error_code: Option<String>,
    /// 这条语句对连接级事务的效果；非关系型连接或没有影响时为 None。
    pub transaction: Option<TransactionEffect>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchReport {
    pub status: &'static str,
    pub statements: Vec<StatementReport>,
    pub cancel: CancelView,
    pub calls: usize,
    /// 批次结束时这条连接共享的事务快照；没有事务面时为 None。
    pub transaction: Option<TransactionSnapshot>,
    /// 批次里最后一条影响事务的语句的效果。
    pub transaction_effect: Option<TransactionEffect>,
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
    match crate::adapters::query::syntax_named(engine) {
        Some(syntax) => syntax.database_gate(requested),
        None => DatabaseGate::Refuse,
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
            at: None,
            code: None,
        };
    }
    if is_disconnect(&message) && is_write(sql) {
        return StatementStop::Unknown {
            message: format!("连接中断，无法确认写入是否完成。不会自动重放。{message}"),
        };
    }
    StatementStop::Error { message, at: None, code: None }
}

/// 执行前的改写只在一处插入或替换文本（行数上限）。把改写后文本里的字符偏移映射回原文：
/// 落在公共前缀里原样保留，落在公共后缀里减去长度差，落在改写片段里就停在改写起点。
pub fn map_rewritten_char(original: &str, rewritten: &str, offset: usize) -> usize {
    let source: Vec<char> = original.chars().collect();
    let sent: Vec<char> = rewritten.chars().collect();
    let prefix = source.iter().zip(sent.iter()).take_while(|(left, right)| left == right).count();
    let limit = source.len().min(sent.len()) - prefix;
    let suffix = source
        .iter()
        .rev()
        .zip(sent.iter().rev())
        .take(limit)
        .take_while(|(left, right)| left == right)
        .count();
    if offset <= prefix {
        return offset.min(source.len());
    }
    let tail_start = sent.len() - suffix;
    if offset >= tail_start {
        return (source.len() - (sent.len() - offset)).min(source.len());
    }
    prefix.min(source.len())
}

/// 驱动一层的失败：文本照旧，另外带上驱动给出的错误位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    pub at: Option<ErrorAt>,
    /// 驱动给出的结构化错误码；只在驱动错误对象里确实有时才填，不从文本猜。
    pub code: Option<String>,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self { message, at: None, code: None }
    }
}

/// 毫秒耗时，向上取整：真实执行过的语句至少记 1 ms，不把亚毫秒的执行写成 0。
pub fn elapsed_ms(spent: Duration) -> u64 {
    let micros = spent.as_micros();
    let millis = micros.div_ceil(1000).max(1);
    u64::try_from(millis).unwrap_or(u64::MAX)
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
/// 写法与单位按方言（适配器 SqlSyntax）；不支持会话级超时的引擎没有设置。
pub fn timeout_statements(engine: &str, timeout_ms: Option<u64>) -> Vec<String> {
    let ms = match timeout_ms {
        Some(ms) if ms > 0 => ms,
        _ => return Vec::new(),
    };
    crate::adapters::query::syntax_named(engine).map(|syntax| syntax.timeout_statements(ms)).unwrap_or_default()
}

/// 整批结束后把会话级超时设置复位，避免泄漏到目录连接。
pub fn timeout_reset_statements(engine: &str) -> Vec<String> {
    crate::adapters::query::syntax_named(engine).map(|syntax| syntax.timeout_reset_statements()).unwrap_or_default()
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

/// 驱动一次执行的结果：实际发出的语句与执行结果一起交回，
/// 执行层据此记录“真正跑的是哪条 SQL”，不再靠调用方猜。
pub struct DriverReport {
    pub sent: CappedSql,
    pub outcome: Result<StatementValue, StatementStop>,
    /// 驱动原始失败（含结构化错误码）。事务登记用它按方言判定事务影响；
    /// `StatementStop::Unknown` 不会丢掉错误码。
    pub failure: Option<Failure>,
}

impl DriverReport {
    /// 没有改写、直接发出原语句的结果。给测试替身用：真实驱动都经 cap_result_sql_detail。
    #[cfg(test)]
    pub fn unchanged(sql: &str, outcome: Result<StatementValue, StatementStop>) -> Self {
        Self { sent: CappedSql::unchanged(sql), outcome, failure: None }
    }
}

#[async_trait::async_trait]
pub trait StatementDriver: Send + Sync {
    async fn run(&self, sql: &str, limit: usize, cancel: &Arc<AtomicBool>) -> DriverReport;

    /// 方言的事务状态探针。它不是用户语句：不占语句序号，也不向结果区发布行。
    /// 默认实现按普通语句执行；真实驱动覆盖它，避免探针行进入结果区或打乱语句序号。
    async fn probe(&self, sql: &str) -> Option<StatementValue> {
        let cancel = Arc::new(AtomicBool::new(false));
        self.run(sql, 1, &cancel).await.outcome.ok()
    }

    /// 累计等待调用方“继续读取”的时间。语句耗时要扣掉它，不把用户停留算成执行时间。
    fn paused(&self) -> Duration {
        Duration::ZERO
    }
}

/// 逐条执行一批语句。`transactions` 是这条连接的协调器：有事务面时每条语句的意图与结果都要
/// 经过它登记，手写控制语句按方言映射后执行（R3、R8、R9、R10）。
pub async fn run_batch(
    parts: &[Piece],
    limit: usize,
    cancel_flag: &Arc<AtomicBool>,
    via_ssh: bool,
    driver: &dyn StatementDriver,
    transactions: Option<&TransactionCoordinator>,
) -> BatchReport {
    let mut statements = Vec::new();
    let mut calls = 0usize;
    let mut cancel = CancelView {
        phase: "none",
        message: String::new(),
        fresh_channel: false,
    };
    let mut status = "done";
    let mut effect = None;
    // 上一批留下的未知/层数不明先核对一次：确认后这一批的语句才能按真实状态登记（R10）。
    super::transaction::reconcile(transactions, driver).await;
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
        let plan = super::transaction::plan(transactions, &piece.text);
        let trial = super::transaction::attempt(transactions, plan, driver, limit, cancel_flag, &piece.text).await;
        if trial.attempted {
            calls += 1;
        }
        // 只把真正改变了事务状态的效果带到批次层：自动提交下的 Unchanged 会盖掉前面的隐式提交。
        if let Some(value) = trial.transaction.filter(|effect| *effect != TransactionEffect::Unchanged) {
            effect = Some(value);
        }
        let statement_status = trial.status;
        let (stop, remaining) = (trial.stop, trial.remaining);
        statements.push(statement_report(piece.text.clone(), trial));
        // 取消不等于回滚：事务效果已按方言登记，这里只收尾取消状态与未执行语句（R8）。
        if statement_status == "cancelled" {
            cancel::on_statement_end(&mut cancel, "cancelled");
            if cancel.phase == "none" {
                cancel = cancel::on_request(via_ssh);
                cancel.phase = "confirmed";
                cancel.message = "数据库已确认取消。".to_owned();
            }
        }
        if stop {
            for rest in &parts[index + 1..] {
                statements.push(skipped(rest.text.clone(), remaining));
            }
            status = batch_status(statement_status);
            break;
        }
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
    if statements.is_empty() {
        status = "error";
    }
    // 这一批留下了未知或层数不明时再核对一次：能确认的立即显示真实状态，不把猜测留给界面。
    super::transaction::reconcile(transactions, driver).await;
    BatchReport {
        status,
        statements,
        cancel,
        calls,
        transaction: transactions.and_then(|transactions| transactions.snapshot()),
        transaction_effect: effect,
    }
}

/// 批次整体结局：语句成功但事务结果未知时同样不能报完成（R10）。
fn batch_status(statement_status: &str) -> &'static str {
    match statement_status {
        "error" => "error",
        "cancelled" => "cancelled",
        _ => "unknown",
    }
}

fn statement_report(sql: String, trial: StatementTrial) -> StatementReport {
    let (columns, column_types, rows, truncated, affected) = match trial.value {
        Some(value) => (value.columns, value.column_types, value.rows, value.truncated, value.affected),
        None => (Vec::new(), Vec::new(), Vec::new(), false, None),
    };
    StatementReport {
        sql,
        sent_sql: trial.sent_sql,
        rewrite: trial.rewrite,
        status: trial.status,
        message: trial.message,
        columns,
        column_types,
        rows,
        truncated,
        affected,
        elapsed_ms: trial.elapsed_ms,
        error_at: trial.error_at,
        error_code: trial.error_code,
        transaction: trial.transaction,
    }
}

fn skipped(sql: String, message: &str) -> StatementReport {
    StatementReport {
        sql,
        // 没有发送到数据库，所以没有实际语句，也没有改写。
        sent_sql: String::new(),
        rewrite: None,
        status: "skipped",
        message: message.to_owned(),
        columns: Vec::new(),
        column_types: Vec::new(),
        rows: Vec::new(),
        truncated: false,
        affected: None,
        elapsed_ms: None,
        error_at: None,
        error_code: None,
        transaction: None,
    }
}

/// 把同一次结果流按块交给调用方。不负责再执行语句。
#[async_trait::async_trait]
pub trait RowSink: Send + Sync {
    async fn publish(&self, index: usize, value: &StatementValue);
    async fn pause(&self, cancel: &AtomicBool) -> Result<(), String>;
}

pub struct NopSink;

/// 把等待“继续读取”的时间记下来（微秒），其余原样转给真正的 sink。
struct PauseClock<'a> {
    inner: &'a dyn RowSink,
    paused: &'a AtomicU64,
}

#[async_trait::async_trait]
impl RowSink for PauseClock<'_> {
    async fn publish(&self, index: usize, value: &StatementValue) {
        self.inner.publish(index, value).await;
    }

    async fn pause(&self, cancel: &AtomicBool) -> Result<(), String> {
        let started = Instant::now();
        let result = self.inner.pause(cancel).await;
        let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.paused.fetch_add(micros, Ordering::SeqCst);
        result
    }
}

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

pub(crate) fn chunk_rows(limit: usize) -> usize {
    if limit <= 8 { limit.max(1) } else { (limit / 2).max(1) }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn reveal_rows(
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
    let total = rows.len();
    let mut remaining = rows.into_iter();
    let mut value = StatementValue { columns, column_types, rows: Vec::with_capacity(total), truncated: false, affected: None };
    loop {
        if cancel.load(Ordering::SeqCst) { return Err("已取消".to_owned()); }
        value.rows.extend(remaining.by_ref().take(step));
        let done = value.rows.len() == total;
        value.truncated = truncated && done && total > 0;
        value.affected = if done { affected } else { None };
        sink.publish(index, &value).await;
        if done { return Ok(value); }
        sink.pause(cancel).await?;
    }
}

/// 批处理进行中压住探活：正在跑的语句不该被空闲计时误判成断线（R4）。
struct BusyGuard(Arc<crate::connections::SessionHealth>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.note_idle();
    }
}

pub struct LiveDriver {
    session: tokio::sync::Mutex<crate::adapters::exec::Attached>,
    transport: CancelTransport,
    engine: String,
    sink: Arc<dyn RowSink>,
    statement_index: AtomicUsize,
    timeout_applied: bool,
    /// 等待调用方继续读取的累计微秒数；语句耗时扣掉这一段。
    paused_micros: AtomicU64,
    /// 只为 Drop 语义保留：批处理结束后释放探活忙标记。
    _busy: Option<BusyGuard>,
}

/// attach 阶段的失败也走断线分类：池里借不出连接通常就是断线（R4）。
pub(crate) fn attach_failure(transport: &CancelTransport, message: String) -> String {
    if is_disconnect(&message) {
        transport.note_disconnect();
    }
    message
}

impl LiveDriver {
    /// 整批只借出池里已有的那条连接。MySQL / MariaDB 归还时不发 COM_RESET_CONNECTION。
    pub async fn attach(session: CatalogSession, engine: &str, database: &str, transport: CancelTransport, timeout_ms: Option<u64>) -> Result<Self, String> {
        // 从借连接和会话初始化开始保护探活；高延迟握手也属于正在执行。
        let busy = transport.health().map(|health| {
            health.note_busy();
            BusyGuard(Arc::clone(health))
        });
        let setup = timeout_statements(engine, timeout_ms);
        let (attached, timeout_applied) = session.attach_query(engine, database, &transport, &setup).await?;
        Ok(Self {
            session: tokio::sync::Mutex::new(attached),
            transport,
            engine: engine.to_owned(),
            sink: Arc::new(NopSink),
            statement_index: AtomicUsize::new(0),
            timeout_applied,
            paused_micros: AtomicU64::new(0),
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
        self.session.lock().await.reset(&statements).await
    }

    pub fn set_sink(&mut self, sink: Arc<dyn RowSink>) {
        self.sink = sink;
    }
}

#[async_trait::async_trait]
impl StatementDriver for LiveDriver {
    async fn run(&self, sql: &str, limit: usize, cancel: &Arc<AtomicBool>) -> DriverReport {
        let index = self.statement_index.fetch_add(1, Ordering::SeqCst);
        let mut attached = self.session.lock().await;
        // 没有可查询的会话时一条语句都不会发出去：报告里也就不能带改写后的文本，
        // 否则界面会给一条从未执行的语句显示"实际执行的语句"和追加的 LIMIT。
        let capped = if attached.sends_sql() {
            // 读取上限改写只在这里做一次：驱动收到的就是要发出去的文本，报告里也用它。
            cap_result_sql_detail(&self.engine, sql, limit)
        } else {
            CappedSql::unchanged(sql)
        };
        let sent = capped.sql.as_str();
        let sink = PauseClock { inner: self.sink.as_ref(), paused: &self.paused_micros };
        let result: Result<StatementValue, Failure> = attached.run(&self.transport, sent, sql, limit, cancel, &sink, index).await;
        // 成功语句刷新空闲计时；断线类错误挂起 suspect 标记，交轮询探活确认（R4）。
        // 语句超时的报错同样带 timeout 字样，不算断线。
        match &result {
            Ok(_) => self.transport.note_activity(),
            Err(failure) => {
                if is_disconnect(&failure.message) && !is_statement_timeout(&failure.message) {
                    self.transport.note_disconnect();
                }
            }
        }
        // 事务登记要按方言看错误码：StatementStop 的归类可能已经把写入断连折成 Unknown。
        let failure = result.as_ref().err().cloned();
        let outcome = result.map_err(|failure| match classify_failure(sql, failure.message, cancel.load(Ordering::SeqCst)) {
            StatementStop::Error { message, .. } => StatementStop::Error { message, at: failure.at, code: failure.code },
            other => other,
        });
        DriverReport { sent: capped, outcome, failure }
    }

    fn paused(&self) -> Duration {
        Duration::from_micros(self.paused_micros.load(Ordering::SeqCst))
    }

    /// 状态探针：不经过用户结果区，也不占用语句序号（结果区按序号对齐用户语句）。
    async fn probe(&self, sql: &str) -> Option<StatementValue> {
        let mut attached = self.session.lock().await;
        let cancel = Arc::new(AtomicBool::new(false));
        let result = attached.run(&self.transport, sql, sql, 1, &cancel, &NopSink, usize::MAX).await;
        match &result {
            Ok(_) => self.transport.note_activity(),
            Err(failure) => {
                if is_disconnect(&failure.message) {
                    self.transport.note_disconnect();
                }
            }
        }
        result.ok()
    }
}

pub fn clamp_limit(limit: u64) -> usize {
    usize::try_from(limit.clamp(1, 10_000)).unwrap_or(10_000)
}

/// 给会返回行的语句加上不超过 cap + 1 的 LIMIT。多出来的一行只用来判断截断。
/// 方言自有写法（SQL Server 的 TOP）由适配器改写：语句以 SELECT 开头时改写第一个 SELECT，其余不改写或按计数改写。
/// 只返回文本；改写规则的对照断言用它，运行路径统一走 cap_result_sql_detail。
#[cfg(test)]
pub fn cap_result_sql(engine: &str, sql: &str, cap: usize) -> String {
    cap_result_sql_detail(engine, sql, cap).sql
}

/// 与 cap_result_sql 同一套规则，另外说明这句 SQL 被自动改写了哪一处（追加还是替换数字）。
pub fn cap_result_sql_detail(engine: &str, sql: &str, cap: usize) -> CappedSql {
    // 方言自有写法（T-SQL 的 TOP）由适配器改写。
    if let Some(capped) = crate::adapters::query::syntax_named(engine).and_then(|syntax| syntax.cap_rows(sql, cap)) {
        return capped;
    }
    let probe = cap.saturating_add(1);
    let (body, suffix) = peel_trailing_semi(sql);
    if !limit_target(body) {
        return CappedSql::unchanged(sql);
    }
    let scan = scan_top(engine, body);
    if scan.unsafe_limit || scan.has_into {
        return CappedSql::unchanged(sql);
    }
    if let Some(count) = scan.limit {
        if count.value <= probe as u64 {
            return CappedSql::unchanged(sql);
        }
        return replace_count(body, suffix, count.span, probe, "limit");
    }
    if let Some(fetch) = scan.fetch {
        if fetch.value <= probe as u64 {
            return CappedSql::unchanged(sql);
        }
        return replace_count(body, suffix, fetch.span, probe, "fetch");
    }
    let insert_at = scan.insert_at.unwrap_or(scan.end_of_code);
    let mut text = String::from("LIMIT ");
    text.push_str(&probe.to_string());
    insert_text(body, suffix, insert_at, &text, "limit")
}

/// 把替换数字的改写落成文本和改写记录。
pub(crate) fn replace_count(body: &str, suffix: &str, span: (usize, usize), probe: usize, kind: &str) -> CappedSql {
    let mut sql = String::new();
    sql.push_str(&body[..span.0]);
    sql.push_str(&probe.to_string());
    sql.push_str(&body[span.1..]);
    sql.push_str(suffix);
    let rewrite = AutoRewrite { kind: kind.to_owned(), at: span.0, replaced: span.1 - span.0, added: probe.to_string() };
    CappedSql { sql, rewrite: Some(rewrite) }
}

/// 在语句里的某一点追加改写文本；两侧按需补一个空格，避免和相邻标识符粘连。
fn insert_text(body: &str, suffix: &str, at: usize, text: &str, kind: &str) -> CappedSql {
    let mut added = String::new();
    if at > 0 && !ends_with_whitespace(&body[..at]) {
        added.push(' ');
    }
    added.push_str(text);
    if at < body.len() && !starts_with_whitespace(&body[at..]) {
        added.push(' ');
    }
    let mut sql = String::new();
    sql.push_str(&body[..at]);
    sql.push_str(&added);
    sql.push_str(&body[at..]);
    sql.push_str(suffix);
    let rewrite = AutoRewrite { kind: kind.to_owned(), at, replaced: 0, added };
    CappedSql { sql, rewrite: Some(rewrite) }
}

pub(crate) struct CountSpan {
    pub(crate) value: u64,
    pub(crate) span: (usize, usize),
}

pub(crate) struct TopScan {
    pub(crate) limit: Option<CountSpan>,
    pub(crate) fetch: Option<CountSpan>,
    insert_at: Option<usize>,
    end_of_code: usize,
    unsafe_limit: bool,
    pub(crate) has_into: bool,
}

fn ends_with_whitespace(text: &str) -> bool {
    text.chars().next_back().map(|ch| ch.is_whitespace()).unwrap_or(false)
}

fn starts_with_whitespace(text: &str) -> bool {
    text.chars().next().map(|ch| ch.is_whitespace()).unwrap_or(false)
}

pub(crate) fn limit_target(sql: &str) -> bool {
    matches!(split::first_keyword(sql).as_str(), "SELECT" | "WITH" | "VALUES" | "TABLE")
}

pub(crate) fn peel_trailing_semi(sql: &str) -> (&str, &str) {
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
        if crate::adapters::query::is_quote_byte(bytes[i]) {
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

pub(crate) fn scan_top(engine: &str, sql: &str) -> TopScan {
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

pub(crate) fn read_u64(sql: &str, i: usize) -> Option<(u64, usize)> {
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

pub(crate) fn skip_noise(sql: &str, mut i: usize) -> usize {
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
mod buffer_ownership_tests {
    use super::*;

    #[tokio::test]
    async fn revealing_rows_moves_the_payload_instead_of_copying_each_prefix() {
        let rows: Vec<_> = (0..10_000).map(|index| vec![Some(index.to_string()), Some("x".repeat(256))]).collect();
        let original = rows[0][1].as_ref().unwrap().as_ptr();
        let value = reveal_rows(&NopSink, 0, vec!["id".into(), "body".into()], vec!["int".into(), "text".into()], rows, true, None, 10_000, &AtomicBool::new(false)).await.unwrap();
        assert_eq!(value.rows.len(), 10_000);
        assert!(value.truncated);
        assert_eq!(value.rows[0][1].as_ref().unwrap().as_ptr(), original);
    }
}
