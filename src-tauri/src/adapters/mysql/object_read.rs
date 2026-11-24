//! MySQL 家族的对象读回：事件、函数/存储过程与触发器的结构化快照。
//!
//! 元数据优先取 information_schema；需要原文核对时再取 SHOW CREATE。读取失败只影响对应对象，
//! 普通表结构、连接与其它对象分组不受影响（故障隔离）。
//!
//! 未开放编辑但修改时需要保留的属性（DEFINER、SQL SECURITY、COMMENT 等）也读出来，读不到时
//! 由调用方阻断编辑，不能用空值覆盖服务器对象。

use crate::ddl::model::{
    EventCompletion, EventSchedule, EventState, ParamMode, RoutineKind, RoutineParamState, RoutineState, TriggerEvent, TriggerState, TriggerTiming,
};

use super::ddl_read::mysql_string;

/// 事件行的字段。与 SQL 的列顺序一致，读与映射分开，映射可以单独测试。
pub(super) struct EventRow {
    pub name: String,
    pub status: String,
    pub execute_at: String,
    pub interval_value: String,
    pub interval_field: String,
    pub starts: String,
    pub ends: String,
    pub on_completion: String,
    pub body: String,
    pub definer: String,
    pub comment: String,
}

impl EventRow {
    /// 从一行读回来构造。列顺序见 EVENT_SQL。
    pub(super) fn read(row: &mysql_async::Row) -> Self {
        Self {
            name: mysql_string(row, 0),
            status: mysql_string(row, 1),
            execute_at: mysql_string(row, 2),
            interval_value: mysql_string(row, 3),
            interval_field: mysql_string(row, 4),
            starts: mysql_string(row, 5),
            ends: mysql_string(row, 6),
            on_completion: mysql_string(row, 7),
            body: mysql_string(row, 8),
            definer: mysql_string(row, 9),
            comment: mysql_string(row, 10),
        }
    }
}

pub(super) const EVENT_SQL: &str = "SELECT EVENT_NAME, STATUS, CAST(EXECUTE_AT AS CHAR), INTERVAL_VALUE, INTERVAL_FIELD, CAST(STARTS AS CHAR), CAST(ENDS AS CHAR), ON_COMPLETION, EVENT_DEFINITION, DEFINER, EVENT_COMMENT FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ? AND EVENT_NAME = ?";

/// 例程行。只按 schema + name 查，会在同库同名函数与存储过程并存时读到另一个对象，
/// 因此必须再按 ROUTINE_TYPE 过滤（函数与存储过程是两个独立对象）。
pub(super) const ROUTINE_SQL: &str = "SELECT ROUTINE_TYPE, DATA_TYPE, ROUTINE_DEFINITION, DEFINER, ROUTINE_COMMENT FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ? AND ROUTINE_NAME = ? AND ROUTINE_TYPE = ?";

/// 参数行。同样按 ROUTINE_TYPE 过滤，否则同名函数与存储过程的参数会被合并。
/// ROUTINE_TYPE 也作为列读回来：读回类型与请求类型不一致时映射层再挡一次，不靠查询单点保证。
pub(super) const PARAMETERS_SQL: &str = "SELECT ROUTINE_TYPE, ORDINAL_POSITION, PARAMETER_MODE, PARAMETER_NAME, DTD_IDENTIFIER FROM information_schema.PARAMETERS WHERE SPECIFIC_SCHEMA = ? AND SPECIFIC_NAME = ? AND ROUTINE_TYPE = ? ORDER BY ORDINAL_POSITION";

pub(super) const TRIGGER_SQL: &str = "SELECT TRIGGER_NAME, EVENT_OBJECT_TABLE, ACTION_TIMING, EVENT_MANIPULATION, ACTION_STATEMENT, DEFINER FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = ? AND TRIGGER_NAME = ?";

/// 事件行 + 服务器 event_scheduler 实际状态 → 事件快照。
pub(super) fn event_state(row: EventRow, scheduler_running: Option<bool>) -> EventState {
    EventState {
        name: row.name,
        enabled: row.status.eq_ignore_ascii_case("ENABLED"),
        schedule: schedule_of(&row.execute_at, &row.interval_value, &row.interval_field, &row.starts, &row.ends),
        body: row.body,
        on_completion: if row.on_completion.eq_ignore_ascii_case("NOT PRESERVE") { EventCompletion::NotPreserve } else { EventCompletion::Preserve },
        comment: row.comment,
        definer: row.definer,
        scheduler_running,
    }
}

/// 单次事件（AT）与周期事件（EVERY ...）的判定：有间隔值就是周期事件。
pub(super) fn schedule_of(execute_at: &str, interval_value: &str, interval_field: &str, starts: &str, ends: &str) -> EventSchedule {
    if interval_value.is_empty() && interval_field.is_empty() {
        return EventSchedule::OneTime { at: execute_at.to_owned() };
    }
    EventSchedule::Recurring {
        every: interval_value.to_owned(),
        unit: interval_field.to_owned(),
        starts: starts.to_owned(),
        ends: ends.to_owned(),
    }
}

