//! 连接级事务协调器（ADR-1、ADR-2）：同连接共享快照、串行业务租约与控制命令入口。
//!
//! 一个已打开的关系型连接对应一个 [`TransactionCoordinator`]：查询、网格与结构业务共用其中的
//! [`TransactionMachine`]，所有执行由 [`TransactionCoordinator::lease`] 串行；目录、补全与探活走
//! 另一条元数据会话（见 [`crate::adapters::session::Session`]），不进入这里的快照。
//!
//! 协调器只提供“这件事能不能发给数据库、发什么、结果怎么登记”的入口。真正执行 SQL 的是查询批次
//! （`adapters::exec`）；这里不持有驱动句柄，也不重放任何语句。

use std::sync::{Arc, Mutex};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::transaction::{
    ControlMatch, ControlOutcome, ControlSql, ControlStatement, ImplicitCommitVerdict, SavepointName, StatementFailure, StatementOutcome, TransactionCapabilities,
    TransactionDialect, TransactionEffect, TransactionError, TransactionMachine, TransactionProbe, TransactionSnapshot, TransactionState, UnknownReason,
    UnknownResolution, UnknownTransaction,
};

/// 一条连接的事务协调器。
///
/// 非关系型连接（Redis、MongoDB）没有事务方言：快照为 `None`，
/// 任何控制命令都返回 [`TransactionError::TransactionsUnsupported`]，业务入口也不占用租约，
/// 保持它们原有的并发语义（R12）。
pub struct TransactionCoordinator {
    connection: String,
    dialect: Option<&'static dyn TransactionDialect>,
    machine: Mutex<TransactionMachine>,
    /// 业务执行租约：一个查询批次或一次业务读写从开始到结束独占这条连接。
    lease: Arc<AsyncMutex<()>>,
}

impl TransactionCoordinator {
    pub fn new(connection: impl Into<String>, dialect: Option<&'static dyn TransactionDialect>) -> Self {
        let connection = connection.into();
        let capabilities = dialect.map(TransactionDialect::capabilities).unwrap_or(TransactionCapabilities::UNSUPPORTED);
        Self {
            machine: Mutex::new(TransactionMachine::new(connection.clone(), capabilities)),
            connection,
            dialect,
            lease: Arc::new(AsyncMutex::new(())),
        }
    }

    pub fn connection(&self) -> &str {
        &self.connection
    }

