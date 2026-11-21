//! SQL Server（T-SQL）的事务方言：`BEGIN/COMMIT/ROLLBACK TRANSACTION`、`SAVE TRANSACTION`、
//! `@@TRANCOUNT` / `XACT_STATE()` 状态探针与不可提交事务的整体回滚语义。（ADR-2、ADR-3）
//!
//! T-SQL 的 `BEGIN` 是语句块，事务开始必须写 `BEGIN TRAN[SACTION]`；活动事务中再次开始只增加
//! `@@TRANCOUNT`，一次 `COMMIT` 不会结束外层事务。`XACT_STATE() = -1` 的不可提交事务不能局部恢复，
//! 只能整体回滚。SQL Server 没有释放保存点的语句。

use crate::transaction::{
    AbortedRecovery, ControlSql, ControlSyntax, DuplicateBegin, ImplicitCommitSupport, ImplicitCommitVerdict, SavepointName, SettingChange, SettingOption,
    SettingSupport, StatementFailure, TransactionCapabilities, TransactionDialect, TransactionProbe, READ_COMMITTED, READ_UNCOMMITTED, REPEATABLE_READ,
    SERIALIZABLE, SNAPSHOT,
};

const ISOLATION_OPTIONS: &[SettingOption] = &[
    SettingOption { id: READ_UNCOMMITTED, label: "Read uncommitted" },
    SettingOption { id: READ_COMMITTED, label: "Read committed" },
    SettingOption { id: REPEATABLE_READ, label: "Repeatable read" },
    SettingOption { id: SERIALIZABLE, label: "Serializable" },
    SettingOption { id: SNAPSHOT, label: "Snapshot" },
];

/// T-SQL 的事务控制词汇：`BEGIN` 本身是块开始，因此只有 `BEGIN TRAN[SACTION]` 才算控制语句；
/// `ROLLBACK TRANSACTION <name>` 是回滚到保存点，用方括号引用标识符。
const SQLSERVER_SYNTAX: ControlSyntax = ControlSyntax {
    bare_begin: false,
    start_transaction: false,
    end_commits: false,
    abort_rolls_back: false,
    chained: false,
    begin_modes: &[],
    rollback_transaction_name: true,
    hash_comments: false,
    identifier_quotes: &[('"', '"'), ('[', ']')],
};

pub(crate) struct SqlserverTransaction;

pub(crate) static SQLSERVER_TRANSACTION: SqlserverTransaction = SqlserverTransaction;

impl TransactionDialect for SqlserverTransaction {
    fn capabilities(&self) -> TransactionCapabilities {
        TransactionCapabilities {
            explicit_control: true,
            savepoints: true,
            // T-SQL 只有 SAVE TRANSACTION，没有释放保存点的语句。
            release_savepoint: false,
            chained_transactions: false,
            // @@TRANCOUNT 加一：一次提交只结束最内层。
            duplicate_begin: DuplicateBegin::Nests,
            aborted: AbortedRecovery::RollbackOnly,
            // SET TRANSACTION ISOLATION LEVEL 在事务中合法，只影响后续语句，不会隐式提交。
            isolation: SettingSupport { options: ISOLATION_OPTIONS, change: SettingChange::AnyTime },
            mode: SettingSupport::NONE,
            implicit_commit: ImplicitCommitSupport::NotApplicable,
        }
    }

    fn syntax(&self) -> ControlSyntax {
        SQLSERVER_SYNTAX
    }

    fn quote_identifier(&self, name: &str) -> String {
        format!("[{}]", name.replace(']', "]]"))
    }

    fn begin_sql(&self, _mode: Option<&str>) -> String {
        "BEGIN TRANSACTION".to_owned()
    }

    fn commit_sql(&self, _chain: bool) -> String {
        "COMMIT TRANSACTION".to_owned()
    }

    fn rollback_sql(&self, _chain: bool) -> String {
        "ROLLBACK TRANSACTION".to_owned()
    }

    fn savepoint_sql(&self, name: &SavepointName) -> String {
        format!("SAVE TRANSACTION {}", self.quote_identifier(name.as_str()))
    }

    fn rollback_to_savepoint_sql(&self, name: &SavepointName) -> String {
        format!("ROLLBACK TRANSACTION {}", self.quote_identifier(name.as_str()))
    }