/// 参数模式：MySQL 的函数返回行没有模式与名字，这里按空值跳过。
pub(super) fn param_mode_of(raw: &str) -> ParamMode {
    match raw.trim().to_ascii_uppercase().as_str() {
        "OUT" => ParamMode::Out,
        "INOUT" => ParamMode::InOut,
        _ => ParamMode::In,
    }
}

/// 例程类型原文 → 枚举。不是 FUNCTION 的一律按存储过程，但调用方要先用原文核对类型，
/// 不能用这里的默认值把函数当成存储过程替换。
pub(super) fn routine_kind_of(raw: &str) -> RoutineKind {
    if raw.trim().eq_ignore_ascii_case("FUNCTION") {
        RoutineKind::Function
    } else {
        RoutineKind::Procedure
    }
}

/// 例程类型原文：查询参数与读回核对都用它，不靠调用方各自拼字符串。
pub(super) fn routine_type_keyword(kind: RoutineKind) -> &'static str {
    match kind {
        RoutineKind::Function => "FUNCTION",
        RoutineKind::Procedure => "PROCEDURE",
    }
}

/// 例程参数行（例程类型、顺序号、模式、名字、类型）→ 结构化参数。
/// 函数在信息模式里有一行顺序号 0 的返回类型（模式与名字为空），它不是参数。
///
/// 同名函数与存储过程在同一个库里可以并存，它们是两个独立对象：这里按请求的类型再过滤一次，
/// 查询本身也已经按 ROUTINE_TYPE 过滤；两处都做是为了让参数合并这种事故不可能只靠一条 SQL 成立。
pub(super) fn routine_params(rows: &[(String, String, String, String, String)], kind: RoutineKind) -> (Vec<RoutineParamState>, String) {
    let mut params = Vec::new();
    let mut returns = String::new();
    for (routine_type, position, mode, name, type_name) in rows {
        if routine_kind_of(routine_type) != kind {
            continue;
        }
        let ordinal: u32 = position.trim().parse().unwrap_or(0);
        if mode.trim().is_empty() && name.trim().is_empty() {
            if ordinal == 0 {
                returns = type_name.clone();
            }
            continue;
        }
        params.push(RoutineParamState { position: ordinal, mode: param_mode_of(mode), name: name.clone(), type_name: type_name.clone() });
    }
    (params, returns)
}

/// 触发时机与事件的原文 → 枚举。
pub(super) fn trigger_timing_of(raw: &str) -> TriggerTiming {
    if raw.trim().eq_ignore_ascii_case("AFTER") {
        TriggerTiming::After
    } else {
        TriggerTiming::Before
    }
}

pub(super) fn trigger_event_of(raw: &str) -> TriggerEvent {
    match raw.trim().to_ascii_uppercase().as_str() {
        "UPDATE" => TriggerEvent::Update,
        "DELETE" => TriggerEvent::Delete,
        _ => TriggerEvent::Insert,
    }
}

/// 读服务器 event_scheduler 变量。读不到时返回 None：调度器状态未知，不能当成“会运行”。
pub(super) async fn event_scheduler_running(conn: &mut mysql_async::Conn) -> Option<bool> {
    let row: Option<mysql_async::Row> = mysql_async::prelude::Queryable::query_first(conn, "SHOW VARIABLES LIKE 'event_scheduler'")
        .await
        .ok()?;
    let value = row.map(|row| mysql_string(&row, 1)).unwrap_or_default();
    Some(value.eq_ignore_ascii_case("ON") || value == "1")
}

/// 读一个事件。找不到时给出可识别的原因；调度器状态单独读取，失败只让状态为 None。
pub(super) async fn load_event(pool: &mysql_async::Pool, database: &str, name: &str) -> Result<EventState, String> {
    use mysql_async::prelude::Queryable;
    if database.is_empty() {
        return Err("需要数据库名，没有另开连接。".to_owned());
    }
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    let row: Option<mysql_async::Row> = conn
        .exec_first(EVENT_SQL, (database.to_owned(), name.to_owned()))
        .await
        .map_err(|err| format!("读不到事件 {database}.{name}：{}", crate::error_text::chain(&err)))?;
    let row = row.ok_or_else(|| format!("找不到事件 {database}.{name}。"))?;
    let scheduler = event_scheduler_running(&mut conn).await;
    Ok(event_state(EventRow::read(&row), scheduler))
}

