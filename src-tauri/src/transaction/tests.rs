//! 事务核心模型的单元测试：状态与代次、控制语句分类、保存点、隔离、隐式提交与未知结果。
//!
//! 这里的方言只用于验证接口边界（引用方式、能力表、分类词汇）；真实 SQL 由各适配器在后续任务提供。

use super::dialect::{
    classify_control, AbortedRecovery, ControlMatch, ControlSql, ControlSyntax, DuplicateBegin, ImplicitCommitSupport, ImplicitCommitVerdict, SettingChange,
    SettingOption, SettingSupport, TransactionCapabilities, TransactionDialect, READ_COMMITTED, READ_ONLY, READ_UNCOMMITTED, READ_WRITE, REPEATABLE_READ,
    SERIALIZABLE, SNAPSHOT,
};
use super::model::{
    ControlOutcome, ControlStatement, PendingControl, SavepointName, StatementFailure, StatementOutcome, TransactionEffect, TransactionError, TransactionMachine,
    TransactionProbe, TransactionState, UnknownReason, UnknownResolution,
};

const READ_UNCOMMITTED_OPTION: SettingOption = SettingOption { id: READ_UNCOMMITTED, label: "Read uncommitted" };
const READ_COMMITTED_OPTION: SettingOption = SettingOption { id: READ_COMMITTED, label: "Read committed" };
const REPEATABLE_READ_OPTION: SettingOption = SettingOption { id: REPEATABLE_READ, label: "Repeatable read" };
const SERIALIZABLE_OPTION: SettingOption = SettingOption { id: SERIALIZABLE, label: "Serializable" };
const SNAPSHOT_OPTION: SettingOption = SettingOption { id: SNAPSHOT, label: "Snapshot" };
const READ_ONLY_OPTION: SettingOption = SettingOption { id: READ_ONLY, label: "Read only" };
const READ_WRITE_OPTION: SettingOption = SettingOption { id: READ_WRITE, label: "Read write" };
const DEFERRED_OPTION: SettingOption = SettingOption { id: "deferred", label: "Deferred" };
const IMMEDIATE_OPTION: SettingOption = SettingOption { id: "immediate", label: "Immediate" };
const EXCLUSIVE_OPTION: SettingOption = SettingOption { id: "exclusive", label: "Exclusive" };

const NO_MODE: SettingSupport = SettingSupport { options: &[], change: SettingChange::Unsupported };
const STANDARD_LEVELS: SettingSupport = SettingSupport {
    options: &[READ_UNCOMMITTED_OPTION, READ_COMMITTED_OPTION, REPEATABLE_READ_OPTION, SERIALIZABLE_OPTION],
    change: SettingChange::WhileIdle,
};

/// 标识符引用方式：各引擎保存点名称的写法不同。
#[derive(Debug, Clone, Copy)]
enum Quote {
    Double,
    Backtick,
    Bracket,
}

impl Quote {
    fn apply(self, name: &str) -> String {
        match self {
            Self::Double => format!("\"{}\"", name.replace('"', "\"\"")),
            Self::Backtick => format!("`{}`", name.replace('`', "``")),
            Self::Bracket => format!("[{}]", name.replace(']', "]]")),
        }
    }
}

/// 保存点语句写法：标准 SQL 与 T-SQL 不能互相替代。
#[derive(Debug, Clone, Copy)]
enum SavepointStyle {
    Standard,
    Transact,
}

/// 测试用方言：按能力表与词汇表提供 SQL，不代表任何适配器的最终实现。
#[derive(Debug, Clone, Copy)]
struct TestDialect {
    capabilities: TransactionCapabilities,
    syntax: ControlSyntax,
    quote: Quote,
    savepoint: SavepointStyle,
}

impl TestDialect {
    fn new(capabilities: TransactionCapabilities, syntax: ControlSyntax, quote: Quote, savepoint: SavepointStyle) -> Self {
        Self { capabilities, syntax, quote, savepoint }
    }
}

impl TransactionDialect for TestDialect {
    fn capabilities(&self) -> TransactionCapabilities {
        self.capabilities
    }

    fn syntax(&self) -> ControlSyntax {
        self.syntax
    }

    fn quote_identifier(&self, name: &str) -> String {
        self.quote.apply(name)
    }

    fn begin_sql(&self, mode: Option<&str>) -> String {
        match mode {
            Some(mode) => format!("BEGIN {}", mode.to_ascii_uppercase()),
            None => "BEGIN".to_owned(),
        }
    }

    fn commit_sql(&self, chain: bool) -> String {
        if chain { "COMMIT AND CHAIN".to_owned() } else { "COMMIT".to_owned() }
    }

    fn rollback_sql(&self, chain: bool) -> String {
        if chain { "ROLLBACK AND CHAIN".to_owned() } else { "ROLLBACK".to_owned() }
    }

    fn savepoint_sql(&self, name: &SavepointName) -> String {
        match self.savepoint {
            SavepointStyle::Standard => format!("SAVEPOINT {}", self.quote_identifier(name.as_str())),
            SavepointStyle::Transact => format!("SAVE TRANSACTION {}", self.quote_identifier(name.as_str())),
        }
    }

    fn rollback_to_savepoint_sql(&self, name: &SavepointName) -> String {
        match self.savepoint {
            SavepointStyle::Standard => format!("ROLLBACK TO SAVEPOINT {}", self.quote_identifier(name.as_str())),
            SavepointStyle::Transact => format!("ROLLBACK TRANSACTION {}", self.quote_identifier(name.as_str())),
        }
    }

    fn isolation_sql(&self, level: Option<&str>) -> ControlSql {
        let level = match level {
            Some(level) => level.replace('_', " ").to_ascii_uppercase(),
            None => "DEFAULT".to_owned(),
        };
        ControlSql::Statement(format!("SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL {level}"))
    }

    fn mode_sql(&self, mode: Option<&str>) -> ControlSql {
        match mode {
            Some(READ_ONLY) => ControlSql::Statement("SET TRANSACTION READ ONLY".to_owned()),
            Some(READ_WRITE) => ControlSql::Statement("SET TRANSACTION READ WRITE".to_owned()),
            Some(_) => ControlSql::Unsupported,
            None => ControlSql::NoStatement,
        }
    }
}

fn postgres_dialect() -> TestDialect {
    TestDialect::new(
        TransactionCapabilities {
            explicit_control: true,
            savepoints: true,
            release_savepoint: true,
            chained_transactions: true,
            duplicate_begin: DuplicateBegin::Ignored,
            aborted: AbortedRecovery::SavepointRecovery,
            isolation: STANDARD_LEVELS,
            mode: SettingSupport { options: &[READ_ONLY_OPTION, READ_WRITE_OPTION], change: SettingChange::WhileIdle },
            implicit_commit: ImplicitCommitSupport::NotApplicable,
        },
        ControlSyntax { bare_begin: true, start_transaction: true, end_commits: true, abort_rolls_back: true, chained: true, begin_modes: &[], rollback_transaction_name: false, hash_comments: false, identifier_quotes: &[('"', '"')] },
        Quote::Double,
        SavepointStyle::Standard,
    )
}

fn mysql_dialect() -> TestDialect {
    TestDialect::new(
        TransactionCapabilities {
            explicit_control: true,
            savepoints: true,
            release_savepoint: true,
            chained_transactions: true,
            // BEGIN/START TRANSACTION 会隐式提交旧事务。
            duplicate_begin: DuplicateBegin::EndsPrevious,
            aborted: AbortedRecovery::NotApplicable,
            isolation: STANDARD_LEVELS,
            mode: SettingSupport { options: &[READ_ONLY_OPTION, READ_WRITE_OPTION], change: SettingChange::WhileIdle },
            implicit_commit: ImplicitCommitSupport::MysqlFamily,
        },
        ControlSyntax { bare_begin: true, start_transaction: true, end_commits: false, abort_rolls_back: false, chained: true, begin_modes: &[], rollback_transaction_name: false, hash_comments: true, identifier_quotes: &[('"', '"'), ('`', '`')] },
        Quote::Backtick,
        SavepointStyle::Standard,
    )
}

fn sqlite_dialect() -> TestDialect {
    TestDialect::new(
        TransactionCapabilities {
            explicit_control: true,
            savepoints: true,
            release_savepoint: true,
            chained_transactions: false,
            duplicate_begin: DuplicateBegin::Rejected,
            aborted: AbortedRecovery::NotApplicable,
            isolation: SettingSupport { options: &[READ_UNCOMMITTED_OPTION, SERIALIZABLE_OPTION], change: SettingChange::WhileIdle },
            mode: SettingSupport { options: &[DEFERRED_OPTION, IMMEDIATE_OPTION, EXCLUSIVE_OPTION], change: SettingChange::WhileIdle },
            implicit_commit: ImplicitCommitSupport::NotApplicable,
        },
        ControlSyntax { bare_begin: true, start_transaction: false, end_commits: true, abort_rolls_back: false, chained: false, begin_modes: &["deferred", "immediate", "exclusive"], rollback_transaction_name: false, hash_comments: false, identifier_quotes: &[('"', '"'), ('`', '`'), ('[', ']')] },
        Quote::Double,
        SavepointStyle::Standard,
    )
}

