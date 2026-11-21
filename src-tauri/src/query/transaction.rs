//! 查询批次与连接级事务协调器的接线（ADR-1、ADR-2、ADR-3）。
//!
//! 一个查询批次在同一个业务租约里逐条执行。这里把每条语句的**意图**与驱动的**结果**交给
//! [`TransactionCoordinator`]：手写控制语句先经方言分类、预检与 SQL 映射，执行后按数据库确认结果登记；
//! 普通语句按方言报告成功、失败、中止、隐式提交与未知（R3、R8、R9、R10）。
//!
//! 两条硬规则：
//!
//! - 所有登记都在数据库给出结果之后；预检拒绝的语句一律不发送，也不会为了“执行成功”换会话。
//! - 这里从不重放语句，也不因一条语句失败而提交或回滚整个用户事务；数据库自己结束的事务只按未知保留。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::connections::transaction::TransactionCoordinator;
use crate::transaction::{
    ControlMatch, ControlOutcome, ControlSql, ControlStatement, ImplicitCommitVerdict, StatementFailure, StatementOutcome, TransactionEffect, TransactionState,
    UnknownReason,
};

use super::exec::{elapsed_ms, AutoRewrite, DriverReport, ErrorAt, Failure, StatementDriver, StatementStop, StatementValue};

/// 隐式提交已经发生：此前的修改不能再由原事务回滚（R9）。
const IMPLICIT_COMMIT_NOTE: &str = "这条语句执行前，数据库已隐式提交当前事务；此前的修改不能再通过原事务回滚。";
/// 无法静态确认这条语句是否隐式提交：不宣称发生，也不宣称事务仍然活动（R9、R10）。
const UNCERTAIN_IMPLICIT_COMMIT_NOTE: &str = "无法确认这条语句是否让数据库隐式提交；事务状态标为未知。";
/// 会隐式提交的语句在结果未知时：既不能宣称已经提交，也不能宣称仍可回滚。
const UNCONFIRMED_IMPLICIT_COMMIT_NOTE: &str = "无法确认这条语句是否已隐式提交当前事务；事务结果未知。";
/// 控制语句的结果没有返回。
const CONTROL_UNCONFIRMED_NOTE: &str = "控制语句的结果没有返回，事务结果未知；没有重放这条语句。";
/// 控制语句执行了，但事务结局仍无法确认（例如更早的未知事务）。
const CONTROL_UNRESOLVED_NOTE: &str = "事务结果仍无法确认；没有重放这条语句，请用独立查询核对。";
/// 控制语句失败并把事务置为中止。
const CONTROL_ABORTED_NOTE: &str = "控制语句失败，事务被数据库置为中止；只能回滚或回滚到保存点。";
/// 以事务控制关键字开头、但形态无法确认的手写语句。
const AMBIGUOUS_CONTROL_NOTE: &str = "这条语句以事务控制关键字开头，但实际效果无法确认；事务状态标为未知。";
/// 普通语句失败并把事务置为中止（PostgreSQL 类）。
const ABORTED_STATEMENT_NOTE: &str = "语句失败，事务已被数据库置为中止；只能回滚或回滚到保存点。";
/// 数据库在语句期间自行回滚了整个事务（MySQL 死锁、SQLite 部分错误）。
const SERVER_ROLLBACK_NOTE: &str = "数据库可能已自行回滚整个事务，最终结果无法确认；请用独立查询核对。";

/// 发送前为一条语句定下的执行方式。
pub(crate) enum StatementPlan {
    /// 普通语句（或连接没有事务面）：按用户文本发送，按方言登记事务效果。
    Ordinary,
    /// 明确的事务控制语句：发送方言映射后的 SQL，执行后登记控制结果。
    Control { statement: ControlStatement, sql: ControlSql },
    /// 以控制关键字开头但形态无法确认：执行后只能按未知登记，不能当成普通语句忽略。
    Ambiguous,
    /// 本地拒绝：不发送，批次以失败结束。
    Refused(String),
}

