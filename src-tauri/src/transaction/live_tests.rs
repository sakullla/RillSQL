//! 五种关系型数据库的实库事务验收（T6、ADR-6）。
//!
//! 这些测试默认被 `#[ignore]` 跳过；需要 `docker/compose.yaml` 的 PostgreSQL、MySQL、
//! MariaDB、SQL Server 服务（SQLite 使用临时文件）。运行方式：
//!
//! ```text
//! cargo test --manifest-path src-tauri/Cargo.toml --locked transaction::live_tests -- --ignored --nocapture
//! ```
//!
//! 每条检查都以 `LIVE engine=... check=... PASS detail=...` 输出，便于把原始输出存进
//! `docs/verification/2026-11-21-transaction/live.txt`。端口和口令默认取 compose 固定值，
//! 可用 `RILLSQL_LIVE_*` 环境变量覆盖。夹具不可用时测试失败，不能记为通过（R1）。
//!
//! 控制语句 SQL 全部来自适配器的 [`TransactionDialect`]，因此这里验证的是 RillSQL 真正会
//! 下发的语句；事务效果再交给 [`TransactionMachine`] 登记，核对实库结果与应用状态机一致。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use crate::adapters::adapter;
use crate::connections::model::Engine;
use crate::transaction::{
    ControlOutcome, ControlSql, ControlStatement, SavepointName, StatementFailure, StatementOutcome, TransactionDialect, TransactionEffect, TransactionMachine,
    TransactionState, UnknownReason, READ_UNCOMMITTED, SERIALIZABLE,
};

// ---------------------------------------------------------------------------
// 共用输出与断言
// ---------------------------------------------------------------------------

fn record(engine: &str, check: &str, detail: impl std::fmt::Display) {
    println!("LIVE engine={engine} check={check} PASS detail={detail}");
}

fn skipped(engine: &str, check: &str, detail: impl std::fmt::Display) {
    println!("LIVE engine={engine} check={check} SKIP detail={detail}");
}

fn ensure(condition: bool, message: impl std::fmt::Display) {
    if !condition {
        panic!("LIVE assertion failed: {message}");
    }
}

fn env_port(name: &str, default: u16) -> u16 {
    std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

fn suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_owned()
}

#[derive(Debug)]
struct ExecError {
    code: Option<String>,
    message: String,
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}{}", self.code.as_deref().map(|code| format!("[{code}] ")).unwrap_or_default(), self.message)
    }
}

// ---------------------------------------------------------------------------
// 实库会话抽象：同一份契约分别跑在五种引擎上
// ---------------------------------------------------------------------------

#[async_trait]
trait LiveSession: Send {
    fn dialect(&self) -> &'static dyn TransactionDialect;

    async fn exec(&mut self, sql: &str) -> Result<(), ExecError>;

    async fn text(&mut self, sql: &str) -> Result<String, ExecError>;

    async fn count(&mut self, table: &str, id: i32) -> Result<i64, ExecError>;

    async fn version(&mut self) -> Result<String, ExecError>;

    /// SQL Server 的 `@@TRANCOUNT` / `XACT_STATE()` 探针；其他引擎没有通用探针。
    async fn probe(&mut self) -> Result<Option<(Vec<String>, Vec<Vec<Option<String>>>)>, ExecError> {
        Ok(None)
    }
}

fn dialect_of(engine: Engine) -> &'static dyn TransactionDialect {
    adapter(engine).transaction().unwrap_or_else(|| panic!("{engine:?} 没有事务方言"))
}

fn ack(machine: &mut TransactionMachine, statement: ControlStatement, outcome: ControlOutcome) -> TransactionEffect {
    machine
        .precheck(&statement)
        .unwrap_or_else(|error| panic!("precheck {statement:?} 拒绝: {error:?}"));
    machine
        .acknowledge(&statement, outcome)
        .unwrap_or_else(|error| panic!("acknowledge {statement:?} 失败: {error:?}"))
}

fn statement_succeeded(machine: &mut TransactionMachine) {
    let effect = machine.observe_statement(StatementOutcome::Succeeded);
    ensure(effect == TransactionEffect::StillActive, format!("普通语句成功应保持活动事务，实际 {effect:?}"));
}

async fn begin(primary: &mut dyn LiveSession, machine: &mut TransactionMachine) {
    let sql = primary.dialect().begin_sql(None);
    primary.exec(&sql).await.unwrap_or_else(|error| panic!("BEGIN 失败: {error}"));
    ack(machine, ControlStatement::Begin { mode: None }, ControlOutcome::Confirmed);
}

async fn commit(primary: &mut dyn LiveSession, machine: &mut TransactionMachine) {
    let sql = primary.dialect().commit_sql(false);
    primary.exec(&sql).await.unwrap_or_else(|error| panic!("COMMIT 失败: {error}"));
    ack(machine, ControlStatement::Commit { chain: false }, ControlOutcome::Confirmed);
}

async fn rollback(primary: &mut dyn LiveSession, machine: &mut TransactionMachine) {
    let sql = primary.dialect().rollback_sql(false);
    primary.exec(&sql).await.unwrap_or_else(|error| panic!("ROLLBACK 失败: {error}"));
    ack(machine, ControlStatement::Rollback { chain: false }, ControlOutcome::Confirmed);
}



