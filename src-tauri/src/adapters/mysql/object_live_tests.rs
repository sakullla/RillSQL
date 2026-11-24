//! 真实服务器上的对象验收（R5 / R6 / R7 / R8 / R9）。
//!
//! 这些用例只在 scripts/verify-mysql-objects.py 启动的 MySQL 与 MariaDB 夹具上运行，默认被
//! #[ignore] 跳过：单元测试不能替代真实服务器上的建/改/删与读回。脚本核对每个引擎都真的跑过，
//! 并把实际版本写进 docs/verification/2026-11-21-mysql-proprietary-support/objects-live.md。
//!
//! 端口来自环境变量：RILLSQL_OBJECT_MYSQL_PORT / RILLSQL_OBJECT_MARIADB_PORT（默认 3436 / 3437）。
//!
//! 每个用例都走真实链路：preview_request 生成并阻断，execute_request 逐条执行，读回走
//! information_schema（object_read），不用单元测试里的假快照。

use std::sync::Arc;

use mysql_async::prelude::Queryable;

use crate::application::ddl;
use crate::application::AppServices;
use crate::connections::model::{ConnectionDraft, Engine, Secret, SecretInput, TlsMode};
use crate::connections::CatalogSession;
use crate::connections::Service;
use crate::ddl::model::{
    Action, EventCompletion, EventDraft, EventSchedule, ParamMode, Plan, RoutineDraft, RoutineKind, RoutineParamDraft, StepStatus, TriggerDraft,
    TriggerEvent, TriggerTiming,
};

use super::object_read;

/// 夹具里的库名（docker/init/mysql-objects.sql 会建好；脚本再执行一次也是幂等的）。
const DATABASE: &str = "rillsql_proprietary";

/// 一个真实服务器夹具：连接已经保存并打开。
struct Live {
    services: Arc<AppServices>,
    id: String,
    engine: Engine,
    pool: mysql_async::Pool,
}

impl Live {
    async fn connect(engine: Engine, port: String) -> Self {
        let dir = std::env::temp_dir().join(format!("rillsql-object-live-{}-{}", engine.as_str(), uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(dir.join("connections.sqlite")))));
        let id = format!("object-live-{}", engine.as_str());
        let mut draft = ConnectionDraft::blank(engine);
        draft.id = Some(id.clone());
        draft.name = id.clone();
        draft.host = "127.0.0.1".into();
        draft.port = port;
        draft.username = "root".into();
        draft.database = DATABASE.into();
        draft.tls_mode = TlsMode::Off;
        draft.save_password = false;
        services.connections.save(draft, SecretInput::default()).await.expect("保存连接");
        let opened = services
            .connections
            .open(&id, SecretInput { password: Some(Secret::new("root")), ..Default::default() })
            .await
            .expect("打开连接");
        assert!(opened.connected, "{:?}", opened.report);
        let session = services.connections.catalog_session(&id).expect("已打开的会话");
        let CatalogSession::Mysql(pool) = session else {
            panic!("{} 夹具不是 MySQL 家族的会话", engine.as_str());
        };
        Self { services, id, engine, pool }
    }

    fn engine_name(&self) -> &'static str {
        match self.engine {
            Engine::Mariadb => "mariadb",
            _ => "mysql",
        }
    }

    fn request(&self, action: Action) -> crate::ddl::Request {
        ddl::request(DATABASE.into(), String::new(), self.engine_name(), action).expect("请求")
    }

    async fn preview(&self, action: Action) -> Plan {
        self.preview_result(action).await.expect("预览")
    }

    /// 预览本身失败（例如按另一类去改一个读回不到的对象）时拿到原因，不当成通过。
    async fn preview_result(&self, action: Action) -> Result<Plan, String> {
        let request = self.request(action);
        ddl::preview_request(&self.services.connections, &self.id, &request).await
    }

    /// 预览与执行走同一条链：执行提交的 SQL 就是预览里的语句（不一致会被执行层拒绝）。
    async fn run(&self, action: Action) -> (Plan, crate::ddl::Receipt) {
        let request = self.request(action);
        let plan = ddl::preview_request(&self.services.connections, &self.id, &request).await.expect("预览");
        assert!(plan.blocked.is_empty(), "预览被阻断：{:?}", plan.blocked);
        let sql: Vec<String> = plan.steps.iter().map(|step| step.sql.clone()).collect();
        let receipt = ddl::execute_request(&self.services.connections, &self.id, &request, &sql, true).await.expect("执行");
        assert!(receipt.blocked.is_empty(), "执行被阻断：{:?}", receipt.blocked);
        (plan, receipt)
    }

    /// 只预览 + 执行，不要求全部成功：失败路径要看到真实回执。
    async fn run_allowing_failure(&self, action: Action) -> crate::ddl::Receipt {
        let request = self.request(action);
        let plan = ddl::preview_request(&self.services.connections, &self.id, &request).await.expect("预览");
        assert!(plan.blocked.is_empty(), "这不是客户端能直接判断的错误：{:?}", plan.blocked);
        let sql: Vec<String> = plan.steps.iter().map(|step| step.sql.clone()).collect();
        ddl::execute_request(&self.services.connections, &self.id, &request, &sql, true).await.expect("执行")
    }

    async fn event_state(&self, name: &str) -> Result<crate::ddl::model::EventState, String> {
        object_read::load_event(&self.pool, DATABASE, name).await
    }

    async fn routine_state(&self, kind: RoutineKind, name: &str) -> Result<crate::ddl::model::RoutineState, String> {
        object_read::load_routine(&self.pool, DATABASE, kind, name).await
    }

    async fn trigger_state(&self, name: &str) -> Result<crate::ddl::model::TriggerState, String> {
        object_read::load_trigger(&self.pool, DATABASE, name).await
    }

    async fn scheduler(&self) -> Option<bool> {
        object_read::event_scheduler_running(&mut self.pool.get_conn().await.expect("连接")).await
    }

    /// 一次 DML：用来证明触发器绑定到预期的表与事件。
    async fn sql(&self, statement: &str) {
        let mut conn = self.pool.get_conn().await.expect("连接");
        conn.query_drop(statement).await.unwrap_or_else(|err| panic!("{statement}: {err}"));
    }

    async fn count(&self, statement: &str) -> i64 {
        let mut conn = self.pool.get_conn().await.expect("连接");
        conn.query_first::<i64, _>(statement).await.expect("查询").unwrap_or(0)
    }
}

