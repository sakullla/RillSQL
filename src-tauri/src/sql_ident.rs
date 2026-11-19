//! 生成 SQL 时的标识符引用：按方言最小引号。
//!
//! 只有“确定安全”的名字才省略包裹符号：`[A-Za-z_][A-Za-z0-9_]*`（MySQL/MariaDB 另允许 `$`），
//! 不是该方言的保留字，PostgreSQL 还要求全小写（否则会被折叠成小写，指不回原对象）。
//! 任何不确定的情况都加引号。

use crate::connections::model::Engine;

/// 按需引号：安全标识符原样返回，其余按方言加引号并转义。
pub fn quote_ident(engine: Engine, name: &str) -> String {
    if is_bare(engine, name) {
        name.to_owned()
    } else {
        quote_always(engine, name)
    }
}

/// 总是加引号（方言引号字符，内部同字符加倍转义）。
pub fn quote_always(engine: Engine, name: &str) -> String {
    match engine {
        Engine::Mysql | Engine::Mariadb => format!("`{}`", name.replace('`', "``")),
        // T-SQL 的括号标识符转义是 ]]。
        Engine::Sqlserver => format!("[{}]", name.replace(']', "]]")),
        // Redis/MongoDB 没有 SQL 生成面，占位保持穷尽。
        Engine::Postgres | Engine::Sqlite | Engine::Redis | Engine::Mongodb => {
            format!("\"{}\"", name.replace('"', "\"\""))
        }
    }
}

/// 限定名：每段按需引号后用 `.` 连接，空段跳过。
pub fn qualified(engine: Engine, parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|part| !part.is_empty())
        .map(|part| quote_ident(engine, part))
        .collect::<Vec<_>>()
        .join(".")
}