fn sqlserver_dialect() -> TestDialect {
    TestDialect::new(
        TransactionCapabilities {
            explicit_control: true,
            savepoints: true,
            release_savepoint: false,
            chained_transactions: false,
            // 嵌套 BEGIN 只增 @@TRANCOUNT。
            duplicate_begin: DuplicateBegin::Nests,
            // XACT_STATE() = -1：只能整体回滚。
            aborted: AbortedRecovery::RollbackOnly,
            isolation: SettingSupport {
                options: &[READ_UNCOMMITTED_OPTION, READ_COMMITTED_OPTION, REPEATABLE_READ_OPTION, SERIALIZABLE_OPTION, SNAPSHOT_OPTION],
                change: SettingChange::AnyTime,
            },
            mode: NO_MODE,
            implicit_commit: ImplicitCommitSupport::NotApplicable,
        },
        ControlSyntax { bare_begin: false, start_transaction: false, end_commits: false, abort_rolls_back: false, chained: false, begin_modes: &[], rollback_transaction_name: true, hash_comments: false, identifier_quotes: &[('"', '"'), ('[', ']')] },
        Quote::Bracket,
        SavepointStyle::Transact,
    )
}

fn unsupported_dialect() -> TestDialect {
    TestDialect::new(TransactionCapabilities::UNSUPPORTED, ControlSyntax::STANDARD, Quote::Double, SavepointStyle::Standard)
}

fn caps_without_savepoints() -> TransactionCapabilities {
    let mut capabilities = postgres_dialect().capabilities();
    capabilities.savepoints = false;
    capabilities.release_savepoint = false;
    capabilities
}

fn caps_without_settings() -> TransactionCapabilities {
    let mut capabilities = postgres_dialect().capabilities();
    capabilities.isolation = SettingSupport::NONE;
    capabilities
}

fn machine(capabilities: TransactionCapabilities) -> TransactionMachine {
    TransactionMachine::new("conn-1", capabilities)
}

fn machine_for(dialect: &TestDialect) -> TransactionMachine {
    machine(dialect.capabilities())
}

fn postgres_machine() -> TransactionMachine {
    machine_for(&postgres_dialect())
}

fn mysql_machine() -> TransactionMachine {
    machine_for(&mysql_dialect())
}

fn begin() -> ControlStatement {
    ControlStatement::Begin { mode: None }
}

fn begin_with(mode: &str) -> ControlStatement {
    ControlStatement::Begin { mode: Some(mode.to_owned()) }
}

fn commit() -> ControlStatement {
    ControlStatement::Commit { chain: false }
}

fn rollback() -> ControlStatement {
    ControlStatement::Rollback { chain: false }
}

fn savepoint(name: &str) -> ControlStatement {
    ControlStatement::Savepoint(SavepointName::parse(name).unwrap())
}

fn rollback_to(name: &str) -> ControlStatement {
    ControlStatement::RollbackToSavepoint(SavepointName::parse(name).unwrap())
}

fn release(name: &str) -> ControlStatement {
    ControlStatement::ReleaseSavepoint(SavepointName::parse(name).unwrap())
}

fn isolation(level: Option<&str>) -> ControlStatement {
    ControlStatement::SetIsolation { level: level.map(str::to_owned) }
}

fn control(statement: ControlStatement) -> ControlMatch {
    ControlMatch::Control(statement)
}

fn confirmed(machine: &mut TransactionMachine, statement: &ControlStatement) -> TransactionEffect {
    machine.acknowledge(statement, ControlOutcome::Confirmed).expect("数据库确认的控制语句")
}

fn started(capabilities: TransactionCapabilities) -> TransactionMachine {
    let mut machine = machine(capabilities);
    confirmed(&mut machine, &begin());
    machine
}

/// 一条完整事务：开始、保存点、回滚到保存点、提交。用于验证代次可重复。
fn committed_scenario() -> TransactionMachine {
    let mut machine = postgres_machine();
    assert_eq!(machine.state(), TransactionState::Idle);
    confirmed(&mut machine, &begin());
    confirmed(&mut machine, &savepoint("before-update"));
    machine.acknowledge(&rollback_to("before-update"), ControlOutcome::Confirmed).unwrap();
    confirmed(&mut machine, &commit());
    machine
}

#[test]
fn states_and_generations_are_repeatable() {
    let first = committed_scenario();
    let second = committed_scenario();
    assert_eq!(first.snapshot(), second.snapshot(), "同一操作序列产生同一快照");
    assert_eq!(first.snapshot().state, TransactionState::Idle);
    assert_eq!(first.snapshot().generation, 2, "开始与提交各推进一次代次");
    assert!(first.snapshot().revision > first.snapshot().generation);
    assert_eq!(first.snapshot().last_effect, TransactionEffect::Committed);
    assert_eq!(first.snapshot().savepoints, Vec::<SavepointName>::new());
    assert_eq!(first.snapshot().depth, Some(0));
}

#[test]
fn new_machine_starts_idle_without_a_transaction() {
    let machine = postgres_machine();
    let snapshot = machine.snapshot();
    assert_eq!(snapshot.connection, "conn-1");
    assert_eq!(snapshot.state, TransactionState::Idle);
    assert_eq!(snapshot.generation, 0);
    assert_eq!(snapshot.revision, 0);
    assert_eq!(snapshot.depth, Some(0));
    assert_eq!(snapshot.savepoints, Vec::<SavepointName>::new());
    assert_eq!(snapshot.settings.isolation, None, "未设置时使用服务器默认");
    assert_eq!(snapshot.settings.mode, None);
    assert!(snapshot.unresolved.is_none());
    assert!(snapshot.state.allows_statements());
    assert!(!snapshot.state.needs_recovery());
}

#[test]
fn savepoints_change_revision_but_not_generation() {
    let mut machine = started(postgres_dialect().capabilities());
    let generation = machine.snapshot().generation;
    let revision = machine.snapshot().revision;
    assert_eq!(confirmed(&mut machine, &savepoint("sp")), TransactionEffect::StillActive);
    assert_eq!(machine.snapshot().generation, generation, "保存点不改变事务身份");
    assert!(machine.snapshot().revision > revision);
    assert_eq!(machine.state(), TransactionState::Active);
}

#[test]
fn committing_without_an_active_transaction_is_not_success() {
    let mut machine = postgres_machine();
    let before = machine.snapshot().clone();
    assert_eq!(machine.acknowledge(&commit(), ControlOutcome::Confirmed), Err(TransactionError::NoActiveTransaction));
    assert_eq!(machine.snapshot(), &before, "失败不推进状态，也不写成功效果");
    assert_eq!(machine.acknowledge(&rollback(), ControlOutcome::Confirmed), Err(TransactionError::NoActiveTransaction));
    assert_eq!(machine.snapshot(), &before);
}

#[test]
fn precheck_refuses_duplicate_begin_where_it_would_not_end_the_old_transaction() {
    // MySQL/MariaDB 的重复开始会隐式提交旧事务：允许下发，但结果必须报告隐式提交。
    let mysql = started(mysql_dialect().capabilities());
    assert_eq!(mysql.precheck(&begin()), Ok(()));
    // 其余方言的重复开始只会被忽略、拒绝或增加层数：界面先引导到保存点。
    for capabilities in [postgres_dialect().capabilities(), sqlite_dialect().capabilities(), sqlserver_dialect().capabilities()] {
        let machine = started(capabilities);
        assert_eq!(machine.precheck(&begin()), Err(TransactionError::AlreadyActive));
        assert_eq!(machine.state(), TransactionState::Active, "预检不改变状态");
    }
}

#[test]
fn confirmed_duplicate_begin_follows_the_dialect_instead_of_keeping_the_old_active_state() {
    // MySQL/MariaDB：旧事务被隐式提交，新事务开始；旧保存点不再有效。
    let mut mysql = started(mysql_dialect().capabilities());
    confirmed(&mut mysql, &savepoint("sp"));
    let generation = mysql.snapshot().generation;
    assert_eq!(confirmed(&mut mysql, &begin()), TransactionEffect::ImplicitCommit);
    assert_eq!(mysql.state(), TransactionState::Active);
    assert_eq!(mysql.snapshot().generation, generation + 1);
    assert_eq!(mysql.snapshot().depth, Some(1));
    assert!(mysql.snapshot().savepoints.is_empty());

    // PostgreSQL：警告并忽略，事务身份与保存点都不变。
    let mut postgres = started(postgres_dialect().capabilities());
    confirmed(&mut postgres, &savepoint("sp"));
    let generation = postgres.snapshot().generation;
    assert_eq!(confirmed(&mut postgres, &begin()), TransactionEffect::StillActive);
    assert_eq!(postgres.state(), TransactionState::Active);
    assert_eq!(postgres.snapshot().generation, generation);
    assert_eq!(postgres.snapshot().savepoints.len(), 1);

    // SQL Server：层数加一，一次提交不结束外层事务。
    let mut sqlserver = started(sqlserver_dialect().capabilities());
    assert_eq!(confirmed(&mut sqlserver, &begin()), TransactionEffect::StillActive);
    assert_eq!(sqlserver.snapshot().depth, Some(2));
    assert_eq!(confirmed(&mut sqlserver, &commit()), TransactionEffect::StillActive);
    assert_eq!(sqlserver.state(), TransactionState::Active, "嵌套事务的第一层提交不结束事务");
    assert_eq!(sqlserver.snapshot().depth, Some(1));
    assert_eq!(confirmed(&mut sqlserver, &commit()), TransactionEffect::Committed);
    assert_eq!(sqlserver.state(), TransactionState::Idle);
    assert_eq!(sqlserver.snapshot().depth, Some(0));

    // 方言声明拒绝重复开始，数据库却确认成功：无法解释，按未知处理。
    let mut sqlite = started(sqlite_dialect().capabilities());
    assert_eq!(confirmed(&mut sqlite, &begin()), TransactionEffect::Unknown);
    assert_eq!(sqlite.state(), TransactionState::Unknown);
    assert!(sqlite.snapshot().unresolved.is_some());
}