/// 五个关系型引擎共用的行为契约：提交、回滚、保存点、隔离、错误恢复，以及 MySQL 家族的
/// 隐式提交。`isolation_*` 是该引擎真实的“读取当前隔离设置”查询。
#[allow(clippy::too_many_arguments)]
async fn run_contract(
    engine_name: &str,
    primary: &mut dyn LiveSession,
    observer: &mut dyn LiveSession,
    isolation_level: &str,
    isolation_query: &str,
    isolation_expected: &str,
    implicit_commit: bool,
) {
    let dialect = primary.dialect();
    let table = format!("rillsql_live_{}", suffix());
    let implicit_table = format!("{table}_ddl");
    let savepoint = SavepointName::parse("live_sp").expect("savepoint name");
    let release_point = SavepointName::parse("live_release").expect("savepoint name");

    let version = primary.version().await.unwrap_or_else(|error| panic!("{engine_name} 读取版本失败: {error}"));
    println!("LIVE engine={engine_name} fixture version={version}");

    primary.exec(&format!("DROP TABLE IF EXISTS {table}")).await.ok();
    primary
        .exec(&format!("CREATE TABLE {table} (id INTEGER PRIMARY KEY, label VARCHAR(32) NOT NULL)"))
        .await
        .unwrap_or_else(|error| panic!("{engine_name} 建表失败: {error}"));

    let mut machine = TransactionMachine::new(engine_name, dialect.capabilities());

    // 1. 提交持久化：同一事务内独立连接不可见，提交后可见（R2、R3、R4）。
    begin(primary, &mut machine).await;
    ensure(machine.state() == TransactionState::Active, format!("{engine_name} BEGIN 后应为 Active"));
    primary
        .exec(&format!("INSERT INTO {table} (id, label) VALUES (1, 'committed')"))
        .await
        .unwrap_or_else(|error| panic!("{engine_name} 事务内 INSERT 失败: {error}"));
    statement_succeeded(&mut machine);
    let before = observer.count(&table, 1).await.unwrap_or_else(|error| panic!("{engine_name} 独立连接读取失败: {error}"));
    ensure(before == 0, format!("{engine_name} 提交前独立连接不应看到未提交行，实际 {before}"));
    record(engine_name, "commit_uncommitted_invisible", format!("observer_rows={before}"));
    commit(primary, &mut machine).await;
    ensure(machine.state() == TransactionState::Idle, format!("{engine_name} COMMIT 后应为 Idle"));
    let after = observer.count(&table, 1).await.unwrap_or_else(|error| panic!("{engine_name} 提交后独立连接读取失败: {error}"));
    ensure(after == 1, format!("{engine_name} 提交后独立连接应看到 1 行，实际 {after}"));
    record(engine_name, "commit_persists", format!("observer_rows={after}"));

    // 2. 回滚：修改被撤销，独立连接始终看不到（R3、R4）。
    begin(primary, &mut machine).await;
    primary
        .exec(&format!("INSERT INTO {table} (id, label) VALUES (2, 'rolled back')"))
        .await
        .unwrap_or_else(|error| panic!("{engine_name} 回滚用例 INSERT 失败: {error}"));
    statement_succeeded(&mut machine);
    rollback(primary, &mut machine).await;
    ensure(machine.state() == TransactionState::Idle, format!("{engine_name} ROLLBACK 后应为 Idle"));
    let rolled = observer.count(&table, 2).await.unwrap_or_else(|error| panic!("{engine_name} 回滚后读取失败: {error}"));
    ensure(rolled == 0, format!("{engine_name} 回滚行不应保留，实际 {rolled}"));
    record(engine_name, "rollback_discards", format!("rows_after_rollback={rolled}"));

    // 3. 保存点部分回滚：A 保留、B 撤销、外层事务仍活动；最终提交只保留 A（R6）。
    begin(primary, &mut machine).await;
    primary
        .exec(&format!("INSERT INTO {table} (id, label) VALUES (3, 'before savepoint')"))
        .await
        .unwrap_or_else(|error| panic!("{engine_name} 保存点前 INSERT 失败: {error}"));
    statement_succeeded(&mut machine);
    primary.exec(&dialect.savepoint_sql(&savepoint)).await.unwrap_or_else(|error| panic!("{engine_name} SAVEPOINT 失败: {error}"));
    ack(&mut machine, ControlStatement::Savepoint(savepoint.clone()), ControlOutcome::Confirmed);
    primary
        .exec(&format!("INSERT INTO {table} (id, label) VALUES (4, 'after savepoint')"))
        .await
        .unwrap_or_else(|error| panic!("{engine_name} 保存点后 INSERT 失败: {error}"));
    statement_succeeded(&mut machine);
    primary
        .exec(&dialect.rollback_to_savepoint_sql(&savepoint))
        .await
        .unwrap_or_else(|error| panic!("{engine_name} ROLLBACK TO SAVEPOINT 失败: {error}"));
    ack(&mut machine, ControlStatement::RollbackToSavepoint(savepoint.clone()), ControlOutcome::Confirmed);
    ensure(machine.state() == TransactionState::Active, format!("{engine_name} 回滚到保存点后外层事务应仍活动"));
    let kept = primary.count(&table, 3).await.unwrap_or_else(|error| panic!("{engine_name} 保存点后读取失败: {error}"));
    let undone = primary.count(&table, 4).await.unwrap_or_else(|error| panic!("{engine_name} 保存点后读取失败: {error}"));
    ensure(kept == 1, format!("{engine_name} 保存点前的行应保留，实际 {kept}"));
    ensure(undone == 0, format!("{engine_name} 保存点后的行应撤销，实际 {undone}"));
    commit(primary, &mut machine).await;
    let kept = observer.count(&table, 3).await.unwrap();
    let undone = observer.count(&table, 4).await.unwrap();
    ensure(kept == 1 && undone == 0, format!("{engine_name} 保存点提交后独立连接应只看到 A，实际 A={kept} B={undone}"));
    record(engine_name, "savepoint_partial_rollback", format!("kept_a={kept} undone_b={undone}"));

    // 4. 释放保存点：支持则释放后外层事务仍活动；不支持则明确没有入口（R6）。
    match dialect.release_savepoint_sql(&release_point) {
        ControlSql::Statement(sql) => {
            begin(primary, &mut machine).await;
            primary
                .exec(&format!("INSERT INTO {table} (id, label) VALUES (5, 'release')"))
                .await
                .unwrap();
            statement_succeeded(&mut machine);
            primary.exec(&dialect.savepoint_sql(&release_point)).await.unwrap();
            ack(&mut machine, ControlStatement::Savepoint(release_point.clone()), ControlOutcome::Confirmed);
            primary.exec(&sql).await.unwrap_or_else(|error| panic!("{engine_name} RELEASE SAVEPOINT 失败: {error}"));
            ack(&mut machine, ControlStatement::ReleaseSavepoint(release_point.clone()), ControlOutcome::Confirmed);
            ensure(machine.state() == TransactionState::Active, format!("{engine_name} 释放保存点后事务应仍活动"));
            rollback(primary, &mut machine).await;
            let released = observer.count(&table, 5).await.unwrap();
            ensure(released == 0, format!("{engine_name} 释放保存点不等于提交，实际保留 {released}"));
            record(engine_name, "release_savepoint_keeps_transaction", "released=true");
        }
        other => {
            ensure(
                other == ControlSql::Unsupported,
                format!("{engine_name} 未支持的释放保存点必须明确拒绝，实际 {other:?}"),
            );
            ensure(
                dialect.sql_for(&ControlStatement::ReleaseSavepoint(release_point.clone())) == ControlSql::Unsupported,
                format!("{engine_name} 释放保存点入口应为 Unsupported"),
            );
            skipped(engine_name, "release_savepoint", "方言明确不支持 RELEASE SAVEPOINT（SQL Server 只有 SAVE TRANSACTION）");
        }
    }

    // 5. 隔离设置：方言 SQL 生效后，会话实际设置与所选值一致（R7）。
    let isolation_sql = match dialect.isolation_sql(Some(isolation_level)) {
        ControlSql::Statement(sql) => sql,
        other => panic!("{engine_name} 隔离 SQL 应为 Statement，实际 {other:?}"),
    };
    primary.exec(&isolation_sql).await.unwrap_or_else(|error| panic!("{engine_name} 设置隔离失败: {error}"));
    begin(primary, &mut machine).await;
    let actual = primary.text(isolation_query).await.unwrap_or_else(|error| panic!("{engine_name} 读取隔离设置失败: {error}"));
    ensure(
        actual.eq_ignore_ascii_case(isolation_expected),
        format!("{engine_name} 隔离设置应为 {isolation_expected}，实际 {actual}"),
    );
    rollback(primary, &mut machine).await;
    record(engine_name, "isolation_applied", format!("requested={isolation_level} actual={actual}"));

    // 6. 错误恢复：失败语句后的真实事务状态；PG 中止后只能恢复操作（R8）。
    let bad_sql = format!("SELECT * FROM rillsql_missing_{table}");
    begin(primary, &mut machine).await;
    primary
        .exec(&format!("INSERT INTO {table} (id, label) VALUES (6, 'before error')"))
        .await
        .unwrap();
    statement_succeeded(&mut machine);
    let error = primary.exec(&bad_sql).await.expect_err(&format!("{engine_name} 缺失表语句应失败"));
    let failure = dialect.statement_failure(error.code.as_deref(), &error.message);
    let expected_failure = if engine_name == "PostgreSQL" { StatementFailure::TransactionAborted } else { StatementFailure::TransactionUsable };
    ensure(
        failure == expected_failure,
        format!("{engine_name} 错误后分类应为 {expected_failure:?}，实际 {failure:?} ({error})"),
    );
    let effect = machine.observe_statement(StatementOutcome::Failed(failure));
    if expected_failure == StatementFailure::TransactionAborted {
        ensure(effect == TransactionEffect::Aborted, format!("{engine_name} 错误后应进入 Aborted，实际 {effect:?}"));
        let subsequent = primary.text("SELECT 1").await;
        let subsequent = subsequent.expect_err("PostgreSQL 中止事务中的普通语句必须失败");
        ensure(
            subsequent.code.as_deref() == Some("25P02"),
            format!("PostgreSQL 中止事务应返回 25P02，实际 {subsequent}"),
        );
        record(engine_name, "error_aborts_transaction", format!("code={:?} followup={subsequent}", error.code));
    } else {
        ensure(effect == TransactionEffect::StillActive, format!("{engine_name} 错误后事务应仍可用，实际 {effect:?}"));
        primary.text("SELECT 1").await.unwrap_or_else(|error| panic!("{engine_name} 错误后普通语句应可继续: {error}"));
        record(engine_name, "error_keeps_transaction_usable", format!("code={:?}", error.code));
    }
    rollback(primary, &mut machine).await;
    let error_rows = observer.count(&table, 6).await.unwrap();
    ensure(error_rows == 0, format!("{engine_name} 错误后回滚不应保留行，实际 {error_rows}"));
    record(engine_name, "error_then_rollback", format!("rows={error_rows}"));

    // 7. PostgreSQL 专属：中止后回滚到保存点恢复，以及中止事务的 COMMIT 实际回滚。
    if engine_name == "PostgreSQL" {
        let recovery = SavepointName::parse("live_recovery").expect("savepoint name");
        begin(primary, &mut machine).await;
        primary
            .exec(&format!("INSERT INTO {table} (id, label) VALUES (7, 'before abort')"))
            .await
            .unwrap();
        statement_succeeded(&mut machine);
        primary.exec(&dialect.savepoint_sql(&recovery)).await.unwrap();
        ack(&mut machine, ControlStatement::Savepoint(recovery.clone()), ControlOutcome::Confirmed);
        let error = primary.exec(&bad_sql).await.expect_err("中止事务语句应失败");
        let effect = machine.observe_statement(StatementOutcome::Failed(dialect.statement_failure(error.code.as_deref(), &error.message)));
        ensure(effect == TransactionEffect::Aborted && machine.state() == TransactionState::Aborted, "错误后应为 Aborted");
        primary
            .exec(&dialect.rollback_to_savepoint_sql(&recovery))
            .await
            .unwrap_or_else(|error| panic!("PostgreSQL 中止后 ROLLBACK TO SAVEPOINT 失败: {error}"));
        ack(&mut machine, ControlStatement::RollbackToSavepoint(recovery.clone()), ControlOutcome::Confirmed);
        ensure(machine.state() == TransactionState::Active, "保存点恢复后事务应可继续");
        primary
            .exec(&format!("INSERT INTO {table} (id, label) VALUES (8, 'after recovery')"))
            .await
            .unwrap();
        statement_succeeded(&mut machine);
        commit(primary, &mut machine).await;
        let recovered = observer.count(&table, 7).await.unwrap();
        let after = observer.count(&table, 8).await.unwrap();
        ensure(recovered == 1 && after == 1, format!("保存点恢复后提交应保留两行，实际 7={recovered} 8={after}"));
        record(engine_name, "aborted_savepoint_recovery", format!("kept_7={recovered} kept_8={after}"));

        begin(primary, &mut machine).await;
        primary
            .exec(&format!("INSERT INTO {table} (id, label) VALUES (9, 'aborted commit')"))
            .await
            .unwrap();
        statement_succeeded(&mut machine);
        let error = primary.exec(&bad_sql).await.expect_err("中止事务语句应失败");
        machine.observe_statement(StatementOutcome::Failed(dialect.statement_failure(error.code.as_deref(), &error.message)));
        let commit = ControlStatement::Commit { chain: false };
        let outcome = dialect.control_success(&commit, Some("ROLLBACK"));
        ensure(outcome == ControlOutcome::RolledBack, "中止事务的 COMMIT 在 PostgreSQL 实际按 ROLLBACK 返回");
        primary.exec(&dialect.commit_sql(false)).await.unwrap_or_else(|error| panic!("PostgreSQL 中止事务 COMMIT 应成功返回: {error}"));
        machine
            .acknowledge(&commit, outcome)
            .unwrap_or_else(|error| panic!("登记中止 COMMIT 实际回滚失败: {error:?}"));
        ensure(machine.state() == TransactionState::Idle, "中止事务 COMMIT 实际回滚后应为 Idle");
        let aborted = observer.count(&table, 9).await.unwrap();
        ensure(aborted == 0, format!("中止事务的 COMMIT 不应持久化，实际 {aborted}"));
        record(engine_name, "aborted_commit_rolls_back", format!("rows={aborted}"));
    }

    // 8. MySQL/MariaDB 隐式提交：DDL 前修改被隐式提交，回滚不能再撤销（R9）。
    if implicit_commit {
        let classification = dialect.implicit_commit(&format!("CREATE TABLE {implicit_table} (id INTEGER PRIMARY KEY)"));
        ensure(classification == crate::transaction::ImplicitCommitVerdict::Yes, format!("{engine_name} 应识别 DDL 隐式提交，实际 {classification:?}"));
        let insert_classification = dialect.implicit_commit(&format!("INSERT INTO {table} (id, label) VALUES (10, 'implicit')"));
        ensure(
            insert_classification == crate::transaction::ImplicitCommitVerdict::NotApplicable,
            format!("{engine_name} INSERT 不应误报隐式提交，实际 {insert_classification:?}"),
        );
        begin(primary, &mut machine).await;
        primary
            .exec(&format!("INSERT INTO {table} (id, label) VALUES (10, 'implicit commit')"))
            .await
            .unwrap();
        statement_succeeded(&mut machine);
        primary
            .exec(&format!("CREATE TABLE {implicit_table} (id INTEGER PRIMARY KEY)"))
            .await
            .unwrap_or_else(|error| panic!("{engine_name} 事务内 DDL 失败: {error}"));
        let effect = machine.observe_implicit_commit().unwrap_or_else(|error| panic!("{engine_name} 隐式提交登记失败: {error:?}"));
        ensure(effect == TransactionEffect::ImplicitCommit && machine.state() == TransactionState::Idle, format!("隐式提交后应为 Idle，实际 {effect:?}"));
        primary.exec(&dialect.rollback_sql(false)).await.unwrap();
        let committed = observer.count(&table, 10).await.unwrap();
        ensure(committed == 1, format!("{engine_name} 隐式提交后的行应持久化，实际 {committed}"));
        record(engine_name, "implicit_commit_persists_ddl", format!("rows_after_rollback={committed}"));
        primary.exec(&format!("DROP TABLE IF EXISTS {implicit_table}")).await.ok();
    }

    primary.exec(&format!("DROP TABLE IF EXISTS {table}")).await.ok();
    ensure(machine.state() == TransactionState::Idle, format!("{engine_name} 契约结束时事务应为 Idle"));
    record(engine_name, "contract_complete", "all contract sections ran");
}

