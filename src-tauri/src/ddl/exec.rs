//! 在已打开的会话上预览并逐条执行结构变更。执行前核对确认过的 SQL 与当前计划一致；
//! 各引擎的逐条执行（失败即停、事务回滚、外键开关恢复）在适配器里。

use crate::connections::CatalogSession;

use super::introspect::{attach_reference, enrich, is_object_action, load, maintenance_note, missing_snapshot, object_snapshot, session_snapshot, table_of};
use super::model::{Action, Plan, Request, Snapshot, Step, StepReceipt, StepStatus};
use super::plan;

pub struct Receipt {
    pub blocked: Vec<String>,
    pub rule: String,
    pub steps: Vec<StepReceipt>,
    pub target: String,
    pub rebuild: bool,
}

pub async fn preview(session: &CatalogSession, request: &Request) -> Result<Plan, String> {
    let snapshot = prepare_snapshot(session, request).await?;
    Ok(plan::plan(&snapshot, &request.action))
}

pub async fn execute(session: &CatalogSession, request: &Request, confirmed_sql: &[String]) -> Result<Receipt, String> {
    let snapshot = prepare_snapshot(session, request).await?;
    let plan = plan::plan(&snapshot, &request.action);
    if !plan.blocked.is_empty() {
        return Ok(Receipt {
            blocked: plan.blocked,
            rule: plan.rule,
            steps: Vec::new(),
            target: plan.target,
            rebuild: false,
        });
    }
    let planned: Vec<String> = plan.steps.iter().map(|step| step.sql.clone()).collect();
    if planned != confirmed_sql {
        return Err("预览已过期：数据库里的结构或生成的 SQL 和确认时看到的不一致，没有执行。".to_owned());
    }
    // 重建的失败补偿恢复操作开始时读到的 live。SetForeignKeys 的语句本身就是目标值，不能再改回去。
    let restore = match &request.action {
        Action::SetForeignKeys { enabled } => Some(*enabled),
        _ => snapshot.live_foreign_keys,
    };
    let mut steps = run_steps(session, &plan.steps, restore).await?;
    // 维护动作（VACUUM/REINDEX）没有返回值，完成后补一句读到的结果，别让界面只看到“成功”。
    if !steps.is_empty() && steps.iter().all(|step| step.status == StepStatus::Ok) {
        if let Some(note) = maintenance_note(session, request.engine, &request.action).await {
            if let Some(last) = steps.last_mut() {
                last.message = note;
            }
        }
    }
    Ok(Receipt { blocked: Vec::new(), rule: plan.rule, steps, target: plan.target, rebuild: plan.rebuild })
}

async fn prepare_snapshot(session: &CatalogSession, request: &Request) -> Result<Snapshot, String> {
    if matches!(request.action, Action::SetForeignKeys { .. }) {
        return session_snapshot(session, request).await;
    }
    if is_object_action(&request.action) {
        return object_snapshot(session, request).await;
    }
    if let Action::CreateTable { name, .. } = &request.action {
        return match load(session, request, name).await {
            Ok(mut snapshot) => {
                snapshot.object_exists = true;
                snapshot.name = name.clone();
                Ok(snapshot)
            }
            Err(message) if message.starts_with("找不到表或视图") => missing_snapshot(session, request, name).await,
            Err(message) => Err(message),
        };
    }
    let table = table_of(&request.action).ok_or_else(|| "没有目标表".to_owned())?;
    let mut snapshot = load(session, request, table).await?;
    enrich(session, &mut snapshot, &request.action).await?;
    attach_reference(session, request, &mut snapshot, &request.action).await?;
    Ok(snapshot)
}

/// 逐条执行，失败即停。由会话所属的适配器执行（重建事务的回滚、外键开关的恢复各按方言）。
pub async fn run_steps(session: &CatalogSession, steps: &[Step], restore_foreign_keys: Option<bool>) -> Result<Vec<StepReceipt>, String> {
    session.run_ddl_steps(steps, restore_foreign_keys).await
}

/// 外键开关动作执行成功后连接上的新开关值。
pub(crate) fn committed_foreign_keys(engine: super::model::Engine, action: &Action, receipt: &Receipt) -> Option<bool> {
    let Action::SetForeignKeys { enabled } = action else {
        return None;
    };
    if !receipt.blocked.is_empty() {
        return None;
    }
    let applied = receipt.steps.iter().any(|step| step.status == StepStatus::Ok && crate::adapters::ddl::sets_session_foreign_keys(engine, step, *enabled));
    applied.then_some(*enabled)
}

/// 一条语句的执行回执。
pub(crate) fn receipt(step: &Step, status: StepStatus, message: &str) -> StepReceipt {
    StepReceipt { sql: step.sql.clone(), label: step.label.clone(), status, message: message.to_owned() }
}