#[test]
fn lost_rollback_is_not_reported_as_a_later_commit() {
    let mut machine = started(postgres_dialect().capabilities());
    assert_eq!(
        machine.acknowledge(&rollback(), ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed)),
        Ok(TransactionEffect::Unknown)
    );
    assert_eq!(machine.state(), TransactionState::Unknown);
    assert_eq!(machine.snapshot().unresolved.as_ref().map(|record| record.pending), Some(PendingControl::Rollback));

    // 若 ROLLBACK 实际已在服务器成功，这条 COMMIT 只是 warning / no-op：不能报告已提交。
    assert_eq!(machine.acknowledge(&commit(), ControlOutcome::Confirmed), Ok(TransactionEffect::Unknown));
    assert_eq!(machine.state(), TransactionState::Idle, "事务确实结束了");
    let unresolved = machine.snapshot().unresolved.as_ref().expect("结局仍未知");
    assert_eq!(unresolved.pending, PendingControl::Rollback);
    // 独立核对给出结论后才关闭。
    assert!(machine.resolve_unknown(UnknownResolution::RolledBack).is_some());
}

#[test]
fn confirmed_commit_resolves_a_lost_commit_record() {
    let mut machine = started(postgres_dialect().capabilities());
    machine.acknowledge(&commit(), ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed)).unwrap();
    assert_eq!(machine.snapshot().unresolved.as_ref().map(|record| record.pending), Some(PendingControl::Commit));
    // 方向一致（丢失的也是一次提交）：这次成功可以确认事务已提交。
    assert_eq!(machine.acknowledge(&commit(), ControlOutcome::Confirmed), Ok(TransactionEffect::Committed));
    assert_eq!(machine.state(), TransactionState::Idle);
    assert!(machine.snapshot().unresolved.is_none());
}

#[test]
fn lost_rollback_then_begin_does_not_fake_an_implicit_commit() {
    // MySQL/MariaDB：BEGIN 是否隐式提交取决于旧事务是否仍活动；丢失的是 ROLLBACK 时无法确认。
    let mut machine = started(mysql_dialect().capabilities());
    machine.acknowledge(&rollback(), ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed)).unwrap();
    assert_eq!(confirmed(&mut machine, &begin()), TransactionEffect::Unknown);
    assert_eq!(machine.state(), TransactionState::Active, "新事务确实已经开始");
    assert_eq!(machine.snapshot().depth, Some(1));
    assert!(machine.snapshot().savepoints.is_empty());
    let unresolved = machine.snapshot().unresolved.as_ref().expect("旧事务结局未知，记录保留");
    assert_eq!(unresolved.pending, PendingControl::Rollback);

    // 丢失的是提交时方向一致：BEGIN 的隐式提交可以确认旧事务已提交。
    let mut machine = started(mysql_dialect().capabilities());
    machine.acknowledge(&commit(), ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed)).unwrap();
    assert_eq!(confirmed(&mut machine, &begin()), TransactionEffect::ImplicitCommit);
    assert!(machine.snapshot().unresolved.is_none());
}

#[test]
fn lost_rollback_then_implicit_commit_stays_unknown() {
    let mut machine = started(mysql_dialect().capabilities());
    machine.acknowledge(&rollback(), ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed)).unwrap();
    // 若回滚已在服务器成功，这条 DDL 没有可隐式提交的事务。
    assert_eq!(machine.observe_implicit_commit(), Ok(TransactionEffect::Unknown));
    assert_eq!(machine.state(), TransactionState::Idle);
    assert!(machine.snapshot().unresolved.is_some());

    // 丢失的是提交时，隐式提交可以确认结局。
    let mut machine = started(mysql_dialect().capabilities());
    machine.acknowledge(&commit(), ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed)).unwrap();
    assert_eq!(machine.observe_implicit_commit(), Ok(TransactionEffect::ImplicitCommit));
    assert!(machine.snapshot().unresolved.is_none());
}

#[test]
fn unrecognized_effects_mark_sqlserver_depth_unverified() {
    // 过程/动态 SQL 可能嵌套 BEGIN，@@TRANCOUNT 不能再凭旧值判断。
    let mut machine = started(sqlserver_dialect().capabilities());
    assert_eq!(machine.observe_statement(StatementOutcome::Unconfirmed(UnknownReason::UnrecognizedEffect)), TransactionEffect::Unknown);
    assert_eq!(machine.state(), TransactionState::Unknown);
    assert_eq!(machine.snapshot().depth, None, "无法识别的效果可能改变 @@TRANCOUNT");
    assert_eq!(machine.acknowledge(&commit(), ControlOutcome::Confirmed), Ok(TransactionEffect::Unknown));
    assert_eq!(machine.state(), TransactionState::Unknown, "层数不明时不能宣称事务结束");
    assert!(machine.snapshot().unresolved.is_some());

    // 中止事务里出现“成功”普通语句同样无法解释，层数也不可信。
    let mut machine = started(sqlserver_dialect().capabilities());
    machine.observe_statement(StatementOutcome::Failed(StatementFailure::TransactionAborted));
    assert_eq!(machine.snapshot().depth, Some(1));
    assert_eq!(machine.observe_statement(StatementOutcome::Succeeded), TransactionEffect::Unknown);
    assert_eq!(machine.snapshot().depth, None);
    assert_eq!(machine.state(), TransactionState::Unknown);
}

#[test]
fn probe_confirmed_current_transaction_is_resolved_by_a_confirmed_end() {
    let mut machine = started(postgres_dialect().capabilities());
    machine.observe_statement(StatementOutcome::Failed(StatementFailure::ConnectionLost));
    assert_eq!(machine.state(), TransactionState::Unknown);
    let generation = machine.snapshot().generation;
    assert_eq!(machine.observe_probe(TransactionProbe::Active { depth: 1 }), TransactionEffect::StillActive);
    assert_eq!(machine.state(), TransactionState::Active);
    assert_eq!(machine.snapshot().generation, generation, "确认同一事务不推进代次");
    assert!(machine.snapshot().unresolved.is_some());
    // 结束的就是这条未知事务：提交成功后记录随之关闭。
    assert_eq!(confirmed(&mut machine, &commit()), TransactionEffect::Committed);
    assert!(machine.snapshot().unresolved.is_none());
}

#[test]
fn nested_rollback_ends_the_whole_transaction() {
    let mut machine = started(sqlserver_dialect().capabilities());
    confirmed(&mut machine, &begin());
    assert_eq!(machine.snapshot().depth, Some(2));
    assert_eq!(confirmed(&mut machine, &rollback()), TransactionEffect::RolledBack);
    assert_eq!(machine.state(), TransactionState::Idle, "T-SQL 的回滚结束所有嵌套层");
    assert_eq!(machine.snapshot().depth, Some(0));
}

#[test]
fn sqlserver_commit_in_unknown_with_unfinished_nesting_stays_unknown() {
    // @@TRANCOUNT = 2，随后丢失一次语句结果进入未知。
    let mut machine = started(sqlserver_dialect().capabilities());
    confirmed(&mut machine, &begin());
    assert_eq!(machine.snapshot().depth, Some(2));
    machine.observe_statement(StatementOutcome::Failed(StatementFailure::ConnectionLost));
    assert_eq!(machine.state(), TransactionState::Unknown);
    assert_eq!(machine.snapshot().depth, Some(2), "普通语句丢失不改变已知层数");
    assert!(machine.snapshot().unresolved.is_some());

    // 这次 COMMIT 只把 @@TRANCOUNT 从 2 降到 1：事务仍活动，不得报告提交成功。
    assert_eq!(machine.acknowledge(&commit(), ControlOutcome::Confirmed), Ok(TransactionEffect::StillActive));
    assert_eq!(machine.state(), TransactionState::Unknown, "外层事务仍未结束，未知记录不能关闭");
    assert_eq!(machine.snapshot().depth, Some(1));
    assert!(machine.snapshot().unresolved.is_some(), "事务未结束时未知记录保留");

    // 第二次提交确实结束了事务，但丢失的是普通语句结果（方向不明）：结局仍无法确认。
    assert_eq!(machine.acknowledge(&commit(), ControlOutcome::Confirmed), Ok(TransactionEffect::Unknown));
    assert_eq!(machine.state(), TransactionState::Idle);
    assert_eq!(machine.snapshot().depth, Some(0));
    assert!(machine.snapshot().unresolved.is_some(), "方向不明的未知仍须独立核对");
    assert!(machine.resolve_unknown(UnknownResolution::Committed).is_some());
}

#[test]
fn sqlserver_begin_in_unknown_requires_a_probe_instead_of_resetting_depth() {
    let mut machine = started(sqlserver_dialect().capabilities());
    machine.observe_statement(StatementOutcome::Failed(StatementFailure::ConnectionLost));
    assert_eq!(machine.state(), TransactionState::Unknown);
    assert_eq!(machine.snapshot().depth, Some(1));

    // 确认执行的嵌套 BEGIN 让 @@TRANCOUNT 无法核实：保持未知，不重置为单一层。
    assert_eq!(confirmed(&mut machine, &begin()), TransactionEffect::Unknown);
    assert_eq!(machine.state(), TransactionState::Unknown);
    assert_eq!(machine.snapshot().depth, None, "层数无法确认，必须由探针核对");
    assert!(machine.snapshot().unresolved.is_some());

    // 层数不明时一次 COMMIT 不足以宣称事务结束。
    assert_eq!(machine.acknowledge(&commit(), ControlOutcome::Confirmed), Ok(TransactionEffect::Unknown));
    assert_eq!(machine.state(), TransactionState::Unknown);
    assert!(machine.snapshot().unresolved.is_some());

    // 探针确认没有事务后才回到 Idle，且未知记录仍需独立核对。
    assert_eq!(machine.observe_probe(TransactionProbe::Idle), TransactionEffect::Unknown);
    assert_eq!(machine.state(), TransactionState::Idle);
    assert!(machine.snapshot().unresolved.is_some());
}