/// 整轮验收：四类对象的建/改/删/读回、失败路径与调度器语义都在真实服务器上走一遍。
#[test]
#[ignore = "需要 scripts/verify-mysql-objects.py 启动的 MySQL 与 MariaDB 夹具"]
fn object_lifecycle_round_trip_on_real_servers() {
    let mysql_port = std::env::var("RILLSQL_OBJECT_MYSQL_PORT").unwrap_or_else(|_| "3436".to_owned());
    let mariadb_port = std::env::var("RILLSQL_OBJECT_MARIADB_PORT").unwrap_or_else(|_| "3437".to_owned());
    crate::app::shell::jobs::runtime().block_on(async move {
        for (engine, port) in [(Engine::Mysql, mysql_port), (Engine::Mariadb, mariadb_port)] {
            run_engine(engine, port).await;
        }
    });
}

async fn run_engine(engine: Engine, port: String) {
    let live = Live::connect(engine, port).await;
    let suffix = format!("{}_{}", engine.as_str(), std::process::id());
    event_round_trip(&live, &suffix).await;
    event_failure_paths(&live, &suffix).await;
    routine_round_trip(&live, &suffix).await;
    same_name_routine_read_back(&live).await;
    routine_failure_paths(&live, &suffix).await;
    trigger_round_trip(&live, &suffix).await;
    trigger_failure_paths(&live, &suffix).await;
    println!("object-live: {} 全部用例完成", live.engine_name());
}

