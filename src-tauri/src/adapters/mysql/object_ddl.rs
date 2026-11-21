//! MySQL 家族的事件、例程与触发器变更计划。
//!
//! 这一层只把对象草稿变成计划：语句形状、DROP + CREATE 的风险与保留属性都在这里判定，
//! 执行仍走既有的确认执行链（预览与执行逐字比对、逐条 Receipt）。
//!
//! 现状：对象模型、读回与分派已经就位，但本文件里的变更计划还没有接入语句生成。
//! 此时一律返回带原因的阻断计划——不生成语句，也不把动作报成成功。

use crate::ddl::model::{Action, Plan, Risk, Snapshot};
use crate::ddl::plan::{finish, target_label};

/// 对象变更还没有接入语句生成时的原因。不假装支持，也不要生成半条语句。
pub(super) const NOT_WIRED: &str = "事件、例程与触发器的变更计划还没有接入，这里不生成语句。";

/// 对象动作 → 计划。不是对象生命周期动作时返回 None，由调用方继续按表结构动作处理。
pub(super) fn plan(snapshot: &Snapshot, action: &Action, supported: bool) -> Option<Plan> {
    let (target_name, risks) = match action {
        Action::CreateEvent { event } => (event.name.clone(), Vec::new()),
        Action::AlterEvent { event } => (event.original.clone().unwrap_or_else(|| event.name.clone()), Vec::new()),
        Action::DropEvent { event } => (event.clone(), vec![drop_risk("事件")]),
        Action::CreateRoutine { routine } => (routine.name.clone(), Vec::new()),
        Action::ReplaceRoutine { routine } => (routine.name.clone(), vec![replace_risk("例程")]),
        Action::DropRoutine { name, .. } => (name.clone(), vec![drop_risk("例程")]),
        Action::CreateTrigger { trigger } => (trigger.name.clone(), Vec::new()),
        Action::ReplaceTrigger { trigger } => (trigger.name.clone(), vec![replace_risk("触发器")]),
        Action::DropTrigger { trigger } => (trigger.clone(), vec![drop_risk("触发器")]),
        _ => return None,
    };
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, &target_name);
    if !supported {
        let mut risks = risks;
        risks.push(Risk { level: "info".to_owned(), text: "只有 MySQL 与 MariaDB 有这些对象。".to_owned() });
        return Some(finish(
            snapshot.engine,
            false,
            target,
            vec!["只有 MySQL 与 MariaDB 支持事件、例程与触发器。".to_owned()],
            risks,
            Vec::new(),
        ));
    }
    Some(finish(snapshot.engine, false, target, vec![NOT_WIRED.to_owned()], risks, Vec::new()))
}

/// 删除对象：对象消失，依赖它的调度或数据操作随之失效。
fn drop_risk(label: &str) -> Risk {
    Risk { level: "warn".to_owned(), text: format!("删除{label}后它不再存在，依赖它的操作会直接失败。") }
}

/// 替换对象：DROP + CREATE 两步，中途失败时对象可能已经不存在。
fn replace_risk(label: &str) -> Risk {
    Risk {
        level: "danger".to_owned(),
        text: format!("替换{label}会先删除再创建：两步之间失败时原{label}已经不存在，不能声称整体回滚。"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::model::Engine;
    use crate::ddl::model::{EventDraft, RoutineDraft, TriggerDraft};

    fn snapshot() -> Snapshot {
        Snapshot::bare(Engine::Mysql, "app", "app", "app")
    }

    /// 对象动作都被分派到这里：现在只给阻断原因，不生成语句，也不报成功。
    #[test]
    fn object_actions_are_dispatched_and_never_reported_as_ready() {
        let event = Action::CreateEvent { event: EventDraft { name: "purge".to_owned(), ..EventDraft::default() } };
        let planned = plan(&snapshot(), &event, true).expect("dispatched");
        assert_eq!(planned.blocked, vec![NOT_WIRED.to_owned()]);
        assert!(planned.steps.is_empty());

        let routine = Action::ReplaceRoutine { routine: RoutineDraft { name: "total".to_owned(), ..RoutineDraft::default() } };
        let planned = plan(&snapshot(), &routine, true).expect("dispatched");
        assert!(planned.steps.is_empty());
        // 替换是 DROP + CREATE：风险要先说清对象可能已经不存在。
        assert!(planned.risks.iter().any(|risk| risk.level == "danger" && risk.text.contains("不能声称整体回滚")));

        let trigger = Action::DropTrigger { trigger: "audit".to_owned() };
        let planned = plan(&snapshot(), &trigger, true).expect("dispatched");
        assert!(planned.risks.iter().any(|risk| risk.text.contains("不再存在")));

        // 表结构动作不在这里处理。
        assert!(plan(&snapshot(), &Action::Vacuum, true).is_none());
    }

    /// 别的引擎请求这些对象时给出拒绝原因，不生成语句。
    #[test]
    fn other_engines_get_a_refusal_without_statements() {
        let mut postgres = Snapshot::bare(Engine::Postgres, "app", "public", "app");
        postgres.kind = "table".to_owned();
        let action = Action::CreateTrigger { trigger: TriggerDraft { name: "audit".to_owned(), table: "orders".to_owned(), ..TriggerDraft::default() } };
        let planned = plan(&postgres, &action, false).expect("dispatched");
        assert!(planned.steps.is_empty());
        assert!(planned.blocked.iter().any(|text| text.contains("MySQL")));
    }
}
