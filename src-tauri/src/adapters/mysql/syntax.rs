//! MySQL 家族（MySQL 与 MariaDB 共用）的 SQL 语法：标识符引号、保留字、网格投影、
//! 词法、库切换、语句超时与执行计划。两者只在超时变量与计划格式上不同。

use crate::adapters::{Lexis, SqlSyntax};
use crate::query::exec::DatabaseGate;
use crate::query::functions::FunctionSig;
use crate::query::plan::PlanNode;

pub(in crate::adapters) struct MysqlSyntax {
    mariadb: bool,
}

pub(in crate::adapters) const MYSQL_SYNTAX: MysqlSyntax = MysqlSyntax { mariadb: false };
pub(in crate::adapters) const MARIADB_SYNTAX: MysqlSyntax = MysqlSyntax { mariadb: true };

impl SqlSyntax for MysqlSyntax {
    /// MySQL/MariaDB 用反引号，内部反引号加倍。
    fn quote_always(&self, name: &str) -> String {
        format!("`{}`", name.replace('`', "``"))
    }

    fn reserved_words(&self) -> &'static [&'static str] {
        RESERVED
    }

    fn allows_dollar(&self) -> bool {
        true
    }

    /// MySQL 词法把 `_binary`、`_utf8mb4` 等读作字符集引导符；下划线开头一律加引号。
    fn bare_leading_underscore(&self) -> bool {
        false
    }

    fn insert_defaults(&self, table: &str) -> String {
        format!("INSERT INTO {table} () VALUES ()")
    }

    fn text_cast(&self, ident: &str) -> String {
        format!("CAST({ident} AS CHAR)")
    }

    fn text_preview(&self, plain: &str, cap: usize) -> String {
        format!("CASE WHEN LENGTH({plain}) > {cap} THEN CONCAT(CHAR(1), 't:', LENGTH({plain}), ':', LEFT({plain}, {cap})) ELSE {plain} END")
    }

    fn binary_length(&self, ident: &str) -> String {
        format!("CASE WHEN {ident} IS NULL THEN NULL ELSE CONCAT(CHAR(1), 'b:', LENGTH({ident})) END")
    }

    fn binary_hex(&self, ident: &str) -> String {
        format!("LOWER(HEX({ident}))")
    }

    fn lexis(&self) -> Lexis {
        Lexis { hash_comments: true, transact: false, delimiter_command: true, executable_comments: true, backslash_escapes: true, backslash_ambiguous: true }
    }

    /// 同一条连接上用 USE 切到目标库。
    fn database_gate(&self, _requested: &str) -> DatabaseGate {
        DatabaseGate::Switch
    }

    /// MySQL 的 MAX_EXECUTION_TIME 只对 SELECT 生效；MariaDB 的单位是秒。
    fn timeout_statements(&self, ms: u64) -> Vec<String> {
        if self.mariadb {
            vec![format!("SET SESSION max_statement_time = {}", ms as f64 / 1000.0)]
        } else {
            vec![format!("SET SESSION MAX_EXECUTION_TIME = {ms}")]
        }
    }

    fn timeout_reset_statements(&self) -> Vec<String> {
        if self.mariadb {
            vec!["SET SESSION max_statement_time = 0".to_owned()]
        } else {
            vec!["SET SESSION MAX_EXECUTION_TIME = 0".to_owned()]
        }
    }

    fn dialect_words(&self) -> &'static [&'static str] {
        &["REGEXP", "STRAIGHT_JOIN", "IFNULL", "GROUP_CONCAT", "UNSIGNED"]
    }

    /// MySQL 8 用 TREE 格式；MariaDB 没有 TREE，用 JSON。
    fn explain(&self, body: &str) -> String {
        if body.to_ascii_uppercase().starts_with("EXPLAIN") {
            body.to_owned()
        } else if self.mariadb {
            format!("EXPLAIN FORMAT=JSON {body}")
        } else {
            format!("EXPLAIN FORMAT=TREE {body}")
        }
    }

    fn parse_plan(&self, _sql: &str, explained: bool, columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Vec<PlanNode>> {
        if !explained {
            return None;
        }
        super::plan::parse_mysql_family(columns, rows)
    }

    fn functions(&self) -> &'static [FunctionSig] {
        super::functions::MYSQL
    }
}

