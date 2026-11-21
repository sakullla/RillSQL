//! 结构变更。未确认不执行；确认过的 SQL 仍必须和当前预览一致。
//!
//! 结构业务读取/预览/执行都走连接级用户事务的业务会话（ADR-1、ADR-4）：活动事务里的变更加入
//! 同一个用户事务，执行结果的每一步都按数据库真实结果登记到共享事务状态。回执区分
//! “已应用到当前事务、尚未提交”与“已经是最终结果”，MySQL/MariaDB 的隐式提交单独报告。

use std::sync::Arc;

use crate::connections::model::Engine;
use crate::connections::transaction::{ExecutionLease, TransactionCoordinator};
use crate::connections::{CatalogSession, Service};
use crate::catalog::definition::{object_ddl, DdlKey, ObjectDdl};
use crate::ddl::exec::preview_in;
use crate::ddl::{Action, Plan, Receipt, Request, Snapshot, StepStatus};
use crate::transaction::{StatementOutcome, TransactionEffect, TransactionSnapshot, TransactionState, UnknownReason};

const NOT_OPEN: &str = "这条连接还没有打开，没有另开新的连接。";

pub fn require_execute(confirmed: bool) -> Result<(), String> {
    if !confirmed {
        return Err("结构变更需要先确认预览中的 SQL，未确认不会执行。".to_owned());
    }
    Ok(())
}

/// 一次结构执行的结果：回执加共享事务效果，供命令层区分“已应用未提交”与最终结果。
pub(crate) struct Executed {
    pub receipt: Receipt,
    pub effect: TransactionEffect,
    pub transaction: Option<TransactionSnapshot>,
    /// 这次执行命中 MySQL/MariaDB 的隐式提交。
    pub implicit_commit: bool,
    /// 步骤只应用到当前事务，尚未提交。
    pub applied_not_committed: bool,
    /// 步骤已经是数据库的最终结果（没有活动事务）。
    pub committed: bool,
}

/// 结构业务会话：共享用户事务句柄、串行执行租约与协调器。
struct Business {
    session: CatalogSession,
    /// 租约持有到这个值被丢弃为止：预览/执行不会和同连接其他业务操作并发。
    _lease: Option<ExecutionLease>,
    coordinator: Option<Arc<TransactionCoordinator>>,
}

impl Business {
    fn state(&self) -> TransactionState {
        self.snapshot().map(|snapshot| snapshot.state).unwrap_or(TransactionState::Idle)
    }

    fn snapshot(&self) -> Option<TransactionSnapshot> {
        self.coordinator.as_ref().and_then(|coordinator| coordinator.snapshot())
    }

    /// 读取失败也按数据库真实结果登记：PostgreSQL 的事务内错误会把事务置为中止（R8）。
    fn note_read_failure(&self, message: &str) {
        let Some(coordinator) = &self.coordinator else { return };
        if coordinator.snapshot().is_none() {
            return;
        }
        let outcome = if coordinator.error_may_have_ended_transaction(None, message) {
            StatementOutcome::Unconfirmed(UnknownReason::UnrecognizedEffect)
        } else {
            StatementOutcome::Failed(coordinator.statement_failure(None, message))
        };
        coordinator.observe_statement(outcome);
    }
}

async fn business(service: &Service, connection_id: &str) -> Result<Business, String> {
    let coordinator = service.transaction_coordinator(connection_id);
    let query = service.query_session(connection_id).await?.ok_or_else(|| NOT_OPEN.to_owned())?;
    Ok(Business { session: query.session, _lease: query.lease, coordinator })
}

pub async fn describe_saved(service: &Service, connection_id: &str, database: String, schema: String, engine: &str, table: &str) -> Result<Snapshot, String> {
    let request = request(database, schema, engine, Action::DropTable { table: table.to_owned() })?;
    let business = business(service, connection_id).await?;
    let mut snapshot = match crate::ddl::load(&business.session, &request, table).await {
        Ok(snapshot) => snapshot,
        Err(message) => {
            business.note_read_failure(&message);
            return Err(message);
        }
    };
    if snapshot.kind == "table" {
        crate::ddl::load_comments(&business.session, &mut snapshot).await;
    }
    Ok(snapshot)
}

/// 对象树“查看/复制 DDL”的读取入口。只在已打开的业务会话上读，不另开连接。
pub async fn object_definition(service: &Service, connection_id: &str, engine: Engine, key: &DdlKey) -> Result<ObjectDdl, String> {
    let business = business(service, connection_id).await?;
    match object_ddl(&business.session, engine, key).await {
        Ok(definition) => Ok(definition),
        Err(message) => {
            business.note_read_failure(&message);
            Err(message)
        }
    }
}

pub async fn preview_request(service: &Service, connection_id: &str, request: &Request) -> Result<Plan, String> {
    let business = business(service, connection_id).await?;
    match preview_in(&business.session, request, business.state()).await {
        Ok(plan) => Ok(plan),
        Err(message) => {
            business.note_read_failure(&message);
            Err(message)
        }
    }
}

