//! SQLite 的事务方言：原生控制、三种开始模式、保存点与自动回滚错误。（ADR-2、ADR-3）
//!
//! SQLite 没有通用的隔离级别集合，也没有 SQL 层可读的“是否在事务中”探针：这里只暴露原生的事务开始
//! 模式（`DEFERRED` / `IMMEDIATE` / `EXCLUSIVE`）和连接级的 `read_uncommitted` 设置，不把它们伪装成
//! 四种标准隔离级别。部分错误（磁盘满、I/O 错误、内存不足、忙）可能由数据库自动回滚整个事务，
//! 因此按“可能已结束事务”报告，交由调用方登记未知。

use crate::transaction::{
    AbortedRecovery, ControlSql, ControlSyntax, DuplicateBegin, ImplicitCommitSupport,
    ImplicitCommitVerdict, SavepointName, SettingChange, SettingOption, SettingSupport,
    StatementFailure, TransactionCapabilities, TransactionDialect, READ_UNCOMMITTED,
};

const DEFERRED_OPTION: SettingOption = SettingOption {
    id: "deferred",
    label: "Deferred",
};
const IMMEDIATE_OPTION: SettingOption = SettingOption {
    id: "immediate",
    label: "Immediate",
};
const EXCLUSIVE_OPTION: SettingOption = SettingOption {
    id: "exclusive",
    label: "Exclusive",
};
const READ_UNCOMMITTED_OPTION: SettingOption = SettingOption {
    id: READ_UNCOMMITTED,
    label: "Read uncommitted",
};

const MODE_OPTIONS: &[SettingOption] = &[DEFERRED_OPTION, IMMEDIATE_OPTION, EXCLUSIVE_OPTION];
const ISOLATION_OPTIONS: &[SettingOption] = &[READ_UNCOMMITTED_OPTION];

/// SQLite 的手写事务控制词汇：`BEGIN [DEFERRED|IMMEDIATE|EXCLUSIVE]`、`END` 等于提交；
/// 不认 `START TRANSACTION`，也不支持 `AND CHAIN`。
const SQLITE_SYNTAX: ControlSyntax = ControlSyntax {
    bare_begin: true,
    start_transaction: false,
    end_commits: true,
    abort_rolls_back: false,
    chained: false,
    begin_modes: &["deferred", "immediate", "exclusive"],
    rollback_transaction_name: false,
    save_transaction: false,
    hash_comments: false,
    identifier_quotes: &[('"', '"'), ('`', '`'), ('[', ']')],
};

pub(crate) struct SqliteTransaction;

pub(crate) static SQLITE_TRANSACTION: SqliteTransaction = SqliteTransaction;

impl TransactionDialect for SqliteTransaction {
    fn capabilities(&self) -> TransactionCapabilities {
        TransactionCapabilities {
            explicit_control: true,
            savepoints: true,
            release_savepoint: true,
            chained_transactions: false,
            // 在事务中再次 BEGIN 会被明确拒绝。
            duplicate_begin: DuplicateBegin::Rejected,
            aborted: AbortedRecovery::NotApplicable,
            // 只表达 SQLite 自己的 read_uncommitted 设置，不伪装成四种标准隔离级别。
            isolation: SettingSupport {
                options: ISOLATION_OPTIONS,
                change: SettingChange::WhileIdle,
            },
            mode: SettingSupport {
                options: MODE_OPTIONS,
                change: SettingChange::WhileIdle,
            },
            implicit_commit: ImplicitCommitSupport::NotApplicable,
        }
    }

    fn syntax(&self) -> ControlSyntax {
        SQLITE_SYNTAX
    }

    fn quote_identifier(&self, name: &str) -> String {
        format!("\"{}\"", name.replace('"', "\"\""))
    }

    fn begin_sql(&self, mode: Option<&str>) -> String {
        match mode {
            Some(mode) if mode.eq_ignore_ascii_case("immediate") => "BEGIN IMMEDIATE".to_owned(),
            Some(mode) if mode.eq_ignore_ascii_case("exclusive") => "BEGIN EXCLUSIVE".to_owned(),
            _ => "BEGIN".to_owned(),
        }
    }

    fn commit_sql(&self, _chain: bool) -> String {
        "COMMIT".to_owned()
    }

    fn rollback_sql(&self, _chain: bool) -> String {
        "ROLLBACK".to_owned()
    }

    fn savepoint_sql(&self, name: &SavepointName) -> String {
        format!("SAVEPOINT {}", self.quote_identifier(name.as_str()))
    }

    fn rollback_to_savepoint_sql(&self, name: &SavepointName) -> String {
        format!(
            "ROLLBACK TO SAVEPOINT {}",
            self.quote_identifier(name.as_str())
        )
    }

    fn isolation_sql(&self, level: Option<&str>) -> ControlSql {
        match level {
            None => ControlSql::Statement("PRAGMA read_uncommitted = 0".to_owned()),
            Some(READ_UNCOMMITTED) => {
                ControlSql::Statement("PRAGMA read_uncommitted = 1".to_owned())
            }
            Some(_) => ControlSql::Unsupported,
        }
    }