/// R5：单次与周期事件的建/改/启停/删，每一步都读回服务器的实际定义与状态。
async fn event_round_trip(live: &Live, suffix: &str) {
    let name = format!("t3_event_{suffix}");
    let scheduler = live.scheduler().await;
    println!("object-live: {} event_scheduler 当前状态 {:?}", live.engine_name(), scheduler);

    // 周期事件：EVERY … STARTS … ENDS、主体、ON COMPLETION、停用。
    let recurring = EventDraft {
        original: None,
        name: name.clone(),
        schedule: EventSchedule::Recurring {
            every: "1".to_owned(),
            unit: "DAY".to_owned(),
            starts: "2026-01-01 00:00:00".to_owned(),
            ends: String::new(),
        },
        body: "INSERT INTO audit_log (order_id, action) VALUES (0, 'tick')".to_owned(),
        enabled: false,
        on_completion: EventCompletion::NotPreserve,
        comment: "t3 周期事件".to_owned(),
    };
    let (plan, receipt) = live.run(Action::CreateEvent { event: recurring.clone() }).await;
    println!("object-live: {} 建周期事件 -> {}", live.engine_name(), plan.steps[0].sql.replace('\n', " "));
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);

    let state = live.event_state(&name).await.expect("读回周期事件");
    assert_eq!(state.schedule, recurring.schedule, "调度要原样读回");
    assert!(!state.enabled, "DISABLE 的事件读回必须是停用");
    assert_eq!(state.on_completion, EventCompletion::NotPreserve);
    assert!(state.body.contains("'tick'"), "{}", state.body);
    println!("object-live: {} 周期事件读回 body={:?} enabled={}", live.engine_name(), state.body, state.enabled);

    // 修改调度与主体：ALTER EVENT 只改这个事件，不改服务器调度器。
    let altered = EventDraft {
        schedule: EventSchedule::Recurring {
            every: "2".to_owned(),
            unit: "HOUR".to_owned(),
            starts: String::new(),
            ends: String::new(),
        },
        body: "INSERT INTO audit_log (order_id, action) VALUES (0, 'tick2')".to_owned(),
        on_completion: EventCompletion::Preserve,
        ..recurring.clone()
    };
    let (plan, receipt) = live.run(Action::AlterEvent { event: altered.clone() }).await;
    println!("object-live: {} 改周期事件 -> {}", live.engine_name(), plan.steps[0].sql.replace('\n', " "));
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let state = live.event_state(&name).await.expect("改后读回");
    // STARTS 留空时服务器会把 ALTER 的起始时间补成当前时间：这是服务器的实际行为，不是读写不一致。
    // 其余调度字段必须逐字一致；补出来的 STARTS 照实记录。
    match (&state.schedule, &altered.schedule) {
        (
            EventSchedule::Recurring { every, unit, starts, ends },
            EventSchedule::Recurring { every: want_every, unit: want_unit, starts: want_starts, ends: want_ends },
        ) => {
            assert_eq!((every, unit, ends), (want_every, want_unit, want_ends));
            if want_starts.is_empty() {
                assert!(!starts.is_empty(), "服务器把没有 STARTS 的 ALTER 补成了现在的时间");
                println!("object-live: {} 周期事件未写 STARTS，服务器补成 {starts}", live.engine_name());
            } else {
                assert_eq!(starts, want_starts);
            }
        }
        (actual, want) => panic!("调度形态不一致：{actual:?} != {want:?}"),
    }
    assert!(state.body.contains("'tick2'"), "{}", state.body);
    assert_eq!(state.on_completion, EventCompletion::Preserve);

    // 启用：调度器关闭时提示必须说清"仍然不会运行"，并且不改服务器级状态。
    let enable_plan = live.preview(Action::SetEventEnabled { event: name.clone(), enabled: true }).await;
    assert!(enable_plan.blocked.is_empty(), "{:?}", enable_plan.blocked);
    let scheduler_risk = enable_plan.risks.iter().find(|risk| risk.text.contains("event_scheduler")).expect("启用事件要有调度器提示");
    match scheduler {
        Some(false) => assert!(scheduler_risk.text.contains("仍然不会运行"), "{}", scheduler_risk.text),
        Some(true) => assert!(scheduler_risk.text.contains("已打开"), "{}", scheduler_risk.text),
        None => assert!(scheduler_risk.text.contains("没有读到"), "{}", scheduler_risk.text),
    }
    println!("object-live: {} 启用事件提示：{}", live.engine_name(), scheduler_risk.text);
    let (_, receipt) = live.run(Action::SetEventEnabled { event: name.clone(), enabled: true }).await;
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    assert!(live.event_state(&name).await.expect("启用后读回").enabled, "启用后读回必须是启用");
    assert_eq!(live.scheduler().await, scheduler, "启用事件不能改变服务器级调度器状态");

    // 停用后读回。
    let (_, receipt) = live.run(Action::SetEventEnabled { event: name.clone(), enabled: false }).await;
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    assert!(!live.event_state(&name).await.expect("停用后读回").enabled);

    // 单次事件：AT 写服务器给的时间值（和读回、表单里看到的一样，不带引号；字面量引号由计划层加）。
    let once = format!("t3_event_once_{suffix}");
    let one_time = EventDraft {
        original: None,
        name: once.clone(),
        schedule: EventSchedule::OneTime { at: "2027-01-01 00:00:00".to_owned() },
        body: "INSERT INTO audit_log (order_id, action) VALUES (0, 'once')".to_owned(),
        enabled: false,
        on_completion: EventCompletion::Preserve,
        comment: String::new(),
    };
    let (plan, receipt) = live.run(Action::CreateEvent { event: one_time.clone() }).await;
    println!("object-live: {} 建单次事件 -> {}", live.engine_name(), plan.steps[0].sql.replace('\n', " "));
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    assert_eq!(live.event_state(&once).await.expect("读回单次事件").schedule, one_time.schedule);

    // 删除：读回必须找不到。
    let (_, receipt) = live.run(Action::DropEvent { event: name.clone() }).await;
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let gone = live.event_state(&name).await;
    assert!(gone.is_err(), "删除后不能再读到 {name}");
    println!("object-live: {} 删除事件后读回：{}", live.engine_name(), gone.err().unwrap_or_default());
    let _ = live.run(Action::DropEvent { event: once }).await;
}

/// R8：事件定义无效、重名时给出原始错误，输入保留、不伪报成功。
async fn event_failure_paths(live: &Live, suffix: &str) {
    let name = format!("t3_event_bad_{suffix}");
    // 主体不是合法 SQL：服务器拒绝，事件不会被创建。
    let invalid = EventDraft {
        original: None,
        name: name.clone(),
        schedule: EventSchedule::OneTime { at: "'2027-01-01 00:00:00'".to_owned() },
        body: "THIS IS NOT SQL".to_owned(),
        enabled: false,
        on_completion: EventCompletion::Preserve,
        comment: String::new(),
    };
    let receipt = live.run_allowing_failure(Action::CreateEvent { event: invalid }).await;
    let failed = receipt.steps.iter().find(|step| step.status == StepStatus::Failed).expect("服务器应该拒绝无效主体");
    assert!(!failed.message.trim().is_empty(), "失败必须保留服务器的原始错误");
    println!("object-live: {} 无效事件主体被服务器拒绝：{}", live.engine_name(), failed.message);
    assert!(live.event_state(&name).await.is_err(), "被拒绝的事件不能被当成已创建");

    // 重名：先建一个，再建同名。
    let event = EventDraft {
        original: None,
        name: name.clone(),
        schedule: EventSchedule::OneTime { at: "'2027-01-01 00:00:00'".to_owned() },
        body: "INSERT INTO audit_log (order_id, action) VALUES (0, 'dup')".to_owned(),
        enabled: false,
        on_completion: EventCompletion::Preserve,
        comment: String::new(),
    };
    live.run(Action::CreateEvent { event: event.clone() }).await;
    let receipt = live.run_allowing_failure(Action::CreateEvent { event }).await;
    let failed = receipt.steps.iter().find(|step| step.status == StepStatus::Failed).expect("重名必须被服务器拒绝");
    println!("object-live: {} 重名事件被服务器拒绝：{}", live.engine_name(), failed.message);
    let _ = live.run(Action::DropEvent { event: name }).await;
}