/// MySQL 8.x 保留字并入 MariaDB 保留字；另含 USER（保守起见）。按字母序。
const RESERVED: &[&str] = &[
    "ACCESSIBLE", "ADD", "ALL", "ALTER", "ANALYZE", "AND", "AS", "ASC", "ASENSITIVE", "BEFORE", "BETWEEN", "BIGINT", "BINARY",
    "BLOB", "BOTH", "BY", "CALL", "CASCADE", "CASE", "CHANGE", "CHAR", "CHARACTER", "CHECK", "COLLATE", "COLUMN", "CONDITION",
    "CONSTRAINT", "CONTINUE", "CONVERT", "CREATE", "CROSS", "CUBE", "CUME_DIST", "CURRENT_DATE", "CURRENT_ROLE", "CURRENT_TIME",
    "CURRENT_TIMESTAMP", "CURRENT_USER", "CURSOR", "DATABASE", "DATABASES", "DAY_HOUR", "DAY_MICROSECOND", "DAY_MINUTE",
    "DAY_SECOND", "DEC", "DECIMAL", "DECLARE", "DEFAULT", "DELAYED", "DELETE", "DELETE_DOMAIN_ID", "DENSE_RANK", "DESC",
    "DESCRIBE", "DETERMINISTIC", "DISTINCT", "DISTINCTROW", "DIV", "DOUBLE", "DO_DOMAIN_IDS", "DROP", "DUAL", "EACH", "ELSE",
    "ELSEIF", "EMPTY", "ENCLOSED", "ESCAPED", "EXCEPT", "EXISTS", "EXIT", "EXPLAIN", "FALSE", "FETCH", "FIRST_VALUE", "FLOAT",
    "FLOAT4", "FLOAT8", "FOR", "FORCE", "FOREIGN", "FROM", "FULLTEXT", "FUNCTION", "GENERAL", "GENERATED", "GET", "GRANT",
    "GROUP", "GROUPING", "GROUPS", "HAVING", "HIGH_PRIORITY", "HOUR_MICROSECOND", "HOUR_MINUTE", "HOUR_SECOND", "IF", "IGNORE",
    "IGNORE_DOMAIN_IDS", "IGNORE_SERVER_IDS", "IN", "INDEX", "INFILE", "INNER", "INOUT", "INSENSITIVE", "INSERT", "INT", "INT1",
    "INT2", "INT3", "INT4", "INT8", "INTEGER", "INTERSECT", "INTERVAL", "INTO", "IO_AFTER_GTIDS", "IO_BEFORE_GTIDS", "IS",
    "ITERATE", "JOIN", "JSON_TABLE", "KEY", "KEYS", "KILL", "LAG", "LAST_VALUE", "LATERAL", "LEAD", "LEADING", "LEAVE", "LEFT",
    "LIKE", "LIMIT", "LINEAR", "LINES", "LOAD", "LOCALTIME", "LOCALTIMESTAMP", "LOCK", "LONG", "LONGBLOB", "LONGTEXT", "LOOP",
    "LOW_PRIORITY", "MANUAL", "MASTER_BIND", "MASTER_HEARTBEAT_PERIOD", "MASTER_SSL_VERIFY_SERVER_CERT", "MATCH", "MAXVALUE",
    "MEDIUMBLOB", "MEDIUMINT", "MEDIUMTEXT", "MIDDLEINT", "MINUTE_MICROSECOND", "MINUTE_SECOND", "MOD", "MODIFIES", "NATURAL",
    "NOT", "NO_WRITE_TO_BINLOG", "NTH_VALUE", "NTILE", "NULL", "NUMERIC", "OF", "OFFSET", "ON", "OPTIMIZE", "OPTIMIZER_COSTS",
    "OPTION", "OPTIONALLY", "OR", "ORDER", "OUT", "OUTER", "OUTFILE", "OVER", "PAGE_CHECKSUM", "PARALLEL", "PARSE_VCOL_EXPR",
    "PARTITION", "PERCENT_RANK", "POSITION", "PRECISION", "PRIMARY", "PROCEDURE", "PURGE", "QUALIFY", "RANGE", "RANK", "READ",
    "READS", "READ_WRITE", "REAL", "RECURSIVE", "REFERENCES", "REF_SYSTEM_ID", "REGEXP", "RELEASE", "RENAME", "REPEAT",
    "REPLACE", "REQUIRE", "RESIGNAL", "RESTRICT", "RETURN", "RETURNING", "REVOKE", "RIGHT", "RLIKE", "ROW", "ROWS",
    "ROW_NUMBER", "SCHEMA", "SCHEMAS", "SECOND_MICROSECOND", "SELECT", "SENSITIVE", "SEPARATOR", "SET", "SHOW", "SIGNAL",
    "SLOW", "SMALLINT", "SPATIAL", "SPECIFIC", "SQL", "SQLEXCEPTION", "SQLSTATE", "SQLWARNING", "SQL_BIG_RESULT",
    "SQL_CALC_FOUND_ROWS", "SQL_SMALL_RESULT", "SSL", "STARTING", "STATS_AUTO_RECALC", "STATS_PERSISTENT", "STATS_SAMPLE_PAGES",
    "STORED", "STRAIGHT_JOIN", "SYSTEM", "TABLE", "TABLESAMPLE", "TERMINATED", "THEN", "TINYBLOB", "TINYINT", "TINYTEXT", "TO",
    "TRAILING", "TRIGGER", "TRUE", "UNDO", "UNION", "UNIQUE", "UNLOCK", "UNSIGNED", "UPDATE", "USAGE", "USE", "USER", "USING",
    "UTC_DATE", "UTC_TIME", "UTC_TIMESTAMP", "VALUES", "VARBINARY", "VARCHAR", "VARCHARACTER", "VARYING", "VIRTUAL", "WHEN",
    "WHERE", "WHILE", "WINDOW", "WITH", "WRITE", "XOR", "YEAR_MONTH", "ZEROFILL",
];