    /// SQLite 没有会话级的默认事务模式：模式随下一次 `BEGIN` 下发，所以这里不发送任何语句。
    fn mode_sql(&self, mode: Option<&str>) -> ControlSql {
        match mode {
            None => ControlSql::NoStatement,
            Some(mode) if MODE_OPTIONS.iter().any(|option| option.id == mode) => {
                ControlSql::NoStatement
            }
            Some(_) => ControlSql::Unsupported,
        }
    }

    fn implicit_commit(&self, _body: &str) -> ImplicitCommitVerdict {
        ImplicitCommitVerdict::NotApplicable
    }

    /// SQLite 的语句错误通常是语句级回滚；连接类错误在本地文件上不存在。
    fn statement_failure(&self, _code: Option<&str>, _message: &str) -> StatementFailure {
        StatementFailure::TransactionUsable
    }

    /// 磁盘满、I/O 错误、内存不足与忙等错误可能让 SQLite 自动回滚整个事务（R8、R10）。
    fn error_may_have_ended_transaction(&self, code: Option<&str>, message: &str) -> bool {
        if matches!(code, Some("5" | "7" | "10" | "13")) {
            return true;
        }
        let lower = message.to_ascii_lowercase();
        lower.contains("database or disk is full")
            || lower.contains("disk i/o error")
            || lower.contains("out of memory")
            || lower.contains("database is locked")
            || lower.contains("database is busy")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::{
        classify_control, ControlMatch, ControlStatement, TransactionDialect, TransactionError,
    };

    #[test]
    fn control_sql_uses_sqlite_native_forms() {
        let dialect = SqliteTransaction;
        assert_eq!(dialect.begin_sql(None), "BEGIN");
        assert_eq!(dialect.begin_sql(Some("immediate")), "BEGIN IMMEDIATE");
        assert_eq!(dialect.begin_sql(Some("exclusive")), "BEGIN EXCLUSIVE");
        assert_eq!(dialect.commit_sql(false), "COMMIT");
        let name = SavepointName::parse("sp").unwrap();
        assert_eq!(dialect.savepoint_sql(&name), "SAVEPOINT \"sp\"");
        assert_eq!(
            dialect.rollback_to_savepoint_sql(&name),
            "ROLLBACK TO SAVEPOINT \"sp\""
        );
        assert_eq!(
            dialect.isolation_sql(Some(READ_UNCOMMITTED)),
            ControlSql::Statement("PRAGMA read_uncommitted = 1".to_owned())
        );
        assert_eq!(
            dialect.isolation_sql(Some("serializable")),
            ControlSql::Unsupported,
            "SQLite 不伪装标准隔离级别"
        );
        assert_eq!(
            dialect.mode_sql(Some("immediate")),
            ControlSql::NoStatement,
            "模式随 BEGIN 下发"
        );
    }

    #[test]
    fn handwritten_begin_modes_are_recognized() {
        let dialect = SqliteTransaction;
        assert_eq!(
            classify_control("BEGIN IMMEDIATE", dialect.syntax()),
            ControlMatch::Control(ControlStatement::Begin {
                mode: Some("immediate".to_owned())
            })
        );
        assert_eq!(
            classify_control("END TRANSACTION", dialect.syntax()),
            ControlMatch::Control(ControlStatement::Commit { chain: false })
        );
        assert_eq!(
            classify_control("START TRANSACTION", dialect.syntax()),
            ControlMatch::Ordinary,
            "SQLite 不认 START TRANSACTION"
        );
        assert_eq!(
            classify_control("RELEASE sp", dialect.syntax()),
            ControlMatch::Control(ControlStatement::ReleaseSavepoint(
                SavepointName::parse("sp").unwrap()
            ))
        );
    }

    #[test]
    fn duplicate_begin_is_rejected_and_auto_rollback_errors_are_flagged() {
        let dialect = SqliteTransaction;
        assert_eq!(
            dialect.capabilities().duplicate_begin,
            DuplicateBegin::Rejected
        );
        assert_eq!(
            dialect.capabilities().aborted,
            AbortedRecovery::NotApplicable
        );
        assert!(
            dialect.probe_sql().is_none(),
            "SQLite 没有 SQL 层可读的事务状态探针"
        );
        assert!(dialect.error_may_have_ended_transaction(None, "database or disk is full"));
        assert!(dialect.error_may_have_ended_transaction(Some("10"), "disk I/O error"));
        assert!(!dialect.error_may_have_ended_transaction(Some("1"), "no such table: t"));
        // 重复 BEGIN 由模型在预检阶段拒绝，而不是下发后再猜。
        let mut machine =
            crate::transaction::TransactionMachine::new("sqlite", dialect.capabilities());
        machine
            .acknowledge(
                &ControlStatement::Begin { mode: None },
                crate::transaction::ControlOutcome::Confirmed,
            )
            .unwrap();
        assert_eq!(
            machine.precheck(&ControlStatement::Begin { mode: None }),
            Err(TransactionError::AlreadyActive)
        );
    }
}
