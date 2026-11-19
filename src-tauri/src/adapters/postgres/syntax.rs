//! PostgreSQL 的 SQL 语法：标识符引号、保留字、网格投影、语句超时与执行计划。

use crate::adapters::SqlSyntax;
use crate::query::functions::FunctionSig;
use crate::query::plan::PlanNode;

pub(super) struct PostgresSyntax;

impl SqlSyntax for PostgresSyntax {
    fn quote_always(&self, name: &str) -> String {
        format!("\"{}\"", name.replace('"', "\"\""))
    }

    fn reserved_words(&self) -> &'static [&'static str] {
        RESERVED
    }

    /// 未加引号的名字被折叠成小写；含大写的名字必须加引号。
    fn folds_to_lowercase(&self) -> bool {
        true
    }

    fn contains_operator(&self) -> &'static str {
        "ILIKE"
    }

    fn count_expression(&self) -> &'static str {
        "COUNT(*)::bigint"
    }

    fn text_cast(&self, ident: &str) -> String {
        format!("{ident}::text")
    }

    fn text_preview(&self, plain: &str, cap: usize) -> String {
        format!("CASE WHEN octet_length({plain}) > {cap} THEN chr(1) || 't:' || octet_length({plain})::text || ':' || left({plain}, {cap}) ELSE {plain} END")
    }

    fn binary_length(&self, ident: &str) -> String {
        format!("CASE WHEN {ident} IS NULL THEN NULL ELSE chr(1) || 'b:' || octet_length({ident})::text END")
    }

    fn binary_hex(&self, ident: &str) -> String {
        format!("encode({ident}, 'hex')")
    }

    fn binary_literal(&self, hex: &str) -> String {
        format!("'\\x{hex}'")
    }

    fn timeout_statements(&self, ms: u64) -> Vec<String> {
        vec![format!("SET statement_timeout = {ms}")]
    }

    fn timeout_reset_statements(&self) -> Vec<String> {
        vec!["SET statement_timeout = 0".to_owned()]
    }

    fn dialect_words(&self) -> &'static [&'static str] {
        &["ILIKE", "RETURNING", "LATERAL", "SIMILAR", "TABLESAMPLE", "STRING_AGG"]
    }

    /// JSON 格式带结构化的代价、行数与条件，计划视图据此画树。
    fn explain(&self, body: &str) -> String {
        if body.to_ascii_uppercase().starts_with("EXPLAIN") {
            body.to_owned()
        } else {
            format!("EXPLAIN (FORMAT JSON) {body}")
        }
    }

    fn parse_plan(&self, _sql: &str, explained: bool, columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Vec<PlanNode>> {
        if !explained {
            return None;
        }
        super::plan::parse_postgres(columns, rows)
    }

    fn functions(&self) -> &'static [FunctionSig] {
        super::functions::POSTGRES
    }

    fn system_object_prefixes(&self) -> &'static [&'static str] {
        &["pg_"]
    }
}

/// PostgreSQL 保留字、可作函数/类型的保留字与不能作函数/类型的非保留字（列名类）。按字母序。
const RESERVED: &[&str] = &[
    "ALL", "ANALYSE", "ANALYZE", "AND", "ANY", "ARRAY", "AS", "ASC", "ASYMMETRIC", "AUTHORIZATION", "BETWEEN", "BIGINT",
    "BINARY", "BIT", "BOOLEAN", "BOTH", "CASE", "CAST", "CHAR", "CHARACTER", "CHECK", "COALESCE", "COLLATE", "COLLATION",
    "COLUMN", "CONCURRENTLY", "CONSTRAINT", "CREATE", "CROSS", "CURRENT_CATALOG", "CURRENT_DATE", "CURRENT_ROLE",
    "CURRENT_SCHEMA", "CURRENT_TIME", "CURRENT_TIMESTAMP", "CURRENT_USER", "DEC", "DECIMAL", "DEFAULT", "DEFERRABLE", "DESC",
    "DISTINCT", "DO", "ELSE", "END", "EXCEPT", "EXISTS", "EXTRACT", "FALSE", "FETCH", "FLOAT", "FOR", "FOREIGN", "FREEZE", "FROM",
    "FULL", "GRANT", "GREATEST", "GROUP", "GROUPING", "HAVING", "ILIKE", "IN", "INITIALLY", "INNER", "INOUT", "INT", "INTEGER",
    "INTERSECT", "INTERVAL", "INTO", "IS", "ISNULL", "JOIN", "JSON", "JSON_ARRAY", "JSON_ARRAYAGG", "JSON_EXISTS", "JSON_OBJECT",
    "JSON_OBJECTAGG", "JSON_QUERY", "JSON_SCALAR", "JSON_SERIALIZE", "JSON_TABLE", "JSON_VALUE", "LATERAL", "LEADING", "LEAST",
    "LEFT", "LIKE", "LIMIT", "LOCALTIME", "LOCALTIMESTAMP", "MERGE_ACTION", "NATIONAL", "NATURAL", "NCHAR", "NONE", "NORMALIZE",
    "NOT", "NOTNULL", "NULL", "NULLIF", "NUMERIC", "OFFSET", "ON", "ONLY", "OR", "ORDER", "OUT", "OUTER", "OVERLAPS", "OVERLAY",
    "PLACING", "POSITION", "PRECISION", "PRIMARY", "REAL", "REFERENCES", "RETURNING", "RIGHT", "ROW", "SELECT", "SESSION_USER",
    "SETOF", "SIMILAR", "SMALLINT", "SOME", "SUBSTRING", "SYMMETRIC", "SYSTEM_USER", "TABLE", "TABLESAMPLE", "THEN", "TIME",
    "TIMESTAMP", "TO", "TRAILING", "TREAT", "TRIM", "TRUE", "UNION", "UNIQUE", "USER", "USING", "VALUES", "VARCHAR", "VARIADIC",
    "VERBOSE", "WHEN", "WHERE", "WINDOW", "WITH", "XMLATTRIBUTES", "XMLCONCAT", "XMLELEMENT", "XMLEXISTS", "XMLFOREST",
    "XMLNAMESPACES", "XMLPARSE", "XMLPI", "XMLROOT", "XMLSERIALIZE", "XMLTABLE",
];