// ---------------------------------------------------------------------------
// PostgreSQL
// ---------------------------------------------------------------------------

struct PgLive {
    client: tokio_postgres::Client,
    connection: tokio::task::JoinHandle<()>,
}

impl Drop for PgLive {
    fn drop(&mut self) {
        self.connection.abort();
    }
}

fn pg_error(error: tokio_postgres::Error) -> ExecError {
    ExecError { code: error.code().map(|code| code.code().to_owned()), message: error.to_string() }
}

impl PgLive {
    async fn connect(host: &str, port: u16) -> Result<Self, ExecError> {
        let mut config = tokio_postgres::Config::new();
        config
            .host(host)
            .port(port)
            .user(std::env::var("RILLSQL_LIVE_PG_USER").unwrap_or_else(|_| "postgres".to_owned()))
            .password(std::env::var("RILLSQL_LIVE_PG_PASSWORD").unwrap_or_else(|_| "postgres".to_owned()))
            .dbname(std::env::var("RILLSQL_LIVE_PG_DATABASE").unwrap_or_else(|_| "rillsql".to_owned()))
            .connect_timeout(Duration::from_secs(10));
        let (client, connection) = config.connect(tokio_postgres::NoTls).await.map_err(pg_error)?;
        let connection = tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(Self { client, connection })
    }
}

