//! MySQL / MariaDB 的事务方言：原生控制、保存点、隔离/读写模式与隐式提交分类。（ADR-2、ADR-3）
//!
//! MySQL 家族没有可靠的通用“当前是否在事务中”探针，因此不伪造探针；状态只按已确认的控制结果推进，
//! 过程、动态 SQL 与未分类语句按未知处理。DDL 与部分管理语句会在执行前隐式提交当前事务（R9），
//! 分类覆盖查询与结构页面能生成的语句，临时表等明确不提交的语句不报隐式提交。

use crate::transaction::{
    AbortedRecovery, ControlSql, ControlSyntax, DuplicateBegin, ImplicitCommitSupport, ImplicitCommitVerdict, SavepointName, SettingChange, SettingOption,
    SettingSupport, StatementFailure, TransactionCapabilities, TransactionDialect, READ_COMMITTED, READ_ONLY, READ_UNCOMMITTED, READ_WRITE, REPEATABLE_READ,
    SERIALIZABLE,
};

const ISOLATION_OPTIONS: &[SettingOption] = &[
    SettingOption { id: READ_UNCOMMITTED, label: "Read uncommitted" },
    SettingOption { id: READ_COMMITTED, label: "Read committed" },
    SettingOption { id: REPEATABLE_READ, label: "Repeatable read" },
    SettingOption { id: SERIALIZABLE, label: "Serializable" },
];

const MODE_OPTIONS: &[SettingOption] = &[
    SettingOption { id: READ_ONLY, label: "Read only" },
    SettingOption { id: READ_WRITE, label: "Read write" },
];

/// MySQL 家族的手写事务控制词汇：`#` 是行注释，标识符用反引号，没有 `AND CHAIN`。
const MYSQL_SYNTAX: ControlSyntax = ControlSyntax {
    bare_begin: true,
    start_transaction: true,
    end_commits: false,
    abort_rolls_back: false,
    chained: false,
    begin_modes: &[],
    rollback_transaction_name: false,
    save_transaction: false,
    hash_comments: true,
    identifier_quotes: &[('`', '`')],
};

pub(crate) struct MysqlTransaction;

pub(crate) static MYSQL_TRANSACTION: MysqlTransaction = MysqlTransaction;

impl TransactionDialect for MysqlTransaction {
    fn capabilities(&self) -> TransactionCapabilities {
        TransactionCapabilities {
            explicit_control: true,
            savepoints: true,
            release_savepoint: true,
            chained_transactions: false,
            // 活动事务中再次 BEGIN / START TRANSACTION 会隐式提交旧事务并开始新事务。
            duplicate_begin: DuplicateBegin::EndsPrevious,
            aborted: AbortedRecovery::NotApplicable,
            isolation: SettingSupport { options: ISOLATION_OPTIONS, change: SettingChange::WhileIdle },
            mode: SettingSupport { options: MODE_OPTIONS, change: SettingChange::WhileIdle },
            implicit_commit: ImplicitCommitSupport::MysqlFamily,
        }
    }

    fn syntax(&self) -> ControlSyntax {
        MYSQL_SYNTAX
    }

    fn quote_identifier(&self, name: &str) -> String {
        format!("`{}`", name.replace('`', "``"))
    }