#[test]
fn begin_in_unknown_keeps_dialect_semantics() {
    // MySQL/MariaDB：确认的 BEGIN 会隐式提交此前可能存在的未知事务，并开始新事务。
    let mut mysql = started(mysql_dialect().capabilities());
    mysql.acknowledge(&commit(), ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed)).unwrap();
    assert_eq!(mysql.state(), TransactionState::Unknown);
    assert!(mysql.snapshot().unresolved.is_some());
    assert_eq!(confirmed(&mut mysql, &begin()), TransactionEffect::ImplicitCommit);
    assert_eq!(mysql.state(), TransactionState::Active);
    assert_eq!(mysql.snapshot().depth, Some(1));
    assert!(mysql.snapshot().unresolved.is_none(), "隐式提交结束了未知事务，记录已解决");

    // PostgreSQL：BEGIN 被忽略，事务身份与仍有效的保存点都不变。
    let mut postgres = started(postgres_dialect().capabilities());
    confirmed(&mut postgres, &savepoint("sp"));
    postgres.observe_statement(StatementOutcome::Failed(StatementFailure::ConnectionLost));
    assert_eq!(postgres.state(), TransactionState::Unknown);
    let generation = postgres.snapshot().generation;
    assert_eq!(confirmed(&mut postgres, &begin()), TransactionEffect::StillActive);
    assert_eq!(postgres.state(), TransactionState::Active);
    assert_eq!(postgres.snapshot().generation, generation, "不伪造新事务代次");
    assert_eq!(postgres.snapshot().savepoints.len(), 1, "仍有效的保存点不能被清空");
    assert!(postgres.snapshot().unresolved.is_some(), "事务身份仍未确认，保留未知记录");

    // 结束时确认的就是这条未知事务：提交成功后记录才能关闭（P3）。
    assert_eq!(confirmed(&mut postgres, &commit()), TransactionEffect::Committed);
    assert!(postgres.snapshot().unresolved.is_none());
}

#[test]
fn manual_sql_and_buttons_share_the_same_state_machine() {
    let dialect = postgres_dialect();
    let mut machine = machine(dialect.capabilities());
    // 手写 BEGIN：先分类，再按数据库确认结果推进。
    let ControlMatch::Control(statement) = dialect.classify_control("BEGIN") else { panic!("BEGIN 应识别为开始事务") };
    assert_eq!(statement, begin());
    assert_eq!(confirmed(&mut machine, &statement), TransactionEffect::Began);
    // 按钮提交与手写提交使用同一入口与同一状态。
    assert_eq!(confirmed(&mut machine, &commit()), TransactionEffect::Committed);
    let ControlMatch::Control(statement) = dialect.classify_control("ROLLBACK") else { panic!("ROLLBACK 应识别为回滚") };
    assert_eq!(statement, rollback());
    assert_eq!(machine.acknowledge(&statement, ControlOutcome::Confirmed), Err(TransactionError::NoActiveTransaction));
}

#[test]
fn classifying_control_text_does_not_advance_state() {
    let dialect = postgres_dialect();
    let mut machine = machine(dialect.capabilities());
    let before = machine.snapshot().clone();
    assert_eq!(dialect.classify_control("COMMIT"), control(commit()));
    assert_eq!(dialect.classify_control("ROLLBACK"), control(rollback()));
    assert_eq!(machine.snapshot(), &before, "只解析语句文本不改变状态");
    confirmed(&mut machine, &begin());
    let active = machine.snapshot().clone();
    assert_eq!(dialect.classify_control("COMMIT;"), control(commit()));
    assert_eq!(machine.snapshot(), &active, "文本相似不代表已提交");
}

#[test]
fn manual_control_is_classified_by_dialect_syntax() {
    let postgres = postgres_dialect();
    assert_eq!(postgres.classify_control("BEGIN"), control(begin()));
    assert_eq!(postgres.classify_control("  begin work  "), control(begin()));
    assert_eq!(postgres.classify_control("START TRANSACTION"), control(begin()));
    assert_eq!(postgres.classify_control("END"), control(commit()));
    assert_eq!(postgres.classify_control("ABORT"), control(rollback()));
    assert_eq!(postgres.classify_control("COMMIT AND CHAIN"), control(ControlStatement::Commit { chain: true }));
    assert_eq!(postgres.classify_control("-- 提交\nCOMMIT;"), control(commit()));
    assert_eq!(postgres.classify_control("COMMIT /* done */"), control(commit()));
    assert_eq!(postgres.classify_control("SAVEPOINT \"sp one\""), control(savepoint("sp one")));
    assert_eq!(postgres.classify_control("ROLLBACK TO SAVEPOINT sp"), control(rollback_to("sp")));
    assert_eq!(postgres.classify_control("RELEASE SAVEPOINT sp"), control(release("sp")));
    assert_eq!(postgres.classify_control("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ"), control(isolation(Some(REPEATABLE_READ))));
    assert_eq!(postgres.classify_control("SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL DEFAULT"), control(isolation(None)));
    // 工作单元选项超出可确认范围：执行后只能按未知处理。
    assert_eq!(postgres.classify_control("BEGIN ISOLATION LEVEL SERIALIZABLE"), ControlMatch::Ambiguous);
    assert_eq!(postgres.classify_control("SELECT 1"), ControlMatch::Ordinary);

    let mysql = mysql_dialect();
    assert_eq!(mysql.classify_control("START TRANSACTION"), control(begin()));
    assert_eq!(mysql.classify_control("# 注释\nBEGIN"), control(begin()));
    assert_eq!(mysql.classify_control("END"), ControlMatch::Ordinary, "MySQL 的 END 不是提交");
    assert_eq!(mysql.classify_control("SAVEPOINT `sp`"), control(savepoint("sp")));
    assert_eq!(mysql.classify_control("ROLLBACK TO sp"), control(rollback_to("sp")));
    assert_eq!(mysql.classify_control("COMMIT AND NO CHAIN"), control(commit()));
    assert_eq!(mysql.classify_control("SET SESSION TRANSACTION ISOLATION LEVEL READ COMMITTED"), control(isolation(Some(READ_COMMITTED))));
    assert_eq!(mysql.classify_control("SET TRANSACTION READ ONLY"), control(ControlStatement::SetMode { mode: Some(READ_ONLY.to_owned()) }));
    // 改变事务边界的设置不在控制模型内。
    assert_eq!(mysql.classify_control("SET autocommit = 0"), ControlMatch::Ambiguous);
    assert_eq!(mysql.classify_control("SET @@autocommit = 1"), ControlMatch::Ambiguous);

    let sqlite = sqlite_dialect();
    assert_eq!(sqlite.classify_control("BEGIN IMMEDIATE"), control(begin_with("immediate")));
    assert_eq!(sqlite.classify_control("BEGIN TRANSACTION"), control(begin()));
    assert_eq!(sqlite.classify_control("END TRANSACTION"), control(commit()));
    assert_eq!(sqlite.classify_control("START TRANSACTION"), ControlMatch::Ordinary, "SQLite 只认 BEGIN");
    assert_eq!(sqlite.classify_control("COMMIT AND CHAIN"), ControlMatch::Ambiguous, "SQLite 不支持链式事务");
    assert_eq!(sqlite.classify_control("SAVEPOINT 'sp'"), control(savepoint("sp")));

    let sqlserver = sqlserver_dialect();
    assert_eq!(sqlserver.classify_control("BEGIN"), ControlMatch::Ordinary, "T-SQL 的 BEGIN 是块开始");
    assert_eq!(sqlserver.classify_control("BEGIN TRY"), ControlMatch::Ordinary);
    assert_eq!(sqlserver.classify_control("BEGIN TRAN"), control(begin()));
    assert_eq!(sqlserver.classify_control("BEGIN TRANSACTION"), control(begin()));
    assert_eq!(sqlserver.classify_control("ROLLBACK TRAN"), control(rollback()));
    assert_eq!(sqlserver.classify_control("ROLLBACK TRANSACTION"), control(rollback()));
    assert_eq!(sqlserver.classify_control("ROLLBACK TRANSACTION sp1"), control(rollback_to("sp1")));
    assert_eq!(sqlserver.classify_control("SAVEPOINT [sp one]"), control(savepoint("sp one")));
    assert_eq!(sqlserver.classify_control("SET TRANSACTION ISOLATION LEVEL SNAPSHOT"), control(isolation(Some(SNAPSHOT))));
    assert_eq!(sqlserver.classify_control("END"), ControlMatch::Ordinary);
    assert_eq!(sqlserver.classify_control("COMMIT AND CHAIN"), ControlMatch::Ambiguous);
}