/// R6：函数与存储过程的建/改/删，参数、返回类型与多语句主体原样读回。
async fn routine_round_trip(live: &Live, suffix: &str) {
    let function = format!("t3_function_{suffix}");
    let draft = RoutineDraft {
        kind: RoutineKind::Function,
        original: None,
        name: function.clone(),
        params: vec![
            RoutineParamDraft { mode: ParamMode::In, name: "seed".to_owned(), type_name: "int".to_owned() },
            RoutineParamDraft { mode: ParamMode::In, name: "step".to_owned(), type_name: "int".to_owned() },
        ],
        returns: "int".to_owned(),
        body: "BEGIN\n  DECLARE total INT;\n  SET total = seed * step;\n  RETURN total;\nEND".to_owned(),
        comment: "t3 函数".to_owned(),
    };
    let (plan, receipt) = live.run(Action::CreateRoutine { routine: draft.clone() }).await;
    println!("object-live: {} 建函数 -> {}", live.engine_name(), plan.steps[0].sql.replace('\n', " "));
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);

    let state = live.routine_state(RoutineKind::Function, &function).await.expect("读回函数");
    assert_eq!(state.kind, RoutineKind::Function);
    assert_eq!(state.params.len(), 2, "{:?}", state.params);
    assert_eq!(state.params[0].name, "seed");
    assert_eq!(state.params[1].name, "step");
    // MariaDB 把返回类型读回成 int(11)（显示宽度），MySQL 是 int：只断言类型本身，不锁显示宽度。
    assert!(state.returns.to_ascii_lowercase().starts_with("int"), "返回类型读回：{}", state.returns);
    assert!(state.body.contains("RETURN total"), "多语句主体要完整读回：{}", state.body);
    assert!(!state.definer.is_empty(), "替换要用的 DEFINER 必须读到");
    println!(
        "object-live: {} 函数读回 params={:?} returns={:?} body={:?}",
        live.engine_name(),
        state.params.iter().map(|param| param.name.clone()).collect::<Vec<_>>(),
        state.returns,
        state.body
    );

    // 替换：DROP + CREATE，改参数与主体，DEFINER 与注释保留。
    let replaced = RoutineDraft {
        original: Some(function.clone()),
        params: vec![
            RoutineParamDraft { mode: ParamMode::In, name: "seed".to_owned(), type_name: "int".to_owned() },
            RoutineParamDraft { mode: ParamMode::In, name: "step".to_owned(), type_name: "int".to_owned() },
            RoutineParamDraft { mode: ParamMode::In, name: "bonus".to_owned(), type_name: "int".to_owned() },
        ],
        body: "BEGIN\n  DECLARE total INT;\n  SET total = (seed + bonus) * step;\n  RETURN total;\nEND".to_owned(),
        ..draft.clone()
    };
    let (plan, receipt) = live.run(Action::ReplaceRoutine { routine: replaced.clone() }).await;
    assert_eq!(plan.steps.len(), 2, "替换是 DROP + CREATE 两步");
    assert!(plan.steps[0].sql.starts_with("DROP FUNCTION"), "{}", plan.steps[0].sql);
    assert!(plan.steps[1].sql.contains("DEFINER="), "替换要保留读到的 DEFINER：{}", plan.steps[1].sql);
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    println!("object-live: {} 替换函数 -> {} | {}", live.engine_name(), plan.steps[0].sql, plan.steps[1].sql.replace('\n', " "));

    let state = live.routine_state(RoutineKind::Function, &function).await.expect("替换后读回");
    assert_eq!(state.params.len(), 3, "{:?}", state.params);
    assert!(state.body.contains("bonus"), "{}", state.body);

    // 存储过程：IN / OUT / INOUT 与多语句主体。
    let procedure = format!("t3_procedure_{suffix}");
    let proc_draft = RoutineDraft {
        kind: RoutineKind::Procedure,
        original: None,
        name: procedure.clone(),
        params: vec![
            RoutineParamDraft { mode: ParamMode::In, name: "seed".to_owned(), type_name: "int".to_owned() },
            RoutineParamDraft { mode: ParamMode::Out, name: "total".to_owned(), type_name: "int".to_owned() },
            RoutineParamDraft { mode: ParamMode::InOut, name: "count".to_owned(), type_name: "int".to_owned() },
        ],
        returns: String::new(),
        body: "BEGIN\n  SET total = seed * 2;\n  SET count = count + 1;\nEND".to_owned(),
        comment: String::new(),
    };
    let (plan, receipt) = live.run(Action::CreateRoutine { routine: proc_draft.clone() }).await;
    println!("object-live: {} 建存储过程 -> {}", live.engine_name(), plan.steps[0].sql.replace('\n', " "));
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    let state = live.routine_state(RoutineKind::Procedure, &procedure).await.expect("读回存储过程");
    assert_eq!(state.params.len(), 3, "{:?}", state.params);
    assert_eq!(state.params[0].mode, ParamMode::In);
    assert_eq!(state.params[1].mode, ParamMode::Out);
    assert_eq!(state.params[2].mode, ParamMode::InOut);
    assert!(state.returns.is_empty(), "存储过程不应有返回类型：{:?}", state.returns);
    println!("object-live: {} 存储过程读回参数模式 {:?}", live.engine_name(), state.params.iter().map(|param| param.mode).collect::<Vec<_>>());

    // 删除：两个对象都不能再读到。
    let (_, receipt) = live.run(Action::DropRoutine { routine_kind: RoutineKind::Function, name: function.clone() }).await;
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    assert!(live.routine_state(RoutineKind::Function, &function).await.is_err(), "删除后不能再读到函数");
    let (_, receipt) = live.run(Action::DropRoutine { routine_kind: RoutineKind::Procedure, name: procedure.clone() }).await;
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    assert!(live.routine_state(RoutineKind::Procedure, &procedure).await.is_err(), "删除后不能再读到存储过程");
    println!("object-live: {} 函数与存储过程删除后都读回不到", live.engine_name());
}

