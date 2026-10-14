use super::model::Engine;

pub fn quote_ident(engine: Engine, name: &str) -> String {
    let quote = match engine {
        Engine::Mysql | Engine::Mariadb => '`',
        Engine::Postgres | Engine::Sqlite => '"',
    };
    format!("{quote}{}{quote}", name.replace(quote, &format!("{quote}{quote}")))
}

pub fn qualified_table(engine: Engine, database: &str, schema: &str, table: &str) -> Result<String, String> {
    match engine {
        Engine::Sqlite => {
            if !database.is_empty() && database != "main" {
                return Err(format!("当前连接打开的是 main，没有另开连接修改 {database}"));
            }
            Ok(quote_ident(engine, table))
        }
        Engine::Mysql | Engine::Mariadb => {
            if database.is_empty() {
                return Err("需要数据库名，没有另开连接。".to_owned());
            }
            Ok(format!("{}.{}", quote_ident(engine, database), quote_ident(engine, table)))
        }
        Engine::Postgres => {
            if schema.is_empty() {
                return Err("PostgreSQL 需要指定 schema（表所在的分组名），没有另开连接。".to_owned());
            }
            Ok(format!("{}.{}", quote_ident(engine, schema), quote_ident(engine, table)))
        }
    }
}

pub fn sqlite_at_least(version: &str, major: u64, minor: u64, patch: u64) -> bool {
    let mut parts = version.split(['.', '-', ' ']);
    let got = [
        parts.next().unwrap_or("0").parse::<u64>().unwrap_or(0),
        parts.next().unwrap_or("0").parse::<u64>().unwrap_or(0),
        parts.next().unwrap_or("0").parse::<u64>().unwrap_or(0),
    ];
    got >= [major, minor, patch]
}

/// 默认值只接受字面量和少量当前时间函数，避免把用户输入拼成任意语句。
pub fn valid_default(engine: Engine, raw: &str) -> bool {
    let text = raw.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("null") {
        return text.eq_ignore_ascii_case("null");
    }
    if is_number(text) || is_quoted(text) {
        return true;
    }
    let upper = text.to_ascii_uppercase();
    if matches!(upper.as_str(), "CURRENT_TIMESTAMP" | "CURRENT_DATE" | "CURRENT_TIME" | "(CURRENT_TIMESTAMP)") {
        return true;
    }
    if engine == Engine::Postgres && matches!(upper.as_str(), "TRUE" | "FALSE" | "NOW()") {
        return true;
    }
    if matches!(engine, Engine::Mysql | Engine::Mariadb) && upper == "CURRENT_TIMESTAMP()" {
        return true;
    }
    false
}

pub fn non_null_default(raw: &str) -> bool {
    let text = raw.trim();
    !text.is_empty() && !text.eq_ignore_ascii_case("null")
}

pub fn valid_type(engine: Engine, raw: &str) -> bool {
    let text = raw.trim();
    if text.is_empty() || text.len() > 80 {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    if !lower.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '_' | '(' | ')' | ',' | '[' | ']')) {
        return false;
    }
    let base = lower.split('(').next().unwrap_or("").trim();
    match engine {
        Engine::Sqlite => !base.is_empty() && base.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic()),
        Engine::Postgres => POSTGRES_TYPES.iter().any(|item| *item == base),
        Engine::Mysql | Engine::Mariadb => MYSQL_TYPES.iter().any(|item| *item == base),
    }
}

pub fn valid_name(name: &str) -> bool {
    let text = name.trim();
    !text.is_empty() && text.len() <= 128 && !text.chars().any(|ch| ch.is_control() || ch == '\u{0}')
}

pub fn mentions(sql: &str, name: &str) -> bool {
    let needle = name.trim();
    if needle.is_empty() {
        return false;
    }
    let quoted = [
        format!("\"{}\"", needle.replace('"', "\"\"")),
        format!("`{}`", needle.replace('`', "``")),
        format!("[{needle}]"),
    ];
    if quoted.iter().any(|item| sql.contains(item)) {
        return true;
    }
    sql.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .any(|word| word.eq_ignore_ascii_case(needle))
}

fn is_number(value: &str) -> bool {
    let mut chars = value.chars().peekable();
    if chars.peek() == Some(&'+') || chars.peek() == Some(&'-') {
        chars.next();
    }
    let mut digits = 0;
    let mut dot = false;
    for ch in chars {
        if ch == '.' && !dot {
            dot = true;
            continue;
        }
        if ch.is_ascii_digit() {
            digits += 1;
            continue;
        }
        return false;
    }
    digits > 0
}

fn is_quoted(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'\'' || bytes[bytes.len() - 1] != b'\'' {
        return false;
    }
    let inner = &value[1..value.len() - 1];
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            if chars.next() != Some('\'') {
                return false;
            }
        }
    }
    true
}

const POSTGRES_TYPES: &[&str] = &[
    "smallint",
    "integer",
    "bigint",
    "int",
    "int2",
    "int4",
    "int8",
    "real",
    "double precision",
    "numeric",
    "decimal",
    "text",
    "varchar",
    "character varying",
    "char",
    "character",
    "boolean",
    "bool",
    "date",
    "timestamp",
    "timestamptz",
    "timestamp with time zone",
    "timestamp without time zone",
    "time",
    "time with time zone",
    "time without time zone",
    "json",
    "jsonb",
    "uuid",
    "bytea",
    "serial",
    "bigserial",
];

const MYSQL_TYPES: &[&str] = &[
    "tinyint",
    "smallint",
    "mediumint",
    "int",
    "integer",
    "bigint",
    "decimal",
    "numeric",
    "float",
    "double",
    "real",
    "bit",
    "char",
    "varchar",
    "binary",
    "varbinary",
    "tinytext",
    "text",
    "mediumtext",
    "longtext",
    "tinyblob",
    "blob",
    "mediumblob",
    "longblob",
    "date",
    "datetime",
    "timestamp",
    "time",
    "year",
    "json",
    "boolean",
    "bool",
];