#[test]
fn semicolons_and_multiple_statements_are_classified_without_hanging() {
    let postgres = postgres_dialect();
    // 多条语句：调用方应先切分，这里必须明确拒绝而不是把整段当成一条控制语句。
    assert_eq!(postgres.classify_control("COMMIT;SELECT 1"), ControlMatch::Ambiguous);
    assert_eq!(postgres.classify_control("BEGIN; COMMIT"), ControlMatch::Ambiguous);
    assert_eq!(postgres.classify_control("SAVEPOINT sp; COMMIT"), ControlMatch::Ambiguous);
    // 空语句与重复结尾分号仍是同一条语句。
    assert_eq!(postgres.classify_control("COMMIT;;"), control(commit()));
    assert_eq!(postgres.classify_control("COMMIT ; ; "), control(commit()));
    assert_eq!(postgres.classify_control("; COMMIT"), control(commit()));
    assert_eq!(postgres.classify_control(";; ;"), ControlMatch::Ordinary);
    assert_eq!(postgres.classify_control(""), ControlMatch::Ordinary);
    // dollar-quote 里的分号不是语句分隔符（PostgreSQL）。
    assert_eq!(postgres.classify_control("SELECT $$a;b$$"), ControlMatch::Ordinary);
    assert_eq!(postgres.classify_control("DO $$BEGIN COMMIT; END$$"), ControlMatch::Ordinary);
    assert_eq!(postgres.classify_control("SELECT 1; COMMIT"), ControlMatch::Ambiguous, "真正的多条语句仍按未知处理");
    // 多条语句同样不会推进状态。
    let machine = started(postgres_dialect().capabilities());
    let before = machine.snapshot().clone();
    assert_eq!(postgres.classify_control("COMMIT;SELECT 1"), ControlMatch::Ambiguous);
    assert_eq!(machine.snapshot(), &before);
}

#[test]
fn savepoint_names_are_validated_and_quoted() {
    assert_eq!(SavepointName::parse("  sp  ").unwrap().as_str(), "sp");
    assert!(SavepointName::parse("   ").is_err());
    assert!(SavepointName::parse("bad\nname").is_err());
    assert_eq!(SavepointName::parse("   "), Err(TransactionError::InvalidSavepointName("   ".to_owned())));

    let name = SavepointName::parse("sp one").unwrap();
    assert_eq!(postgres_dialect().savepoint_sql(&name), "SAVEPOINT \"sp one\"");
    assert_eq!(mysql_dialect().savepoint_sql(&name), "SAVEPOINT `sp one`");
    assert_eq!(sqlserver_dialect().savepoint_sql(&name), "SAVE TRANSACTION [sp one]", "T-SQL 没有 SAVEPOINT 语句");
    assert_eq!(postgres_dialect().rollback_to_savepoint_sql(&name), "ROLLBACK TO SAVEPOINT \"sp one\"");
    assert_eq!(sqlserver_dialect().rollback_to_savepoint_sql(&name), "ROLLBACK TRANSACTION [sp one]");
    let quote = SavepointName::parse("a\"b").unwrap();
    assert_eq!(postgres_dialect().savepoint_sql(&quote), "SAVEPOINT \"a\"\"b\"");
    assert_eq!(mysql_dialect().release_savepoint_sql(&name), ControlSql::Statement("RELEASE SAVEPOINT `sp one`".to_owned()));
    assert_eq!(sqlserver_dialect().release_savepoint_sql(&name), ControlSql::Unsupported, "SQL Server 没有释放保存点");
}

#[test]
fn savepoint_boundaries_are_matched_without_case_sensitivity() {
    let mut machine = started(postgres_dialect().capabilities());
    confirmed(&mut machine, &savepoint("Keep"));
    assert_eq!(machine.acknowledge(&rollback_to("keep"), ControlOutcome::Confirmed), Ok(TransactionEffect::StillActive));
    assert_eq!(machine.state(), TransactionState::Active, "大小写不同不是未知保存点");
    assert_eq!(machine.snapshot().savepoints.len(), 1);
    assert_eq!(machine.acknowledge(&release("KEEP"), ControlOutcome::Confirmed), Ok(TransactionEffect::StillActive));
    assert!(machine.snapshot().savepoints.is_empty());
    assert_eq!(machine.state(), TransactionState::Active);
}

#[test]
fn control_commands_map_to_dialect_sql_or_unsupported() {
    assert_eq!(postgres_dialect().sql_for(&begin()), ControlSql::Statement("BEGIN".to_owned()));
    assert_eq!(sqlite_dialect().sql_for(&begin_with("immediate")), ControlSql::Statement("BEGIN IMMEDIATE".to_owned()));
    assert_eq!(sqlite_dialect().sql_for(&begin_with("bogus")), ControlSql::Unsupported, "未声明的模式不下发");
    assert_eq!(postgres_dialect().sql_for(&ControlStatement::Commit { chain: true }), ControlSql::Statement("COMMIT AND CHAIN".to_owned()));
    assert_eq!(sqlite_dialect().sql_for(&ControlStatement::Commit { chain: true }), ControlSql::Unsupported);
    assert_eq!(postgres_dialect().sql_for(&savepoint("sp")), ControlSql::Statement("SAVEPOINT \"sp\"".to_owned()));
    assert_eq!(postgres_dialect().sql_for(&rollback_to("sp")), ControlSql::Statement("ROLLBACK TO SAVEPOINT \"sp\"".to_owned()));
    assert_eq!(postgres_dialect().sql_for(&release("sp")), ControlSql::Statement("RELEASE SAVEPOINT \"sp\"".to_owned()));
    assert_eq!(sqlserver_dialect().sql_for(&savepoint("sp")), ControlSql::Statement("SAVE TRANSACTION [sp]".to_owned()));
    assert_eq!(sqlserver_dialect().sql_for(&rollback_to("sp")), ControlSql::Statement("ROLLBACK TRANSACTION [sp]".to_owned()));
    assert_eq!(sqlserver_dialect().sql_for(&release("sp")), ControlSql::Unsupported);
    // 隔离/模式的选项之外不下发。
    assert_eq!(postgres_dialect().sql_for(&isolation(Some(READ_COMMITTED))), postgres_dialect().isolation_sql(Some(READ_COMMITTED)));
    assert_eq!(postgres_dialect().sql_for(&isolation(Some(SNAPSHOT))), ControlSql::Unsupported, "PostgreSQL 不提供快照隔离");
    assert_eq!(postgres_dialect().sql_for(&isolation(Some("bogus"))), ControlSql::Unsupported);
    assert_eq!(postgres_dialect().sql_for(&ControlStatement::SetMode { mode: Some("bogus".to_owned()) }), ControlSql::Unsupported);
    assert_eq!(sqlserver_dialect().sql_for(&ControlStatement::SetMode { mode: Some(READ_ONLY.to_owned()) }), ControlSql::Unsupported);
    // 没有显式事务控制的方言不生成任何控制 SQL。
    for statement in [begin(), commit(), rollback(), savepoint("sp"), rollback_to("sp"), isolation(Some(READ_COMMITTED))] {
        assert_eq!(unsupported_dialect().sql_for(&statement), ControlSql::Unsupported, "{statement:?}");
    }
}

#[test]
fn rollback_to_savepoint_keeps_the_outer_transaction_active() {
    let mut machine = started(postgres_dialect().capabilities());
    confirmed(&mut machine, &savepoint("a"));
    confirmed(&mut machine, &savepoint("b"));
    assert_eq!(machine.snapshot().savepoints.len(), 2);
    let generation = machine.snapshot().generation;

    assert_eq!(machine.acknowledge(&rollback_to("a"), ControlOutcome::Confirmed), Ok(TransactionEffect::StillActive));
    assert_eq!(machine.state(), TransactionState::Active, "回滚到保存点不结束外层事务");
    assert_eq!(machine.snapshot().savepoints.iter().map(SavepointName::as_str).collect::<Vec<_>>(), vec!["a"]);
    assert_eq!(machine.snapshot().generation, generation, "保存点回滚不改变事务代次");

    // 保存点本身保留，可以再次回滚到它。
    assert_eq!(machine.acknowledge(&rollback_to("a"), ControlOutcome::Confirmed), Ok(TransactionEffect::StillActive));
    assert_eq!(machine.acknowledge(&release("a"), ControlOutcome::Confirmed), Ok(TransactionEffect::StillActive));
    assert!(machine.snapshot().savepoints.is_empty());
    assert_eq!(machine.state(), TransactionState::Active);

    assert_eq!(confirmed(&mut machine, &commit()), TransactionEffect::Committed);
    assert_eq!(machine.state(), TransactionState::Idle);
}

#[test]
fn rolling_back_to_an_unknown_savepoint_reports_failure_without_changes() {
    let mut machine = started(postgres_dialect().capabilities());
    confirmed(&mut machine, &savepoint("a"));
    let before = machine.snapshot().clone();
    assert_eq!(machine.acknowledge(&rollback_to("missing"), ControlOutcome::Rejected), Err(TransactionError::UnknownSavepoint("missing".to_owned())));
    assert_eq!(machine.precheck(&rollback_to("missing")), Err(TransactionError::UnknownSavepoint("missing".to_owned())));
    assert_eq!(machine.snapshot(), &before, "不伪造部分回滚结果");
    assert_eq!(machine.acknowledge(&rollback_to("a"), ControlOutcome::Rejected), Err(TransactionError::ControlRejected));
    assert_eq!(machine.snapshot(), &before);

    // 数据库确认了一个没有登记的保存点：保存点位置不可知。
    assert_eq!(machine.acknowledge(&rollback_to("ghost"), ControlOutcome::Confirmed), Ok(TransactionEffect::Unknown));
    assert_eq!(machine.state(), TransactionState::Unknown);
}