    /// 事务方言；非关系型连接为 `None`。
    pub fn dialect(&self) -> Option<&'static dyn TransactionDialect> {
        self.dialect
    }

    pub fn capabilities(&self) -> TransactionCapabilities {
        self.dialect.map(TransactionDialect::capabilities).unwrap_or(TransactionCapabilities::UNSUPPORTED)
    }

    /// 这条连接是否参与用户事务。只有这类连接需要串行业务执行。
    pub fn is_transactional(&self) -> bool {
        self.dialect.is_some()
    }

    /// 同连接所有业务标签共享的事务快照；非关系型连接没有。
    pub fn snapshot(&self) -> Option<TransactionSnapshot> {
        self.dialect?;
        Some(self.machine().snapshot().clone())
    }

    pub fn state(&self) -> Option<TransactionState> {
        self.dialect?;
        Some(self.machine().state())
    }

    /// 取得串行业务租约。租约持有到返回的 guard 被丢弃为止；活动事务期间的执行因此不会并发。
    pub async fn lease(&self) -> ExecutionLease {
        ExecutionLease {
            connection: self.connection.clone(),
            _guard: Arc::clone(&self.lease).lock_owned().await,
        }
    }

    /// 事务控制语句的意图分类（手写 SQL）。非事务连接一律按普通语句处理。
    pub fn classify_control(&self, body: &str) -> ControlMatch {
        match self.dialect {
            Some(dialect) => dialect.classify_control(body),
            None => ControlMatch::Ordinary,
        }
    }

    pub fn implicit_commit(&self, body: &str) -> ImplicitCommitVerdict {
        match self.dialect {
            Some(dialect) => dialect.implicit_commit(body),
            None => ImplicitCommitVerdict::NotApplicable,
        }
    }

    pub fn probe_sql(&self) -> Option<&'static str> {
        self.dialect?.probe_sql()
    }

    pub fn read_probe(&self, columns: &[String], rows: &[Vec<Option<String>>]) -> Option<TransactionProbe> {
        self.dialect?.read_probe(columns, rows)
    }

    pub fn statement_failure(&self, code: Option<&str>, message: &str) -> StatementFailure {
        self.dialect.map(|dialect| dialect.statement_failure(code, message)).unwrap_or(StatementFailure::TransactionUsable)
    }

    pub fn error_may_have_ended_transaction(&self, code: Option<&str>, message: &str) -> bool {
        self.dialect.is_some_and(|dialect| dialect.error_may_have_ended_transaction(code, message))
    }

    pub fn control_failure(&self, statement: &ControlStatement, failure: StatementFailure) -> ControlOutcome {
        match self.dialect {
            Some(dialect) => dialect.control_failure(failure, statement),
            None => ControlOutcome::Rejected,
        }
    }

    pub fn control_success(&self, statement: &ControlStatement, command_tag: Option<&str>) -> ControlOutcome {
        match self.dialect {
            Some(dialect) => dialect.control_success(statement, command_tag),
            None => ControlOutcome::Rejected,
        }
    }

    // ---- 控制命令入口（按钮与手写 SQL 共用） ----

    /// 预检并生成一条控制语句的 SQL。返回 `Ok(ControlSql::Unsupported)` 表示方言不接受这个操作：
    /// 调用方不得改为其他会话或静默降级。
    pub fn prepare(&self, statement: &ControlStatement) -> Result<ControlSql, TransactionError> {
        let dialect = self.dialect.ok_or(TransactionError::TransactionsUnsupported)?;
        self.machine().precheck(statement)?;
        Ok(dialect.sql_for(statement))
    }

    pub fn begin(&self, mode: Option<&str>) -> Result<ControlCommand, TransactionError> {
        self.command(ControlStatement::Begin { mode: self.mode_or_recorded(mode) })
    }

    pub fn commit(&self) -> Result<ControlCommand, TransactionError> {
        self.command(ControlStatement::Commit { chain: false })
    }

    pub fn rollback(&self) -> Result<ControlCommand, TransactionError> {
        self.command(ControlStatement::Rollback { chain: false })
    }

    pub fn savepoint(&self, name: &SavepointName) -> Result<ControlCommand, TransactionError> {
        self.command(ControlStatement::Savepoint(name.clone()))
    }

    pub fn rollback_to_savepoint(&self, name: &SavepointName) -> Result<ControlCommand, TransactionError> {
        self.command(ControlStatement::RollbackToSavepoint(name.clone()))
    }

    pub fn release_savepoint(&self, name: &SavepointName) -> Result<ControlCommand, TransactionError> {
        self.command(ControlStatement::ReleaseSavepoint(name.clone()))
    }

    pub fn set_isolation(&self, level: Option<&str>) -> Result<ControlCommand, TransactionError> {
        self.command(ControlStatement::SetIsolation { level: level.map(str::to_owned) })
    }

    pub fn set_mode(&self, mode: Option<&str>) -> Result<ControlCommand, TransactionError> {
        self.command(ControlStatement::SetMode { mode: mode.map(str::to_owned) })
    }

    /// 命令入口：返回要登记的语句与实际下发的 SQL，执行后把结果交给 [`Self::acknowledge`]。
    fn command(&self, statement: ControlStatement) -> Result<ControlCommand, TransactionError> {
        let sql = self.prepare(&statement)?;
        Ok(ControlCommand { statement, sql })
    }

    /// 确认一条控制语句的结果并推进共享快照。
    pub fn acknowledge(&self, statement: &ControlStatement, outcome: ControlOutcome) -> Result<TransactionEffect, TransactionError> {
        self.machine().acknowledge(statement, outcome)
    }

    pub fn observe_statement(&self, outcome: StatementOutcome) -> TransactionEffect {
        match self.dialect {
            Some(_) => self.machine().observe_statement(outcome),
            None => TransactionEffect::Unchanged,
        }
    }

    pub fn observe_implicit_commit(&self) -> Result<TransactionEffect, TransactionError> {
        match self.dialect {
            Some(_) => self.machine().observe_implicit_commit(),
            None => Err(TransactionError::ImplicitCommitUnsupported),
        }
    }

    pub fn observe_probe(&self, probe: TransactionProbe) -> TransactionEffect {
        match self.dialect {
            Some(_) => self.machine().observe_probe(probe),
            None => TransactionEffect::Unchanged,
        }
    }

    /// 独立核对（用户确认或另一条连接查询）关闭旧事务的未知记录。不重放任何语句。
    pub fn resolve_unknown(&self, resolution: UnknownResolution) -> Option<UnknownTransaction> {
        self.dialect?;
        self.machine().resolve_unknown(resolution)
    }

    /// 会话重建（手动重开、自动重连、断开）：新会话从无活动事务开始，旧事务结果保留为未知。
    pub fn reconnect(&self, reason: UnknownReason) -> TransactionEffect {
        match self.dialect {
            Some(_) => self.machine().reconnect(reason),
            None => TransactionEffect::Unchanged,
        }
    }

    /// 显式模式优先；未给出时沿用已登记的设置（SQLite 等把模式放在 `BEGIN` 上的方言据此生效）。
    fn mode_or_recorded(&self, mode: Option<&str>) -> Option<String> {
        mode.map(str::to_owned)
            .or_else(|| self.machine().snapshot().settings.mode.clone())
            .filter(|mode| self.capabilities().mode.options.iter().any(|option| option.id == mode))
    }

    fn machine(&self) -> std::sync::MutexGuard<'_, TransactionMachine> {
        self.machine.lock().expect("transaction machine")
    }
}