    fn isolation_sql(&self, level: Option<&str>) -> ControlSql {
        // 标准安装的默认级别是 READ COMMITTED；恢复默认时明确写回它。
        let level = level.unwrap_or(READ_COMMITTED);
        match isolation_keyword(level) {
            Some(keyword) => ControlSql::Statement(format!("SET TRANSACTION ISOLATION LEVEL {keyword}")),
            None => ControlSql::Unsupported,
        }
    }

    fn mode_sql(&self, _mode: Option<&str>) -> ControlSql {
        ControlSql::Unsupported
    }

    fn implicit_commit(&self, _body: &str) -> ImplicitCommitVerdict {
        ImplicitCommitVerdict::NotApplicable
    }

    fn probe_sql(&self) -> Option<&'static str> {
        Some("SELECT @@TRANCOUNT AS trancount, XACT_STATE() AS state")
    }

    fn read_probe(&self, columns: &[String], rows: &[Vec<Option<String>>]) -> Option<TransactionProbe> {
        let position = |name: &str| columns.iter().position(|column| column.eq_ignore_ascii_case(name));
        let trancount = read_number(rows.first()?.get(position("trancount")?)?.as_deref()?)?;
        let state = read_number(rows.first()?.get(position("state")?)?.as_deref()?)?;
        match (state, trancount) {
            // -1：不可提交的事务，只能整体回滚。
            (-1, _) => Some(TransactionProbe::Aborted),
            (0, 0) => Some(TransactionProbe::Idle),
            (1, depth) if depth > 0 => Some(TransactionProbe::Active { depth: u32::try_from(depth).ok()? }),
            // 层数或状态自相矛盾（0 层却可提交、1 层却没有事务）：按未知处理。
            _ => None,
        }
    }

    /// 连接类错误按断线处理；不可提交事务（3930/3931/3998）只能整体回滚。
    /// 6401 只是回滚目标保存点不存在：事务仍活动（@@TRANCOUNT 不变），不能标成中止。
    fn statement_failure(&self, code: Option<&str>, message: &str) -> StatementFailure {
        if is_connection_failure(code, message) {
            return StatementFailure::ConnectionLost;
        }
        if matches!(code, Some("3998" | "3930" | "3931")) {
            return StatementFailure::TransactionAborted;
        }
        StatementFailure::TransactionUsable
    }

    /// 死锁牺牲品（1205）的事务由服务器回滚，不能继续宣称它仍然活动。
    fn error_may_have_ended_transaction(&self, code: Option<&str>, message: &str) -> bool {
        code == Some("1205") || message.to_ascii_lowercase().contains("deadlock victim")
    }
}

fn read_number(value: &str) -> Option<i64> {
    value.trim().parse().ok()
}

fn is_connection_failure(code: Option<&str>, message: &str) -> bool {
    if matches!(code, Some("233" | "10053" | "10054" | "10060" | "64" | "104")) {
        return true;
    }
    let lower = message.to_ascii_lowercase();
    lower.contains("connection reset")
        || lower.contains("connection was closed")
        || lower.contains("transport-level error")
        || lower.contains("communication link failure")
        || lower.contains("connection is broken")
}