#[test]
fn savepoints_require_an_active_transaction() {
    let mut machine = postgres_machine();
    let before = machine.snapshot().clone();
    assert_eq!(machine.precheck(&savepoint("sp")), Err(TransactionError::NoActiveTransaction));
    assert_eq!(machine.acknowledge(&savepoint("sp"), ControlOutcome::Confirmed), Err(TransactionError::NoActiveTransaction));
    assert_eq!(machine.snapshot(), &before);
    assert_eq!(machine.acknowledge(&rollback_to("sp"), ControlOutcome::Confirmed), Err(TransactionError::NoActiveTransaction));
    assert_eq!(machine.acknowledge(&release("sp"), ControlOutcome::Confirmed), Err(TransactionError::NoActiveTransaction));
    assert_eq!(machine.snapshot(), &before);
}

#[test]
fn dialects_without_savepoints_refuse_every_savepoint_operation() {
    let mut machine = machine(caps_without_savepoints());
    confirmed(&mut machine, &begin());
    assert_eq!(machine.precheck(&savepoint("sp")), Err(TransactionError::SavepointsUnsupported));
    assert_eq!(machine.acknowledge(&savepoint("sp"), ControlOutcome::Confirmed), Err(TransactionError::SavepointsUnsupported));
    assert_eq!(machine.acknowledge(&rollback_to("sp"), ControlOutcome::Confirmed), Err(TransactionError::SavepointsUnsupported));
    assert_eq!(machine.acknowledge(&release("sp"), ControlOutcome::Confirmed), Err(TransactionError::SavepointsUnsupported));
    assert_eq!(machine.state(), TransactionState::Active);
}

#[test]
fn dialects_without_explicit_control_refuse_transaction_control() {
    let mut machine = machine(TransactionCapabilities::UNSUPPORTED);
    assert_eq!(machine.precheck(&begin()), Err(TransactionError::TransactionsUnsupported));
    assert_eq!(machine.acknowledge(&begin(), ControlOutcome::Confirmed), Err(TransactionError::TransactionsUnsupported));
    assert_eq!(machine.state(), TransactionState::Idle, "不支持事务的方言不会进入活动状态");
    assert_eq!(unsupported_dialect().sql_for(&begin()), ControlSql::Unsupported);
    assert_eq!(unsupported_dialect().sql_for(&commit()), ControlSql::Unsupported);
    assert_eq!(unsupported_dialect().sql_for(&rollback()), ControlSql::Unsupported);
}

#[test]
fn releasing_a_savepoint_needs_the_capability() {
    let mut machine = machine(sqlserver_dialect().capabilities());
    confirmed(&mut machine, &begin());
    confirmed(&mut machine, &savepoint("sp"));
    assert_eq!(machine.precheck(&release("sp")), Err(TransactionError::ReleaseSavepointUnsupported));
    assert_eq!(machine.acknowledge(&release("sp"), ControlOutcome::Confirmed), Err(TransactionError::ReleaseSavepointUnsupported));
    assert_eq!(machine.snapshot().savepoints.iter().map(SavepointName::as_str).collect::<Vec<_>>(), vec!["sp"]);
    assert_eq!(machine.state(), TransactionState::Active);
}

#[test]
fn statement_failure_marks_aborted_and_savepoint_recovery_resumes() {
    let mut machine = started(postgres_dialect().capabilities());
    confirmed(&mut machine, &savepoint("keep"));
    assert_eq!(machine.observe_statement(StatementOutcome::Failed(StatementFailure::TransactionAborted)), TransactionEffect::Aborted);
    assert_eq!(machine.state(), TransactionState::Aborted);
    assert!(!machine.state().allows_statements(), "中止事务不能继续执行普通语句");
    assert!(machine.state().needs_recovery());
    assert_eq!(machine.snapshot().savepoints.len(), 1, "中止状态不丢弃保存点");

    // 回滚到保存点后恢复使用。
    assert_eq!(machine.precheck(&rollback_to("keep")), Ok(()));
    assert_eq!(machine.acknowledge(&rollback_to("keep"), ControlOutcome::Confirmed), Ok(TransactionEffect::StillActive));
    assert_eq!(machine.state(), TransactionState::Active);
    assert_eq!(machine.observe_statement(StatementOutcome::Succeeded), TransactionEffect::StillActive);

    // 普通失败不会自动回滚事务。
    assert_eq!(machine.observe_statement(StatementOutcome::Failed(StatementFailure::TransactionUsable)), TransactionEffect::StillActive);
    assert_eq!(machine.state(), TransactionState::Active);
}

#[test]
fn aborted_recovery_is_limited_to_the_dialect_capability() {
    // PostgreSQL：可以回滚到保存点恢复。
    let mut postgres = started(postgres_dialect().capabilities());
    confirmed(&mut postgres, &savepoint("keep"));
    postgres.observe_statement(StatementOutcome::Failed(StatementFailure::TransactionAborted));
    assert_eq!(postgres.precheck(&rollback_to("keep")), Ok(()));
    assert_eq!(postgres.acknowledge(&rollback_to("keep"), ControlOutcome::Confirmed), Ok(TransactionEffect::StillActive));
    assert_eq!(postgres.state(), TransactionState::Active);

    // SQL Server：uncommittable，只能整体回滚；保存点恢复入口不可用。
    let mut sqlserver = started(sqlserver_dialect().capabilities());
    confirmed(&mut sqlserver, &savepoint("keep"));
    sqlserver.observe_statement(StatementOutcome::Failed(StatementFailure::TransactionAborted));
    assert_eq!(sqlserver.precheck(&rollback_to("keep")), Err(TransactionError::RecoveryRequiresFullRollback));
    assert_eq!(sqlserver.state(), TransactionState::Aborted, "预检不改变状态");
    assert_eq!(sqlserver.acknowledge(&rollback(), ControlOutcome::Confirmed), Ok(TransactionEffect::RolledBack));
    assert_eq!(sqlserver.state(), TransactionState::Idle);
}

#[test]
fn a_successful_statement_in_an_aborted_transaction_is_not_trusted() {
    let mut machine = started(postgres_dialect().capabilities());
    assert_eq!(machine.observe_statement(StatementOutcome::Failed(StatementFailure::TransactionAborted)), TransactionEffect::Aborted);
    assert_eq!(machine.observe_statement(StatementOutcome::Succeeded), TransactionEffect::Unknown);
    assert_eq!(machine.state(), TransactionState::Unknown, "与数据库规则矛盾的观测不能推出可用状态");
}

#[test]
fn aborted_commit_reports_rollback_and_never_fakes_success() {
    let mut machine = started(postgres_dialect().capabilities());
    machine.observe_statement(StatementOutcome::Failed(StatementFailure::TransactionAborted));
    // PostgreSQL 中止事务里的 COMMIT 实际执行成 ROLLBACK。
    assert_eq!(machine.acknowledge(&commit(), ControlOutcome::RolledBack), Ok(TransactionEffect::RolledBack));
    assert_eq!(machine.state(), TransactionState::Idle);

    let mut contradicted = started(postgres_dialect().capabilities());
    contradicted.observe_statement(StatementOutcome::Failed(StatementFailure::TransactionAborted));
    assert_eq!(contradicted.acknowledge(&commit(), ControlOutcome::Confirmed), Ok(TransactionEffect::Unknown));
    assert_eq!(contradicted.state(), TransactionState::Unknown);
}

#[test]
fn unconfirmed_commit_becomes_unknown_and_new_transactions_do_not_clear_the_old_record() {
    let mut machine = started(mysql_dialect().capabilities());
    let generation = machine.snapshot().generation;
    confirmed(&mut machine, &savepoint("sp"));
    assert_eq!(
        machine.acknowledge(&commit(), ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed)),
        Ok(TransactionEffect::Unknown)
    );
    assert_eq!(machine.state(), TransactionState::Unknown);
    let unresolved = machine.snapshot().unresolved.clone().expect("记录旧事务");
    assert_eq!(unresolved.reason, UnknownReason::ControlUnconfirmed);
    assert_eq!(unresolved.state, TransactionState::Active);
    assert_eq!(unresolved.generation, generation);
    assert!(!machine.state().allows_statements());
    assert!(!TransactionEffect::Unknown.ends_transaction(), "未知不能显示为已提交或已回滚");

    // 后续语句无法据此推进。
    assert_eq!(machine.observe_statement(StatementOutcome::Succeeded), TransactionEffect::Unknown);
    assert_eq!(machine.state(), TransactionState::Unknown);

    // 重连形成新会话：从 Idle 开始，旧事务信息保留。
    assert_eq!(machine.reconnect(UnknownReason::ConnectionLost), TransactionEffect::Unknown);
    assert_eq!(machine.state(), TransactionState::Idle);
    assert!(machine.snapshot().savepoints.is_empty());
    assert_eq!(machine.snapshot().settings.isolation, None);
    assert_eq!(machine.snapshot().depth, Some(0));
    let unresolved = machine.snapshot().unresolved.clone().expect("重连后仍保留旧事务未知信息");
    assert_eq!(unresolved.state, TransactionState::Active);
    assert_eq!(unresolved.generation, generation);

    // 新事务的提交不得清除旧事务的未知信息（R10）。
    confirmed(&mut machine, &begin());
    assert_eq!(confirmed(&mut machine, &commit()), TransactionEffect::Committed);
    assert_eq!(machine.state(), TransactionState::Idle);
    let unresolved = machine.snapshot().unresolved.clone().expect("新事务结束不清除旧未知");
    assert_eq!(unresolved.generation, generation);

    // 只有独立核对（用户确认或另一条连接的结果）能关闭未知记录。
    let resolved = machine.resolve_unknown(UnknownResolution::Committed).expect("核对旧事务");
    assert_eq!(resolved.reason, UnknownReason::ControlUnconfirmed);
    assert_eq!(resolved.state, TransactionState::Active);
    assert!(machine.snapshot().unresolved.is_none());
    assert_eq!(machine.state(), TransactionState::Idle, "核对不改变已确认的会话状态");
    assert_eq!(machine.snapshot().last_effect, TransactionEffect::Committed);
    assert!(machine.resolve_unknown(UnknownResolution::RolledBack).is_none(), "没有未知记录时不做任何事");
}