/// 读一个函数或存储过程。类型不一致时明确报错，不把函数当成存储过程。
pub(super) async fn load_routine(pool: &mysql_async::Pool, database: &str, kind: RoutineKind, name: &str) -> Result<RoutineState, String> {
    use mysql_async::prelude::Queryable;
    if database.is_empty() {
        return Err("需要数据库名，没有另开连接。".to_owned());
    }
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    let row: Option<mysql_async::Row> = conn
        .exec_first(ROUTINE_SQL, (database.to_owned(), name.to_owned(), routine_type_keyword(kind).to_owned()))
        .await
        .map_err(|err| format!("读不到例程 {database}.{name}：{}", crate::error_text::chain(&err)))?;
    let row = row.ok_or_else(|| format!("找不到例程 {database}.{name}。"))?;
    let reported = mysql_string(&row, 0);
    if routine_kind_of(&reported) != kind {
        return Err(format!("{database}.{name} 是{}，不是{}。", label_of(routine_kind_of(&reported)), label_of(kind)));
    }
    let params: Vec<mysql_async::Row> = conn
        .exec(PARAMETERS_SQL, (database.to_owned(), name.to_owned(), routine_type_keyword(kind).to_owned()))
        .await
        .map_err(|err| format!("读不到例程 {database}.{name} 的参数：{}", crate::error_text::chain(&err)))?;
    let tuples: Vec<(String, String, String, String, String)> = params
        .iter()
        .map(|row| (mysql_string(row, 0), mysql_string(row, 1), mysql_string(row, 2), mysql_string(row, 3), mysql_string(row, 4)))
        .collect();
    let (params, returns) = routine_params(&tuples, kind);
    // 非定义者且缺少 SHOW_ROUTINE（或 MariaDB 的 mysql.proc 读权限）时服务器把定义返回为 NULL：
    // 不能当成空主体交给编辑器，给出与只读定义查看器相同的权限原因（R6）。
    if matches!(row.as_ref(2), None | Some(mysql_async::Value::NULL)) {
        return Err(super::routine_definition::hidden_definition_error(&mut conn, database, name, &mysql_string(&row, 3)).await);
    }
    let reported_returns = mysql_string(&row, 1);
    Ok(RoutineState {
        kind,
        name: name.to_owned(),
        params,
        returns: if returns.is_empty() { reported_returns } else { returns },
        body: mysql_string(&row, 2),
        definer: mysql_string(&row, 3),
        comment: mysql_string(&row, 4),
        create_sql: String::new(),
    })
}

fn label_of(kind: RoutineKind) -> &'static str {
    match kind {
        RoutineKind::Function => "函数",
        RoutineKind::Procedure => "存储过程",
    }
}

