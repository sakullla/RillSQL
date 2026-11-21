//! 真实夹具上的执行前风险验收（R8 / R9 / R10）。
//!
//! 每个引擎都走查询台真实的执行前决策与执行链：Desk::begin_run 给出「要不要确认」，
//! 需要确认时按确认后的路径发送同一份 SQL，再由 commands/exec 真正打到服务器。
//! 只读对照用来证明只读语句不被无差别阻断。
//!
//! 只在 scripts/verify-execution-risk.py 启动的夹具上运行，默认 #[ignore]：
//! 单元测试不能替代真实服务器上的后果。端口来自环境变量，默认取 docker/compose.yaml 的映射：
//! RILLSQL_RISK_PG_PORT / MYSQL_PORT / MARIADB_PORT / MSSQL_PORT / REDIS_PORT / MONGO_PORT。

use std::path::PathBuf;
use std::sync::Arc;

use crate::application::tasks::TaskBoard;
use crate::application::AppServices;
use crate::connections::model::{ConnectionDraft, Engine, Secret, SecretInput, TlsMode};
use crate::connections::Service;
use crate::query::assist::RunScope;
use crate::query::commands::{self, QueryState};
use crate::query::desk::{self, BeginRun, Desk, PreparedRun};
use crate::query::risk::RiskKind;

/// 一个真实服务器夹具：连接已经保存并打开。
struct Live {
    services: Arc<AppServices>,
    id: String,
    name: String,
    engine: Engine,
    database: String,
}

impl Live {
    fn engine_id(&self) -> &'static str {
        self.engine.as_str()
    }

    async fn remote(engine: Engine, port_env: &str, default_port: &str, user: &str, database: &str, password: &str) -> Self {
        let port = std::env::var(port_env).unwrap_or_else(|_| default_port.to_owned());
        let dir = temp_dir(engine.as_str());
        let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(dir.join("connections.sqlite")))));
        let id = format!("risk-{}", engine.as_str());
        let mut draft = ConnectionDraft::blank(engine);
        draft.id = Some(id.clone());
        draft.name = format!("{} 夹具", engine.as_str());
        draft.host = "127.0.0.1".into();
        draft.port = port;
        draft.username = user.to_owned();
        draft.database = database.to_owned();
        // SQL Server 官方镜像用自签名证书：按「加密但信任服务器证书」连接，
        // 与 adapters/sqlserver/connect.rs 的 TlsMode::Require 分支一致。
        draft.tls_mode = if engine == Engine::Sqlserver { TlsMode::Require } else { TlsMode::Off };
        draft.save_password = false;
        services.connections.save(draft, SecretInput::default()).await.expect("保存连接");
        let opened = services
            .connections
            .open(&id, SecretInput { password: Some(Secret::new(password)), ..Default::default() })
            .await
            .expect("打开连接");
        assert!(opened.connected, "{id}: {:?}", opened.report);
        let name = format!("{} 夹具", engine.as_str());
        Self { services, id, name, engine, database: database.to_owned() }
    }

    async fn sqlite(path: PathBuf) -> Self {
        // 打开 SQLite 需要文件已经存在：先落一个空库文件，再按正常连接流程打开。
        std::fs::File::create(&path).expect("建 SQLite 夹具文件");
        let dir = temp_dir("sqlite");
        let services = Arc::new(AppServices::new(Arc::new(Service::new_for_test(dir.join("connections.sqlite")))));
        services.connections.open_sqlite_for_test("risk-sqlite", "SQLite 夹具", &path).await.expect("打开 SQLite 夹具");
        Self {
            services,
            id: "risk-sqlite".to_owned(),
            name: "SQLite 夹具".to_owned(),
            engine: Engine::Sqlite,
            database: "main".to_owned(),
        }
    }

    /// 与查询台完全相同的执行前决策：有风险先确认，再按同一条执行链发送。
    async fn decide_and_run(&self, sql: &str) -> LiveRun {
        let tasks = TaskBoard::new();
        let mut desk = Desk::new();
        desk.open_database(&self.id, &self.name, self.engine_id(), &self.database);
        desk.set_draft(sql);
        let mut prompted = None;
        let begin = match desk.begin_run(&tasks, RunScope::Script, None) {
            BeginRun::Risks(_) => {
                prompted = desk.pending_risks();
                let confirmed = desk.confirm_risks().expect("确认后要执行的文本");
                assert_eq!(desk.executions(), 0, "确认前不能发送");
                desk.begin_run(&tasks, RunScope::Script, Some(confirmed))
            }
            other => other,
        };
        let prepared = match begin {
            BeginRun::Ready(prepared) => prepared,
            BeginRun::Rejected(reason) => panic!("{}: 执行被拒绝：{reason}", self.engine_id()),
            BeginRun::Variables(_) | BeginRun::Risks(_) => panic!("{}: 意外的决策", self.engine_id()),
        };
        let run = self.execute(&prepared).await;
        LiveRun { prompt: prompted, run }
    }

    async fn execute(&self, prepared: &PreparedRun) -> commands::RunDto {
        let query = QueryState::new(temp_dir("history").join("query-history.sqlite"));
        desk::execute_prepared(&self.services.connections, &query, prepared)
            .await
            .unwrap_or_else(|err| panic!("{}: 执行失败：{err}", self.engine_id()))
    }
}