/// 一次语句尝试：驱动给出的结果与登记后的事务效果。
pub(crate) struct StatementTrial {
    pub status: &'static str,
    pub message: String,
    pub sent_sql: String,
    pub rewrite: Option<AutoRewrite>,
    pub value: Option<StatementValue>,
    pub elapsed_ms: Option<u64>,
    pub error_at: Option<ErrorAt>,
    pub error_code: Option<String>,
    /// 这条语句对事务的效果；没有事务面或没有登记时为 None。
    pub transaction: Option<TransactionEffect>,
    /// 语句是否真的交给驱动执行（本地拒绝与不需要下发的控制语句不计）。
    pub attempted: bool,
    /// 语句是“形态无法确认的手写控制语句”：这一批不再用探针改写它的未知结果（R10）。
    pub ambiguous_control: bool,
    /// 这一步之后不能再发送后续语句。
    pub stop: bool,
    /// 后续语句没有发送的原因。
    pub remaining: &'static str,
}

/// 按方言决定一条语句怎么执行。预检拒绝的控制语句在这里就被拦下，不会下发（R3）。
pub(crate) fn plan(transactions: Option<&TransactionCoordinator>, body: &str) -> StatementPlan {
    let Some(transactions) = transactions.filter(|transactions| transactions.is_transactional()) else {
        return StatementPlan::Ordinary;
    };
    match transactions.classify_control(body) {
        ControlMatch::Control(statement) => match transactions.prepare(&statement) {
            Ok(sql) => StatementPlan::Control { statement, sql },
            Err(error) => StatementPlan::Refused(format!("{error}（这条语句没有发送）")),
        },
        ControlMatch::Ambiguous => StatementPlan::Ambiguous,
        ControlMatch::Ordinary => StatementPlan::Ordinary,
    }
}

/// 执行一条语句并把结果登记到连接级事务。`user_sql` 是用户写的那条语句（错误位置映射回它）。
pub(crate) async fn attempt(
    transactions: Option<&TransactionCoordinator>,
    plan: StatementPlan,
    driver: &dyn StatementDriver,
    limit: usize,
    cancel: &Arc<AtomicBool>,
    user_sql: &str,
) -> StatementTrial {
    match plan {
        StatementPlan::Refused(message) => refused(message),
        StatementPlan::Control { statement, sql } => match sql {
            // 不需要单独下发（SQLite 的事务模式随下一次 BEGIN 生效）：只登记设置。
            ControlSql::NoStatement => acknowledge(transactions, statement, ControlOutcome::Confirmed, None),
            ControlSql::Unsupported => refused("这条事务控制语句在当前数据库上没有对应的执行方式（这条语句没有发送）".to_owned()),
            ControlSql::Statement(sql) => {
                let ran = run(driver, limit, cancel, &sql).await;
                let outcome = control_result(transactions, &statement, &ran);
                acknowledge(transactions, statement, outcome, Some(ran))
            }
        },
        StatementPlan::Ambiguous => {
            let ran = run(driver, limit, cancel, user_sql).await;
            ambiguous(transactions, ran)
        }
        StatementPlan::Ordinary => {
            let ran = run(driver, limit, cancel, user_sql).await;
            ordinary(transactions, user_sql, ran)
        }
    }
}

/// 驱动一次执行的实际结果。`spent` 已扣掉等待调用方继续读取的时间。
struct Ran {
    sent: String,
    rewrite: Option<AutoRewrite>,
    outcome: Result<StatementValue, StatementStop>,
    /// 驱动原始失败（错误码与位置）：按方言判定事务影响时用它，不从文本猜。
    failure: Option<Failure>,
    spent: Duration,
}

impl Ran {
    fn status(&self) -> &'static str {
        match &self.outcome {
            Ok(_) => "ok",
            Err(StatementStop::Error { .. }) => "error",
            Err(StatementStop::Unknown { .. }) => "unknown",
            Err(StatementStop::Cancelled { .. }) => "cancelled",
        }
    }

    /// 失败语句的驱动文案；成功语句为空。
    fn message(&self) -> String {
        match &self.outcome {
            Ok(_) => String::new(),
            Err(StatementStop::Error { message, .. } | StatementStop::Unknown { message } | StatementStop::Cancelled { message }) => message.clone(),
        }
    }

    /// 驱动给出的错误码与原文：`StatementStop` 与原始失败里有一份就够。
    fn failure_parts(&self) -> (Option<&str>, &str) {
        let stop_message = match &self.outcome {
            Ok(_) => "",
            Err(StatementStop::Error { message, .. } | StatementStop::Unknown { message } | StatementStop::Cancelled { message }) => message.as_str(),
        };
        let message = self.failure.as_ref().map(|failure| failure.message.as_str()).unwrap_or(stop_message);
        let stop_code = match &self.outcome {
            Err(StatementStop::Error { code, .. }) => code.as_deref(),
            _ => None,
        };
        let code = self.failure.as_ref().and_then(|failure| failure.code.as_deref()).or(stop_code);
        (code, message)
    }
}