/// T1 交接修正的真机证据：同库同名函数与存储过程并存时，参数不会互相串。
async fn same_name_routine_read_back(live: &Live) {
    let function = live.routine_state(RoutineKind::Function, "same_name").await.expect("读回同名函数");
    let procedure = live.routine_state(RoutineKind::Procedure, "same_name").await.expect("读回同名存储过程");
    assert!(function.returns.to_ascii_lowercase().starts_with("int"), "函数的返回类型：{:?}", function.returns);
    assert_eq!(function.params.len(), 1, "函数只有一个参数：{:?}", function.params);
    assert_eq!(procedure.params.len(), 2, "存储过程有 IN/OUT 两个参数：{:?}", procedure.params);
    assert_eq!(procedure.returns, "", "存储过程没有返回类型");
    assert!(function.body.contains("* 2"), "{}", function.body);
    assert!(procedure.body.contains("+ 100"), "{}", procedure.body);
    println!(
        "object-live: {} 同名函数/存储过程读回分开：函数 {:?} / 过程 {:?}",
        live.engine_name(),
        function.params.len(),
        procedure.params.len()
    );
}

/// R8：无效参数、函数与存储过程互相替换、替换中途失败时对象可能已经不存在。
async fn routine_failure_paths(live: &Live, suffix: &str) {
    // 无效参数类型：服务器拒绝，函数不会被创建。
    let invalid = format!("t3_function_bad_{suffix}");
    let draft = RoutineDraft {
        kind: RoutineKind::Function,
        original: None,
        name: invalid.clone(),
        params: vec![RoutineParamDraft { mode: ParamMode::In, name: "seed".to_owned(), type_name: "int not a type".to_owned() }],
        returns: "int".to_owned(),
        body: "RETURN seed".to_owned(),
        comment: String::new(),
    };
    let receipt = live.run_allowing_failure(Action::CreateRoutine { routine: draft }).await;
    let failed = receipt.steps.iter().find(|step| step.status == StepStatus::Failed).expect("服务器应该拒绝无效参数类型");
    println!("object-live: {} 无效参数类型被服务器拒绝：{}", live.engine_name(), failed.message);
    assert!(live.routine_state(RoutineKind::Function, &invalid).await.is_err());

    // 函数与存储过程不能互相替换：existing_function 在服务器上是函数，按存储过程替换时连读回都取不到它，
    // 预览直接给出原因、不生成任何语句（读回类型与草稿不一致的第二条阻断见 object_ddl 的单元测试）。
    // 夹具里的 same_name 函数与存储过程各有一个，两边都能合法替换，证明不了这条规则。
    let swap = RoutineDraft {
        kind: RoutineKind::Procedure,
        original: Some("existing_function".to_owned()),
        name: "existing_function".to_owned(),
        params: Vec::new(),
        returns: String::new(),
        body: "SELECT 1".to_owned(),
        comment: String::new(),
    };
    let refused = live.preview_result(Action::ReplaceRoutine { routine: swap }).await;
    let message = refused.expect_err("按存储过程替换函数必须被拒绝");
    assert!(message.contains("existing_function"), "{message}");
    assert!(
        live.routine_state(RoutineKind::Function, "existing_function").await.is_ok(),
        "预览被拒绝不能动服务器上的原对象"
    );
    println!("object-live: {} 函数/存储过程互换被拒绝：{message}", live.engine_name());

    // 替换中途失败：DROP 成功、CREATE 失败，原对象已经不存在，不能声称整体回滚。
    let name = format!("t3_function_gap_{suffix}");
    let draft = RoutineDraft {
        kind: RoutineKind::Function,
        original: None,
        name: name.clone(),
        params: Vec::new(),
        returns: "int".to_owned(),
        body: "RETURN 7".to_owned(),
        comment: String::new(),
    };
    live.run(Action::CreateRoutine { routine: draft.clone() }).await;
    let broken = RoutineDraft {
        original: Some(name.clone()),
        body: "BEGIN RETURN ; END".to_owned(),
        ..draft
    };
    let receipt = live.run_allowing_failure(Action::ReplaceRoutine { routine: broken }).await;
    assert_eq!(receipt.steps.len(), 2, "替换是两步");
    assert_eq!(receipt.steps[0].status, StepStatus::Ok, "第一步 DROP 已经执行");
    assert_eq!(receipt.steps[1].status, StepStatus::Failed, "第二步 CREATE 被服务器拒绝");
    let gone = live.routine_state(RoutineKind::Function, &name).await;
    assert!(gone.is_err(), "两步之间失败时原对象可能已经不存在");
    println!(
        "object-live: {} 替换中途失败：DROP={:?} CREATE={:?}，读回 {:?}",
        live.engine_name(),
        receipt.steps[0].status,
        receipt.steps[1].status,
        gone.err().unwrap_or_default()
    );
}

