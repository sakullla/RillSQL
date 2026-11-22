//! PostgreSQL 的事务方言：原生控制、保存点、隔离/读写模式与事务中止状态识别。（ADR-2、ADR-3）
//!
//! PostgreSQL 的普通语句错误会把事务置为中止状态（SQLSTATE `25P02`），之后只能 `ROLLBACK` 或
//! `ROLLBACK TO SAVEPOINT`；中止事务里的 `COMMIT` 不会报错，而是以 `ROLLBACK` 命令标签成功返回。
//! 没有可靠的通用“是否在事务中”探针：只读事务不分配事务号，因此不做猜测，保持未知。

use crate::transaction::{
    AbortedRecovery, ControlOutcome, ControlSql, ControlStatement, ControlSyntax, DuplicateBegin,
    ImplicitCommitSupport, ImplicitCommitVerdict, SavepointName, SettingChange, SettingOption,
    SettingSupport, StatementFailure, TransactionCapabilities, TransactionDialect, READ_COMMITTED,
    READ_ONLY, READ_WRITE, REPEATABLE_READ, SERIALIZABLE,
};

const ISOLATION_OPTIONS: &[SettingOption] = &[
    SettingOption {
        id: READ_COMMITTED,
        label: "Read committed",
    },
    SettingOption {
        id: REPEATABLE_READ,
        label: "Repeatable read",
    },
    SettingOption {
        id: SERIALIZABLE,
        label: "Serializable",
    },
];

const MODE_OPTIONS: &[SettingOption] = &[
    SettingOption {
        id: READ_ONLY,
        label: "Read only",
    },
    SettingOption {
        id: READ_WRITE,
        label: "Read write",
    },
];

/// PostgreSQL 的手写事务控制词汇：`END` 等于提交，`ABORT` 等于回滚，支持 `AND CHAIN`。
const POSTGRES_SYNTAX: ControlSyntax = ControlSyntax {
    bare_begin: true,
    start_transaction: true,
    end_commits: true,
    abort_rolls_back: true,
    chained: true,
    begin_modes: &[],
    rollback_transaction_name: false,
    save_transaction: false,
    hash_comments: false,
    identifier_quotes: &[('"', '"')],
};

pub(crate) struct PostgresTransaction;

pub(crate) static POSTGRES_TRANSACTION: PostgresTransaction = PostgresTransaction;

impl TransactionDialect for PostgresTransaction {
    fn capabilities(&self) -> TransactionCapabilities {
        TransactionCapabilities {
            explicit_control: true,
            savepoints: true,
            release_savepoint: true,
            chained_transactions: true,
            // 活动事务中再次 BEGIN 只产生警告，事务身份不变。
            duplicate_begin: DuplicateBegin::Ignored,
            aborted: AbortedRecovery::SavepointRecovery,
            // PostgreSQL 不接受 READ UNCOMMITTED（会被静默当成 READ COMMITTED），因此不展示。
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
        POSTGRES_SYNTAX
    }

    fn quote_identifier(&self, name: &str) -> String {
        format!("\"{}\"", name.replace('"', "\"\""))
    }

    fn begin_sql(&self, mode: Option<&str>) -> String {
        match mode {
            Some(READ_ONLY) => "BEGIN READ ONLY".to_owned(),
            Some(READ_WRITE) => "BEGIN READ WRITE".to_owned(),
            _ => "BEGIN".to_owned(),
        }
    }

    fn commit_sql(&self, chain: bool) -> String {
        if chain {
            "COMMIT AND CHAIN".to_owned()
        } else {
            "COMMIT".to_owned()
        }
    }

    fn rollback_sql(&self, chain: bool) -> String {
        if chain {
            "ROLLBACK AND CHAIN".to_owned()
        } else {
            "ROLLBACK".to_owned()
        }
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
            None => ControlSql::Statement("RESET default_transaction_isolation".to_owned()),
            Some(level) => match isolation_keyword(level) {
                Some(keyword) => ControlSql::Statement(format!(
                    "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL {keyword}"
                )),
                None => ControlSql::Unsupported,
            },
        }
    }

    fn mode_sql(&self, mode: Option<&str>) -> ControlSql {
        match mode {
            None => ControlSql::Statement("RESET default_transaction_read_only".to_owned()),
            Some(READ_ONLY) => ControlSql::Statement(
                "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY".to_owned(),
            ),
            Some(READ_WRITE) => ControlSql::Statement(
                "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE".to_owned(),
            ),
            Some(_) => ControlSql::Unsupported,
        }
    }

    fn implicit_commit(&self, _body: &str) -> ImplicitCommitVerdict {
        ImplicitCommitVerdict::NotApplicable
    }