impl StatementTrial {
    fn new(ran: Ran) -> Self {
        let status = ran.status();
        let message = ran.message();
        let (error_at, error_code, value) = match ran.outcome {
            Ok(value) => (None, None, Some(value)),
            Err(StatementStop::Error { at, code, .. }) => (at, code, None),
            Err(StatementStop::Unknown { .. } | StatementStop::Cancelled { .. }) => (None, None, None),
        };
        Self {
            status,
            message,
            sent_sql: ran.sent,
            rewrite: ran.rewrite,
            value,
            // 被取消的语句只标“已取消”，不报一个不完整的耗时。
            elapsed_ms: if status == "cancelled" { None } else { Some(elapsed_ms(ran.spent)) },
            error_at,
            error_code,
            transaction: None,
            attempted: true,
            ambiguous_control: false,
            stop: status != "ok",
            remaining: "未执行",
        }
    }
}

fn refused(message: String) -> StatementTrial {
    StatementTrial {
        status: "error",
        message,
        sent_sql: String::new(),
        rewrite: None,
        value: None,
        elapsed_ms: None,
        error_at: None,
        error_code: None,
        transaction: None,
        attempted: false,
        ambiguous_control: false,
        stop: true,
        remaining: "未执行",
    }
}

async fn run(driver: &dyn StatementDriver, limit: usize, cancel: &Arc<AtomicBool>, sql: &str) -> Ran {
    let paused_before = driver.paused();
    let started = Instant::now();
    let report = driver.run(sql, limit, cancel).await;
    let spent = started.elapsed().saturating_sub(driver.paused().saturating_sub(paused_before));
    let DriverReport { sent, outcome, failure } = report;
    Ran {
        sent: sent.sql,
        rewrite: sent.rewrite,
        outcome,
        failure,
        spent,
    }
}

/// 普通语句：成功/失败按方言登记，MySQL/MariaDB 的隐式提交单独报告（R9）。
fn ordinary(transactions: Option<&TransactionCoordinator>, body: &str, ran: Ran) -> StatementTrial {
    let Some(transactions) = transactions.filter(|transactions| transactions.is_transactional()) else {
        return StatementTrial::new(ran);
    };
    let outcome = statement_outcome(transactions, &ran);
    let mut trial = StatementTrial::new(ran);
    match transactions.implicit_commit(body) {
        // 隐式提交发生在语句执行之前：即使语句本身随后失败，此前的修改也已经被提交。
        ImplicitCommitVerdict::Yes => match outcome {
            StatementOutcome::Unconfirmed(reason) => {
                trial.transaction = Some(transactions.observe_statement(StatementOutcome::Unconfirmed(reason)));
                append_note(&mut trial.message, UNCONFIRMED_IMPLICIT_COMMIT_NOTE);
            }
            _ => {
                let effect = transactions.observe_implicit_commit().unwrap_or(TransactionEffect::Unknown);
                trial.transaction = Some(effect);
                if effect == TransactionEffect::ImplicitCommit {
                    append_note(&mut trial.message, IMPLICIT_COMMIT_NOTE);
                }
            }
        },
        // 过程体、动态 SQL 与未分类语句可能隐式提交：不宣称发生，也不宣称事务仍然活动。
        // 没有活动事务（自动提交）时同一条语句不改变任何事务状态，不留下未知提示。
        ImplicitCommitVerdict::Uncertain => {
            let effect = transactions.observe_statement(StatementOutcome::Unconfirmed(UnknownReason::UnrecognizedEffect));
            trial.transaction = Some(effect);
            if effect == TransactionEffect::Unknown {
                append_note(&mut trial.message, UNCERTAIN_IMPLICIT_COMMIT_NOTE);
            }
        }
        ImplicitCommitVerdict::NotApplicable => {
            // 过程调用/动态 SQL 可能已经在内部提交、回滚或开始了事务：活动状态保留，
            // 但层数不能再沿用旧值，交给批次前后的探针核对（R8、R10）。
            let opaque = transactions.statement_may_change_transaction(body)
                && matches!(outcome, StatementOutcome::Succeeded | StatementOutcome::Failed(StatementFailure::TransactionUsable));
            let observed = if opaque { transactions.observe_opaque_statement() } else { transactions.observe_statement(outcome) };
            trial.transaction = Some(observed);
            if outcome == StatementOutcome::Failed(StatementFailure::TransactionAborted) {
                append_note(&mut trial.message, ABORTED_STATEMENT_NOTE);
            } else if outcome == StatementOutcome::Unconfirmed(UnknownReason::UnrecognizedEffect) {
                append_note(&mut trial.message, SERVER_ROLLBACK_NOTE);
            }
        }
    }
    settle(&mut trial, transactions);
    trial
}