/// 一次真实执行的结果：确认面板（有风险时）与服务器实际返回的状态。
struct LiveRun {
    prompt: Option<desk::RiskPrompt>,
    run: commands::RunDto,
}

impl LiveRun {
    fn kinds(&self) -> Vec<RiskKind> {
        self.prompt
            .as_ref()
            .map(|prompt| {
                prompt
                    .statements
                    .iter()
                    .flat_map(|statement| statement.consequences.iter().map(|item| item.kind))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn statuses(&self) -> Vec<String> {
        self.run.statements.iter().map(|statement| statement.status.clone()).collect()
    }

    fn all_ok(&self) -> bool {
        self.run.statements.iter().all(|statement| statement.status == "ok")
    }

    /// 第一条语句第一行的第一个值（版本探测用）。
    fn first_value(&self) -> Option<String> {
        self.run.statements.first()?.rows.first()?.first()?.clone()
    }

    fn printed(&self) -> String {
        self.run
            .statements
            .iter()
            .map(|statement| {
                let size = if !statement.columns.is_empty() {
                    format!("{} 行", statement.rows.len())
                } else {
                    format!("{} 行受影响", statement.affected.unwrap_or(0))
                };
                format!("{} {}", statement.status, size)
            })
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rillsql-risk-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("临时目录");
    dir
}

/// 建一张用于制造后果的临时表；CREATE TABLE 本身没有后果，所以不弹确认。
async fn create_scratch(live: &Live, table: &str) -> LiveRun {
    let sql = match live.engine {
        Engine::Sqlserver => format!("CREATE TABLE {table} (id INT PRIMARY KEY, note VARCHAR(20))"),
        Engine::Sqlite => format!("CREATE TABLE IF NOT EXISTS {table} (id INTEGER PRIMARY KEY, note TEXT)"),
        _ => format!("CREATE TABLE {table} (id INT PRIMARY KEY, note VARCHAR(20))"),
    };
    let started = live.decide_and_run(&sql).await;
    assert!(started.prompt.is_none(), "{}: 建表不该弹风险确认", live.engine_id());
    assert!(started.all_ok(), "{}: 建表失败 {:?}", live.engine_id(), started.statuses());
    started
}

#[test]
#[ignore = "requires the docker/compose.yaml fixtures; run scripts/verify-execution-risk.py"]
fn execution_risk_on_real_fixtures() {
    crate::app::shell::jobs::runtime().block_on(async move {
        let postgres = Live::remote(Engine::Postgres, "RILLSQL_RISK_PG_PORT", "5432", "postgres", "rillsql", "postgres").await;
        let mysql = Live::remote(Engine::Mysql, "RILLSQL_RISK_MYSQL_PORT", "3306", "rillsql", "rillsql", "rillsql").await;
        let mariadb = Live::remote(Engine::Mariadb, "RILLSQL_RISK_MARIADB_PORT", "3307", "rillsql", "rillsql", "rillsql").await;
        let mssql = Live::remote(Engine::Sqlserver, "RILLSQL_RISK_MSSQL_PORT", "1433", "sa", "rillsql", "Rillsql-Passw0rd!").await;
        let sqlite_path = temp_dir("file").join("risk.sqlite");
        let sqlite = Live::sqlite(sqlite_path).await;

        // SQLite 用临时文件夹具：版本由驱动读回，写进报告。
        if let Some(version) = sqlite.decide_and_run("SELECT sqlite_version()").await.first_value() {
            println!("execution-risk: sqlite 版本 {version}");
        }

        for live in [&postgres, &mysql, &mariadb, &mssql, &sqlite] {
            run_sql_engine(live).await;
            println!("execution-risk: {} 全部用例完成", live.engine_id());
        }
        run_redis().await;
        println!("execution-risk: redis 全部用例完成");
        run_mongo().await;
        println!("execution-risk: mongo 全部用例完成");
    });
}

/// 每个 SQL 引擎：只读对照直通，风险代表操作先确认再执行，并核对真实后果。
async fn run_sql_engine(live: &Live) {
    let engine = live.engine_id();
    let suffix = std::process::id();

    // 只读对照：不弹确认，真的执行。
    let control = live.decide_and_run("SELECT 1").await;
    assert!(control.prompt.is_none(), "{engine}: 只读语句被要求确认");
    assert!(control.all_ok(), "{engine}: 只读语句失败 {:?}", control.statuses());
    println!("execution-risk: {engine} 只读对照 SELECT 1 -> {}", control.printed());

    let table = format!("risk_drop_{suffix}");
    create_scratch(live, &table).await;

    // 风险代表操作：删表（数据丢失；MySQL/MariaDB 还会隐式提交）。
    let dropped = live.decide_and_run(&format!("DROP TABLE {table}")).await;
    let kinds = dropped.kinds();
    assert!(kinds.contains(&RiskKind::DataLoss), "{engine}: 删表没被判成数据丢失 {kinds:?}");
    if matches!(live.engine, Engine::Mysql | Engine::Mariadb) {
        assert!(kinds.contains(&RiskKind::ImplicitCommit), "{engine}: DDL 没有隐式提交说明 {kinds:?}");
    }
    assert!(dropped.all_ok(), "{engine}: 删表失败 {:?}", dropped.statuses());
    println!("execution-risk: {engine} 风险操作 DROP TABLE -> {} 后果 {:?}", dropped.printed(), kinds);

    // 真的删掉了：再查这张表必须失败。
    let probe = live.decide_and_run(&format!("SELECT * FROM {table}")).await;
    assert!(!probe.all_ok(), "{engine}: 删表之后还能读到 {table}");
    println!("execution-risk: {engine} 删表后读回 -> {}", probe.printed());

    // 各引擎的另一类后果：整表删除 / 整表更新 / 清空表。
    let (sql, expect) = match live.engine {
        Engine::Sqlite => (format!("DELETE FROM {table}_rows"), RiskKind::DataLoss),
        Engine::Sqlserver => (format!("TRUNCATE TABLE {table}_rows"), RiskKind::DataLoss),
        _ => (format!("UPDATE {table}_rows SET note = 'x'"), RiskKind::MassRewrite),
    };
    let rows = format!("{table}_rows");
    let create = format!("CREATE TABLE {rows} (id INT PRIMARY KEY, note VARCHAR(20))");
    let started = live.decide_and_run(&create).await;
    assert!(started.all_ok(), "{engine}: 建探测表失败 {:?}", started.statuses());
    let seeded = live.decide_and_run(&format!("INSERT INTO {rows} (id, note) VALUES (1, 'a'), (2, 'b')")).await;
    assert!(seeded.all_ok(), "{engine}: 写入探测行失败 {:?}", seeded.statuses());
    assert!(seeded.prompt.is_none(), "{engine}: 普通 INSERT 不该确认");
    let flagged = live.decide_and_run(&sql).await;
    assert!(flagged.kinds().contains(&expect), "{engine}: {} 没有判成 {expect:?}：{:?}", sql, flagged.kinds());
    assert!(flagged.all_ok(), "{engine}: {sql} 执行失败 {:?}", flagged.statuses());
    println!("execution-risk: {engine} 风险操作 {sql} -> {} 后果 {:?}", flagged.printed(), flagged.kinds());

    // 多语句脚本：只读语句直通，只有风险语句被拦下。SQLite 没有 TRUNCATE，用它支持的全表删除。
    let risky_piece = if live.engine == Engine::Sqlite {
        format!("DELETE FROM {rows}")
    } else {
        format!("TRUNCATE TABLE {rows}")
    };
    let mixed = format!("SELECT 1;\n{risky_piece};\nSELECT 2");
    let run = live.decide_and_run(&mixed).await;
    let prompt = run.prompt.as_ref().expect("{engine}: 多语句脚本没有确认面板");
    assert_eq!(prompt.statements.len(), 1, "{engine}: 只该拦风险语句 {:?}", prompt.statements);
    assert_eq!(prompt.passthrough, 2, "{engine}: 只读语句没有被算作直通");
    assert!(run.all_ok(), "{engine}: 多语句脚本执行失败 {:?}", run.statuses());
    println!(
        "execution-risk: {engine} 多语句混合 -> 确认 {} 条 / 直通 {} 条 -> {}",
        prompt.statements.len(),
        prompt.passthrough,
        run.printed()
    );
}

/// Redis：Blocked/Dangerous/Readonly 三个档次不变，未确认的写命令在应用层被拒。
async fn run_redis() {
    let live = Live::remote(Engine::Redis, "RILLSQL_RISK_REDIS_PORT", "6379", "", "0", "rillsql-redis").await;
    use crate::adapters::redis::exec::CommandRisk;
    assert_eq!(crate::adapters::redis::exec::command_risk("KEYS"), CommandRisk::Blocked);
    assert_eq!(crate::adapters::redis::exec::command_risk("FLUSHDB"), CommandRisk::Dangerous);
    assert_eq!(crate::adapters::redis::exec::command_risk("GET"), CommandRisk::Readonly);
    let control = crate::application::redis::exec(&live.services.connections, &live.id, "DBSIZE".to_owned(), Vec::new(), false).await;
    assert!(control.is_ok(), "redis 只读命令失败：{control:?}");
    println!("execution-risk: redis 只读对照 DBSIZE -> 成功");
    let refused = crate::application::redis::exec(&live.services.connections, &live.id, "FLUSHDB".to_owned(), Vec::new(), false).await;
    assert!(refused.is_err(), "redis 写命令没有二次确认就被发送");
    println!(
        "execution-risk: redis 风险操作 FLUSHDB（未确认）-> 被应用层拒绝：{}",
        refused.err().unwrap_or_default()
    );
}

/// MongoDB：删除文档的确认与 application/mongo 的守卫一致，读操作不受影响。
async fn run_mongo() {
    // 认证库是 admin（root 建在 admin 上），业务库由调用参数显式给出。
    let live = Live::remote(Engine::Mongodb, "RILLSQL_RISK_MONGO_PORT", "27017", "root", "admin", "rillsql-mongo").await;
    let found = crate::application::mongo::list_databases(&live.services.connections, &live.id).await;
    assert!(found.is_ok(), "mongo 读取失败：{found:?}");
    println!("execution-risk: mongo 只读对照 listDatabases -> 成功");
    let refused = crate::application::mongo::delete_document(
        &live.services.connections,
        &live.id,
        "rillsql",
        "customers",
        "\"65f2c1e4a1b2c3d4e5f60701\"",
        false,
    )
    .await;
    assert!(refused.is_err(), "mongo 删除没有确认就被执行");
    println!(
        "execution-risk: mongo 风险操作 deleteOne（未确认）-> 被应用层拒绝：{}",
        refused.err().unwrap_or_default()
    );
}
