//! SQL Server 的 SQL 语法：标识符引号、保留字、Unicode 字面量、OFFSET/FETCH 分页、
//! 网格投影、T-SQL 词法、TOP 行数上限与 SHOWPLAN 执行计划。

use std::sync::OnceLock;

use crate::adapters::{Lexis, SqlSyntax};
use crate::query::exec::CappedSql;
use crate::query::functions::FunctionSig;
use crate::query::plan::PlanNode;

pub(super) struct SqlserverSyntax;

impl SqlSyntax for SqlserverSyntax {
    /// T-SQL 的括号标识符转义是 ]]。
    fn quote_always(&self, name: &str) -> String {
        format!("[{}]", name.replace(']', "]]"))
    }

    fn reserved_words(&self) -> &'static [&'static str] {
        RESERVED
    }

    /// T-SQL 没有 LIMIT，取前几行用 TOP。
    fn select_first_rows(&self, table: &str, limit: usize) -> String {
        format!("SELECT TOP {limit} * FROM {table};")
    }

    fn national_prefix(&self) -> &'static str {
        "N"
    }

    /// T-SQL 的 OFFSET/FETCH 必须跟在 ORDER BY 后面，没有排序时补一个恒定序。
    fn page_tail(&self, limit: u64, offset: u64, ordered: bool) -> String {
        if ordered {
            format!(" OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY")
        } else {
            format!(" ORDER BY (SELECT NULL) OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY")
        }
    }

    fn count_expression(&self) -> &'static str {
        "CAST(COUNT_BIG(*) AS BIGINT)"
    }

    /// NVARCHAR(MAX) 承接任意长度的文本形态，日期/数字等隐式转换后参与 LIKE 匹配。
    fn text_cast(&self, ident: &str) -> String {
        format!("CAST({ident} AS NVARCHAR(MAX))")
    }

    /// LEN 按字符计；预览协议的字节数对文本形态按字符数呈现，超长截断按设置的字符数。
    fn text_preview(&self, plain: &str, cap: usize) -> String {
        format!("CASE WHEN LEN({plain}) > {cap} THEN CHAR(1) + N't:' + CAST(LEN({plain}) AS NVARCHAR(20)) + N':' + LEFT({plain}, {cap}) ELSE {plain} END")
    }

    fn binary_length(&self, ident: &str) -> String {
        format!("CASE WHEN {ident} IS NULL THEN NULL ELSE CHAR(1) + N'b:' + CAST(DATALENGTH({ident}) AS NVARCHAR(20)) END")
    }

    /// CONVERT 样式 2 输出不带 0x 前缀的小写十六进制。
    fn binary_hex(&self, ident: &str) -> String {
        format!("LOWER(CONVERT(VARCHAR(MAX), {ident}, 2))")
    }

    /// T-SQL 的二进制字面量是 0x 前缀（空二进制是 0x）。
    fn binary_literal(&self, hex: &str) -> String {
        format!("0x{hex}")
    }

    fn lexis(&self) -> Lexis {
        Lexis { hash_comments: false, transact: true, delimiter_command: false, executable_comments: false, backslash_escapes: false }
    }

    fn cap_rows(&self, sql: &str, cap: usize) -> Option<CappedSql> {
        Some(super::exec::cap_rows_with_top(sql, cap))
    }

    fn dialect_words(&self) -> &'static [&'static str] {
        &["TOP", "NOLOCK", "ISNULL", "OFFSET", "FETCH", "IDENTITY"]
    }

    fn explain(&self, body: &str) -> String {
        super::exec::showplan_batch(body)
    }

    fn parse_plan(&self, _sql: &str, _explained: bool, columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Vec<PlanNode>> {
        let column = |name: &str| columns.iter().position(|column| column.eq_ignore_ascii_case(name));
        if column("NodeId").is_none() || column("PhysicalOp").is_none() {
            return None;
        }
        super::plan::parse_showplan_all(columns, rows)
    }

    fn functions(&self) -> &'static [FunctionSig] {
        super::functions::SQLSERVER
    }

    /// T-SQL 批次里语句可以不加分号首尾相接（`SELECT 1 KILL 53`），切分无法分开。
    /// 因此按保留字白名单反推：除查询子句常用的保留字外，任何保留字出现即按可能写入处理，
    /// 再补上不是保留字的语句开头（RECEIVE、SEND、ENABLE/DISABLE TRIGGER、THROW 等）。
    fn side_effect_words(&self) -> &'static [&'static str] {
        static WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
        WORDS.get_or_init(|| {
            RESERVED
                .iter()
                .copied()
                .filter(|word| !READ_ONLY_RESERVED.contains(word))
                .chain(UNRESERVED_STATEMENT_WORDS.iter().copied())
                .collect()
        })
    }
}