fn isolation_keyword(level: &str) -> Option<&'static str> {
    match level {
        READ_UNCOMMITTED => Some("READ UNCOMMITTED"),
        READ_COMMITTED => Some("READ COMMITTED"),
        REPEATABLE_READ => Some("REPEATABLE READ"),
        SERIALIZABLE => Some("SERIALIZABLE"),
        SNAPSHOT => Some("SNAPSHOT"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::{classify_control, ControlMatch, ControlStatement, TransactionDialect};

    #[test]
    fn control_sql_uses_transact_syntax() {
        let dialect = SqlserverTransaction;
        assert_eq!(dialect.begin_sql(None), "BEGIN TRANSACTION");
        assert_eq!(dialect.commit_sql(false), "COMMIT TRANSACTION");
        assert_eq!(dialect.rollback_sql(false), "ROLLBACK TRANSACTION");
        let name = SavepointName::parse("sp").unwrap();
        assert_eq!(dialect.savepoint_sql(&name), "SAVE TRANSACTION [sp]");
        assert_eq!(dialect.rollback_to_savepoint_sql(&name), "ROLLBACK TRANSACTION [sp]");
        assert_eq!(dialect.release_savepoint_sql(&name), ControlSql::Unsupported, "T-SQL 没有释放保存点");
        assert_eq!(
            dialect.isolation_sql(Some(SNAPSHOT)),
            ControlSql::Statement("SET TRANSACTION ISOLATION LEVEL SNAPSHOT".to_owned())
        );
        assert_eq!(dialect.mode_sql(Some("read_only")), ControlSql::Unsupported);
    }

    #[test]
    fn begin_without_transaction_is_a_block_not_a_transaction() {
        let dialect = SqlserverTransaction;
        assert_eq!(classify_control("BEGIN", dialect.syntax()), ControlMatch::Ordinary);
        assert_eq!(classify_control("BEGIN TRAN", dialect.syntax()), ControlMatch::Control(ControlStatement::Begin { mode: None }));
        assert_eq!(
            classify_control("BEGIN TRANSACTION", dialect.syntax()),
            ControlMatch::Control(ControlStatement::Begin { mode: None })
        );
        assert_eq!(
            classify_control("ROLLBACK TRANSACTION sp", dialect.syntax()),
            ControlMatch::Control(ControlStatement::RollbackToSavepoint(SavepointName::parse("sp").unwrap()))
        );
        assert_eq!(classify_control("COMMIT AND CHAIN", dialect.syntax()), ControlMatch::Ambiguous);
    }

    #[test]
    fn transaction_state_probe_reads_trancount_and_xact_state() {
        let dialect = SqlserverTransaction;
        let columns = vec!["trancount".to_owned(), "state".to_owned()];
        let row = |trancount: &str, state: &str| vec![Some(trancount.to_owned()), Some(state.to_owned())];
        assert_eq!(dialect.read_probe(&columns, &[row("0", "0")]), Some(TransactionProbe::Idle));
        assert_eq!(dialect.read_probe(&columns, &[row("2", "1")]), Some(TransactionProbe::Active { depth: 2 }));
        assert_eq!(dialect.read_probe(&columns, &[row("1", "-1")]), Some(TransactionProbe::Aborted));
        assert_eq!(dialect.read_probe(&columns, &[row("0", "1")]), None, "自相矛盾的观测按未知处理");
        assert_eq!(dialect.read_probe(&columns, &[]), None);
        assert_eq!(dialect.read_probe(&["other".to_owned()], &[]), None);
        assert!(dialect.probe_sql().is_some_and(|sql| sql.contains("XACT_STATE")));
    }

    #[test]
    fn uncommittable_and_deadlock_errors_are_not_reported_as_usable() {
        let dialect = SqlserverTransaction;
        assert_eq!(dialect.statement_failure(Some("3998"), "cannot be committed"), StatementFailure::TransactionAborted);
        assert_eq!(dialect.statement_failure(Some("3930"), "cannot support operations that write to the log"), StatementFailure::TransactionAborted);
        assert_eq!(dialect.statement_failure(Some("3931"), "cannot be rolled back to a savepoint"), StatementFailure::TransactionAborted);
        assert_eq!(dialect.statement_failure(Some("10054"), "connection reset"), StatementFailure::ConnectionLost);
        assert_eq!(dialect.statement_failure(Some("2627"), "violation of primary key"), StatementFailure::TransactionUsable);
        assert!(dialect.error_may_have_ended_transaction(Some("1205"), "deadlock victim"));
        assert_eq!(dialect.capabilities().aborted, AbortedRecovery::RollbackOnly);
        assert_eq!(dialect.capabilities().duplicate_begin, DuplicateBegin::Nests);
        assert!(!dialect.capabilities().release_savepoint);
    }

    /// 6401 是回滚到不存在的保存点/名字：事务仍可用，不能迫使整体回滚。
    #[test]
    fn a_missing_savepoint_rollback_keeps_the_transaction_usable() {
        let dialect = SqlserverTransaction;
        assert_eq!(
            dialect.statement_failure(Some("6401"), "Cannot roll back sp. No transaction or savepoint of that name was found."),
            StatementFailure::TransactionUsable
        );
        // 3931 是 XACT_STATE()=-1 时回滚到保存点被拒：只能整体回滚，后续语句不得继续。
        assert_eq!(
            dialect.statement_failure(Some("3931"), "The current transaction cannot be rolled back to a savepoint."),
            StatementFailure::TransactionAborted
        );
    }
}