/// 引擎以字符串给出时的按需引号；未知引擎按标准 SQL 双引号总是引用。
pub fn quote_ident_named(engine: &str, name: &str) -> String {
    match Engine::parse(engine) {
        Ok(engine) => quote_ident(engine, name),
        Err(_) => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

/// 名字能否不加引号直接写进该方言的 SQL。
pub fn is_bare(engine: Engine, name: &str) -> bool {
    let mysql = matches!(engine, Engine::Mysql | Engine::Mariadb);
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    // MySQL 词法把 `_binary`、`_utf8mb4` 等读作字符集引导符；下划线开头一律加引号。
    if mysql && first == '_' {
        return false;
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || (mysql && ch == '$')) {
        return false;
    }
    match engine {
        Engine::Postgres if name.chars().any(|ch| ch.is_ascii_uppercase()) => return false,
        Engine::Redis | Engine::Mongodb => return false,
        _ => {}
    }
    !is_reserved(engine, name)
}

/// 该方言的保留字（大小写不敏感）。
pub fn is_reserved(engine: Engine, name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let list: &[&str] = match engine {
        Engine::Mysql | Engine::Mariadb => MYSQL_RESERVED,
        Engine::Postgres => POSTGRES_RESERVED,
        Engine::Sqlite => SQLITE_RESERVED,
        Engine::Sqlserver => SQLSERVER_RESERVED,
        Engine::Redis | Engine::Mongodb => return true,
    };
    list.binary_search(&upper.as_str()).is_ok()
}

/// MySQL 8.x 保留字并入 MariaDB 保留字；另含 USER（保守起见）。按字母序。
const MYSQL_RESERVED: &[&str] = &[
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

/// PostgreSQL 保留字、可作函数/类型的保留字与不能作函数/类型的非保留字（列名类）。按字母序。
const POSTGRES_RESERVED: &[&str] = &[
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

/// SQLite 的全部关键字（保守：不区分可否作标识符）；另含 USER。按字母序。
const SQLITE_RESERVED: &[&str] = &[
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

/// SQL Server 保留关键字。按字母序。
const SQLSERVER_RESERVED: &[&str] = &[
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

#[cfg(test)]
mod tests {
    use super::*;

    const SQL_ENGINES: [Engine; 5] = [Engine::Mysql, Engine::Mariadb, Engine::Postgres, Engine::Sqlite, Engine::Sqlserver];

    #[test]
    fn reserved_tables_are_sorted_and_unique_for_binary_search() {
        for list in [MYSQL_RESERVED, POSTGRES_RESERVED, SQLITE_RESERVED, SQLSERVER_RESERVED] {
            for pair in list.windows(2) {
                assert!(pair[0] < pair[1], "{} 应排在 {} 之前", pair[0], pair[1]);
            }
        }
    }

    #[test]
    fn plain_names_stay_bare_in_every_dialect() {
        for engine in SQL_ENGINES {
            assert_eq!(quote_ident(engine, "content"), "content");
            assert_eq!(quote_ident(engine, "etoe_alarm_rule_set"), "etoe_alarm_rule_set");
            assert_eq!(quote_ident(engine, "e6yun3_etoe_business"), "e6yun3_etoe_business");
        }
        for engine in [Engine::Postgres, Engine::Sqlite, Engine::Sqlserver] {
            assert_eq!(quote_ident(engine, "_tmp1"), "_tmp1");
        }
    }

    #[test]
    fn mysql_family_quotes_leading_underscore_to_avoid_charset_introducers() {
        for engine in [Engine::Mysql, Engine::Mariadb] {
            for name in ["_binary", "_utf8mb4", "_latin1", "_tmp1", "_"] {
                assert_eq!(quote_ident(engine, name), format!("`{name}`"));
            }
            assert_eq!(quote_ident(engine, "tmp_1"), "tmp_1");
            assert_eq!(qualified(engine, &["db", "_binary"]), "db.`_binary`");
        }
    }

    #[test]
    fn reserved_words_keep_quotes_regardless_of_case() {
        for word in ["order", "user", "select", "SELECT", "Order"] {
            assert_eq!(quote_ident(Engine::Mysql, word), format!("`{word}`"));
            assert_eq!(quote_ident(Engine::Mariadb, word), format!("`{word}`"));
            assert_eq!(quote_ident(Engine::Postgres, word), format!("\"{word}\""));
            assert_eq!(quote_ident(Engine::Sqlite, word), format!("\"{word}\""));
            assert_eq!(quote_ident(Engine::Sqlserver, word), format!("[{word}]"));
        }
        assert_eq!(quote_ident(Engine::Mysql, "key"), "`key`");
        assert_eq!(quote_ident(Engine::Sqlserver, "key"), "[key]");
    }

    #[test]
    fn postgres_quotes_names_with_uppercase_only() {
        assert_eq!(quote_ident(Engine::Postgres, "User"), "\"User\"");
        assert_eq!(quote_ident(Engine::Postgres, "OrderItems"), "\"OrderItems\"");
        assert_eq!(quote_ident(Engine::Postgres, "order_items"), "order_items");
        assert_eq!(quote_ident(Engine::Mysql, "OrderItems"), "OrderItems");
        assert_eq!(quote_ident(Engine::Sqlite, "OrderItems"), "OrderItems");
        assert_eq!(quote_ident(Engine::Sqlserver, "OrderItems"), "OrderItems");
    }

    #[test]
    fn leading_digit_special_chars_and_unicode_are_quoted() {
        assert_eq!(quote_ident(Engine::Mysql, "1st"), "`1st`");
        assert_eq!(quote_ident(Engine::Sqlserver, "2024_sales"), "[2024_sales]");
        assert_eq!(quote_ident(Engine::Postgres, "9lives"), "\"9lives\"");
        assert_eq!(quote_ident(Engine::Mysql, "order items"), "`order items`");
        assert_eq!(quote_ident(Engine::Mysql, "a-b"), "`a-b`");
        assert_eq!(quote_ident(Engine::Mysql, "we`ird"), "`we``ird`");
        assert_eq!(quote_ident(Engine::Postgres, "we\"ird"), "\"we\"\"ird\"");
        assert_eq!(quote_ident(Engine::Sqlserver, "we]ird"), "[we]]ird]");
        assert_eq!(quote_ident(Engine::Sqlite, "a.b"), "\"a.b\"");
        assert_eq!(quote_ident(Engine::Mysql, "订单"), "`订单`");
        assert_eq!(quote_ident(Engine::Postgres, "café"), "\"café\"");
        assert_eq!(quote_ident(Engine::Sqlserver, "名字"), "[名字]");
        for engine in SQL_ENGINES {
            assert_ne!(quote_ident(engine, ""), "");
        }
    }

    #[test]
    fn dollar_is_bare_only_for_mysql_family() {
        assert_eq!(quote_ident(Engine::Mysql, "price$"), "price$");
        assert_eq!(quote_ident(Engine::Mariadb, "price$"), "price$");
        assert_eq!(quote_ident(Engine::Postgres, "price$"), "\"price$\"");
        assert_eq!(quote_ident(Engine::Mysql, "$price"), "`$price`");
    }

    #[test]
    fn quote_always_and_qualified() {
        assert_eq!(quote_always(Engine::Mysql, "content"), "`content`");
        assert_eq!(quote_always(Engine::Postgres, "content"), "\"content\"");
        assert_eq!(quote_always(Engine::Sqlserver, "content"), "[content]");
        assert_eq!(
            qualified(Engine::Mysql, &["e6yun3_etoe_business", "etoe_alarm_rule_set"]),
            "e6yun3_etoe_business.etoe_alarm_rule_set"
        );
        assert_eq!(qualified(Engine::Postgres, &["public", "User"]), "public.\"User\"");
        assert_eq!(qualified(Engine::Sqlserver, &["dbo", "order"]), "dbo.[order]");
        assert_eq!(qualified(Engine::Sqlite, &["", "orders"]), "orders");
    }

    #[test]
    fn named_engines_follow_the_same_rules() {
        assert_eq!(quote_ident_named("mysql", "content"), "content");
        assert_eq!(quote_ident_named("postgres", "User"), "\"User\"");
        assert_eq!(quote_ident_named("sqlserver", "order"), "[order]");
        assert_eq!(quote_ident_named("unknown", "content"), "\"content\"");
    }
}