/// 读一个触发器。
pub(super) async fn load_trigger(pool: &mysql_async::Pool, database: &str, name: &str) -> Result<TriggerState, String> {
    use mysql_async::prelude::Queryable;
    if database.is_empty() {
        return Err("需要数据库名，没有另开连接。".to_owned());
    }
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    let row: Option<mysql_async::Row> = conn
        .exec_first(TRIGGER_SQL, (database.to_owned(), name.to_owned()))
        .await
        .map_err(|err| format!("读不到触发器 {database}.{name}：{}", crate::error_text::chain(&err)))?;
    let row = row.ok_or_else(|| format!("找不到触发器 {database}.{name}。"))?;
    Ok(TriggerState {
        name: mysql_string(&row, 0),
        table: mysql_string(&row, 1),
        timing: trigger_timing_of(&mysql_string(&row, 2)),
        event: trigger_event_of(&mysql_string(&row, 3)),
        body: mysql_string(&row, 4),
        definer: mysql_string(&row, 5),
        create_sql: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 参数行：例程类型、顺序号、模式、名字、类型。
    fn row(values: &[(&str, &str, &str, &str, &str)]) -> Vec<(String, String, String, String, String)> {
        values
            .iter()
            .map(|(routine_type, position, mode, name, type_name)| {
                ((*routine_type).to_owned(), (*position).to_owned(), (*mode).to_owned(), (*name).to_owned(), (*type_name).to_owned())
            })
            .collect()
    }

    /// 有间隔值是周期事件，没有就是单次事件；STARTS/ENDS 按读回原文保留。
    #[test]
    fn schedules_distinguish_one_time_from_recurring_events() {
        assert_eq!(
            schedule_of("2026-12-01 10:00:00", "", "", "", ""),
            EventSchedule::OneTime { at: "2026-12-01 10:00:00".to_owned() }
        );
        assert_eq!(
            schedule_of("", "1", "DAY", "2026-12-01 10:00:00", "2027-01-01 10:00:00"),
            EventSchedule::Recurring {
                every: "1".to_owned(),
                unit: "DAY".to_owned(),
                starts: "2026-12-01 10:00:00".to_owned(),
                ends: "2027-01-01 10:00:00".to_owned(),
            }
        );
    }

    /// 函数的返回类型行（顺序号 0、没有模式与名字）不算参数，但它的类型是返回类型。
    #[test]
    fn function_return_row_is_not_a_parameter() {
        let (params, returns) = routine_params(
            &row(&[
                ("FUNCTION", "0", "", "", "int"),
                ("FUNCTION", "1", "IN", "seed", "int"),
                ("FUNCTION", "2", "IN", "step", "int"),
            ]),
            RoutineKind::Function,
        );
        assert_eq!(returns, "int");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "seed");
        assert_eq!(params[0].mode, ParamMode::In);
        let (params, returns) = routine_params(
            &row(&[("PROCEDURE", "1", "OUT", "total", "decimal(10,2)"), ("PROCEDURE", "2", "INOUT", "count", "int")]),
            RoutineKind::Procedure,
        );
        assert_eq!(returns, "");
        assert_eq!(params[1].mode, ParamMode::InOut);
        assert_eq!(params[0].type_name, "decimal(10,2)");
    }

    /// 同库同名函数与存储过程并存：参数行必须按 ROUTINE_TYPE 分开，不能把两类参数合并。
    /// 查询本身带 ROUTINE_TYPE 条件，这里的映射层再挡一次，两条防线都要成立。
    #[test]
    fn same_name_function_and_procedure_do_not_share_parameters() {
        let rows = row(&[
            ("FUNCTION", "0", "", "", "int"),
            ("FUNCTION", "1", "IN", "seed", "int"),
            ("PROCEDURE", "1", "IN", "seed", "varchar(20)"),
            ("PROCEDURE", "2", "OUT", "total", "decimal(10,2)"),
        ]);
        let (function_params, function_returns) = routine_params(&rows, RoutineKind::Function);
        assert_eq!(function_returns, "int");
        assert_eq!(function_params.len(), 1, "{function_params:?}");
        assert_eq!(function_params[0].name, "seed");
        assert_eq!(function_params[0].type_name, "int");
        assert_eq!(function_params[0].mode, ParamMode::In);

        let (procedure_params, procedure_returns) = routine_params(&rows, RoutineKind::Procedure);
        assert_eq!(procedure_returns, "");
        assert_eq!(procedure_params.len(), 2, "{procedure_params:?}");
        assert_eq!(procedure_params[0].type_name, "varchar(20)");
        assert_eq!(procedure_params[1].mode, ParamMode::Out);

        // 两条查询都必须按 ROUTINE_TYPE 过滤：只按 schema + name 查会读到另一个对象。
        assert!(ROUTINE_SQL.contains("ROUTINE_TYPE = ?"), "{ROUTINE_SQL}");
        assert!(PARAMETERS_SQL.contains("ROUTINE_TYPE = ?"), "{PARAMETERS_SQL}");
        assert_eq!(routine_type_keyword(RoutineKind::Function), "FUNCTION");
        assert_eq!(routine_type_keyword(RoutineKind::Procedure), "PROCEDURE");
    }

    /// 时机关联到枚举；不认识的值按下限处理但原文仍可用于核对。
    #[test]
    fn trigger_timing_and_event_map_to_the_model() {
        assert_eq!(trigger_timing_of("BEFORE"), TriggerTiming::Before);
        assert_eq!(trigger_timing_of("after"), TriggerTiming::After);
        assert_eq!(trigger_event_of("INSERT"), TriggerEvent::Insert);
        assert_eq!(trigger_event_of("update"), TriggerEvent::Update);
        assert_eq!(trigger_event_of("DELETE"), TriggerEvent::Delete);
        assert_eq!(routine_kind_of("FUNCTION"), RoutineKind::Function);
        assert_eq!(routine_kind_of("PROCEDURE"), RoutineKind::Procedure);
    }

    /// 事件行映射：停用事件就是停用，ON COMPLETION NOT PRESERVE 单独建模，调度器状态原样保留。
    #[test]
    fn event_state_keeps_status_completion_and_scheduler_fact() {
        let state = event_state(
            EventRow {
                name: "purge".to_owned(),
                status: "DISABLED".to_owned(),
                execute_at: String::new(),
                interval_value: "1".to_owned(),
                interval_field: "HOUR".to_owned(),
                starts: String::new(),
                ends: String::new(),
                on_completion: "NOT PRESERVE".to_owned(),
                body: "DELETE FROM log".to_owned(),
                definer: "root@localhost".to_owned(),
                comment: "每小时清理".to_owned(),
            },
            Some(false),
        );
        assert!(!state.enabled);
        assert_eq!(state.on_completion, EventCompletion::NotPreserve);
        assert_eq!(state.scheduler_running, Some(false));
        assert_eq!(state.body, "DELETE FROM log");
        assert!(matches!(state.schedule, EventSchedule::Recurring { .. }));
    }
}