pub async fn execute_request(service: &Service, connection_id: &str, request: &Request, confirmed_sql: &[String], confirmed: bool) -> Result<Receipt, String> {
    Ok(execute_request_tracked(service, connection_id, request, confirmed_sql, confirmed).await?.receipt)
}

/// 执行结构变更并把每一步的结果登记到共享事务，返回带事务效果的回执。
pub(crate) async fn execute_request_tracked(
    service: &Service,
    connection_id: &str,
    request: &Request,
    confirmed_sql: &[String],
    confirmed: bool,
) -> Result<Executed, String> {
    require_execute(confirmed)?;
    let business = business(service, connection_id).await?;
    let receipt = crate::ddl::commands::execute_on(service, connection_id, &business.session, request, confirmed_sql, business.state()).await?;
    let (effect, implicit_commit) = match &business.coordinator {
        Some(coordinator) => observe_steps(coordinator, &receipt),
        None => (TransactionEffect::Unchanged, false),
    };
    let snapshot = business.snapshot();
    let state = snapshot.as_ref().map(|snapshot| snapshot.state).unwrap_or(TransactionState::Idle);
    let executed = !receipt.steps.is_empty() && receipt.blocked.is_empty();
    let mut receipt = receipt;
    // 回执本身说明这次执行落在哪里：活动事务里是“已应用未提交”，不是已经保存。
    if executed {
        let note = if implicit_commit {
            "这些结构语句触发了隐式提交：此前未提交的修改已经随隐式提交结束，不能再通过回滚撤销。"
        } else if state == TransactionState::Active {
            "这些步骤已应用到当前事务，尚未提交。"
        } else {
            ""
        };
        if !note.is_empty() && !receipt.rule.contains(note) {
            if !receipt.rule.is_empty() {
                receipt.rule.push(' ');
            }
            receipt.rule.push_str(note);
        }
    }
    Ok(Executed { receipt, effect, transaction: snapshot, implicit_commit, applied_not_committed: executed && state == TransactionState::Active, committed: executed && state == TransactionState::Idle })
}

/// 把回执里每一步按数据库真实结果登记到共享事务状态；MySQL/MariaDB 的隐式提交由方言判定。
pub(crate) fn observe_steps(coordinator: &Arc<TransactionCoordinator>, receipt: &Receipt) -> (TransactionEffect, bool) {
    let mut effect = TransactionEffect::Unchanged;
    let mut attempted: Vec<&str> = Vec::new();
    for step in &receipt.steps {
        match step.status {
            StepStatus::Ok => {
                effect = coordinator.observe_statement(StatementOutcome::Succeeded);
                attempted.push(step.sql.as_str());
            }
            StepStatus::Failed => {
                // 数据库可能自行回滚了整个事务（死锁、磁盘错误）：不继续宣称事务活动（R10）。
                let outcome = if coordinator.error_may_have_ended_transaction(None, &step.message) {
                    StatementOutcome::Unconfirmed(UnknownReason::UnrecognizedEffect)
                } else {
                    StatementOutcome::Failed(coordinator.statement_failure(None, &step.message))
                };
                effect = coordinator.observe_statement(outcome);
                attempted.push(step.sql.as_str());
            }
            StepStatus::Unknown => {
                // 语句可能已经到达数据库：不能据此宣称事务仍可使用（R8、R10）。
                effect = coordinator.observe_statement(StatementOutcome::Unconfirmed(UnknownReason::UnrecognizedEffect));
            }
            // 没有执行的语句不会改变事务状态。
            StepStatus::Skipped | StepStatus::RolledBack => {}
        }
    }
    let mut confirmed_commit = false;
    let mut uncertain = false;
    for sql in attempted {
        match coordinator.implicit_commit(sql) {
            crate::transaction::ImplicitCommitVerdict::Yes => confirmed_commit = true,
            crate::transaction::ImplicitCommitVerdict::Uncertain => uncertain = true,
            crate::transaction::ImplicitCommitVerdict::NotApplicable => {}
        }
    }
    if confirmed_commit {
        // 数据库确认隐式提交后，事务状态更新为无活动事务；不额外确认（R9）。
        return match coordinator.observe_implicit_commit() {
            Ok(value) => (value, matches!(value, TransactionEffect::ImplicitCommit)),
            Err(_) => (effect, false),
        };
    }
    if uncertain {
        // 无法静态确认是否隐式提交时不宣称发生，按未知处理（R9、R10）。
        effect = coordinator.observe_statement(StatementOutcome::Unconfirmed(UnknownReason::UnrecognizedEffect));
    }
    (effect, false)
}

pub fn request(database: String, schema: String, engine: &str, action: Action) -> Result<Request, String> {
    Ok(Request {
        database,
        schema,
        engine: Engine::parse(engine)?,
        action,
    })
}