/// 业务执行租约。持有期间同一连接的其他业务执行必须等待。
pub struct ExecutionLease {
    connection: String,
    _guard: OwnedMutexGuard<()>,
}

impl ExecutionLease {
    pub fn connection(&self) -> &str {
        &self.connection
    }
}

/// 一条控制命令：要登记的语句（可能与调用方给出的写法不同，例如方言补上事务模式）
/// 与实际要下发的 SQL。
pub struct ControlCommand {
    pub statement: ControlStatement,
    pub sql: ControlSql,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::TransactionCoordinator;
    use crate::adapters::adapter;
    use crate::connections::model::Engine;
    use crate::transaction::{
        ControlOutcome, ControlSql, ControlStatement, ImplicitCommitVerdict, SavepointName, StatementFailure, TransactionEffect, TransactionError, TransactionProbe,
        TransactionState, UnknownReason, READ_COMMITTED,
    };

    fn coordinator(engine: Engine) -> Arc<TransactionCoordinator> {
        Arc::new(TransactionCoordinator::new("conn", adapter(engine).transaction()))
    }

    #[test]
    fn non_relational_connections_have_no_transaction_surface() {
        for engine in [Engine::Redis, Engine::Mongodb] {
            let coordinator = coordinator(engine);
            assert!(!coordinator.is_transactional());
            assert!(coordinator.snapshot().is_none());
            assert!(matches!(coordinator.begin(None), Err(TransactionError::TransactionsUnsupported)));
            assert_eq!(coordinator.observe_statement(crate::transaction::StatementOutcome::Succeeded), TransactionEffect::Unchanged);
        }
    }

    #[test]
    fn control_commands_advance_the_shared_snapshot() {
        let coordinator = coordinator(Engine::Postgres);
        assert_eq!(coordinator.prepare(&ControlStatement::Begin { mode: None }).unwrap(), ControlSql::Statement("BEGIN".to_owned()));
        let begin = coordinator.begin(None).unwrap();
        assert_eq!(begin.statement, ControlStatement::Begin { mode: None });
        coordinator.acknowledge(&begin.statement, ControlOutcome::Confirmed).unwrap();
        assert_eq!(coordinator.snapshot().unwrap().state, TransactionState::Active);
        let name = SavepointName::parse("sp").unwrap();
        let savepoint = coordinator.savepoint(&name).unwrap();
        assert!(matches!(savepoint.sql, ControlSql::Statement(_)));
        coordinator.acknowledge(&savepoint.statement, ControlOutcome::Confirmed).unwrap();
        assert_eq!(coordinator.snapshot().unwrap().savepoints, vec![name.clone()]);
        coordinator.acknowledge(&ControlStatement::Commit { chain: false }, ControlOutcome::Confirmed).unwrap();
        let snapshot = coordinator.snapshot().unwrap();
        assert_eq!(snapshot.state, TransactionState::Idle);
        assert!(snapshot.savepoints.is_empty());
    }

