use super::model::Engine;

pub fn quote_ident(engine: Engine, name: &str) -> String {
    match engine {
        Engine::Mysql | Engine::Mariadb => {
            let quote = '`';
            format!("{quote}{}{quote}", name.replace(quote, &format!("{quote}{quote}")))
        }
        Engine::Postgres | Engine::Sqlite => {
            let quote = '"';
            format!("{quote}{}{quote}", name.replace(quote, &format!("{quote}{quote}")))
        }
        Engine::Sqlserver => format!("[{}]", name.replace(']', "]]")),
    }
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
        Engine::Sqlserver => {
            if schema.is_empty() {
                return Err("SQL Server 需要指定 schema（表所在的分组名），没有另开连接。".to_owned());
            }
            Ok(format!("{}.{}", quote_ident(engine, schema), quote_ident(engine, table)))
        }
    }
}

/// SQL Server 的默认值是具名约束。删除时优先用读到的 sys.default_constraints 约束名；
/// 名字缺失时按系统视图动态查找后删除，找不到就不发语句（不猜 DF_ 前缀名）。
/// `table` 是已加引号的限定名（如 [dbo].[orders]），`column` 是裸列名。
pub fn sqlserver_drop_default_sql(table: &str, column: &str, constraint: &str) -> String {
    let name = constraint.trim();
    if !name.is_empty() {
        return format!("ALTER TABLE {table} DROP CONSTRAINT [{}]", name.replace(']', "]]"));
    }
    let column_escaped = column.replace('\'', "''");
    let table_escaped = table.replace('\'', "''");
    format!(
        "DECLARE @dc sysname = (SELECT dc.name FROM sys.default_constraints dc JOIN sys.columns c ON c.object_id = dc.parent_object_id AND c.column_id = dc.parent_column_id WHERE dc.parent_object_id = OBJECT_ID(N'{table_escaped}') AND c.name = N'{column_escaped}'); IF @dc IS NOT NULL EXEC(N'ALTER TABLE {table_escaped} DROP CONSTRAINT ' + QUOTENAME(@dc))"
    )
}

/// 新增默认值：生成 DF_<表>_<列> 约束名（与 SQL Server 自动命名习惯一致，重名冲突由执行报错暴露）。
pub fn sqlserver_add_default_sql(table: &str, table_name: &str, column: &str, expression: &str) -> String {
    let constraint = format!("DF_{table_name}_{column}");
    format!(
        "ALTER TABLE {table} ADD CONSTRAINT [{}] DEFAULT {expression} FOR [{}]",
        constraint.replace(']', "]]"),
        column.replace(']', "]]")
    )
}

/// 从限定名 [dbo].[orders] 取末段裸表名，用于生成 DF_<表>_<列> 约束名。
pub fn sqlserver_bare_table_name(qualified: &str) -> String {
    qualified
        .rsplit('.')
        .next()
        .unwrap_or(qualified)
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned()
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
    // T-SQL 的 Unicode 字符串默认值 N'...'。
    if engine == Engine::Sqlserver
        && (text.starts_with("N'") || text.starts_with("n'"))
        && is_quoted(&text[1..])
    {
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
    // T-SQL 的 GETDATE()/SYSDATETIME() 是常见默认值；GETUTCDATE() 同属当前时间函数。
    if engine == Engine::Sqlserver && matches!(upper.as_str(), "GETDATE()" | "GETDATE" | "SYSDATETIME()" | "GETUTCDATE()" | "SYSUTCDATETIME()") {
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
        Engine::Sqlserver => SQLSERVER_TYPES.iter().any(|item| *item == base),
    }
}

const SQLSERVER_TYPES: &[&str] = &[
    "bigint", "int", "smallint", "tinyint", "bit", "decimal", "numeric", "money", "smallmoney",
    "float", "real", "date", "datetime", "datetime2", "datetimeoffset", "smalldatetime", "time",
    "char", "varchar", "text", "nchar", "nvarchar", "ntext", "binary", "varbinary", "image",
    "uniqueidentifier", "xml",
];

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
