//! MySQL 家族的事件、例程与触发器变更计划。
//!
//! 这一层只把对象草稿变成计划：语句形状、DROP + CREATE 的风险与保留属性都在这里判定，
//! 执行仍走既有的确认执行链（预览与执行逐字比对、逐条 Receipt），不另建 SQL 拼接或执行路径。
//!
//! 多语句主体（BEGIN … END）整段留在同一条 Step.sql 里：执行链把每条 Step 作为一条语句发送，
//! 不再按分号切分，所以主体里的分号不会被拆开（R6 / R7）。
//!
//! 未开放编辑但服务器上存在的属性（DEFINER、COMMENT）在修改时必须原样带回：本文件只用读回值，
//! 读不到就阻断编辑，不用空值覆盖服务器对象。函数与存储过程的类型来自读回，不互相替换。

use crate::adapters::quote_ident;
use crate::connections::model::Engine;
use crate::ddl::model::{
    Action, EventCompletion, EventDraft, EventSchedule, ParamMode, Plan, Risk, RoutineDraft, RoutineKind, Snapshot, Step, TriggerDraft,
    TriggerEvent, TriggerTiming,
};
use crate::ddl::plan::{finish, target_label};
use crate::ddl::sql::{qualified_table, valid_name};

use super::literal;
use super::profile::MysqlProfile;

/// 对象动作 → 计划。不是对象生命周期动作时返回 None，由调用方继续按表结构动作处理。
pub(super) fn plan(snapshot: &Snapshot, action: &Action, supported: bool) -> Option<Plan> {
    match action {
        Action::CreateEvent { event } => Some(plan_create_event(snapshot, event, supported)),
        Action::AlterEvent { event } => Some(plan_alter_event(snapshot, event, supported)),
        Action::DropEvent { event } => Some(plan_drop_event(snapshot, event, supported)),
        Action::CreateRoutine { routine } => Some(plan_create_routine(snapshot, routine, supported)),
        Action::ReplaceRoutine { routine } => Some(plan_replace_routine(snapshot, routine, supported)),
        Action::DropRoutine { routine_kind, name } => Some(plan_drop_routine(snapshot, *routine_kind, name, supported)),
        Action::CreateTrigger { trigger } => Some(plan_create_trigger(snapshot, trigger, supported)),
        Action::ReplaceTrigger { trigger } => Some(plan_replace_trigger(snapshot, trigger, supported)),
        Action::DropTrigger { trigger } => Some(plan_drop_trigger(snapshot, trigger, supported)),
        _ => None,
    }
}