/// 只读查询里会出现的保留字（子句、运算、函数、类型名）。不在这里的保留字一律视为可能写入。
const READ_ONLY_RESERVED: &[&str] = &[
    "ALL", "AND", "ANY", "AS", "ASC", "BETWEEN", "BY", "CASE", "COALESCE", "COLLATE", "CONTAINS", "CONTAINSTABLE", "CONVERT",
    "CROSS", "CURRENT_DATE", "CURRENT_TIME", "CURRENT_TIMESTAMP", "CURRENT_USER", "DESC", "DISTINCT", "DOUBLE", "ELSE", "END",
    "ESCAPE", "EXCEPT", "EXISTS", "FOR", "FREETEXT", "FREETEXTTABLE", "FROM", "FULL", "GROUP", "HAVING", "IN", "INNER",
    "INTERSECT", "IS", "JOIN", "LEFT", "LIKE", "NATIONAL", "NOT", "NULL", "NULLIF", "ON", "OPTION", "OR", "ORDER", "OUTER",
    "OVER", "PERCENT", "PIVOT", "PRECISION", "RIGHT", "SELECT", "SEMANTICKEYPHRASETABLE", "SEMANTICSIMILARITYDETAILSTABLE",
    "SEMANTICSIMILARITYTABLE", "SESSION_USER", "SOME", "SYSTEM_USER", "TABLESAMPLE", "THEN", "TOP", "TRY_CONVERT", "UNION",
    "UNPIVOT", "USER", "VALUES", "VARYING", "WHEN", "WHERE", "WITH", "WITHIN",
];

/// 不是保留字、但能开始一条会改动数据或服务器状态的 T-SQL 语句的词。
const UNRESERVED_STATEMENT_WORDS: &[&str] = &[
    "RECEIVE", "SEND", "MOVE", "GET", "ENABLE", "DISABLE", "THROW", "CONVERSATION", "DIALOG", "OUTPUT", "UPSERT",
];

/// SQL Server 保留关键字。按字母序。
const RESERVED: &[&str] = &[
    "ADD", "ALL", "ALTER", "AND", "ANY", "AS", "ASC", "AUTHORIZATION", "BACKUP", "BEGIN", "BETWEEN", "BREAK", "BROWSE", "BULK",
    "BY", "CASCADE", "CASE", "CHECK", "CHECKPOINT", "CLOSE", "CLUSTERED", "COALESCE", "COLLATE", "COLUMN", "COMMIT", "COMPUTE",
    "CONSTRAINT", "CONTAINS", "CONTAINSTABLE", "CONTINUE", "CONVERT", "CREATE", "CROSS", "CURRENT", "CURRENT_DATE",
    "CURRENT_TIME", "CURRENT_TIMESTAMP", "CURRENT_USER", "CURSOR", "DATABASE", "DBCC", "DEALLOCATE", "DECLARE", "DEFAULT",
    "DELETE", "DENY", "DESC", "DISK", "DISTINCT", "DISTRIBUTED", "DOUBLE", "DROP", "DUMP", "ELSE", "END", "ERRLVL", "ESCAPE",
    "EXCEPT", "EXEC", "EXECUTE", "EXISTS", "EXIT", "EXTERNAL", "FETCH", "FILE", "FILLFACTOR", "FOR", "FOREIGN", "FREETEXT",
    "FREETEXTTABLE", "FROM", "FULL", "FUNCTION", "GOTO", "GRANT", "GROUP", "HAVING", "HOLDLOCK", "IDENTITY", "IDENTITYCOL",
    "IDENTITY_INSERT", "IF", "IN", "INDEX", "INNER", "INSERT", "INTERSECT", "INTO", "IS", "JOIN", "KEY", "KILL", "LEFT", "LIKE",
    "LINENO", "LOAD", "MERGE", "NATIONAL", "NOCHECK", "NONCLUSTERED", "NOT", "NULL", "NULLIF", "OF", "OFF", "OFFSETS", "ON",
    "OPEN", "OPENDATASOURCE", "OPENQUERY", "OPENROWSET", "OPENXML", "OPTION", "OR", "ORDER", "OUTER", "OVER", "PERCENT", "PIVOT",
    "PLAN", "PRECISION", "PRIMARY", "PRINT", "PROC", "PROCEDURE", "PUBLIC", "RAISERROR", "READ", "READTEXT", "RECONFIGURE",
    "REFERENCES", "REPLICATION", "RESTORE", "RESTRICT", "RETURN", "REVERT", "REVOKE", "RIGHT", "ROLLBACK", "ROWCOUNT",
    "ROWGUIDCOL", "RULE", "SAVE", "SCHEMA", "SECURITYAUDIT", "SELECT", "SEMANTICKEYPHRASETABLE",
    "SEMANTICSIMILARITYDETAILSTABLE", "SEMANTICSIMILARITYTABLE", "SESSION_USER", "SET", "SETUSER", "SHUTDOWN", "SOME",
    "STATISTICS", "SYSTEM_USER", "TABLE", "TABLESAMPLE", "TEXTSIZE", "THEN", "TO", "TOP", "TRAN", "TRANSACTION", "TRIGGER",
    "TRUNCATE", "TRY_CONVERT", "TSEQUAL", "UNION", "UNIQUE", "UNPIVOT", "UPDATE", "UPDATETEXT", "USE", "USER", "VALUES",
    "VARYING", "VIEW", "WAITFOR", "WHEN", "WHERE", "WHILE", "WITH", "WITHIN", "WRITETEXT",
];