    /// 事务中的任何错误都会把事务置为中止（R8）；连接的 SQLSTATE 08xxx、57P0x 按断线处理。
    fn statement_failure(&self, code: Option<&str>, message: &str) -> StatementFailure {
        match code {
            Some("25P02") => StatementFailure::TransactionAborted,
            Some(code) if is_connection_failure(code) => StatementFailure::ConnectionLost,
            // 事务中其余错误同样让事务进入中止状态。
            Some(_) => StatementFailure::TransactionAborted,
            None => {
                let lower = message.to_ascii_lowercase();
                if lower.contains("connection closed")
                    || lower.contains("connection reset")
                    || lower.contains("server closed the connection")
                {
                    StatementFailure::ConnectionLost
                } else {
                    StatementFailure::TransactionAborted
                }
            }
        }
    }

    /// 中止事务里的 `COMMIT` 以 `ROLLBACK` 命令标签成功返回：必须登记为实际回滚。
    fn control_success(
        &self,
        statement: &ControlStatement,
        command_tag: Option<&str>,
    ) -> ControlOutcome {
        if matches!(statement, ControlStatement::Commit { .. })
            && command_tag.is_some_and(|tag| tag.eq_ignore_ascii_case("ROLLBACK"))
        {
            return ControlOutcome::RolledBack;
        }
        ControlOutcome::Confirmed
    }
}

fn is_connection_failure(code: &str) -> bool {
    code.starts_with("08") || matches!(code, "57P01" | "57P02" | "57P03" | "57P05")
}

fn isolation_keyword(level: &str) -> Option<&'static str> {
    match level {
        READ_COMMITTED => Some("READ COMMITTED"),
        REPEATABLE_READ => Some("REPEATABLE READ"),
        SERIALIZABLE => Some("SERIALIZABLE"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::{classify_control, ControlMatch, TransactionDialect};

    #[test]
    fn control_sql_uses_standard_postgres_vocabulary() {
        let dialect = PostgresTransaction;
        assert_eq!(dialect.begin_sql(None), "BEGIN");
        assert_eq!(dialect.commit_sql(false), "COMMIT");
        assert_eq!(dialect.rollback_sql(true), "ROLLBACK AND CHAIN");
        let name = SavepointName::parse("before edit").unwrap();
        assert_eq!(dialect.savepoint_sql(&name), "SAVEPOINT \"before edit\"");
        assert_eq!(
            dialect.rollback_to_savepoint_sql(&name),
            "ROLLBACK TO SAVEPOINT \"before edit\""
        );
        assert_eq!(
            dialect.isolation_sql(Some(REPEATABLE_READ)),
            ControlSql::Statement(
                "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ"
                    .to_owned()
            )
        );
        assert_eq!(
            dialect.isolation_sql(Some("read_uncommitted")),
            ControlSql::Unsupported,
            "PG 不能展示会被当成其他级别的选项"
        );
        assert_eq!(
            dialect.mode_sql(None),
            ControlSql::Statement("RESET default_transaction_read_only".to_owned())
        );
    }

    #[test]
    fn handwritten_control_matches_postgres_aliases() {
        let dialect = PostgresTransaction;
        assert_eq!(
            classify_control("END", dialect.syntax()),
            ControlMatch::Control(ControlStatement::Commit { chain: false })
        );
        assert_eq!(
            classify_control("ABORT", dialect.syntax()),
            ControlMatch::Control(ControlStatement::Rollback { chain: false })
        );
        assert_eq!(
            classify_control("ROLLBACK TO SAVEPOINT sp1", dialect.syntax()),
            ControlMatch::Control(ControlStatement::RollbackToSavepoint(
                SavepointName::parse("sp1").unwrap()
            ))
        );
        assert!(matches!(
            classify_control(
                "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE",
                dialect.syntax()
            ),
            ControlMatch::Control(_)
        ));
        assert_eq!(
            classify_control("SELECT 1", dialect.syntax()),
            ControlMatch::Ordinary,
            "普通语句不是控制语句"
        );
    }

    #[test]
    fn aborted_transactions_only_allow_recovery() {
        let dialect = PostgresTransaction;
        assert_eq!(
            dialect.statement_failure(Some("25P02"), "current transaction is aborted"),
            StatementFailure::TransactionAborted
        );
        assert_eq!(
            dialect.statement_failure(Some("08006"), "connection failure"),
            StatementFailure::ConnectionLost
        );
        assert_eq!(
            dialect.statement_failure(Some("42P01"), "relation does not exist"),
            StatementFailure::TransactionAborted
        );
        assert_eq!(
            dialect.control_success(&ControlStatement::Commit { chain: false }, Some("ROLLBACK")),
            ControlOutcome::RolledBack
        );
        assert_eq!(
            dialect.control_success(&ControlStatement::Commit { chain: false }, Some("COMMIT")),
            ControlOutcome::Confirmed
        );
        // 中止后可以回滚到保存点，模型据此允许恢复。
        assert_eq!(
            dialect.capabilities().aborted,
            AbortedRecovery::SavepointRecovery
        );
        assert!(matches!(
            dialect.capabilities().duplicate_begin,
            DuplicateBegin::Ignored
        ));
        assert!(dialect.capabilities().chained_transactions);
        assert!(matches!(
            dialect.sql_for(&ControlStatement::Begin {
                mode: Some("read_only".to_owned())
            }),
            ControlSql::Statement(_)
        ));
    }
}
