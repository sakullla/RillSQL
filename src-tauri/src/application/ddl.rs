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
    /// 与 `rule` 分开的一句事务说明：整句可独立翻译，不和计划规则拼成一个查不到的句子。
    pub transaction_note: Option<String>,
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
}

async fn business(service: &Service, connection_id: &str) -> Result<Business, String> {
    let coordinator = service.transaction_coordinator(connection_id);
    let query = service.query_session(connection_id).await?.ok_or_else(|| NOT_OPEN.to_owned())?;
    Ok(Business { session: query.session, _lease: query.lease, coordinator })
}

pub async fn describe_saved(service: &Service, connection_id: &str, database: String, schema: String, engine: &str, table: &str) -> Result<Snapshot, String> {
    let request = request(database, schema, engine, Action::DropTable { table: table.to_owned() })?;
    let business = business(service, connection_id).await?;
    // 结构读取可能因本地校验（找不到表、跨库、会话类型）失败，不登记为数据库执行错误。
    let mut snapshot = crate::ddl::load(&business.session, &request, table).await?;
    if snapshot.kind == "table" {
        crate::ddl::load_comments(&business.session, &mut snapshot).await;
    }
    Ok(snapshot)
}

/// 对象树“查看/复制 DDL”的读取入口。只在已打开的业务会话上读，不另开连接。
pub async fn object_definition(service: &Service, connection_id: &str, engine: Engine, key: &DdlKey) -> Result<ObjectDdl, String> {
    let business = business(service, connection_id).await?;
    object_ddl(&business.session, engine, key).await
}

pub async fn preview_request(service: &Service, connection_id: &str, request: &Request) -> Result<Plan, String> {
    let business = business(service, connection_id).await?;
    // 预览同时在本地校验动作，失败不一定发往数据库；执行阶段才按真实步骤登记。
    preview_in(&business.session, request, business.state()).await
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
    // 只有每一步都成功才算“已应用”：失败/回滚/跳过时数据库里没有这次变更，
    // 不能把回执写成已应用或已提交（Idle 内部事务会回滚，Active 只回滚到保存点）。
    let applied = !receipt.steps.is_empty()
        && receipt.blocked.is_empty()
        && receipt.steps.iter().all(|step| step.status == StepStatus::Ok);
    let transaction_note = if implicit_commit {
        // 隐式提交是事务效果：即使结构语句本身失败，事务也已经结束，不能再显示为可回滚（R9）。
        Some("这些结构语句触发了隐式提交：此前未提交的修改已经随隐式提交结束，不能再通过回滚撤销。".to_owned())
    } else if applied && state == TransactionState::Active {
        Some("这些步骤已应用到当前事务，尚未提交。".to_owned())
    } else {
        None
    };
    Ok(Executed {
        receipt,
        effect,
        transaction: snapshot,
        implicit_commit,
        applied_not_committed: applied && state == TransactionState::Active,
        committed: applied && state == TransactionState::Idle,
        transaction_note,
    })
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