    fn begin_sql(&self, mode: Option<&str>) -> String {
        match mode {
            Some(READ_ONLY) => "START TRANSACTION READ ONLY".to_owned(),
            Some(READ_WRITE) => "START TRANSACTION READ WRITE".to_owned(),
            _ => "START TRANSACTION".to_owned(),
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
        format!("ROLLBACK TO SAVEPOINT {}", self.quote_identifier(name.as_str()))
    }

    /// `SET SESSION TRANSACTION ISOLATION LEVEL` 只影响后续事务，不会提交当前事务。
    /// 恢复默认用系统变量赋 `DEFAULT`（回到全局值），全局值可能被服务器配置改过（只读副本常见），
    /// 不能写死 REPEATABLE READ。MariaDB 11.1+ 与 MySQL 都认这个变量名。
    fn isolation_sql(&self, level: Option<&str>) -> ControlSql {
        match level {
            None => ControlSql::Statement("SET SESSION transaction_isolation = DEFAULT".to_owned()),
            Some(level) => match isolation_keyword(level) {
                Some(keyword) => ControlSql::Statement(format!("SET SESSION TRANSACTION ISOLATION LEVEL {keyword}")),
                None => ControlSql::Unsupported,
            },
        }
    }

    /// 恢复默认模式的语义是回到全局 `transaction_read_only`，而不是写死 READ WRITE。
    fn mode_sql(&self, mode: Option<&str>) -> ControlSql {
        match mode {
            None => ControlSql::Statement("SET SESSION transaction_read_only = DEFAULT".to_owned()),
            Some(READ_WRITE) => ControlSql::Statement("SET SESSION TRANSACTION READ WRITE".to_owned()),
            Some(READ_ONLY) => ControlSql::Statement("SET SESSION TRANSACTION READ ONLY".to_owned()),
            Some(_) => ControlSql::Unsupported,
        }
    }

    fn implicit_commit(&self, body: &str) -> ImplicitCommitVerdict {
        mysql_implicit_commit(body)
    }

    /// 连接类错误（服务器消失、连接被中断）按断线处理；其余错误只回滚语句本身。
    fn statement_failure(&self, code: Option<&str>, message: &str) -> StatementFailure {
        if is_connection_failure(code, message) {
            StatementFailure::ConnectionLost
        } else {
            StatementFailure::TransactionUsable
        }
    }

    /// 死锁（1213）由服务器回滚整个事务，不能继续宣称事务活动（R10）。
    fn error_may_have_ended_transaction(&self, code: Option<&str>, message: &str) -> bool {
        code == Some("1213") || message.to_ascii_lowercase().contains("deadlock found")
    }
}

fn isolation_keyword(level: &str) -> Option<&'static str> {
    match level {
        READ_UNCOMMITTED => Some("READ UNCOMMITTED"),
        READ_COMMITTED => Some("READ COMMITTED"),
        REPEATABLE_READ => Some("REPEATABLE READ"),
        SERIALIZABLE => Some("SERIALIZABLE"),
        _ => None,
    }
}

fn is_connection_failure(code: Option<&str>, message: &str) -> bool {
    if matches!(code, Some("2006" | "2013" | "2055" | "1053" | "1152" | "1927")) {
        return true;
    }
    let lower = message.to_ascii_lowercase();
    lower.contains("server has gone away")
        || lower.contains("lost connection")
        || lower.contains("connection reset")
        || lower.contains("connection closed")
        || lower.contains("connection is closed")
        || lower.contains("forcibly closed")
        || lower.contains("server closed")
        || lower.contains("broken pipe")
        || lower.contains("connection was killed")
        || lower.contains("server shutdown in progress")
}