#[test]
fn confirming_the_unknown_transactions_outcome_clears_it() {
    let mut machine = started(postgres_dialect().capabilities());
    assert_eq!(
        machine.acknowledge(&rollback(), ControlOutcome::Unconfirmed(UnknownReason::ConnectionLost)),
        Ok(TransactionEffect::Unknown)
    );
    assert_eq!(machine.state(), TransactionState::Unknown);
    assert!(machine.snapshot().unresolved.is_some());
    // 未知结果仍允许明确的回滚/提交核对路径。
    assert_eq!(machine.precheck(&rollback()), Ok(()));
    // 再次回滚并得到确认：这是对当前未知事务结局的直接观测。
    assert_eq!(machine.acknowledge(&rollback(), ControlOutcome::Confirmed), Ok(TransactionEffect::RolledBack));
    assert_eq!(machine.state(), TransactionState::Idle);
    assert!(machine.snapshot().unresolved.is_none());
}

#[test]
fn unrecognized_effect_never_advances_to_success() {
    let mut machine = started(mysql_dialect().capabilities());
    assert_eq!(machine.acknowledge(&commit(), ControlOutcome::Unrecognized), Ok(TransactionEffect::Unknown));
    assert_eq!(machine.state(), TransactionState::Unknown);
    assert_eq!(machine.snapshot().unresolved.as_ref().map(|unresolved| unresolved.reason), Some(UnknownReason::UnrecognizedEffect));

    let mut savepoint_machine = started(mysql_dialect().capabilities());
    assert_eq!(savepoint_machine.acknowledge(&savepoint("sp"), ControlOutcome::Unrecognized), Ok(TransactionEffect::Unknown));
    assert_eq!(savepoint_machine.state(), TransactionState::Unknown);
    assert!(savepoint_machine.snapshot().savepoints.is_empty(), "未知效果不登记保存点");
}

#[test]
fn reconnect_without_a_transaction_reports_no_loss() {
    let mut machine = postgres_machine();
    let generation = machine.snapshot().generation;
    assert_eq!(machine.reconnect(UnknownReason::ConnectionLost), TransactionEffect::Unchanged);
    assert_eq!(machine.state(), TransactionState::Idle);
    assert_eq!(machine.snapshot().generation, generation, "没有事务时重连不推进代次");
    assert!(machine.snapshot().unresolved.is_none());
}

#[test]
fn losing_the_connection_during_a_statement_marks_the_transaction_unknown() {
    let mut machine = started(postgres_dialect().capabilities());
    assert_eq!(machine.observe_statement(StatementOutcome::Failed(StatementFailure::ConnectionLost)), TransactionEffect::Unknown);
    assert_eq!(machine.state(), TransactionState::Unknown);
    assert_eq!(machine.snapshot().unresolved.as_ref().map(|unresolved| unresolved.reason), Some(UnknownReason::ConnectionLost));
}

#[test]
fn probe_observation_reports_lost_outcome_instead_of_guessing() {
    // 未知事务被探针确认为 Idle：不推断提交或回滚，保留未知记录。
    let mut machine = started(postgres_dialect().capabilities());
    machine.acknowledge(&commit(), ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed)).unwrap();
    let generation = machine.snapshot().generation;
    assert_eq!(machine.observe_probe(TransactionProbe::Idle), TransactionEffect::Unknown);
    assert_eq!(machine.state(), TransactionState::Idle);
    assert_eq!(machine.snapshot().generation, generation + 1);
    assert_eq!(machine.snapshot().depth, Some(0));
    assert!(machine.snapshot().unresolved.is_some(), "终点未知的结果继续保留");

    // 探针确认事务仍活动：采纳观测，旧结果仍未知。
    let mut machine = started(postgres_dialect().capabilities());
    machine.observe_statement(StatementOutcome::Failed(StatementFailure::ConnectionLost));
    assert_eq!(machine.state(), TransactionState::Unknown);
    assert_eq!(machine.observe_probe(TransactionProbe::Active { depth: 1 }), TransactionEffect::StillActive);
    assert_eq!(machine.state(), TransactionState::Active);
    assert!(machine.snapshot().unresolved.is_some());

    // 探针确认中止。
    let mut machine = started(postgres_dialect().capabilities());
    assert_eq!(machine.observe_probe(TransactionProbe::Aborted), TransactionEffect::Aborted);
    assert_eq!(machine.state(), TransactionState::Aborted);

    // 没有事务却报告中止：观测自相矛盾，不能据此推出可用状态。
    let mut machine = postgres_machine();
    assert_eq!(machine.observe_probe(TransactionProbe::Aborted), TransactionEffect::Unknown);
    assert_eq!(machine.state(), TransactionState::Unknown);

    // SQL Server 的探针层数被登记，提交只减少层数。
    let mut machine = started(sqlserver_dialect().capabilities());
    assert_eq!(machine.observe_probe(TransactionProbe::Active { depth: 2 }), TransactionEffect::StillActive);
    assert_eq!(machine.snapshot().depth, Some(2));
    assert_eq!(confirmed(&mut machine, &commit()), TransactionEffect::StillActive);
    assert_eq!(machine.snapshot().depth, Some(1));
}

#[test]
fn generation_and_unresolved_follow_the_documented_rules() {
    // 开始/结束推进代次；保存点与中止不推进。
    let mut machine = started(postgres_dialect().capabilities());
    let after_start = machine.snapshot().generation;
    confirmed(&mut machine, &savepoint("sp"));
    assert_eq!(machine.snapshot().generation, after_start);
    machine.observe_statement(StatementOutcome::Failed(StatementFailure::TransactionAborted));
    assert_eq!(machine.state(), TransactionState::Aborted);
    assert_eq!(machine.snapshot().generation, after_start, "中止不是新事务");
    confirmed(&mut machine, &rollback());
    assert_eq!(machine.snapshot().generation, after_start + 1);

    // Idle 下 BEGIN 结果未确认：记录“无法确认是否已经开始了事务”，不宣称活动。
    let mut idle = postgres_machine();
    assert_eq!(idle.acknowledge(&begin(), ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed)), Ok(TransactionEffect::Unknown));
    assert_eq!(idle.state(), TransactionState::Unknown);
    let unresolved = idle.snapshot().unresolved.as_ref().expect("state == Unknown 必须有未知记录");
    assert_eq!(unresolved.state, TransactionState::Idle);
    assert_eq!(unresolved.reason, UnknownReason::ControlUnconfirmed);

    // 未知状态不能被普通语句或保存点操作推进。
    assert_eq!(idle.precheck(&savepoint("sp")), Err(TransactionError::UnknownOutcome));
    assert_eq!(idle.observe_statement(StatementOutcome::Succeeded), TransactionEffect::Unknown);
    assert_eq!(idle.state(), TransactionState::Unknown);
}

#[test]
fn idle_statements_report_no_transaction_effect() {
    let mut machine = postgres_machine();
    assert_eq!(machine.observe_statement(StatementOutcome::Succeeded), TransactionEffect::Unchanged);
    assert_eq!(machine.observe_statement(StatementOutcome::Failed(StatementFailure::TransactionUsable)), TransactionEffect::Unchanged);
    assert_eq!(machine.observe_statement(StatementOutcome::Failed(StatementFailure::ConnectionLost)), TransactionEffect::Unchanged);
    assert_eq!(machine.state(), TransactionState::Idle, "自动提交下失败不会产生需要确认的事务");
}

#[test]
fn isolation_is_limited_to_the_dialect_levels() {
    let mut machine = postgres_machine();
    assert_eq!(machine.acknowledge(&isolation(Some(SNAPSHOT)), ControlOutcome::Confirmed), Err(TransactionError::IsolationUnsupported(SNAPSHOT.to_owned())));
    assert_eq!(machine.snapshot().settings.isolation, None, "不支持的级别不静默替换");

    assert_eq!(machine.precheck(&isolation(Some(READ_COMMITTED))), Ok(()));
    assert_eq!(machine.acknowledge(&isolation(Some(READ_COMMITTED)), ControlOutcome::Confirmed), Ok(TransactionEffect::Unchanged));
    assert_eq!(machine.snapshot().settings.isolation.as_deref(), Some(READ_COMMITTED));

    assert_eq!(machine.acknowledge(&isolation(None), ControlOutcome::Confirmed), Ok(TransactionEffect::Unchanged));
    assert_eq!(machine.snapshot().settings.isolation, None, "恢复服务器默认");

    // 方言没有这类设置时明确拒绝。
    let mut bare = TransactionMachine::new("conn-1", caps_without_settings());
    assert_eq!(bare.precheck(&isolation(Some(READ_COMMITTED))), Err(TransactionError::IsolationUnsupported(READ_COMMITTED.to_owned())));
    assert_eq!(bare.acknowledge(&isolation(Some(READ_COMMITTED)), ControlOutcome::Confirmed), Err(TransactionError::IsolationUnsupported(READ_COMMITTED.to_owned())));
    // 完全没有显式事务控制的方言：任何控制语句都按“不支持事务”拒绝。
    let mut unsupported = TransactionMachine::new("conn-1", TransactionCapabilities::UNSUPPORTED);
    assert_eq!(unsupported.acknowledge(&isolation(Some(READ_COMMITTED)), ControlOutcome::Confirmed), Err(TransactionError::TransactionsUnsupported));
    assert_eq!(unsupported.state(), TransactionState::Idle);
}