/// 以事务控制关键字开头、但形态无法确认的手写语句：执行后进入未知。
///
/// 不能走普通语句的登记（自动提交下普通语句一律 `Unchanged`）：这类语句可能刚刚开始了事务，
/// 报 `Unchanged` 会让界面停在“无活动事务”而数据库里已有事务，后续 `COMMIT` 还会被
/// 本地预检拒绝、永远发不出去（R3、R10）。
fn ambiguous(transactions: Option<&TransactionCoordinator>, ran: Ran) -> StatementTrial {
    let mut trial = StatementTrial::new(ran);
    trial.ambiguous_control = true;
    let Some(transactions) = transactions.filter(|transactions| transactions.is_transactional()) else {
        return trial;
    };
    trial.transaction = Some(transactions.observe_ambiguous_control());
    append_note(&mut trial.message, AMBIGUOUS_CONTROL_NOTE);
    settle(&mut trial, transactions);
    trial
}

/// 控制语句的驱动结果按方言翻译成模型能登记的结果（控制失败方向由方言判定）。
fn control_result(transactions: Option<&TransactionCoordinator>, statement: &ControlStatement, ran: &Ran) -> ControlOutcome {
    let Some(transactions) = transactions.filter(|transactions| transactions.is_transactional()) else {
        return ControlOutcome::Unconfirmed(UnknownReason::UnrecognizedEffect);
    };
    match &ran.outcome {
        // 驱动没有公开命令标签：中止事务里的 PostgreSQL COMMIT 由预检拒绝，不会走到这里。
        Ok(_) => transactions.control_success(statement, None),
        Err(StatementStop::Cancelled { .. }) => ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed),
        Err(StatementStop::Unknown { .. }) => ControlOutcome::Unconfirmed(UnknownReason::ConnectionLost),
        Err(StatementStop::Error { .. }) => {
            let (code, message) = ran.failure_parts();
            // 数据库可能已自行回滚整个事务（死锁牺牲品）：控制结果同样无法按请求方向确认。
            if transactions.error_may_have_ended_transaction(code, message) {
                return ControlOutcome::Unconfirmed(UnknownReason::UnrecognizedEffect);
            }
            transactions.control_failure(statement, transactions.statement_failure(code, message))
        }
    }
}

/// 登记控制语句的确认结果，并把模型拒绝（数据库已执行但状态不能据此推进）报成失败。
fn acknowledge(
    transactions: Option<&TransactionCoordinator>,
    statement: ControlStatement,
    outcome: ControlOutcome,
    ran: Option<Ran>,
) -> StatementTrial {
    let mut trial = match ran {
        Some(ran) => StatementTrial::new(ran),
        None => StatementTrial {
            status: "ok",
            message: "这条设置不需要单独发送，会在下一次开始事务时生效。".to_owned(),
            sent_sql: String::new(),
            rewrite: None,
            value: None,
            elapsed_ms: None,
            error_at: None,
            error_code: None,
            transaction: None,
            attempted: false,
            ambiguous_control: false,
            stop: false,
            remaining: "未执行",
        },
    };
    let Some(transactions) = transactions.filter(|transactions| transactions.is_transactional()) else {
        return trial;
    };
    match transactions.acknowledge(&statement, outcome) {
        Ok(effect) => {
            trial.transaction = Some(effect);
            if matches!(outcome, ControlOutcome::Unconfirmed(_)) {
                // 这条语句自己的结果没有返回：不能据此推进，也不能据此猜测结局。
                trial.status = "unknown";
                append_note(&mut trial.message, CONTROL_UNCONFIRMED_NOTE);
            } else if effect == TransactionEffect::Aborted {
                trial.status = "error";
                append_note(&mut trial.message, CONTROL_ABORTED_NOTE);
            } else if effect == TransactionEffect::Unknown {
                // 数据库确认执行了这条语句，但事务的最终结局仍然无法确认。
                trial.status = "unknown";
                append_note(&mut trial.message, CONTROL_UNRESOLVED_NOTE);
            } else {
                trial.status = "ok";
            }
            if let Some(note) = effect_note(effect, &statement) {
                append_note(&mut trial.message, note);
            }
        }
        Err(error) => {
            // 数据库可能已经执行了这条控制语句，但状态不能据此推进：按失败报告并保持原状态。
            trial.status = "error";
            trial.transaction = None;
            append_note(&mut trial.message, &format!("{error}；事务状态没有按这条语句推进"));
        }
    }
    settle(&mut trial, transactions);
    trial
}