#[async_trait]
impl LiveSession for PgLive {
    fn dialect(&self) -> &'static dyn TransactionDialect {
        dialect_of(Engine::Postgres)
    }

    async fn exec(&mut self, sql: &str) -> Result<(), ExecError> {
        self.client.simple_query(sql).await.map(|_| ()).map_err(pg_error)
    }

    async fn text(&mut self, sql: &str) -> Result<String, ExecError> {
        let messages = self.client.simple_query(sql).await.map_err(pg_error)?;
        for message in messages {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = message {
                return Ok(row.get(0).unwrap_or_default().to_owned());
            }
        }
        Err(ExecError { code: None, message: format!("查询没有返回行: {sql}") })
    }

    async fn count(&mut self, table: &str, id: i32) -> Result<i64, ExecError> {
        let row = self.client.query_one(&format!("SELECT COUNT(*) FROM {table} WHERE id = {id}"), &[]).await.map_err(pg_error)?;
        Ok(row.get::<_, i64>(0))
    }

    async fn version(&mut self) -> Result<String, ExecError> {
        self.text("SELECT version()").await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the PostgreSQL service in docker/compose.yaml"]
async fn postgres_live_transaction_contract() {
    let port = env_port("RILLSQL_LIVE_PG_PORT", 5432);
    let mut primary = PgLive::connect("127.0.0.1", port).await.unwrap_or_else(|error| panic!("PostgreSQL 夹具不可用（端口 {port}）: {error}"));
    let mut observer = PgLive::connect("127.0.0.1", port).await.expect("PostgreSQL 第二连接");
    run_contract("PostgreSQL", &mut primary, &mut observer, SERIALIZABLE, "SHOW transaction_isolation", "serializable", false).await;
}

// ---------------------------------------------------------------------------
// MySQL / MariaDB
// ---------------------------------------------------------------------------

struct MyLive {
    connection: mysql_async::Conn,
}

fn mysql_error(error: mysql_async::Error) -> ExecError {
    match error {
        mysql_async::Error::Server(server) => ExecError { code: Some(server.code.to_string()), message: server.message },
        other => ExecError { code: None, message: other.to_string() },
    }
}

impl MyLive {
    async fn connect(host: &str, port: u16, prefix: &str) -> Result<Self, ExecError> {
        let mut builder = mysql_async::OptsBuilder::default()
            .ip_or_hostname(host)
            .tcp_port(port)
            .prefer_socket(false)
            .user(Some(std::env::var(format!("RILLSQL_LIVE_{prefix}_USER")).unwrap_or_else(|_| "rillsql".to_owned())))
            .pass(Some(std::env::var(format!("RILLSQL_LIVE_{prefix}_PASSWORD")).unwrap_or_else(|_| "rillsql".to_owned())));
        let database = std::env::var(format!("RILLSQL_LIVE_{prefix}_DATABASE")).unwrap_or_else(|_| "rillsql".to_owned());
        builder = builder.db_name(Some(database));
        let connection = mysql_async::Conn::new(mysql_async::Opts::from(builder)).await.map_err(mysql_error)?;
        Ok(Self { connection })
    }
}

#[async_trait]
impl LiveSession for MyLive {
    fn dialect(&self) -> &'static dyn TransactionDialect {
        // MySQL 与 MariaDB 共用同一事务方言；两个测试分别连到不同夹具。
        dialect_of(Engine::Mysql)
    }

    async fn exec(&mut self, sql: &str) -> Result<(), ExecError> {
        use mysql_async::prelude::Queryable;
        self.connection.query_drop(sql).await.map_err(mysql_error)
    }

    async fn text(&mut self, sql: &str) -> Result<String, ExecError> {
        use mysql_async::prelude::Queryable;
        let row: Option<mysql_async::Row> = self.connection.query_first(sql).await.map_err(mysql_error)?;
        row.and_then(|row| row.get::<String, _>(0))
            .ok_or_else(|| ExecError { code: None, message: format!("查询没有返回文本: {sql}") })
    }

    async fn count(&mut self, table: &str, id: i32) -> Result<i64, ExecError> {
        use mysql_async::prelude::Queryable;
        let row: Option<mysql_async::Row> = self
            .connection
            .query_first(format!("SELECT COUNT(*) FROM {table} WHERE id = {id}"))
            .await
            .map_err(mysql_error)?;
        let count = row.and_then(|row| row.get::<u64, _>(0)).ok_or_else(|| ExecError { code: None, message: "COUNT 没有返回".to_owned() })?;
        Ok(count as i64)
    }

    async fn version(&mut self) -> Result<String, ExecError> {
        self.text("SELECT VERSION()").await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the MySQL service in docker/compose.yaml"]
async fn mysql_live_transaction_contract() {
    let port = env_port("RILLSQL_LIVE_MYSQL_PORT", 3306);
    let mut primary = MyLive::connect("127.0.0.1", port, "MYSQL").await.unwrap_or_else(|error| panic!("MySQL 夹具不可用（端口 {port}）: {error}"));
    let mut observer = MyLive::connect("127.0.0.1", port, "MYSQL").await.expect("MySQL 第二连接");
    run_contract("MySQL", &mut primary, &mut observer, SERIALIZABLE, "SELECT @@transaction_isolation", "SERIALIZABLE", true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the MariaDB service in docker/compose.yaml"]
async fn mariadb_live_transaction_contract() {
    let port = env_port("RILLSQL_LIVE_MARIADB_PORT", 3307);
    let mut primary = MyLive::connect("127.0.0.1", port, "MARIADB").await.unwrap_or_else(|error| panic!("MariaDB 夹具不可用（端口 {port}）: {error}"));
    let mut observer = MyLive::connect("127.0.0.1", port, "MARIADB").await.expect("MariaDB 第二连接");
    run_contract("MariaDB", &mut primary, &mut observer, SERIALIZABLE, "SELECT @@transaction_isolation", "SERIALIZABLE", true).await;
}

// ---------------------------------------------------------------------------
// SQLite
// ---------------------------------------------------------------------------

struct SqliteLive {
    connection: sqlx::sqlite::SqliteConnection,
}

fn sqlite_error(error: sqlx::Error) -> ExecError {
    let code = match &error {
        sqlx::Error::Database(database) => database.code().map(|code| code.to_string()),
        _ => None,
    };
    ExecError { code, message: error.to_string() }
}

impl SqliteLive {
    async fn connect(path: &std::path::Path) -> Result<Self, ExecError> {
        use sqlx::ConnectOptions;
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .busy_timeout(Duration::from_secs(5))
            .disable_statement_logging();
        use sqlx::Connection;
        let connection = sqlx::sqlite::SqliteConnection::connect_with(&options).await.map_err(sqlite_error)?;
        Ok(Self { connection })
    }
}

#[async_trait]
impl LiveSession for SqliteLive {
    fn dialect(&self) -> &'static dyn TransactionDialect {
        dialect_of(Engine::Sqlite)
    }

    async fn exec(&mut self, sql: &str) -> Result<(), ExecError> {
        sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&mut self.connection).await.map(|_| ()).map_err(sqlite_error)
    }

    async fn text(&mut self, sql: &str) -> Result<String, ExecError> {
        use sqlx::Row;
        let row = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_one(&mut self.connection).await.map_err(sqlite_error)?;
        if let Ok(value) = row.try_get::<String, _>(0) {
            return Ok(value);
        }
        if let Ok(value) = row.try_get::<i64, _>(0) {
            return Ok(value.to_string());
        }
        row.try_get::<f64, _>(0)
            .map(|value| value.to_string())
            .map_err(|error| ExecError { code: None, message: error.to_string() })
    }

    async fn count(&mut self, table: &str, id: i32) -> Result<i64, ExecError> {
        sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table} WHERE id = {id}")))
            .fetch_one(&mut self.connection)
            .await
            .map_err(sqlite_error)
    }

    async fn version(&mut self) -> Result<String, ExecError> {
        self.text("SELECT sqlite_version()").await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "runs the full transaction contract against a temporary SQLite file"]
async fn sqlite_live_transaction_contract() {
    let path = std::env::temp_dir().join(format!("rillsql-live-transaction-{}.sqlite", suffix()));
    let mut primary = SqliteLive::connect(&path).await.expect("SQLite 主连接");
    let mut observer = SqliteLive::connect(&path).await.expect("SQLite 第二连接");
    run_contract(
        "SQLite",
        &mut primary,
        &mut observer,
        READ_UNCOMMITTED,
        "SELECT CAST(read_uncommitted AS TEXT) FROM pragma_read_uncommitted",
        "1",
        false,
    )
    .await;
    drop(primary);
    drop(observer);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// SQL Server
// ---------------------------------------------------------------------------

struct MssqlLive {
    client: tiberius::Client<tokio_util::compat::Compat<tokio::net::TcpStream>>,
}

fn mssql_error(error: tiberius::error::Error) -> ExecError {
    ExecError { code: error.code().map(|code| code.to_string()), message: error.to_string() }
}

impl MssqlLive {
    async fn connect() -> Result<Self, ExecError> {
        use tokio_util::compat::TokioAsyncReadCompatExt;
        let mut config = tiberius::Config::new();
        config.host("127.0.0.1");
        config.port(env_port("RILLSQL_LIVE_MSSQL_PORT", 1433));
        config.database(std::env::var("RILLSQL_LIVE_MSSQL_DATABASE").unwrap_or_else(|_| "rillsql".to_owned()));
        config.authentication(tiberius::AuthMethod::sql_server(
            std::env::var("RILLSQL_LIVE_MSSQL_USER").unwrap_or_else(|_| "sa".to_owned()),
            std::env::var("RILLSQL_LIVE_MSSQL_PASSWORD").unwrap_or_else(|_| "Rillsql-Passw0rd!".to_owned()),
        ));
        config.encryption(tiberius::EncryptionLevel::Required);
        config.trust_cert();
        config.application_name("RillSQL live transaction test");
        let tcp = tokio::net::TcpStream::connect(config.get_addr()).await.map_err(|error| ExecError { code: None, message: error.to_string() })?;
        tcp.set_nodelay(true).ok();
        let client = tiberius::Client::connect(config, tcp.compat()).await.map_err(mssql_error)?;
        Ok(Self { client })
    }
}

#[async_trait]
impl LiveSession for MssqlLive {
    fn dialect(&self) -> &'static dyn TransactionDialect {
        dialect_of(Engine::Sqlserver)
    }

    async fn exec(&mut self, sql: &str) -> Result<(), ExecError> {
        self.client.simple_query(sql).await.map_err(mssql_error)?.into_results().await.map(|_| ()).map_err(mssql_error)
    }

    async fn text(&mut self, sql: &str) -> Result<String, ExecError> {
        let results = self.client.simple_query(sql).await.map_err(mssql_error)?.into_first_result().await.map_err(mssql_error)?;
        let row = results.first().ok_or_else(|| ExecError { code: None, message: format!("查询没有返回行: {sql}") })?;
        Ok(row.try_get::<&str, _>(0).ok().flatten().unwrap_or_default().to_owned())
    }

    async fn count(&mut self, table: &str, id: i32) -> Result<i64, ExecError> {
        // READPAST 跳过被未提交事务锁住的行：既不脏读，也不等待提交。
        let sql = format!("SELECT COUNT(*) FROM {table} WITH (READPAST) WHERE id = {id}");
        let results = self.client.simple_query(&sql).await.map_err(mssql_error)?.into_first_result().await.map_err(mssql_error)?;
        let row = results.first().ok_or_else(|| ExecError { code: None, message: "COUNT 没有返回".to_owned() })?;
        Ok(row.try_get::<i32, _>(0).ok().flatten().map(i64::from).unwrap_or(0))
    }

    async fn version(&mut self) -> Result<String, ExecError> {
        self.text("SELECT @@VERSION").await
    }

    async fn probe(&mut self) -> Result<Option<(Vec<String>, Vec<Vec<Option<String>>>)>, ExecError> {
        let sql = self.dialect().probe_sql().expect("SQL Server 方言应有状态探针");
        let results = self.client.simple_query(sql).await.map_err(mssql_error)?.into_first_result().await.map_err(mssql_error)?;
        let row = match results.first() {
            Some(row) => row,
            None => return Ok(None),
        };
        let columns: Vec<String> = row.columns().iter().map(|column| column.name().to_owned()).collect();
        let mut values = Vec::new();
        for index in 0..columns.len() {
            let value = if let Ok(Some(text)) = row.try_get::<&str, _>(index) {
                Some(text.to_owned())
            } else if let Ok(Some(number)) = row.try_get::<i64, _>(index) {
                Some(number.to_string())
            } else if let Ok(Some(number)) = row.try_get::<i32, _>(index) {
                Some(number.to_string())
            } else if let Ok(Some(number)) = row.try_get::<i16, _>(index) {
                Some(number.to_string())
            } else {
                row.try_get::<u8, _>(index).ok().flatten().map(|number| number.to_string())
            };
            values.push(value);
        }
        Ok(Some((columns, vec![values])))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the SQL Server service and mssql-init seed in docker/compose.yaml"]
async fn sqlserver_live_transaction_contract() {
    let mut primary = MssqlLive::connect().await.unwrap_or_else(|error| panic!("SQL Server 夹具不可用: {error}"));
    let mut observer = MssqlLive::connect().await.expect("SQL Server 第二连接");
    run_contract("SQL Server", &mut primary, &mut observer, SERIALIZABLE, "SELECT CASE transaction_isolation_level WHEN 1 THEN 'read_uncommitted' WHEN 2 THEN 'read_committed' WHEN 3 THEN 'repeatable_read' WHEN 4 THEN 'serializable' WHEN 5 THEN 'snapshot' END FROM sys.dm_exec_sessions WHERE session_id = @@SPID", "serializable", false).await;

    // R5/R8：真实 @@TRANCOUNT / XACT_STATE() 探针与模型一致。
    let dialect = primary.dialect();
    let mut machine = TransactionMachine::new("SQL Server probe", dialect.capabilities());
    begin(&mut primary, &mut machine).await;
    let (columns, rows) = primary.probe().await.unwrap().expect("SQL Server 探针应返回行");
    let probe = dialect.read_probe(&columns, &rows).expect("探针结果应可解析");
    let effect = machine.observe_probe(probe);
    ensure(effect == TransactionEffect::StillActive && machine.state() == TransactionState::Active, format!("活动事务探针应确认 Active，实际 {effect:?}"));
    record("SQL Server", "trancount_probe_active", format!("columns={columns:?} rows={rows:?}"));
    rollback(&mut primary, &mut machine).await;
    let (columns, rows) = primary.probe().await.unwrap().expect("SQL Server 探针应返回行");
    let probe = dialect.read_probe(&columns, &rows).expect("探针结果应可解析");
    let effect = machine.observe_probe(probe);
    ensure(effect == TransactionEffect::Unchanged && machine.state() == TransactionState::Idle, format!("回滚后探针应为 Idle，实际 {effect:?}"));
    record("SQL Server", "trancount_probe_idle", format!("columns={columns:?} rows={rows:?}"));
}

// ---------------------------------------------------------------------------
// 控制响应丢失代理：COMMIT / ROLLBACK 已转发、响应未返回
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyFlavor {
    Postgres,
    Mysql,
}

#[derive(Default)]
struct ProxyLog {
    controls: Mutex<Vec<String>>,
    closed: AtomicUsize,
}

struct Proxy {
    port: u16,
    log: Arc<ProxyLog>,
    listener: tokio::task::JoinHandle<()>,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

fn control_command(command: &str) -> bool {
    matches!(command, "COMMIT" | "ROLLBACK")
}

/// PostgreSQL 的 Query 消息以 NUL 结尾；MySQL 的 COM_QUERY 没有。两边统一成裸关键字。
fn normalize_command(text: &str) -> String {
    text.trim_matches(|ch: char| ch == '\0' || ch == ';' || ch.is_whitespace()).to_ascii_uppercase()
}

async fn start_proxy(target_host: &str, target_port: u16, flavor: ProxyFlavor) -> Proxy {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("代理监听");
    let port = listener.local_addr().expect("代理地址").port();
    let target = format!("{target_host}:{target_port}");
    let log = Arc::new(ProxyLog::default());
    let log_for_task = Arc::clone(&log);
    let listener_task = tokio::spawn(async move {
        loop {
            let Ok((client, _)) = listener.accept().await else { break };
            let log = Arc::clone(&log_for_task);
            let target = target.clone();
            tokio::spawn(async move { relay(client, &target, flavor, log).await });
        }
    });
    Proxy { port, log, listener: listener_task }
}

async fn relay(client: tokio::net::TcpStream, target: &str, flavor: ProxyFlavor, log: Arc<ProxyLog>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let Ok(server) = tokio::net::TcpStream::connect(target).await else { return };
    let _ = client.set_nodelay(true);
    let _ = server.set_nodelay(true);
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = server.into_split();

    // 单任务双向转发：控制语句写入服务器后立刻跳出循环，绝不再读取或转发服务器响应。
    // 这样不会出现“后台转发任务抢先把 COMMIT 的响应送回客户端”的竞态。
    let mut parser = FrameParser::new(flavor);
    let mut buffer = Vec::new();
    let mut client_chunk = [0u8; 8192];
    let mut server_chunk = [0u8; 8192];
    loop {
        tokio::select! {
            read = client_read.read(&mut client_chunk) => {
                let read = match read {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                if server_write.write_all(&client_chunk[..read]).await.is_err() {
                    break;
                }
                buffer.extend_from_slice(&client_chunk[..read]);
                let commands = parser.take_controls(&mut buffer);
                if std::env::var_os("RILLSQL_PROXY_DEBUG").is_some() {
                    eprintln!("proxy flavor={flavor:?} parsed={commands:?} startup={} buffered={}", parser.startup, buffer.len());
                }
                let mut kill = false;
                for command in commands {
                    log.controls.lock().expect("proxy controls").push(command.clone());
                    kill |= control_command(&command);
                }
                if kill {
                    // 控制语句已经转发给服务器，但响应不再回给客户端：结果对客户端是未知。
                    let _ = server_write.shutdown().await;
                    break;
                }
            }
            read = server_read.read(&mut server_chunk) => {
                let read = match read {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                if client_write.write_all(&server_chunk[..read]).await.is_err() {
                    break;
                }
            }
        }
    }

    log.closed.fetch_add(1, Ordering::SeqCst);
    let _ = client_write.shutdown().await;
    let _ = server_write.shutdown().await;
}

struct FrameParser {
    flavor: ProxyFlavor,
    startup: bool,
}

impl FrameParser {
    fn new(flavor: ProxyFlavor) -> Self {
        Self { flavor, startup: flavor == ProxyFlavor::Postgres }
    }

    fn take_controls(&mut self, buffer: &mut Vec<u8>) -> Vec<String> {
        let mut controls = Vec::new();
        loop {
            match self.flavor {
                ProxyFlavor::Postgres if self.startup => {
                    if buffer.len() < 4 {
                        break;
                    }
                    let length = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
                    if length < 4 || buffer.len() < length {
                        break;
                    }
                    // SSLRequest / GSSENCRequest 后面还会跟真正的 StartupMessage，继续按启动阶段解析。
                    let code = (length == 8).then(|| u32::from_be_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]));
                    let negotiation = matches!(code, Some(80877102..=80877104));
                    buffer.drain(..length);
                    if !negotiation {
                        self.startup = false;
                    }
                }
                ProxyFlavor::Postgres => {
                    if buffer.len() < 5 {
                        break;
                    }
                    let length = u32::from_be_bytes([buffer[1], buffer[2], buffer[3], buffer[4]]) as usize;
                    let total = length + 1;
                    if length < 4 || buffer.len() < total {
                        break;
                    }
                    if buffer[0] == b'Q' {
                        let text = String::from_utf8_lossy(&buffer[5..total]);
                        controls.push(normalize_command(&text));
                    }
                    buffer.drain(..total);
                }
                ProxyFlavor::Mysql => {
                    if buffer.len() < 4 {
                        break;
                    }
                    let length = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], 0]) as usize;
                    if buffer.len() < 4 + length {
                        break;
                    }
                    let payload = &buffer[4..4 + length];
                    if payload.first() == Some(&0x03) {
                        let text = String::from_utf8_lossy(&payload[1..]);
                        controls.push(normalize_command(&text));
                    }
                    buffer.drain(..4 + length);
                }
            }
        }
        controls
    }
}

/// 复现应用执行层的控制结果翻译：`classify_failure` 先按“断连 + 写入”判定 Unknown，
/// 否则再交给方言按错误码/文本判定。返回（应用看到的失败分类、控制结果、路径）。
fn app_control_outcome(
    dialect: &'static dyn TransactionDialect,
    statement: &ControlStatement,
    sql: &str,
    code: Option<&str>,
    message: &str,
) -> (StatementFailure, ControlOutcome, &'static str) {
    match crate::query::exec::classify_failure(sql, message.to_owned(), false) {
        crate::query::exec::StatementStop::Unknown { .. } => (StatementFailure::ConnectionLost, ControlOutcome::Unconfirmed(UnknownReason::ConnectionLost), "disconnect_unknown"),
        crate::query::exec::StatementStop::Cancelled { .. } => {
            (StatementFailure::ConnectionLost, ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed), "cancelled")
        }
        crate::query::exec::StatementStop::Error { .. } => {
            let failure = dialect.statement_failure(code, message);
            (failure, dialect.control_failure(failure, statement), "dialect_error")
        }
    }
}

/// 一条控制语句的结果在响应前丢失：客户端得到连接错误，方言把提交/回滚登记为未知，
/// 代理只看到一次控制语句且之后没有重放。
async fn assert_unknown_without_replay(
    engine: &str,
    dialect: &'static dyn TransactionDialect,
    statement: ControlStatement,
    sql: &str,
    error: ExecError,
    log: &ProxyLog,
) -> bool {
    let command = match statement {
        ControlStatement::Commit { .. } => "COMMIT",
        ControlStatement::Rollback { .. } => "ROLLBACK",
        _ => panic!("只支持提交/回滚"),
    };
    let all_commands = log.controls.lock().expect("proxy controls").clone();
    let controls: Vec<String> = all_commands.iter().filter(|item| control_command(item)).cloned().collect();
    ensure(controls == vec![command.to_owned()], format!("{engine} 代理应只看到一次 {command}，实际 {all_commands:?}"));
    record(engine, "proxy_control_forwarded_once", format!("command={command} commands={all_commands:?}"));

    // 客户端没有拿到成功响应：这是结果未知的来源，而不是干净的失败。
    ensure(!error.message.is_empty(), format!("{engine} 控制响应丢失应产生客户端错误"));
    record(engine, "proxy_response_lost_client_error", format!("error={}", error));

    let dialect_failure = dialect.statement_failure(error.code.as_deref(), &error.message);
    let dialect_outcome = dialect.control_failure(dialect_failure, &statement);
    let (effective_failure, effective_outcome, path) = app_control_outcome(dialect, &statement, sql, error.code.as_deref(), &error.message);
    record(
        engine,
        "proxy_dialect_outcome",
        format!(
            "is_write={} dialect_failure={dialect_failure:?} dialect_outcome={dialect_outcome:?} app_path={path} app_failure={effective_failure:?} app_outcome={effective_outcome:?}",
            crate::query::split::is_write(sql)
        ),
    );
    let mapped_unknown = matches!(effective_outcome, ControlOutcome::Unconfirmed(UnknownReason::ConnectionLost));
    if mapped_unknown {
        record(engine, "proxy_unknown_mapping", format!("app_outcome={effective_outcome:?} path={path}"));
    } else {
        // 不伪造通过：记录应用实际会显示的结局，交给 Owner 决定返工归属。
        println!(
            "LIVE engine={engine} check=proxy_unknown_mapping GAP app_outcome={effective_outcome:?} app_failure={effective_failure:?} path={path} is_write={} error={}",
            crate::query::split::is_write(sql),
            error.message
        );
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
    let after: Vec<String> = log.controls.lock().expect("proxy controls").iter().filter(|item| control_command(item)).cloned().collect();
    ensure(after.len() == 1, format!("{engine} 客户端不应重放控制语句，实际 {after:?}"));
    record(engine, "proxy_no_replay", format!("controls_after_wait={after:?}"));
    mapped_unknown
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the PostgreSQL service in docker/compose.yaml"]
async fn postgres_control_response_loss_is_unknown_and_not_replayed() {
    let engine = "PostgreSQL";
    let port = env_port("RILLSQL_LIVE_PG_PORT", 5432);
    let dialect = dialect_of(Engine::Postgres);
    let table = format!("rillsql_proxy_{}", suffix());
    let mut direct = PgLive::connect("127.0.0.1", port).await.expect("PostgreSQL 直连");
    direct.exec(&format!("DROP TABLE IF EXISTS {table}")).await.ok();
    direct.exec(&format!("CREATE TABLE {table} (id INTEGER PRIMARY KEY, label VARCHAR(32) NOT NULL)")).await.expect("建表");

    let mut mapped_unknown_count = 0usize;
    for (statement, id) in [(ControlStatement::Commit { chain: false }, 1), (ControlStatement::Rollback { chain: false }, 2)] {
        let proxy = start_proxy("127.0.0.1", port, ProxyFlavor::Postgres).await;
        let mut session = PgLive::connect("127.0.0.1", proxy.port).await.expect("PostgreSQL 经代理连接");
        session.exec(&dialect.begin_sql(None)).await.expect("BEGIN");
        session
            .exec(&format!("INSERT INTO {table} (id, label) VALUES ({id}, 'proxy')"))
            .await
            .expect("INSERT");
        let sql = match statement {
            ControlStatement::Commit { .. } => dialect.commit_sql(false),
            _ => dialect.rollback_sql(false),
        };
        let error = session.exec(&sql).await.expect_err("控制响应丢失必须报错");
        ensure(session.exec("SELECT 1").await.is_err(), "代理断开后同一客户端不能继续使用");
        let mapped_unknown = assert_unknown_without_replay(engine, dialect, statement, &sql, error, &proxy.log).await;
        mapped_unknown_count += usize::from(mapped_unknown);
        drop(session);
        drop(proxy);

        // 结果未知：独立核对只能读出事实，不能把“读到/没读到”当作代理已经确认。
        let observed = direct.count(&table, id).await.expect("独立核对");
        record(engine, "proxy_unknown_independent_probe", format!("id={id} rows_after_loss={observed} (unknown_to_client)"));
    }
    direct.exec(&format!("DROP TABLE IF EXISTS {table}")).await.ok();
    println!("LIVE engine={engine} check=proxy_outcome_summary mapped_unknown={mapped_unknown_count}/2");
    ensure(mapped_unknown_count == 2, format!("{engine} 有两处控制结果丢失未能按未知登记，见 GAP 行"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the MySQL service in docker/compose.yaml"]
async fn mysql_control_response_loss_is_unknown_and_not_replayed() {
    mysql_family_control_loss("MySQL", Engine::Mysql, "MYSQL", 3306).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the MariaDB service in docker/compose.yaml"]
async fn mariadb_control_response_loss_is_unknown_and_not_replayed() {
    mysql_family_control_loss("MariaDB", Engine::Mysql, "MARIADB", 3307).await;
}

async fn mysql_family_control_loss(engine: &str, dialect_engine: Engine, prefix: &str, default_port: u16) {
    let port = env_port(&format!("RILLSQL_LIVE_{prefix}_PORT"), default_port);
    let dialect = dialect_of(dialect_engine);
    let table = format!("rillsql_proxy_{}", suffix());
    let mut direct = MyLive::connect("127.0.0.1", port, prefix).await.unwrap_or_else(|error| panic!("{engine} 夹具不可用（端口 {port}）: {error}"));
    direct.exec(&format!("DROP TABLE IF EXISTS {table}")).await.ok();
    direct.exec(&format!("CREATE TABLE {table} (id INT PRIMARY KEY, label VARCHAR(32) NOT NULL)")).await.expect("建表");

    let mut mapped_unknown_count = 0usize;
    for (statement, id) in [(ControlStatement::Commit { chain: false }, 1), (ControlStatement::Rollback { chain: false }, 2)] {
        let proxy = start_proxy("127.0.0.1", port, ProxyFlavor::Mysql).await;
        let mut session = MyLive::connect("127.0.0.1", proxy.port, prefix).await.expect("MySQL 经代理连接");
        session.exec(&dialect.begin_sql(None)).await.expect("BEGIN");
        session.exec(&format!("INSERT INTO {table} (id, label) VALUES ({id}, 'proxy')")).await.expect("INSERT");
        let sql = match statement {
            ControlStatement::Commit { .. } => dialect.commit_sql(false),
            _ => dialect.rollback_sql(false),
        };
        let error = session.exec(&sql).await.expect_err("控制响应丢失必须报错");
        ensure(session.exec("SELECT 1").await.is_err(), "代理断开后同一客户端不能继续使用");
        let mapped_unknown = assert_unknown_without_replay(engine, dialect, statement, &sql, error, &proxy.log).await;
        mapped_unknown_count += usize::from(mapped_unknown);
        drop(session);
        drop(proxy);

        let observed = direct.count(&table, id).await.expect("独立核对");
        record(engine, "proxy_unknown_independent_probe", format!("id={id} rows_after_loss={observed} (unknown_to_client)"));
    }

    direct.exec(&format!("DROP TABLE IF EXISTS {table}")).await.ok();
    println!("LIVE engine={engine} check=proxy_outcome_summary mapped_unknown={mapped_unknown_count}/2");
    ensure(mapped_unknown_count == 2, format!("{engine} 有两处控制结果丢失未能按未知登记，见 GAP 行"));
}