#[test]
fn changing_isolation_inside_a_transaction_is_refused_without_committing() {
    let mut machine = started(postgres_dialect().capabilities());
    let before = machine.snapshot().clone();
    assert_eq!(machine.precheck(&isolation(Some(SERIALIZABLE))), Err(TransactionError::SettingLocked("隔离级别")));
    assert_eq!(machine.acknowledge(&isolation(Some(SERIALIZABLE)), ControlOutcome::Confirmed), Err(TransactionError::SettingLocked("隔离级别")));
    assert_eq!(machine.snapshot(), &before, "设置失败不提交、不结束原事务");
    assert_eq!(machine.state(), TransactionState::Active);

    // 允许事务中调整的方言仍然保持事务活动。
    let mut anytime = started(sqlserver_dialect().capabilities());
    assert_eq!(anytime.acknowledge(&isolation(Some(SNAPSHOT)), ControlOutcome::Confirmed), Ok(TransactionEffect::StillActive));
    assert_eq!(anytime.snapshot().settings.isolation.as_deref(), Some(SNAPSHOT));
    assert_eq!(anytime.state(), TransactionState::Active);
}

#[test]
fn unconfirmed_or_rejected_isolation_is_not_shown_as_applied() {
    let mut machine = mysql_machine();
    assert_eq!(
        machine.acknowledge(&isolation(Some(READ_COMMITTED)), ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed)),
        Err(TransactionError::ControlUnconfirmed)
    );
    assert_eq!(machine.snapshot().settings.isolation, None);
    assert_eq!(machine.state(), TransactionState::Idle);

    assert_eq!(machine.acknowledge(&isolation(Some(READ_COMMITTED)), ControlOutcome::Rejected), Err(TransactionError::ControlRejected));
    assert_eq!(machine.snapshot().settings.isolation, None);
}

#[test]
fn transaction_mode_is_limited_to_the_dialect_modes() {
    let mut sqlite = machine(sqlite_dialect().capabilities());
    assert_eq!(sqlite.precheck(&begin_with("bogus")), Err(TransactionError::ModeUnsupported("bogus".to_owned())));
    assert_eq!(sqlite.acknowledge(&begin_with("bogus"), ControlOutcome::Confirmed), Err(TransactionError::ModeUnsupported("bogus".to_owned())));
    assert_eq!(sqlite.state(), TransactionState::Idle);

    assert_eq!(confirmed(&mut sqlite, &begin_with("immediate")), TransactionEffect::Began);
    assert_eq!(sqlite.snapshot().settings.mode.as_deref(), Some("immediate"));

    let mut postgres = postgres_machine();
    assert_eq!(postgres.acknowledge(&ControlStatement::SetMode { mode: Some(READ_ONLY.to_owned()) }, ControlOutcome::Confirmed), Ok(TransactionEffect::Unchanged));
    assert_eq!(postgres.snapshot().settings.mode.as_deref(), Some(READ_ONLY));
}

#[test]
fn implicit_commit_ends_the_transaction_and_clears_savepoints() {
    let mut machine = mysql_machine();
    confirmed(&mut machine, &begin());
    confirmed(&mut machine, &savepoint("sp"));
    assert_eq!(machine.observe_implicit_commit(), Ok(TransactionEffect::ImplicitCommit));
    assert_eq!(machine.state(), TransactionState::Idle);
    assert!(machine.snapshot().savepoints.is_empty(), "隐式提交后保存点不再存在");
    assert!(machine.snapshot().last_effect.ends_transaction());

    // 自动提交下没有可被隐式提交的事务，不报告隐式提交。
    assert_eq!(machine.observe_implicit_commit(), Ok(TransactionEffect::Unchanged));
    assert_eq!(machine.state(), TransactionState::Idle);

    // 旧事务的未知记录不因新事务的隐式提交消失。
    let mut machine = mysql_machine();
    machine.acknowledge(&commit(), ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed)).unwrap();
    machine.reconnect(UnknownReason::ConnectionLost);
    confirmed(&mut machine, &begin());
    confirmed(&mut machine, &savepoint("sp"));
    assert_eq!(machine.observe_implicit_commit(), Ok(TransactionEffect::ImplicitCommit));
    assert!(machine.snapshot().unresolved.is_some(), "旧未知只由核对关闭");
}

#[test]
fn implicit_commit_is_refused_where_the_dialect_has_none() {
    let mut machine = started(postgres_dialect().capabilities());
    let before = machine.snapshot().clone();
    assert_eq!(machine.observe_implicit_commit(), Err(TransactionError::ImplicitCommitUnsupported));
    assert_eq!(machine.snapshot(), &before);
    assert_eq!(machine.state(), TransactionState::Active);
}

#[test]
fn chained_transactions_stay_active_only_where_supported() {
    let mut machine = started(postgres_dialect().capabilities());
    confirmed(&mut machine, &savepoint("sp"));
    assert_eq!(confirmed(&mut machine, &ControlStatement::Commit { chain: true }), TransactionEffect::Committed);
    assert_eq!(machine.state(), TransactionState::Active, "AND CHAIN 立即开始新事务");
    assert!(machine.snapshot().savepoints.is_empty());
    assert_eq!(machine.snapshot().generation, 2, "提交加链式开始只推进一次代次");
    assert_eq!(machine.snapshot().depth, Some(1));

    let mut sqlite = started(sqlite_dialect().capabilities());
    let before = sqlite.snapshot().clone();
    assert_eq!(
        sqlite.acknowledge(&ControlStatement::Rollback { chain: true }, ControlOutcome::Confirmed),
        Err(TransactionError::ChainedTransactionUnsupported)
    );
    assert_eq!(sqlite.snapshot(), &before);
}

#[test]
fn dialect_controls_expose_capability_boundaries() {
    assert_eq!(sqlite_dialect().begin_sql(Some("immediate")), "BEGIN IMMEDIATE");
    assert_eq!(postgres_dialect().begin_sql(None), "BEGIN");
    assert_eq!(postgres_dialect().commit_sql(true), "COMMIT AND CHAIN");
    assert_eq!(postgres_dialect().isolation_sql(None), ControlSql::Statement("SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL DEFAULT".to_owned()));
    assert_eq!(
        mysql_dialect().isolation_sql(Some(READ_COMMITTED)),
        ControlSql::Statement("SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL READ COMMITTED".to_owned())
    );
    assert_eq!(mysql_dialect().mode_sql(Some(READ_ONLY)), ControlSql::Statement("SET TRANSACTION READ ONLY".to_owned()));
    assert_eq!(sqlite_dialect().mode_sql(Some("bogus")), ControlSql::Unsupported);

    // 保存点、释放、显式控制与重复开始按能力表回答。
    assert!(postgres_dialect().capabilities().savepoints);
    assert!(!sqlserver_dialect().capabilities().release_savepoint);
    assert!(postgres_dialect().capabilities().explicit_control);
    assert!(!unsupported_dialect().capabilities().explicit_control);
    assert_eq!(postgres_dialect().capabilities().duplicate_begin, DuplicateBegin::Ignored);
    assert_eq!(mysql_dialect().capabilities().duplicate_begin, DuplicateBegin::EndsPrevious);
    assert_eq!(sqlite_dialect().capabilities().duplicate_begin, DuplicateBegin::Rejected);
    assert_eq!(sqlserver_dialect().capabilities().duplicate_begin, DuplicateBegin::Nests);

    // 中止后的恢复能力是显式声明，不让所有引擎共享同一种“中止”。
    assert_eq!(postgres_dialect().capabilities().aborted, AbortedRecovery::SavepointRecovery);
    assert_eq!(sqlserver_dialect().capabilities().aborted, AbortedRecovery::RollbackOnly);
    assert_eq!(mysql_dialect().capabilities().aborted, AbortedRecovery::NotApplicable);

    // 隐式提交只有 MySQL/MariaDB 家族；其余方言必须回答“不适用”。
    assert_eq!(postgres_dialect().capabilities().implicit_commit, ImplicitCommitSupport::NotApplicable);
    assert_eq!(mysql_dialect().capabilities().implicit_commit, ImplicitCommitSupport::MysqlFamily);
    assert_eq!(postgres_dialect().implicit_commit("CREATE TABLE t (id INT)"), ImplicitCommitVerdict::NotApplicable);
    assert_eq!(mysql_dialect().implicit_commit("CREATE TABLE t (id INT)"), ImplicitCommitVerdict::Uncertain, "未由适配器分类前不宣称发生隐式提交");
    assert!(!sqlite_dialect().capabilities().chained_transactions);
    assert_eq!(unsupported_dialect().capabilities().isolation.change, SettingChange::Unsupported);
}

#[test]
fn classify_control_with_standard_syntax_keeps_unknown_forms_ambiguous() {
    let syntax = ControlSyntax::STANDARD;
    assert_eq!(classify_control("BEGIN", syntax), control(begin()));
    assert_eq!(classify_control("BEGIN WORK", syntax), control(begin()));
    assert_eq!(classify_control("COMMIT AND CHAIN", syntax), ControlMatch::Ambiguous, "标准词汇不含链式事务");
    assert_eq!(classify_control("SAVEPOINT", syntax), ControlMatch::Ambiguous);
    assert_eq!(classify_control("ROLLBACK TO", syntax), ControlMatch::Ambiguous);
    assert_eq!(classify_control("", syntax), ControlMatch::Ordinary);
    assert_eq!(classify_control("INSERT INTO t VALUES (1)", syntax), ControlMatch::Ordinary);
}