/// 数据库确认了控制语句之后，效果本身也要说清楚（R3、R9）。
fn effect_note(effect: TransactionEffect, statement: &ControlStatement) -> Option<&'static str> {
    match effect {
        TransactionEffect::ImplicitCommit => Some("数据库把这次开始之前的修改隐式提交了；新事务已经活动。"),
        // PostgreSQL 中止事务里的 COMMIT 会以回滚返回；正常预检不会把 COMMIT 发给中止事务。
        TransactionEffect::RolledBack if matches!(statement, ControlStatement::Commit { .. }) => Some("事务已中止，数据库把这条 COMMIT 执行成回滚。"),
        _ => None,
    }
}

/// 一条普通语句的结果按方言翻译成事务结果。
fn statement_outcome(transactions: &TransactionCoordinator, ran: &Ran) -> StatementOutcome {
    match &ran.outcome {
        Ok(_) => StatementOutcome::Succeeded,
        // 执行层已经判定“连接中断、写入结果无法确认”：旧事务的最终结果不再可从这条连接确认（R10）。
        Err(StatementStop::Unknown { .. }) => StatementOutcome::Unconfirmed(UnknownReason::ConnectionLost),
        Err(_) => {
            let (code, message) = ran.failure_parts();
            match transactions.statement_failure(code, message) {
                StatementFailure::ConnectionLost => StatementOutcome::Unconfirmed(UnknownReason::ConnectionLost),
                // 数据库可能已经自行回滚整个事务：不能继续宣称事务活动，也不重放这条语句。
                _ if transactions.error_may_have_ended_transaction(code, message) => StatementOutcome::Unconfirmed(UnknownReason::UnrecognizedEffect),
                failure => StatementOutcome::Failed(failure),
            }
        }
    }
}

/// 失败或事务状态未知之后不再发送后续语句；未发送的语句保持未执行（R8）。
fn settle(trial: &mut StatementTrial, transactions: &TransactionCoordinator) {
    trial.stop = trial.status != "ok" || transactions.state() == Some(TransactionState::Unknown);
    trial.remaining = match transactions.state() {
        Some(TransactionState::Unknown) => "事务状态未知，未执行",
        Some(TransactionState::Aborted) => "事务已中止，未执行",
        _ => "未执行",
    };
}

/// 模型自己要求核对时（状态未知，或层数无法确认）发一条方言探针，只登记观测结果（R10）。
///
/// 探针不是用户语句：不进入结果区、不占语句序号，也不重放任何写入。没有探针的方言（MySQL、
/// PostgreSQL、SQLite）在这里什么都不做，状态保持未知直到使用者核对或回滚。
pub(crate) async fn reconcile(transactions: Option<&TransactionCoordinator>, driver: &dyn StatementDriver) {
    let Some(transactions) = transactions.filter(|transactions| transactions.is_transactional()) else {
        return;
    };
    let Some(snapshot) = transactions.snapshot() else {
        return;
    };
    // `depth == None` 表示层数不可信：未知状态、过程/动态 SQL 之后，都要用探针重新核对。
    let needs_probe = snapshot.state == TransactionState::Unknown
        || (matches!(snapshot.state, TransactionState::Idle | TransactionState::Active) && snapshot.depth.is_none());
    if !needs_probe {
        return;
    }
    let Some(sql) = transactions.probe_sql() else {
        return;
    };
    let Some(value) = driver.probe(sql).await else {
        // 探针本身失败：不据此推进任何状态，旧事务仍是未知。
        return;
    };
    if let Some(probe) = transactions.read_probe(&value.columns, &value.rows) {
        transactions.observe_probe(probe);
    }
}

fn append_note(message: &mut String, note: &str) {
    if !message.is_empty() {
        message.push(' ');
    }
    message.push_str(note);
}