    #[test]
    fn a_recorded_sqlite_mode_travels_with_the_next_begin_command() {
        let coordinator = coordinator(Engine::Sqlite);
        let mode = coordinator.set_mode(Some("immediate")).unwrap();
        assert_eq!(mode.sql, ControlSql::NoStatement, "SQLite 的模式随 BEGIN 下发");
        coordinator.acknowledge(&mode.statement, ControlOutcome::Confirmed).unwrap();
        let begin = coordinator.begin(None).unwrap();
        assert_eq!(begin.statement, ControlStatement::Begin { mode: Some("immediate".to_owned()) });
        assert_eq!(begin.sql, ControlSql::Statement("BEGIN IMMEDIATE".to_owned()));
    }

    #[test]
    fn implicit_commit_is_only_reported_by_the_mysql_family() {
        let mysql = coordinator(Engine::Mysql);
        assert!(matches!(mysql.implicit_commit("ALTER TABLE t ADD c INT"), ImplicitCommitVerdict::Yes));
        let postgres = coordinator(Engine::Postgres);
        assert!(!matches!(postgres.implicit_commit("ALTER TABLE t ADD c INT"), ImplicitCommitVerdict::Yes));
    }

    #[test]
    fn probe_observations_advance_or_mark_unknown() {
        let coordinator = coordinator(Engine::Sqlserver);
        assert!(coordinator.probe_sql().is_some());
        assert_eq!(coordinator.observe_probe(TransactionProbe::Active { depth: 1 }), TransactionEffect::StillActive);
        assert_eq!(coordinator.state(), Some(TransactionState::Active));
        coordinator.acknowledge(&ControlStatement::Commit { chain: false }, ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed)).unwrap();
        assert_eq!(coordinator.state(), Some(TransactionState::Unknown));
        assert!(coordinator.snapshot().unwrap().unresolved.is_some());
        coordinator.reconnect(UnknownReason::ConnectionLost);
        let snapshot = coordinator.snapshot().unwrap();
        assert_eq!(snapshot.state, TransactionState::Idle, "新会话不继承旧事务的活动标记");
        assert!(snapshot.unresolved.is_some(), "旧事务结果未知必须保留");
    }

    #[test]
    fn statement_failure_classification_comes_from_the_dialect() {
        let postgres = coordinator(Engine::Postgres);
        assert_eq!(postgres.statement_failure(Some("25P02"), "current transaction is aborted"), StatementFailure::TransactionAborted);
        let mysql = coordinator(Engine::Mysql);
        assert!(mysql.error_may_have_ended_transaction(Some("1213"), "Deadlock found"), "死锁由数据库回滚整个事务");
        let sqlite = coordinator(Engine::Sqlite);
        assert!(sqlite.error_may_have_ended_transaction(None, "database or disk is full"), "部分 SQLite 错误会自动回滚事务");
    }

    #[test]
    fn settings_are_not_rewritten_by_other_dialects() {
        let postgres = coordinator(Engine::Postgres);
        let isolation = postgres.set_isolation(Some(READ_COMMITTED)).unwrap();
        assert!(matches!(isolation.sql, ControlSql::Statement(_)));
        assert_eq!(isolation.statement, ControlStatement::SetIsolation { level: Some(READ_COMMITTED.to_owned()) });
    }

    #[tokio::test]
    async fn the_business_lease_serializes_execution() {
        let coordinator = coordinator(Engine::Postgres);
        let first = coordinator.lease().await;
        let waiting = tokio::time::timeout(Duration::from_millis(40), coordinator.lease()).await;
        assert!(waiting.is_err(), "第二个执行必须等待第一个租约");
        drop(first);
        assert!(tokio::time::timeout(Duration::from_secs(1), coordinator.lease()).await.is_ok());
    }
}
