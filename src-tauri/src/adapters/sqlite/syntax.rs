//! SQLite 的 SQL 语法：标识符引号、保留字、网格投影、单库与执行计划。

use crate::adapters::SqlSyntax;
use crate::query::functions::FunctionSig;
use crate::query::plan::PlanNode;

pub(super) struct SqliteSyntax;

impl SqlSyntax for SqliteSyntax {
    fn quote_always(&self, name: &str) -> String {
        format!("\"{}\"", name.replace('"', "\"\""))
    }

    fn reserved_words(&self) -> &'static [&'static str] {
        RESERVED
    }

    fn text_cast(&self, ident: &str) -> String {
        format!("CAST({ident} AS TEXT)")
    }

    fn text_preview(&self, plain: &str, cap: usize) -> String {
        format!("CASE WHEN length(CAST({plain} AS BLOB)) > {cap} THEN char(1) || 't:' || length(CAST({plain} AS BLOB)) || ':' || substr({plain}, 1, {cap}) ELSE {plain} END")
    }

    fn binary_length(&self, ident: &str) -> String {
        format!("CASE WHEN {ident} IS NULL THEN NULL ELSE char(1) || 'b:' || length({ident}) END")
    }

    fn binary_hex(&self, ident: &str) -> String {
        format!("lower(hex({ident}))")
    }

    /// 普通表有 rowid：没有主键也能稳定分页。
    fn implicit_order(&self, kind: &str) -> Option<&'static str> {
        (kind == "table").then_some("rowid")
    }

    fn fixed_database(&self) -> Option<&'static str> {
        Some("main")
    }

    fn dialect_words(&self) -> &'static [&'static str] {
        &["PRAGMA", "VACUUM", "WITHOUT", "ROWID", "AUTOINCREMENT", "IFNULL", "GLOB"]
    }

    /// 只认 EXPLAIN QUERY PLAN；手写的 EXPLAIN（字节码）改成计划形态。
    fn explain(&self, body: &str) -> String {
        let upper = body.to_ascii_uppercase();
        if upper.starts_with("EXPLAIN") {
            if !upper.starts_with("EXPLAIN QUERY PLAN") {
                let rest = body.split_once(char::is_whitespace).map(|(_, rest)| rest).unwrap_or("");
                return format!("EXPLAIN QUERY PLAN {rest}");
            }
            return body.to_owned();
        }
        format!("EXPLAIN QUERY PLAN {body}")
    }

    fn parse_plan(&self, sql: &str, _explained: bool, columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Vec<PlanNode>> {
        super::plan::parse_plan(sql, columns, rows)
    }

    fn functions(&self) -> &'static [FunctionSig] {
        super::functions::SQLITE
    }

    fn system_object_prefixes(&self) -> &'static [&'static str] {
        &["sqlite_"]
    }

    fn statement_keywords(&self) -> &'static [&'static str] {
        &["PRAGMA"]
    }

    fn editor_object_listing(&self) -> Option<&'static str> {
        Some("SELECT name, type FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY name")
    }

    fn editor_column_listing(&self, table: &str) -> Option<String> {
        Some(format!("PRAGMA table_info({})", self.quote_ident(table)))
    }
}

/// SQLite 的全部关键字（保守：不区分可否作标识符）；另含 USER。按字母序。
const RESERVED: &[&str] = &[
    "ABORT", "ACTION", "ADD", "AFTER", "ALL", "ALTER", "ALWAYS", "ANALYZE", "AND", "AS", "ASC", "ATTACH", "AUTOINCREMENT",
    "BEFORE", "BEGIN", "BETWEEN", "BY", "CASCADE", "CASE", "CAST", "CHECK", "COLLATE", "COLUMN", "COMMIT", "CONFLICT",
    "CONSTRAINT", "CREATE", "CROSS", "CURRENT", "CURRENT_DATE", "CURRENT_TIME", "CURRENT_TIMESTAMP", "DATABASE", "DEFAULT",
    "DEFERRABLE", "DEFERRED", "DELETE", "DESC", "DETACH", "DISTINCT", "DO", "DROP", "EACH", "ELSE", "END", "ESCAPE", "EXCEPT",
    "EXCLUDE", "EXCLUSIVE", "EXISTS", "EXPLAIN", "FAIL", "FILTER", "FIRST", "FOLLOWING", "FOR", "FOREIGN", "FROM", "FULL",
    "GENERATED", "GLOB", "GROUP", "GROUPS", "HAVING", "IF", "IGNORE", "IMMEDIATE", "IN", "INDEX", "INDEXED", "INITIALLY",
    "INNER", "INSERT", "INSTEAD", "INTERSECT", "INTO", "IS", "ISNULL", "JOIN", "KEY", "LAST", "LEFT", "LIKE", "LIMIT", "MATCH",
    "MATERIALIZED", "NATURAL", "NO", "NOT", "NOTHING", "NOTNULL", "NULL", "NULLS", "OF", "OFFSET", "ON", "OR", "ORDER", "OTHERS",
    "OUTER", "OVER", "PARTITION", "PLAN", "PRAGMA", "PRECEDING", "PRIMARY", "QUERY", "RAISE", "RANGE", "RECURSIVE",
    "REFERENCES", "REGEXP", "REINDEX", "RELEASE", "RENAME", "REPLACE", "RESTRICT", "RETURNING", "RIGHT", "ROLLBACK", "ROW",
    "ROWS", "SAVEPOINT", "SELECT", "SET", "TABLE", "TEMP", "TEMPORARY", "THEN", "TIES", "TO", "TRANSACTION", "TRIGGER",
    "UNBOUNDED", "UNION", "UNIQUE", "UPDATE", "USER", "USING", "VACUUM", "VALUES", "VIEW", "VIRTUAL", "WHEN", "WHERE", "WINDOW",
    "WITH", "WITHOUT",
];