/// R7：触发器的建/改/删，并用一次真实 DML 证明绑定到目标表与事件。
async fn trigger_round_trip(live: &Live, suffix: &str) {
    let name = format!("t3_trigger_{suffix}");
    let draft = TriggerDraft {
        original: None,
        name: name.clone(),
        table: "orders".to_owned(),
        timing: TriggerTiming::Before,
        event: TriggerEvent::Insert,
        body: "BEGIN\n  INSERT INTO audit_log (order_id, action) VALUES (NEW.id, 'insert');\n  SET NEW.note = 'before-insert';\nEND".to_owned(),
    };
    let (plan, receipt) = live.run(Action::CreateTrigger { trigger: draft.clone() }).await;
    println!("object-live: {} 建触发器 -> {}", live.engine_name(), plan.steps[0].sql.replace('\n', " "));
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);

    let state = live.trigger_state(&name).await.expect("读回触发器");
    assert_eq!(state.table, "orders");
    assert_eq!(state.timing, TriggerTiming::Before);
    assert_eq!(state.event, TriggerEvent::Insert);
    assert!(state.body.contains("before-insert"), "多语句主体要完整读回：{}", state.body);
    println!("object-live: {} 触发器读回 {:?} {:?} on {} body={:?}", live.engine_name(), state.timing, state.event, state.table, state.body);

    // 绑定证明：INSERT 之后 audit_log 多一行，note 被触发器改写。
    let before = live.count("SELECT COUNT(*) FROM audit_log WHERE action = 'insert'").await;
    live.sql("INSERT INTO orders (amount, note) VALUES (10, NULL)").await;
    let after = live.count("SELECT COUNT(*) FROM audit_log WHERE action = 'insert'").await;
    assert_eq!(after, before + 1, "触发器没有绑到 orders 的 INSERT");
    let rewritten = live.count("SELECT COUNT(*) FROM orders WHERE note = 'before-insert'").await;
    assert!(rewritten >= 1, "触发器主体没有改写 note");
    println!("object-live: {} 触发器在 INSERT 上执行：audit_log +1，note 被改写", live.engine_name());

    // 修改时机与主体：换成 AFTER UPDATE。AFTER 触发器不能改 NEW 行（服务器报 ERROR 1362），
    // 所以主体只写审计，改写行值只在 BEFORE 里做。
    let altered = TriggerDraft {
        original: Some(name.clone()),
        timing: TriggerTiming::After,
        event: TriggerEvent::Update,
        body: "BEGIN\n  INSERT INTO audit_log (order_id, action) VALUES (NEW.id, 'update');\nEND".to_owned(),
        ..draft.clone()
    };
    let (plan, receipt) = live.run(Action::ReplaceTrigger { trigger: altered.clone() }).await;
    assert_eq!(plan.steps.len(), 2, "替换触发器是 DROP + CREATE 两步");
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    println!("object-live: {} 替换触发器 -> {} | {}", live.engine_name(), plan.steps[0].sql, plan.steps[1].sql.replace('\n', " "));
    let state = live.trigger_state(&name).await.expect("替换后读回");
    assert_eq!(state.timing, TriggerTiming::After);
    assert_eq!(state.event, TriggerEvent::Update);

    // 绑定证明：UPDATE 触发，INSERT 不再触发本触发器。
    let updates_before = live.count("SELECT COUNT(*) FROM audit_log WHERE action = 'update'").await;
    let inserts_before = live.count("SELECT COUNT(*) FROM audit_log WHERE action = 'insert'").await;
    live.sql("UPDATE orders SET amount = amount + 1 ORDER BY id DESC LIMIT 1").await;
    assert_eq!(live.count("SELECT COUNT(*) FROM audit_log WHERE action = 'update'").await, updates_before + 1, "UPDATE 没有触发");
    live.sql("INSERT INTO orders (amount, note) VALUES (20, NULL)").await;
    assert_eq!(
        live.count("SELECT COUNT(*) FROM audit_log WHERE action = 'insert'").await,
        inserts_before,
        "替换成 UPDATE 触发器后，INSERT 不该再触发它"
    );
    println!("object-live: {} 替换后 UPDATE 触发、INSERT 不再触发", live.engine_name());

    // 删除：对象消失，并且 DML 不再触发原逻辑。
    let (_, receipt) = live.run(Action::DropTrigger { trigger: name.clone() }).await;
    assert!(receipt.steps.iter().all(|step| step.status == StepStatus::Ok), "{:?}", receipt.steps);
    assert!(live.trigger_state(&name).await.is_err(), "删除后不能再读到触发器");
    let updates_after = live.count("SELECT COUNT(*) FROM audit_log WHERE action = 'update'").await;
    live.sql("UPDATE orders SET amount = amount + 1").await;
    assert_eq!(live.count("SELECT COUNT(*) FROM audit_log WHERE action = 'update'").await, updates_after, "删除后 UPDATE 不该再触发");
    println!("object-live: {} 删除触发器后 DML 不再触发", live.engine_name());
}