/// 事件：创建。调度、主体、启停与 ON COMPLETION 都在同一条 CREATE EVENT 里。
fn plan_create_event(snapshot: &Snapshot, event: &EventDraft, supported: bool) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, &event.name);
    if !supported {
        return refuse(snapshot, target, "事件（event）");
    }
    let mut blocked = Vec::new();
    let mut risks = Vec::new();
    blocked.extend(object_capability(snapshot, Feature::Events, "事件"));
    blocked.extend(event_problems(snapshot, event));
    risks.extend(event_scheduler_risks(snapshot, event.enabled, false));
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    match event_statement(snapshot, event, false) {
        Ok(sql) => finish(
            snapshot.engine,
            false,
            target,
            Vec::new(),
            risks,
            vec![Step { label: format!("创建事件 {}", event.name.trim()), sql }],
        ),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

/// 事件：修改。读不到现有事件时阻断——没有读到的定义不能当基线。
fn plan_alter_event(snapshot: &Snapshot, event: &EventDraft, supported: bool) -> Plan {
    let name = event.original.clone().unwrap_or_else(|| event.name.clone());
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, &name);
    if !supported {
        return refuse(snapshot, target, "事件（event）");
    }
    let mut blocked = Vec::new();
    blocked.extend(object_capability(snapshot, Feature::Events, "事件"));
    match &snapshot.event {
        None => blocked.push(format!("没有读到事件 {name}，没有生成语句。")),
        Some(_) => blocked.extend(event_problems(snapshot, event)),
    }
    // ALTER EVENT 不改服务器级调度器状态；调度器关闭时启用的事件仍然不会运行。
    let risks = event_scheduler_risks(snapshot, event.enabled, true);
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    match event_statement(snapshot, event, true) {
        Ok(sql) => finish(
            snapshot.engine,
            false,
            target,
            Vec::new(),
            risks,
            vec![Step { label: format!("修改事件 {}", name.trim()), sql }],
        ),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

/// 事件：删除。必须指明库，不能用同名事件去猜。
fn plan_drop_event(snapshot: &Snapshot, event: &str, supported: bool) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, event);
    if !supported {
        return refuse(snapshot, target, "事件（event）");
    }
    let mut blocked = Vec::new();
    blocked.extend(object_capability(snapshot, Feature::Events, "事件"));
    if !valid_name(event) {
        blocked.push("事件名无效。".to_owned());
    }
    if snapshot.event.is_none() {
        blocked.push(format!("没有读到事件 {}，没有生成语句。", event.trim()));
    }
    let risks = vec![drop_risk("事件")];
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    match qualified_table(snapshot.engine, &snapshot.database, "", event.trim()) {
        Ok(name) => finish(
            snapshot.engine,
            false,
            target,
            Vec::new(),
            risks,
            vec![Step { label: format!("删除事件 {}", event.trim()), sql: format!("DROP EVENT {name};") }],
        ),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

/// 例程：创建函数或存储过程。
fn plan_create_routine(snapshot: &Snapshot, routine: &RoutineDraft, supported: bool) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, &routine.name);
    if !supported {
        return refuse(snapshot, target, "函数与存储过程");
    }
    let mut blocked = Vec::new();
    blocked.extend(object_capability(snapshot, Feature::Routines, "函数与存储过程"));
    blocked.extend(routine_problems(snapshot, routine));
    let risks = routine_risks();
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    match routine_statement(snapshot, routine, None) {
        Ok(sql) => finish(
            snapshot.engine,
            false,
            target,
            Vec::new(),
            risks,
            vec![Step { label: format!("创建{} {}", kind_label(routine.kind), routine.name.trim()), sql }],
        ),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

/// 例程：替换（DROP + CREATE）。读回的类型与草稿不一致时阻断，不把函数换成存储过程。
fn plan_replace_routine(snapshot: &Snapshot, routine: &RoutineDraft, supported: bool) -> Plan {
    let name = routine.original.clone().unwrap_or_else(|| routine.name.clone());
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, &name);
    if !supported {
        return refuse(snapshot, target, "函数与存储过程");
    }
    let mut blocked = Vec::new();
    blocked.extend(object_capability(snapshot, Feature::Routines, "函数与存储过程"));
    let mut risks = vec![replace_risk(kind_label(routine.kind))];
    risks.extend(routine_risks());
    match &snapshot.routine {
        None => blocked.push(format!("没有读到{} {name}，没有生成语句。", kind_label(routine.kind))),
        Some(state) => {
            if state.kind != routine.kind {
                blocked.push(format!(
                    "{name} 读回是{}，这里要写成{}：函数与存储过程不能互相替换。",
                    kind_label(state.kind),
                    kind_label(routine.kind)
                ));
            } else {
                blocked.extend(routine_problems(snapshot, routine));
                if state.definer.trim().is_empty() {
                    blocked.push(format!("没有读到 {name} 的 DEFINER，不能在不丢失它的前提下替换。"));
                }
            }
        }
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    let definer = snapshot.routine.as_ref().map(|state| state.definer.clone()).unwrap_or_default();
    match routine_statement(snapshot, routine, Some(&definer)) {
        Ok(create) => {
            let steps = replace_steps(snapshot, routine_keyword(routine.kind), kind_label(routine.kind), &name, &create);
            finish(snapshot.engine, false, target, Vec::new(), risks, steps)
        }
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

/// 例程：删除。类型必须明确，否则可能删掉另一个对象。
fn plan_drop_routine(snapshot: &Snapshot, kind: RoutineKind, name: &str, supported: bool) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, name);
    if !supported {
        return refuse(snapshot, target, "函数与存储过程");
    }
    let mut blocked = Vec::new();
    blocked.extend(object_capability(snapshot, Feature::Routines, "函数与存储过程"));
    if !valid_name(name) {
        blocked.push("对象名无效。".to_owned());
    }
    match &snapshot.routine {
        None => blocked.push(format!("没有读到{} {}，没有生成语句。", kind_label(kind), name.trim())),
        Some(state) if state.kind != kind => {
            blocked.push(format!("{} 读回是{}，不是{}。", name.trim(), kind_label(state.kind), kind_label(kind)));
        }
        Some(_) => {}
    }
    let risks = vec![drop_risk(kind_label(kind))];
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    match qualified_table(snapshot.engine, &snapshot.database, "", name.trim()) {
        Ok(qualified) => finish(
            snapshot.engine,
            false,
            target,
            Vec::new(),
            risks,
            vec![Step {
                label: format!("删除{} {}", kind_label(kind), name.trim()),
                sql: format!("DROP {} {qualified};", routine_keyword(kind)),
            }],
        ),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

/// 触发器：创建。
fn plan_create_trigger(snapshot: &Snapshot, trigger: &TriggerDraft, supported: bool) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, &trigger.name);
    if !supported {
        return refuse(snapshot, target, "触发器");
    }
    let mut blocked = Vec::new();
    blocked.extend(object_capability(snapshot, Feature::Triggers, "触发器"));
    blocked.extend(trigger_problems(snapshot, trigger));
    let risks = trigger_risks();
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    match trigger_statement(snapshot, trigger, None) {
        Ok(sql) => finish(
            snapshot.engine,
            false,
            target,
            Vec::new(),
            risks,
            vec![Step { label: format!("创建触发器 {}", trigger.name.trim()), sql }],
        ),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

/// 触发器：替换（DROP + CREATE）。目标是明确的库 + 触发器名。
fn plan_replace_trigger(snapshot: &Snapshot, trigger: &TriggerDraft, supported: bool) -> Plan {
    let name = trigger.original.clone().unwrap_or_else(|| trigger.name.clone());
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, &name);
    if !supported {
        return refuse(snapshot, target, "触发器");
    }
    let mut blocked = Vec::new();
    blocked.extend(object_capability(snapshot, Feature::Triggers, "触发器"));
    let mut risks = vec![replace_risk("触发器")];
    risks.extend(trigger_risks());
    match &snapshot.trigger {
        None => blocked.push(format!("没有读到触发器 {name}，没有生成语句。")),
        Some(state) => {
            blocked.extend(trigger_problems(snapshot, trigger));
            if state.definer.trim().is_empty() {
                blocked.push(format!("没有读到 {name} 的 DEFINER，不能在不丢失它的前提下替换。"));
            }
        }
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    let definer = snapshot.trigger.as_ref().map(|state| state.definer.clone()).unwrap_or_default();
    match trigger_statement(snapshot, trigger, Some(&definer)) {
        Ok(create) => {
            let steps = replace_steps(snapshot, "TRIGGER", "触发器", &name, &create);
            finish(snapshot.engine, false, target, Vec::new(), risks, steps)
        }
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

/// 触发器：删除。
fn plan_drop_trigger(snapshot: &Snapshot, trigger: &str, supported: bool) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, trigger);
    if !supported {
        return refuse(snapshot, target, "触发器");
    }
    let mut blocked = Vec::new();
    blocked.extend(object_capability(snapshot, Feature::Triggers, "触发器"));
    if !valid_name(trigger) {
        blocked.push("触发器名无效。".to_owned());
    }
    if snapshot.trigger.is_none() {
        blocked.push(format!("没有读到触发器 {}，没有生成语句。", trigger.trim()));
    }
    let risks = vec![drop_risk("触发器")];
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    match qualified_table(snapshot.engine, &snapshot.database, "", trigger.trim()) {
        Ok(name) => finish(
            snapshot.engine,
            false,
            target,
            Vec::new(),
            risks,
            vec![Step { label: format!("删除触发器 {}", trigger.trim()), sql: format!("DROP TRIGGER {name};") }],
        ),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

/// 版本能力：服务器版本没确认支持这类对象时不生成语句（R1）。
#[derive(Clone, Copy)]
enum Feature {
    Events,
    Routines,
    Triggers,
}

/// 版本能力：读到的版本明确不支持这类对象时才阻断。
///
/// 事件、例程与触发器在全部目标版本上都有（MySQL 5.7 起、MariaDB 10.2 起，见 profile 的
/// FamilyFeatures），语句形状也没有版本分支。对象快照目前不带服务器版本，若"没读到版本就阻断"
/// 会把整条对象链路关死；因此这里只在读到的版本确实不支持时阻断，读不到版本时交给服务器判定，
/// 失败时执行链保留原始错误（R8）。
fn object_capability(snapshot: &Snapshot, feature: Feature, what: &str) -> Option<String> {
    let profile = MysqlProfile::new(snapshot.engine, &snapshot.server_version, &snapshot.sql_mode);
    let features = profile.features();
    if !features.known {
        return None;
    }
    let supported = match feature {
        Feature::Events => features.events,
        Feature::Routines => features.routines,
        Feature::Triggers => features.triggers,
    };
    if supported {
        return None;
    }
    Some(format!("服务器 {} 不支持{what}，这里不生成语句。", snapshot.server_version))
}

/// 别的引擎请求这些对象时的拒绝计划：给原因，不生成语句。
fn refuse(snapshot: &Snapshot, target: String, what: &str) -> Plan {
    let risks = vec![Risk { level: "info".to_owned(), text: "只有 MySQL 与 MariaDB 有这些对象。".to_owned() }];
    finish(snapshot.engine, false, target, vec![format!("只有 MySQL 与 MariaDB 支持{what}。")], risks, Vec::new())
}

/// 事件草稿本身的问题：名字、库、调度、主体。客户端能判断的在这里阻断。
fn event_problems(snapshot: &Snapshot, event: &EventDraft) -> Vec<String> {
    let mut blocked = Vec::new();
    if snapshot.database.trim().is_empty() {
        blocked.push("需要数据库名才能创建或修改事件。".to_owned());
    }
    if !valid_name(event.name.trim()) {
        blocked.push("事件名无效。".to_owned());
    }
    // ALTER EVENT ... RENAME TO 不能跨库，原名也必须有效。
    if let Some(original) = event.original.as_deref() {
        if !valid_name(original) {
            blocked.push("原事件名无效。".to_owned());
        }
    }
    if let Err(message) = event_schedule_sql(&event.schedule) {
        blocked.push(message);
    }
    if event.body.trim().is_empty() {
        blocked.push("事件主体为空。".to_owned());
    }
    blocked
}

/// 例程草稿本身的问题：名字、参数、返回类型、主体。
fn routine_problems(snapshot: &Snapshot, routine: &RoutineDraft) -> Vec<String> {
    let mut blocked = Vec::new();
    if snapshot.database.trim().is_empty() {
        blocked.push("需要数据库名才能创建或修改函数/存储过程。".to_owned());
    }
    if !valid_name(routine.name.trim()) {
        blocked.push("对象名无效。".to_owned());
    }
    for (index, param) in routine.params.iter().enumerate() {
        let position = index + 1;
        if !valid_name(param.name.trim()) {
            blocked.push(format!("第 {position} 个参数的名字无效。"));
        }
        if param.type_name.trim().is_empty() {
            blocked.push(format!("第 {position} 个参数没有类型。"));
        }
        if routine.kind == RoutineKind::Function && param.mode != ParamMode::In {
            blocked.push("函数的参数只能是 IN，OUT/INOUT 只有存储过程有。".to_owned());
        }
    }
    match routine.kind {
        RoutineKind::Function if routine.returns.trim().is_empty() => blocked.push("函数要有返回类型。".to_owned()),
        RoutineKind::Function => {}
        RoutineKind::Procedure if !routine.returns.trim().is_empty() => {
            blocked.push("存储过程没有返回类型，这里不写 RETURNS。".to_owned());
        }
        RoutineKind::Procedure => {}
    }
    if routine.body.trim().is_empty() {
        blocked.push("对象主体为空。".to_owned());
    }
    blocked
}

/// 触发器草稿本身的问题：名字、目标表、主体。
fn trigger_problems(snapshot: &Snapshot, trigger: &TriggerDraft) -> Vec<String> {
    let mut blocked = Vec::new();
    if snapshot.database.trim().is_empty() {
        blocked.push("需要数据库名才能创建或修改触发器。".to_owned());
    }
    if !valid_name(trigger.name.trim()) {
        blocked.push("触发器名无效。".to_owned());
    }
    if !valid_name(trigger.table.trim()) {
        blocked.push("触发器的目标表无效。".to_owned());
    }
    if trigger.body.trim().is_empty() {
        blocked.push("触发器主体为空。".to_owned());
    }
    blocked
}

/// 事件调度子句：AT <时间原文> 或 EVERY <间隔> <单位> [STARTS …] [ENDS …]。
fn event_schedule_sql(schedule: &EventSchedule) -> Result<String, String> {
    match schedule {
        EventSchedule::OneTime { at } => {
            let at = at.trim();
            if at.is_empty() {
                return Err("单次事件要有执行时间。".to_owned());
            }
            Ok(format!("AT {}", schedule_time_sql(at)))
        }
        EventSchedule::Recurring { every, unit, starts, ends } => {
            let every = every.trim();
            if every.is_empty() {
                return Err("周期事件要有执行间隔。".to_owned());
            }
            if !every.chars().all(|ch| ch.is_ascii_digit()) || every.chars().all(|ch| ch == '0') {
                return Err(format!("周期事件的间隔 {every} 不是正整数。"));
            }
            let unit = unit.trim().to_ascii_uppercase();
            if !INTERVAL_UNITS.contains(&unit.as_str()) {
                return Err(format!("周期事件的单位 {} 不是服务器接受的间隔单位。", unit));
            }
            let mut sql = format!("EVERY {every} {unit}");
            let starts = starts.trim();
            if !starts.is_empty() {
                sql.push_str(&format!(" STARTS {}", schedule_time_sql(starts)));
            }
            let ends = ends.trim();
            if !ends.is_empty() {
                sql.push_str(&format!(" ENDS {}", schedule_time_sql(ends)));
            }
            Ok(sql)
        }
    }
}

/// 调度时间原文：纯时间值要按字面量加引号（服务器要求 STARTS '2026-01-01 00:00:00'），
/// 用户写的表达式（CURRENT_TIMESTAMP + INTERVAL 1 HOUR、NOW()）与已经带引号的值原样保留。
fn schedule_time_sql(value: &str) -> String {
    let text = value.trim();
    let expression = text.starts_with('\'')
        || text.starts_with('"')
        || text.chars().any(|ch| ch.is_alphabetic())
        || text.contains('(');
    if expression {
        text.to_owned()
    } else {
        format!("'{text}'")
    }
}

/// MySQL / MariaDB 接受的间隔单位（EVERY <interval_value> <interval_field>）。
const INTERVAL_UNITS: [&str; 16] = [
    "YEAR",
    "QUARTER",
    "MONTH",
    "WEEK",
    "DAY",
    "HOUR",
    "MINUTE",
    "SECOND",
    "MICROSECOND",
    "YEAR_MONTH",
    "DAY_HOUR",
    "DAY_MINUTE",
    "DAY_SECOND",
    "HOUR_MINUTE",
    "HOUR_SECOND",
    "MINUTE_SECOND",
];

/// 事件语句。alter 为假时是 CREATE EVENT，为真时是 ALTER EVENT（改名字走 RENAME TO）。
fn event_statement(snapshot: &Snapshot, event: &EventDraft, alter: bool) -> Result<String, String> {
    let name = if alter { event.original.clone().unwrap_or_else(|| event.name.clone()) } else { event.name.clone() };
    let qualified = qualified_table(snapshot.engine, &snapshot.database, "", name.trim())?;
    let mut clauses = vec![format!("ON SCHEDULE {}", event_schedule_sql(&event.schedule)?)];
    clauses.push(
        match event.on_completion {
            EventCompletion::Preserve => "ON COMPLETION PRESERVE",
            EventCompletion::NotPreserve => "ON COMPLETION NOT PRESERVE",
        }
        .to_owned(),
    );
    if alter && event.name.trim() != name.trim() {
        clauses.push(format!("RENAME TO {}", quote_ident(snapshot.engine, event.name.trim())));
    }
    clauses.push(if event.enabled { "ENABLE".to_owned() } else { "DISABLE".to_owned() });
    let comment = event.comment.trim();
    if !comment.is_empty() {
        clauses.push(format!("COMMENT {}", literal::text(comment, snapshot.literal_quoting())));
    }
    let keyword = if alter { "ALTER" } else { "CREATE" };
    Ok(format!("{keyword} EVENT {qualified}\n{}\nDO {}", clauses.join("\n"), body_text(&event.body)?))
}

/// 例程的创建语句。definer 为 Some 时保留读到的 DEFINER（替换路径），为 None 时不写（服务器用当前用户）。
fn routine_statement(snapshot: &Snapshot, routine: &RoutineDraft, definer: Option<&str>) -> Result<String, String> {
    let qualified = qualified_table(snapshot.engine, &snapshot.database, "", routine.name.trim())?;
    let params = params_sql(routine);
    let mut header = String::from("CREATE ");
    if let Some(definer) = definer {
        header.push_str(&definer_clause(snapshot.engine, definer)?);
        header.push(' ');
    }
    header.push_str(&format!("{} {qualified}({params})", routine_keyword(routine.kind)));
    if routine.kind == RoutineKind::Function {
        header.push_str(&format!(" RETURNS {}", routine.returns.trim()));
    }
    let comment = routine.comment.trim();
    if !comment.is_empty() {
        header.push_str(&format!(" COMMENT {}", literal::text(comment, snapshot.literal_quoting())));
    }
    Ok(format!("{header}\n{}", body_text(&routine.body)?))
}

/// 参数清单：存储过程写 IN/OUT/INOUT，函数的参数只能是 IN（不写模式关键字）。
fn params_sql(routine: &RoutineDraft) -> String {
    let mut rendered = Vec::with_capacity(routine.params.len());
    for param in &routine.params {
        let name = quote_ident(Engine::Mysql, param.name.trim());
        let type_name = param.type_name.trim();
        let mode = match (routine.kind, param.mode) {
            (RoutineKind::Function, _) => "",
            (RoutineKind::Procedure, ParamMode::In) => "IN ",
            (RoutineKind::Procedure, ParamMode::Out) => "OUT ",
            (RoutineKind::Procedure, ParamMode::InOut) => "INOUT ",
        };
        rendered.push(format!("{mode}{name} {type_name}"));
    }
    rendered.join(", ")
}

/// 触发器的创建语句。
fn trigger_statement(snapshot: &Snapshot, trigger: &TriggerDraft, definer: Option<&str>) -> Result<String, String> {
    let qualified = qualified_table(snapshot.engine, &snapshot.database, "", trigger.name.trim())?;
    let table = qualified_table(snapshot.engine, &snapshot.database, "", trigger.table.trim())?;
    let timing = match trigger.timing {
        TriggerTiming::Before => "BEFORE",
        TriggerTiming::After => "AFTER",
    };
    let event = match trigger.event {
        TriggerEvent::Insert => "INSERT",
        TriggerEvent::Update => "UPDATE",
        TriggerEvent::Delete => "DELETE",
    };
    let mut header = String::from("CREATE ");
    if let Some(definer) = definer {
        header.push_str(&definer_clause(snapshot.engine, definer)?);
        header.push(' ');
    }
    header.push_str(&format!("TRIGGER {qualified} {timing} {event} ON {table} FOR EACH ROW"));
    Ok(format!("{header}\n{}", body_text(&trigger.body)?))
}

/// DROP + CREATE 两步：先删后建，中途失败时对象可能已经不存在（风险在计划里已经说明）。
fn replace_steps(snapshot: &Snapshot, drop_keyword: &str, label: &str, name: &str, create: &str) -> Vec<Step> {
    let drop_sql = match qualified_table(snapshot.engine, &snapshot.database, "", name.trim()) {
        Ok(qualified) => format!("DROP {drop_keyword} {qualified};"),
        // 限定名都写不出来时计划本身已经被阻断，这里不留一句会被误当成可执行的语句。
        Err(_) => String::new(),
    };
    vec![
        Step { label: format!("删除旧{label} {}", name.trim()), sql: drop_sql },
        Step { label: format!("创建新{label}"), sql: create.to_owned() },
    ]
}

/// 主体原文：去掉首尾空白，空主体不生成语句。多语句主体原样保留：执行链把整段作为一条语句发送。
fn body_text(body: &str) -> Result<String, String> {
    let text = body.trim();
    if text.is_empty() {
        return Err("对象主体为空。".to_owned());
    }
    Ok(text.to_owned())
}

/// DEFINER 子句：information_schema 给的是 user@host，按标识符引号写回。
fn definer_clause(engine: Engine, raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("没有读到 DEFINER，不能在不丢失它的前提下修改这个对象。".to_owned());
    }
    let Some((user, host)) = raw.split_once('@') else {
        return Err(format!("读到的 DEFINER {raw} 不是 user@host 写法，不能安全写回。"));
    };
    let user = trim_quotes(user);
    let host = trim_quotes(host);
    if user.is_empty() || host.is_empty() {
        return Err(format!("读到的 DEFINER {raw} 缺少用户或主机，不能安全写回。"));
    }
    if user.contains('@') || host.contains('@') {
        return Err(format!("读到的 DEFINER {raw} 里有多余的 @，不能安全写回。"));
    }
    Ok(format!("DEFINER={}@{}", quote_ident(engine, &user), quote_ident(engine, &host)))
}

/// 去掉读回值上可能带的引号（反引号、单引号、双引号）。
fn trim_quotes(value: &str) -> String {
    let tick = '\u{0060}';
    let trimmed = value.trim();
    let trimmed = trimmed.strip_prefix(tick).and_then(|rest| rest.strip_suffix(tick)).unwrap_or(trimmed);
    let trimmed = trimmed.strip_prefix('\'').and_then(|rest| rest.strip_suffix('\'')).unwrap_or(trimmed);
    let trimmed = trimmed.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')).unwrap_or(trimmed);
    trimmed.to_owned()
}

fn routine_keyword(kind: RoutineKind) -> &'static str {
    match kind {
        RoutineKind::Function => "FUNCTION",
        RoutineKind::Procedure => "PROCEDURE",
    }
}

fn kind_label(kind: RoutineKind) -> &'static str {
    match kind {
        RoutineKind::Function => "函数",
        RoutineKind::Procedure => "存储过程",
    }
}

/// 事件启用时的调度器提示：服务器级状态不由这里改变。
pub(super) fn event_scheduler_risks(snapshot: &Snapshot, enabled: bool, alter: bool) -> Vec<Risk> {
    let mut risks = Vec::new();
    let state = snapshot.event.as_ref().and_then(|event| event.scheduler_running);
    if enabled {
        let text = match state {
            Some(false) => "服务器 event_scheduler 现在是关闭的：事件启用后仍然不会运行，这里不改服务器级调度器状态。",
            Some(true) => "事件会按调度运行（服务器 event_scheduler 已打开）；本语句不改调度器状态。",
            None => "没有读到 event_scheduler 的实际状态：启用的事件只有在它打开时才会运行，这里不改服务器级状态。",
        };
        risks.push(Risk { level: if state == Some(false) { "warn" } else { "info" }.to_owned(), text: text.to_owned() });
    } else {
        risks.push(Risk {
            level: "info".to_owned(),
            text: "事件保存为停用状态，不会运行；这里不改服务器级 event_scheduler 状态。".to_owned(),
        });
    }
    if alter {
        risks.push(Risk {
            level: "info".to_owned(),
            text: "ALTER EVENT 只改这个事件的定义与状态，不启动或停止服务器调度器。".to_owned(),
        });
    }
    risks
}

fn routine_risks() -> Vec<Risk> {
    vec![Risk {
        level: "info".to_owned(),
        text: "DEFINER、COMMENT 等已读到的属性按读回值保留；执行权限仍由服务器判定。".to_owned(),
    }]
}

fn trigger_risks() -> Vec<Risk> {
    vec![Risk {
        level: "info".to_owned(),
        text: "触发器在目标表的对应操作上自动执行，主体写错会影响这张表的写入。".to_owned(),
    }]
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
    use crate::ddl::model::{EventState, RoutineParamDraft, RoutineParamState, RoutineState, TriggerState};

    /// 一个读到版本的 MySQL 库级快照：对象动作不读表结构，只用到库名、版本与 sql_mode。
    fn snapshot() -> Snapshot {
        let mut snapshot = Snapshot::bare(Engine::Mysql, "app", "", "");
        snapshot.server_version = "8.4.13".to_owned();
        snapshot.sql_mode = "ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES".to_owned();
        snapshot
    }

    fn event_draft() -> EventDraft {
        EventDraft {
            original: None,
            name: "purge".to_owned(),
            schedule: EventSchedule::Recurring {
                every: "1".to_owned(),
                unit: "DAY".to_owned(),
                starts: "2026-12-01 00:00:00".to_owned(),
                ends: "2027-01-01 00:00:00".to_owned(),
            },
            body: "DELETE FROM app.log WHERE created_at < NOW() - INTERVAL 30 DAY".to_owned(),
            enabled: true,
            on_completion: EventCompletion::NotPreserve,
            comment: "每小时清理".to_owned(),
        }
    }

    fn routine_draft(kind: RoutineKind) -> RoutineDraft {
        RoutineDraft {
            kind,
            original: None,
            name: "total".to_owned(),
            params: vec![RoutineParamDraft { mode: ParamMode::In, name: "seed".to_owned(), type_name: "int".to_owned() }],
            returns: if kind == RoutineKind::Function { "int".to_owned() } else { String::new() },
            body: "BEGIN\n  SELECT 1;\n  SELECT 2;\nEND".to_owned(),
            comment: String::new(),
        }
    }

    fn trigger_draft() -> TriggerDraft {
        TriggerDraft {
            original: None,
            name: "audit".to_owned(),
            table: "orders".to_owned(),
            timing: TriggerTiming::After,
            event: TriggerEvent::Insert,
            body: "BEGIN\n  INSERT INTO app.log VALUES (NEW.id);\n  SET @seen = 1;\nEND".to_owned(),
        }
    }

    /// 周期事件：EVERY 与 STARTS/ENDS、ON COMPLETION、ENABLE、COMMENT、DO 都在同一条语句里。
    #[test]
    fn recurring_event_becomes_one_statement_with_schedule_and_state() {
        let planned = plan(&snapshot(), &Action::CreateEvent { event: event_draft() }, true).expect("dispatched");
        assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
        assert_eq!(planned.steps.len(), 1);
        let sql = planned.steps[0].sql.clone();
        assert!(sql.starts_with("CREATE EVENT app.`purge`"), "{sql}");
        assert!(sql.contains("ON SCHEDULE EVERY 1 DAY STARTS '2026-12-01 00:00:00' ENDS '2027-01-01 00:00:00'"), "{sql}");
        assert!(sql.contains("ON COMPLETION NOT PRESERVE"), "{sql}");
        assert!(sql.contains("\nENABLE\n"), "{sql}");
        assert!(sql.contains("COMMENT '每小时清理'"), "{sql}");
        assert!(sql.contains("DO DELETE FROM app.log"), "{sql}");
        // 调度器状态没读到：提示里不能声称事件会运行。
        assert!(planned.risks.iter().any(|risk| risk.text.contains("没有读到 event_scheduler")));
    }

    /// 单次事件：AT 直接来自用户输入的时间表达式，主体为空时阻断。
    #[test]
    fn one_time_event_keeps_the_at_expression_and_blocks_empty_bodies() {
        let mut event = event_draft();
        event.schedule = EventSchedule::OneTime { at: "CURRENT_TIMESTAMP + INTERVAL 1 HOUR".to_owned() };
        event.enabled = false;
        event.on_completion = EventCompletion::Preserve;
        let planned = plan(&snapshot(), &Action::CreateEvent { event: event.clone() }, true).expect("dispatched");
        let sql = planned.steps[0].sql.clone();
        assert!(sql.contains("ON SCHEDULE AT CURRENT_TIMESTAMP + INTERVAL 1 HOUR"), "{sql}");
        assert!(sql.contains("ON COMPLETION PRESERVE"), "{sql}");
        assert!(sql.contains("\nDISABLE\n"), "{sql}");
        assert!(planned.risks.iter().any(|risk| risk.text.contains("停用状态")));

        let mut empty = event.clone();
        empty.body = "   ".to_owned();
        let blocked = plan(&snapshot(), &Action::CreateEvent { event: empty }, true).expect("dispatched");
        assert!(blocked.steps.is_empty());
        assert!(blocked.blocked.iter().any(|text| text.contains("主体为空")));
    }

    /// 调度写成服务器不接受的间隔时不生成语句；无效输入保留原因。
    #[test]
    fn invalid_schedules_are_blocked_before_any_statement() {
        let mut event = event_draft();
        event.schedule = EventSchedule::Recurring { every: "0".to_owned(), unit: "DAY".to_owned(), starts: String::new(), ends: String::new() };
        let planned = plan(&snapshot(), &Action::CreateEvent { event: event.clone() }, true).expect("dispatched");
        assert!(planned.steps.is_empty());
        assert!(planned.blocked.iter().any(|text| text.contains("不是正整数")), "{:?}", planned.blocked);

        event.schedule = EventSchedule::Recurring { every: "1".to_owned(), unit: "FORTNIGHT".to_owned(), starts: String::new(), ends: String::new() };
        let planned = plan(&snapshot(), &Action::CreateEvent { event }, true).expect("dispatched");
        assert!(planned.steps.is_empty());
        assert!(planned.blocked.iter().any(|text| text.contains("间隔单位")), "{:?}", planned.blocked);
    }

    /// 修改事件：读到的调度器是关闭时，启用事件必须明确说它仍然不会运行。
    #[test]
    fn altering_an_event_reports_the_real_scheduler_state() {
        let mut state_snapshot = snapshot();
        state_snapshot.event = Some(EventState {
            name: "purge".to_owned(),
            enabled: false,
            schedule: EventSchedule::OneTime { at: "2026-12-01 10:00:00".to_owned() },
            body: "DELETE FROM app.log".to_owned(),
            on_completion: EventCompletion::Preserve,
            comment: String::new(),
            definer: "root@localhost".to_owned(),
            scheduler_running: Some(false),
        });
        let mut event = event_draft();
        event.original = Some("purge".to_owned());
        event.name = "purge_v2".to_owned();
        let planned = plan(&state_snapshot, &Action::AlterEvent { event }, true).expect("dispatched");
        assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
        let sql = planned.steps[0].sql.clone();
        assert!(sql.starts_with("ALTER EVENT app.`purge`"), "{sql}");
        assert!(sql.contains("RENAME TO purge_v2"), "{sql}");
        let warn = planned.risks.iter().find(|risk| risk.level == "warn").expect("调度器关闭要有 warn");
        assert!(warn.text.contains("仍然不会运行"), "{}", warn.text);
        assert!(warn.text.contains("不改服务器级"), "{}", warn.text);
    }

    /// 读不到现有事件时阻断：没有读到的定义不能当修改基线。
    #[test]
    fn altering_an_unread_event_is_blocked() {
        let mut event = event_draft();
        event.original = Some("purge".to_owned());
        let planned = plan(&snapshot(), &Action::AlterEvent { event }, true).expect("dispatched");
        assert!(planned.steps.is_empty());
        assert!(planned.blocked.iter().any(|text| text.contains("没有读到事件")));
    }

    /// 函数：参数、RETURNS 与多语句主体整段留在一条语句里，不被分号拆开。
    #[test]
    fn function_keeps_params_returns_and_the_whole_multi_statement_body() {
        let planned = plan(&snapshot(), &Action::CreateRoutine { routine: routine_draft(RoutineKind::Function) }, true).expect("dispatched");
        assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
        let sql = planned.steps[0].sql.clone();
        assert!(sql.starts_with("CREATE FUNCTION app.total(seed int) RETURNS int"), "{sql}");
        assert!(sql.contains("BEGIN\n  SELECT 1;\n  SELECT 2;\nEND"), "{sql}");
        assert_eq!(planned.steps.len(), 1, "多语句主体不能拆成多条 Step");
    }

    /// 存储过程：IN/OUT/INOUT 都写进参数清单，不带 RETURNS。
    #[test]
    fn procedure_keeps_param_modes_without_returns() {
        let mut routine = routine_draft(RoutineKind::Procedure);
        routine.params = vec![
            RoutineParamDraft { mode: ParamMode::In, name: "seed".to_owned(), type_name: "int".to_owned() },
            RoutineParamDraft { mode: ParamMode::Out, name: "total".to_owned(), type_name: "decimal(10,2)".to_owned() },
            RoutineParamDraft { mode: ParamMode::InOut, name: "count".to_owned(), type_name: "int".to_owned() },
        ];
        let planned = plan(&snapshot(), &Action::CreateRoutine { routine }, true).expect("dispatched");
        let sql = planned.steps[0].sql.clone();
        assert!(sql.starts_with("CREATE PROCEDURE app.total("), "{sql}");
        assert!(sql.contains("IN seed int"), "{sql}");
        assert!(sql.contains("OUT total decimal(10,2)"), "{sql}");
        assert!(sql.contains("INOUT count int"), "{sql}");
        assert!(!sql.contains("RETURNS"), "{sql}");
    }

    /// 函数写 OUT 参数、函数缺返回类型：客户端直接阻断，不发给服务器。
    #[test]
    fn invalid_function_shapes_are_blocked() {
        let mut routine = routine_draft(RoutineKind::Function);
        routine.params[0].mode = ParamMode::Out;
        routine.returns = "  ".to_owned();
        let planned = plan(&snapshot(), &Action::CreateRoutine { routine }, true).expect("dispatched");
        assert!(planned.steps.is_empty());
        assert!(planned.blocked.iter().any(|text| text.contains("只能是 IN")), "{:?}", planned.blocked);
        assert!(planned.blocked.iter().any(|text| text.contains("返回类型")), "{:?}", planned.blocked);
    }

    /// 替换例程：DROP + CREATE 两步，保留读到的 DEFINER，并明确两步之间对象可能已不存在。
    #[test]
    fn replacing_a_routine_preserves_the_definer_and_warns_about_the_gap() {
        let mut state_snapshot = snapshot();
        state_snapshot.routine = Some(RoutineState {
            kind: RoutineKind::Function,
            name: "total".to_owned(),
            params: vec![RoutineParamState { position: 1, mode: ParamMode::In, name: "seed".to_owned(), type_name: "int".to_owned() }],
            returns: "int".to_owned(),
            body: "RETURN 1".to_owned(),
            definer: "app_owner@%".to_owned(),
            comment: "旧注释".to_owned(),
            create_sql: String::new(),
        });
        let mut routine = routine_draft(RoutineKind::Function);
        routine.original = Some("total".to_owned());
        let planned = plan(&state_snapshot, &Action::ReplaceRoutine { routine }, true).expect("dispatched");
        assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
        assert_eq!(planned.steps.len(), 2);
        assert_eq!(planned.steps[0].sql, "DROP FUNCTION app.total;");
        assert!(
            planned.steps[1].sql.starts_with("CREATE DEFINER=app_owner@`%` FUNCTION"),
            "{}",
            planned.steps[1].sql
        );
        let danger = planned.risks.iter().find(|risk| risk.level == "danger").expect("替换要有 danger 风险");
        assert!(danger.text.contains("不能声称整体回滚"), "{}", danger.text);
    }

    /// 函数与存储过程不能互相替换：读回类型与草稿不一致时阻断。
    #[test]
    fn routines_cannot_be_swapped_for_each_other() {
        let mut state_snapshot = snapshot();
        state_snapshot.routine = Some(RoutineState {
            kind: RoutineKind::Procedure,
            name: "total".to_owned(),
            params: Vec::new(),
            returns: String::new(),
            body: "SELECT 1".to_owned(),
            definer: "root@localhost".to_owned(),
            comment: String::new(),
            create_sql: String::new(),
        });
        let mut routine = routine_draft(RoutineKind::Function);
        routine.original = Some("total".to_owned());
        let planned = plan(&state_snapshot, &Action::ReplaceRoutine { routine }, true).expect("dispatched");
        assert!(planned.steps.is_empty());
        assert!(planned.blocked.iter().any(|text| text.contains("不能互相替换")), "{:?}", planned.blocked);
    }

    /// 读不到 DEFINER 时不替换：不能用空值覆盖服务器对象。
    #[test]
    fn replacing_without_a_read_definer_is_blocked() {
        let mut state_snapshot = snapshot();
        state_snapshot.routine = Some(RoutineState {
            kind: RoutineKind::Function,
            name: "total".to_owned(),
            params: Vec::new(),
            returns: "int".to_owned(),
            body: "RETURN 1".to_owned(),
            definer: String::new(),
            comment: String::new(),
            create_sql: String::new(),
        });
        let mut routine = routine_draft(RoutineKind::Function);
        routine.original = Some("total".to_owned());
        let planned = plan(&state_snapshot, &Action::ReplaceRoutine { routine }, true).expect("dispatched");
        assert!(planned.steps.is_empty());
        assert!(planned.blocked.iter().any(|text| text.contains("没有读到") && text.contains("DEFINER")), "{:?}", planned.blocked);
    }

    /// 触发器：目标表带库名，时机与事件来自草稿，多语句主体完整。
    #[test]
    fn trigger_keeps_table_timing_event_and_multi_statement_body() {
        let planned = plan(&snapshot(), &Action::CreateTrigger { trigger: trigger_draft() }, true).expect("dispatched");
        assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
        let sql = planned.steps[0].sql.clone();
        assert!(
            sql.starts_with(
                "CREATE TRIGGER app.audit AFTER INSERT ON app.orders FOR EACH ROW"
            ),
            "{sql}"
        );
        assert!(sql.contains("BEGIN\n  INSERT INTO app.log VALUES (NEW.id);\n  SET @seen = 1;\nEND"), "{sql}");

        let missing_table = TriggerDraft { table: "  ".to_owned(), ..trigger_draft() };
        let planned = plan(&snapshot(), &Action::CreateTrigger { trigger: missing_table }, true).expect("dispatched");
        assert!(planned.steps.is_empty());
        assert!(planned.blocked.iter().any(|text| text.contains("目标表无效")));
    }

    /// 替换触发器：DROP 针对明确的库与触发器名，保留 DEFINER。
    #[test]
    fn replacing_a_trigger_drops_the_qualified_name_and_keeps_the_definer() {
        let mut state_snapshot = snapshot();
        state_snapshot.trigger = Some(TriggerState {
            name: "audit".to_owned(),
            table: "orders".to_owned(),
            timing: TriggerTiming::Before,
            event: TriggerEvent::Insert,
            body: "SET @old = 1".to_owned(),
            definer: "root@localhost".to_owned(),
            create_sql: String::new(),
        });
        let mut trigger = trigger_draft();
        trigger.original = Some("audit".to_owned());
        let planned = plan(&state_snapshot, &Action::ReplaceTrigger { trigger }, true).expect("dispatched");
        assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
        assert_eq!(planned.steps.len(), 2);
        assert_eq!(planned.steps[0].sql, "DROP TRIGGER app.audit;");
        assert!(
            planned.steps[1].sql.contains("CREATE DEFINER=root@localhost TRIGGER"),
            "{}",
            planned.steps[1].sql
        );
    }

    /// 删除对象：语句必须是明确的库 + 名字；按错误的类型删除时客户端阻断。
    #[test]
    fn drops_are_scoped_to_the_database() {
        let mut state_snapshot = snapshot();
        state_snapshot.event = Some(EventState {
            name: "purge".to_owned(),
            enabled: true,
            schedule: EventSchedule::OneTime { at: "2026-12-01 10:00:00".to_owned() },
            body: "DELETE FROM app.log".to_owned(),
            on_completion: EventCompletion::Preserve,
            comment: String::new(),
            definer: "root@localhost".to_owned(),
            scheduler_running: Some(true),
        });
        let planned = plan(&state_snapshot, &Action::DropEvent { event: "purge".to_owned() }, true).expect("dispatched");
        assert_eq!(planned.steps[0].sql, "DROP EVENT app.`purge`;");

        state_snapshot.routine = Some(RoutineState {
            kind: RoutineKind::Procedure,
            name: "total".to_owned(),
            params: Vec::new(),
            returns: String::new(),
            body: "SELECT 1".to_owned(),
            definer: "root@localhost".to_owned(),
            comment: String::new(),
            create_sql: String::new(),
        });
        let planned = plan(&state_snapshot, &Action::DropRoutine { routine_kind: RoutineKind::Procedure, name: "total".to_owned() }, true)
            .expect("dispatched");
        assert_eq!(planned.steps[0].sql, "DROP PROCEDURE app.total;");
        // 按函数删一个存储过程：客户端阻断，不发语句。
        let planned = plan(&state_snapshot, &Action::DropRoutine { routine_kind: RoutineKind::Function, name: "total".to_owned() }, true)
            .expect("dispatched");
        assert!(planned.steps.is_empty());
        assert!(planned.blocked.iter().any(|text| text.contains("不是函数")), "{:?}", planned.blocked);
    }

    /// 对象快照没带服务器版本时不阻断：事件/例程/触发器在全部目标版本都有，语句形状也没有版本分支，
    /// 一律阻断会把整条链路关死；真正的语法或权限问题由服务器拒绝并保留原始错误。
    #[test]
    fn unknown_server_version_still_generates_object_statements() {
        let mut unknown = snapshot();
        unknown.server_version = String::new();
        let planned = plan(&unknown, &Action::CreateEvent { event: event_draft() }, true).expect("dispatched");
        assert!(planned.blocked.is_empty(), "{:?}", planned.blocked);
        assert_eq!(planned.steps.len(), 1);
    }

    /// 别的引擎请求这些对象时给出拒绝原因，不生成语句。
    #[test]
    fn other_engines_get_a_refusal_without_statements() {
        let mut postgres = Snapshot::bare(Engine::Postgres, "app", "public", "app");
        postgres.kind = "table".to_owned();
        let action = Action::CreateTrigger { trigger: trigger_draft() };
        let planned = plan(&postgres, &action, false).expect("dispatched");
        assert!(planned.steps.is_empty());
        assert!(planned.blocked.iter().any(|text| text.contains("MySQL")));
    }

    /// 表结构动作不在这里处理。
    #[test]
    fn table_actions_are_left_to_the_table_planner() {
        assert!(plan(&snapshot(), &Action::Vacuum, true).is_none());
    }

    /// DEFINER 写法：user@host 按标识符引号写回，带引号或缺少 @ 时不猜。
    #[test]
    fn definer_clause_quotes_both_halves() {
        assert_eq!(definer_clause(Engine::Mysql, "root@localhost").unwrap(), "DEFINER=root@localhost");
        assert_eq!(definer_clause(Engine::Mysql, "\u{0060}app\u{0060}@\u{0060}%\u{0060}").unwrap(), "DEFINER=app@`%`");
        assert!(definer_clause(Engine::Mysql, "").is_err());
        assert!(definer_clause(Engine::Mysql, "root").is_err());
        assert!(definer_clause(Engine::Mysql, "a@b@c").is_err());
    }
}