/// MySQL/MariaDB 的隐式提交判定。
///
/// 依据服务器文档的“会导致隐式提交的语句”清单，覆盖结构页面和查询页面能生成的常见 DDL；
/// 明确不隐式提交的 `CREATE/DROP TEMPORARY TABLE` 不报隐式提交。过程体、动态 SQL、可执行注释与
/// 未分类语句返回 `Uncertain`：不宣称发生了隐式提交，由调用方按未知处理。
pub(crate) fn mysql_implicit_commit(body: &str) -> ImplicitCommitVerdict {
    let (words, executable_comment) = leading_words(body, 3);
    if executable_comment {
        return ImplicitCommitVerdict::Uncertain;
    }
    let head = words.first().map(String::as_str).unwrap_or_default();
    let second = words.get(1).map(String::as_str).unwrap_or_default();
    match head {
        "BEGIN" => ImplicitCommitVerdict::Yes,
        "START" => match second {
            "TRANSACTION" | "SLAVE" | "REPLICA" | "GROUP_REPLICATION" => ImplicitCommitVerdict::Yes,
            _ => ImplicitCommitVerdict::Uncertain,
        },
        "CREATE" | "DROP" if second == "TEMPORARY" => ImplicitCommitVerdict::NotApplicable,
        "ALTER" | "CREATE" | "DROP" | "RENAME" | "TRUNCATE" | "GRANT" | "REVOKE" | "ANALYZE" | "OPTIMIZE" | "REPAIR" | "FLUSH" | "INSTALL"
        | "UNINSTALL" | "CACHE" => ImplicitCommitVerdict::Yes,
        // RESET 在清单里，但 RESET PERSIST 明确不隐式提交。
        "RESET" => match second {
            "PERSIST" => ImplicitCommitVerdict::NotApplicable,
            _ => ImplicitCommitVerdict::Yes,
        },
        // LOCK TABLES 隐式提交；LOCK INSTANCE FOR BACKUP 不隐式提交，其他形态无法静态确认。
        "LOCK" => match second {
            "TABLES" => ImplicitCommitVerdict::Yes,
            "INSTANCE" => ImplicitCommitVerdict::NotApplicable,
            _ => ImplicitCommitVerdict::Uncertain,
        },
        // UNLOCK TABLES 只在此前 LOCK TABLES 取得表锁时才隐式提交：不能无条件宣称。
        "UNLOCK" => ImplicitCommitVerdict::Uncertain,
        "SET" => match second {
            "PASSWORD" => ImplicitCommitVerdict::Yes,
            "AUTOCOMMIT" => ImplicitCommitVerdict::Uncertain,
            _ => ImplicitCommitVerdict::NotApplicable,
        },
        "LOAD" => match second {
            "INDEX" => ImplicitCommitVerdict::Yes,
            _ => ImplicitCommitVerdict::Uncertain,
        },
        // 过程、动态 SQL 与未分类管理语句可能包含隐式提交，但无法静态确认。
        "CALL" | "DO" | "EXECUTE" | "PREPARE" | "DEALLOCATE" => ImplicitCommitVerdict::Uncertain,
        "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "REPLACE" | "WITH" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN" | "USE" | "COMMIT" | "ROLLBACK"
        | "SAVEPOINT" | "RELEASE" | "KILL" | "SHUTDOWN" => ImplicitCommitVerdict::NotApplicable,
        "" => ImplicitCommitVerdict::Uncertain,
        _ => ImplicitCommitVerdict::Uncertain,
    }
}