/// R8：目标表不存在、主体无效时显示原始错误，草稿保留、不伪报成功。
async fn trigger_failure_paths(live: &Live, suffix: &str) {
    let missing = format!("t3_trigger_missing_{suffix}");
    let draft = TriggerDraft {
        original: None,
        name: missing.clone(),
        table: "no_such_table".to_owned(),
        timing: TriggerTiming::Before,
        event: TriggerEvent::Insert,
        body: "SET @x = 1".to_owned(),
    };
    let receipt = live.run_allowing_failure(Action::CreateTrigger { trigger: draft }).await;
    let failed = receipt.steps.iter().find(|step| step.status == StepStatus::Failed).expect("目标表不存在必须被服务器拒绝");
    println!("object-live: {} 目标表不存在被拒绝：{}", live.engine_name(), failed.message);
    assert!(live.trigger_state(&missing).await.is_err(), "被拒绝的触发器不能被当成已创建");

    let broken = format!("t3_trigger_broken_{suffix}");
    let draft = TriggerDraft {
        name: broken.clone(),
        table: "orders".to_owned(),
        body: "THIS IS NOT SQL".to_owned(),
        ..TriggerDraft::default()
    };
    let receipt = live.run_allowing_failure(Action::CreateTrigger { trigger: draft }).await;
    let failed = receipt.steps.iter().find(|step| step.status == StepStatus::Failed).expect("无效主体必须被服务器拒绝");
    println!("object-live: {} 无效触发器主体被拒绝：{}", live.engine_name(), failed.message);
    assert!(live.trigger_state(&broken).await.is_err());
}
/// R14：CALL 走查询台真实链路（风险确认 → 执行），返回两个结果集的过程产生两个结果页，
/// 消息页带 SHOW WARNINGS 的警告；不返回结果集的过程提示“未返回结果集”。
/// 夹具：docker/compose.yaml 的 mysql 服务（docker/init/mysql-ux-round3.sql 的 shop 库）。
/// 端口来自 RILLSQL_UX_MYSQL_PORT（默认 3306）。
#[test]
#[ignore = "需要 docker compose -f docker/compose.yaml up -d 启动的 MySQL 夹具（shop 库）"]
fn call_returns_every_result_set_and_warnings_on_the_compose_fixture() {
    use crate::application::tasks::TaskBoard;
    use crate::query::assist::RunScope;
    use crate::query::commands::QueryState;
    use crate::query::desk::{self, BeginRun, Desk};

    let port = std::env::var("RILLSQL_UX_MYSQL_PORT").unwrap_or_else(|_| "3306".to_owned());
    crate::app::shell::jobs::runtime().block_on(async move {
        let dir = std::env::temp_dir().join(format!("rillsql-call-live-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(dir.join("connections.sqlite")))));
        let id = "call-live-mysql".to_owned();
        let mut draft = ConnectionDraft::blank(Engine::Mysql);
        draft.id = Some(id.clone());
        draft.name = id.clone();
        draft.host = "127.0.0.1".into();
        draft.port = port;
        draft.username = "root".into();
        draft.database = "shop".into();
        draft.tls_mode = TlsMode::Off;
        draft.save_password = false;
        services.connections.save(draft, SecretInput::default()).await.expect("保存连接");
        let opened = services
            .connections
            .open(&id, SecretInput { password: Some(Secret::new("root")), ..Default::default() })
            .await
            .expect("打开连接");
        assert!(opened.connected, "{:?}", opened.report);
        let run_limited = |sql: &'static str, limit: u64| {
            let services = services.clone();
            let id = id.clone();
            let dir = dir.clone();
            async move {
                let tasks = TaskBoard::new();
                let mut desk = Desk::new();
                desk.open_database(&id, &id, "mysql", "shop");
                desk.set_draft(sql);
                // CALL 可能修改数据：仍然先确认，确认后按同一份文本发送。
                let BeginRun::Risks(prompt) = desk.begin_run_limited(&tasks, RunScope::Script, None, limit) else {
                    panic!("CALL 需要确认");
                };
                assert_eq!(prompt.statements.len(), 1);
                let confirmed = desk.confirm_risks().expect("确认后的文本");
                let BeginRun::Ready(prepared) = desk.begin_run_limited(&tasks, RunScope::Script, Some(confirmed), limit) else {
                    panic!("确认后应能执行");
                };
                let query = QueryState::new(dir.join("query-history.sqlite"));
                desk::execute_prepared(&services.connections, &query, &prepared).await.expect("执行")
            }
        };

        let run = |sql: &'static str| run_limited(sql, 1000);
        let two = run("CALL report_two_sets()").await;
        let pages: Vec<_> = two.statements.iter().filter(|statement| !statement.columns.is_empty()).collect();
        assert_eq!(pages.len(), 2, "两个结果集各一页：{:#?}", two.statements);
        assert_eq!(pages[0].result_set, Some((0, 2)));
        assert_eq!(pages[1].result_set, Some((1, 2)));
        assert_eq!(pages[0].columns, vec!["order_id".to_owned(), "line_count".to_owned()]);
        assert_eq!(pages[1].columns[0], "id");
        assert!(two.statements.iter().all(|statement| statement.status == "ok"), "{:#?}", two.statements);
        let last = two.statements.last().expect("最后一页");
        assert!(last.message.contains("警告") && last.message.contains("report_two_sets: demo warning"), "消息页含警告：{}", last.message);

        let none = run("CALL touch_nothing()").await;
        assert_eq!(none.statements.len(), 1, "{:#?}", none.statements);
        assert!(none.statements[0].columns.is_empty());
        assert!(none.statements[0].message.contains("未返回结果集"), "{}", none.statements[0].message);
        println!("call-live: report_two_sets {} 页，touch_nothing 提示：{}", pages.len(), none.statements[0].message);

        // 这两个过程只给本用例用（夹具 SQL 不在本任务范围）：直接在 shop 库里建，名称通用。
        let session = services.connections.catalog_session(&id).expect("已打开的会话");
        let crate::connections::CatalogSession::Mysql(pool) = session else {
            panic!("夹具不是 MySQL 会话");
        };
        let mut conn = pool.get_conn().await.expect("夹具连接");
        for statement in [
            "DROP PROCEDURE IF EXISTS shop.demo_many_rows",
            "CREATE PROCEDURE shop.demo_many_rows() BEGIN \
               SELECT n FROM (WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 120) SELECT n FROM seq) s; \
               SELECT 'tail' AS label; \
               CREATE TEMPORARY TABLE IF NOT EXISTS demo_scratch (n INT); \
               INSERT INTO demo_scratch VALUES (1), (2), (3); \
             END",
            "DROP PROCEDURE IF EXISTS shop.demo_fail_midway",
            "CREATE PROCEDURE shop.demo_fail_midway() BEGIN \
               SELECT 1 AS opening_value; \
               SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'demo failure after first set'; \
             END",
        ] {
            conn.query_drop(statement).await.expect(statement);
        }
        drop(conn);

        // 第一个结果集超过读取上限（CALL 不加 LIMIT）：第 1 页标截断，后面的结果集与影响行数照样读到。
        // 上限不超过 8 时不分块暂停（chunk_rows），本用例的轮询不需要模拟界面“继续读取”。
        let many = run_limited("CALL demo_many_rows()", 8).await;
        let pages: Vec<_> = many.statements.iter().filter(|statement| !statement.columns.is_empty()).collect();
        assert_eq!(pages.len(), 2, "截断之后仍读到第二个结果集：{:#?}", many.statements);
        assert_eq!(pages[0].rows.len(), 8, "第 1 页按上限截断");
        assert!(pages[0].truncated, "第 1 页带截断标记");
        assert_eq!(pages[1].columns, vec!["label".to_owned()]);
        assert_eq!(pages[1].rows, vec![vec![Some("tail".to_owned())]]);
        assert!(many.statements.iter().all(|statement| statement.status == "ok"), "{:#?}", many.statements);
        let last = many.statements.last().expect("最后一页");
        assert!(last.message.contains("影响 3 行"), "最后的影响行数：{}", last.message);

        // 过程中途失败：已经返回的结果集保留，语句与整次执行都按失败登记，错误原文保留。
        let failed = run("CALL demo_fail_midway()").await;
        assert_eq!(failed.status, "error", "整次执行失败：{:#?}", failed.statements);
        let first = &failed.statements[0];
        assert_eq!(first.status, "error", "{:#?}", failed.statements);
        assert!(first.message.contains("demo failure after first set"), "错误原文：{}", first.message);
        assert_eq!(first.columns, vec!["opening_value".to_owned()], "已经返回的结果集保留：{:#?}", failed.statements);
        assert_eq!(first.rows, vec![vec![Some("1".to_owned())]]);
        assert!(!first.message.contains("警告"), "错误不再作为警告重复一遍：{}", first.message);
        println!("call-live: demo_many_rows 第 1 页 {} 行（截断），demo_fail_midway：{}", pages[0].rows.len(), first.message);
    });
}