/// 取语句开头的若干个代码词（大写）。跳过空白、`#`/`--` 行注释、块注释与引号内容；
/// 遇到服务器会执行的注释（`/*! ... */`、MariaDB `/*M! ... */`）时报告无法静态确认。
fn leading_words(body: &str, limit: usize) -> (Vec<String>, bool) {
    let mut chars = body.char_indices().peekable();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut executable = false;
    let finish = |word: &mut String, words: &mut Vec<String>| {
        if !word.is_empty() && words.len() < limit {
            words.push(std::mem::take(word));
        } else {
            word.clear();
        }
    };
    while let Some((_, ch)) = chars.next() {
        if ch == '#' {
            for (_, next) in chars.by_ref() {
                if next == '\n' {
                    break;
                }
            }
            finish(&mut word, &mut words);
        } else if ch == '-' && chars.peek().is_some_and(|(_, next)| *next == '-') {
            chars.next();
            for (_, next) in chars.by_ref() {
                if next == '\n' {
                    break;
                }
            }
            finish(&mut word, &mut words);
        } else if ch == '/' && chars.peek().is_some_and(|(_, next)| *next == '*') {
            chars.next();
            let mut first = true;
            while let Some((_, next)) = chars.next() {
                if first && (next == '!' || next == 'M') {
                    executable = true;
                }
                first = false;
                if next == '*' && chars.peek().is_some_and(|(_, after)| *after == '/') {
                    chars.next();
                    break;
                }
            }
            finish(&mut word, &mut words);
        } else if ch == '\'' || ch == '"' || ch == '`' {
            let close = ch;
            finish(&mut word, &mut words);
            while let Some((_, next)) = chars.next() {
                if next == close {
                    if chars.peek().is_some_and(|(_, after)| *after == close) {
                        chars.next();
                        continue;
                    }
                    break;
                }
            }
        } else if ch.is_alphanumeric() || ch == '_' || ch == '$' {
            word.push(ch.to_ascii_uppercase());
        } else {
            finish(&mut word, &mut words);
        }
        if words.len() >= limit {
            break;
        }
    }
    finish(&mut word, &mut words);
    (words, executable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::{classify_control, ControlMatch, ControlOutcome, ControlStatement, StatementFailure, TransactionDialect, UnknownReason};

    #[test]
    fn control_sql_uses_mysql_family_vocabulary() {
        let dialect = MysqlTransaction;
        assert_eq!(dialect.begin_sql(None), "START TRANSACTION");
        assert_eq!(dialect.begin_sql(Some(READ_ONLY)), "START TRANSACTION READ ONLY");
        assert_eq!(dialect.commit_sql(false), "COMMIT");
        assert_eq!(dialect.rollback_sql(true), "ROLLBACK");
        let name = SavepointName::parse("sp 1").unwrap();
        assert_eq!(dialect.savepoint_sql(&name), "SAVEPOINT `sp 1`");
        assert_eq!(dialect.rollback_to_savepoint_sql(&name), "ROLLBACK TO SAVEPOINT `sp 1`");
        assert_eq!(dialect.release_savepoint_sql(&name), ControlSql::Statement("RELEASE SAVEPOINT `sp 1`".to_owned()));
        assert_eq!(
            dialect.isolation_sql(Some(SERIALIZABLE)),
            ControlSql::Statement("SET SESSION TRANSACTION ISOLATION LEVEL SERIALIZABLE".to_owned())
        );
        // 恢复默认必须回到全局值：只读副本等环境的全局默认不是 REPEATABLE READ。
        assert_eq!(dialect.isolation_sql(None), ControlSql::Statement("SET SESSION transaction_isolation = DEFAULT".to_owned()));
        assert_eq!(dialect.mode_sql(Some(READ_ONLY)), ControlSql::Statement("SET SESSION TRANSACTION READ ONLY".to_owned()));
        assert_eq!(dialect.mode_sql(Some(READ_WRITE)), ControlSql::Statement("SET SESSION TRANSACTION READ WRITE".to_owned()));
        assert_eq!(dialect.mode_sql(None), ControlSql::Statement("SET SESSION transaction_read_only = DEFAULT".to_owned()));
    }

    #[test]
    fn handwritten_control_matches_mysql_family_syntax() {
        let dialect = MysqlTransaction;
        assert_eq!(classify_control("START TRANSACTION", dialect.syntax()), ControlMatch::Control(ControlStatement::Begin { mode: None }));
        assert_eq!(classify_control("# begin\nBEGIN", dialect.syntax()), ControlMatch::Control(ControlStatement::Begin { mode: None }));
        assert_eq!(classify_control("ROLLBACK WORK", dialect.syntax()), ControlMatch::Control(ControlStatement::Rollback { chain: false }));
        assert_eq!(classify_control("COMMIT AND CHAIN", dialect.syntax()), ControlMatch::Ambiguous, "MySQL 没有 AND CHAIN");
        assert_eq!(classify_control("SELECT 1", dialect.syntax()), ControlMatch::Ordinary);
    }

    #[test]
    fn common_ddl_is_classified_as_implicit_commit_but_temporary_tables_are_not() {
        for sql in [
            "ALTER TABLE notes ADD COLUMN body TEXT",
            "CREATE TABLE t (id INT)",
            "DROP TABLE t",
            "TRUNCATE TABLE t",
            "RENAME TABLE a TO b",
            "CREATE INDEX i ON t (id)",
            "GRANT SELECT ON *.* TO app",
            "BEGIN",
            "START TRANSACTION",
            "SET PASSWORD FOR app = 'x'",
            "FLUSH TABLES",
        ] {
            assert_eq!(mysql_implicit_commit(sql), ImplicitCommitVerdict::Yes, "{sql}");
        }
        for sql in [
            "CREATE TEMPORARY TABLE tmp (id INT)",
            "DROP TEMPORARY TABLE tmp",
            "SELECT * FROM t",
            "INSERT INTO t (id) VALUES (1)",
            "UPDATE t SET id = 2",
            "COMMIT",
            "SAVEPOINT sp",
            "SET SESSION TRANSACTION ISOLATION LEVEL SERIALIZABLE",
        ] {
            assert_eq!(mysql_implicit_commit(sql), ImplicitCommitVerdict::NotApplicable, "{sql}");
        }
        for sql in ["CALL rotate_notes()", "/*!40101 CREATE TABLE t (id INT) */", "SET autocommit = 1", "EXECUTE stmt"] {
            assert_eq!(mysql_implicit_commit(sql), ImplicitCommitVerdict::Uncertain, "{sql}");
        }
        // 条件语句不能无条件宣称隐式提交：只有此前 LOCK TABLES 过的 UNLOCK TABLES 才会提交。
        assert_eq!(mysql_implicit_commit("UNLOCK TABLES"), ImplicitCommitVerdict::Uncertain);
        // RESET PERSIST 明确不提交；其余 RESET 在清单里。
        assert_eq!(mysql_implicit_commit("RESET PERSIST"), ImplicitCommitVerdict::NotApplicable);
        assert_eq!(mysql_implicit_commit("RESET MASTER"), ImplicitCommitVerdict::Yes);
        // LOCK TABLES 隐式提交；LOCK INSTANCE FOR BACKUP 不提交；其他 LOCK 形态无法静态确认。
        assert_eq!(mysql_implicit_commit("LOCK TABLES t READ"), ImplicitCommitVerdict::Yes);
        assert_eq!(mysql_implicit_commit("LOCK INSTANCE FOR BACKUP"), ImplicitCommitVerdict::NotApplicable);
        assert_eq!(mysql_implicit_commit("LOCK TABLE t WRITE"), ImplicitCommitVerdict::Uncertain);
        // 注释与大小写不影响分类。
        assert_eq!(mysql_implicit_commit("-- 说明\n  aLtEr TABLE t ADD c INT"), ImplicitCommitVerdict::Yes);
    }

    #[test]
    fn connection_and_deadlock_errors_are_kept_apart() {
        let dialect = MysqlTransaction;
        assert_eq!(dialect.statement_failure(Some("2013"), "Lost connection to MySQL server"), StatementFailure::ConnectionLost);
        assert_eq!(dialect.statement_failure(Some("1064"), "You have an error in your SQL syntax"), StatementFailure::TransactionUsable);
        // 驱动在“命令已转发、响应前断开”时给出的是 I/O 文本，没有错误码：必须按连接失败处理。
        for message in [
            "Input/output error: Input/output error: connection closed",
            "An existing connection was forcibly closed by the remote host. (os error 10054)",
            "Server closed connection without sending any data",
            "Lost connection to MySQL server during query",
            "MySQL server has gone away",
        ] {
            assert_eq!(dialect.statement_failure(None, message), StatementFailure::ConnectionLost, "{message}");
        }
        // 控制失败路径：连接丢失必须登记未知；普通 SQL 错误仍是被拒绝。
        let commit = ControlStatement::Commit { chain: false };
        assert_eq!(
            dialect.control_failure(StatementFailure::ConnectionLost, &commit),
            ControlOutcome::Unconfirmed(UnknownReason::ConnectionLost)
        );
        assert_eq!(dialect.control_failure(StatementFailure::TransactionUsable, &commit), ControlOutcome::Rejected);
        assert!(dialect.error_may_have_ended_transaction(Some("1213"), "Deadlock found when trying to get lock"));
        assert!(!dialect.error_may_have_ended_transaction(Some("1205"), "Lock wait timeout exceeded"));
        assert_eq!(dialect.capabilities().duplicate_begin, DuplicateBegin::EndsPrevious);
        assert_eq!(dialect.capabilities().aborted, AbortedRecovery::NotApplicable);
        assert!(dialect.probe_sql().is_none(), "没有可靠的通用探针时不猜测事务状态");
    }
}
