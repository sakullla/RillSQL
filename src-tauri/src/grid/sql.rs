use super::store::{Cell, Column, Engine, Filter, FilterOp, Sort, TableSchema};

pub fn quote_ident(engine: Engine, name: &str) -> String {
    let quote = match engine {
        Engine::Mysql | Engine::Mariadb => '`',
        Engine::Postgres | Engine::Sqlite => '"',
    };
    format!("{quote}{}{quote}", name.replace(quote, &format!("{quote}{quote}")))
}

pub fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn is_numeric(type_name: &str) -> bool {
    let lower = type_name.to_ascii_lowercase();
    ["int", "numeric", "decimal", "real", "double", "float", "serial", "number"]
        .iter()
        .any(|token| lower.contains(token))
}

fn number_literal(value: &str) -> bool {
    let mut chars = value.chars().peekable();
    if chars.peek() == Some(&'-') {
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

pub fn sql_cell(engine: Engine, column: Option<&Column>, value: &Cell) -> String {
    match value {
        Cell::Null => "NULL".to_owned(),
        Cell::Text(text) => match column.map(|item| value_family(&item.type_name)) {
            Some(ValueFamily::Bool) => bool_literal(text).unwrap_or_else(|| quote_literal(text)),
            Some(ValueFamily::Binary) => parse_hex(text).map(|hex| binary_literal(engine, &hex)).unwrap_or_else(|| quote_literal(text)),
            _ if column.is_some_and(|item| is_numeric(&item.type_name) && number_literal(text)) => text.clone(),
            _ => quote_literal(text),
        },
    }
}

pub fn reject_literal(column: &Column, value: &Cell) -> Option<String> {
    let family = value_family(&column.type_name);
    match family {
        ValueFamily::Unknown => Some("类型未知，不能编辑".to_owned()),
        ValueFamily::Geometry => Some("几何类型不能从格子保存".to_owned()),
        ValueFamily::Raw => Some("不能判断写回方式，不能编辑".to_owned()),
        _ => match value {
            Cell::Null => None,
            Cell::Text(text) if family == ValueFamily::Binary && parse_hex(text).is_none() => Some("这不是十六进制".to_owned()),
            Cell::Text(text) if family == ValueFamily::Bool && bool_literal(text).is_none() => Some("不能写成布尔字面量".to_owned()),
            Cell::Text(_) => None,
        },
    }
}

fn like_literal(value: &str, contains: bool) -> String {
    let escaped = value.replace('!', "!!").replace('%', "!%").replace('_', "!_").replace('\'', "''");
    if contains {
        format!("'%{escaped}%'")
    } else {
        format!("'{escaped}%'")
    }
}

pub fn filter_clause(engine: Engine, column: &Column, filter: &Filter) -> String {
    let ident = quote_ident(engine, &column.name);
    let raw = filter.value.as_deref().unwrap_or("");
    match filter.op {
        FilterOp::Eq => format!("{ident} = {}", sql_cell(engine, Some(column), &Cell::Text(raw.to_owned()))),
        FilterOp::Ne => format!("{ident} <> {}", sql_cell(engine, Some(column), &Cell::Text(raw.to_owned()))),
        FilterOp::Contains => {
            let op = if engine == Engine::Postgres { "ILIKE" } else { "LIKE" };
            format!("{ident} {op} {} ESCAPE '!'", like_literal(raw, true))
        }
        FilterOp::Starts => format!("{ident} LIKE {} ESCAPE '!'", like_literal(raw, false)),
        FilterOp::Gt => format!("{ident} > {}", sql_cell(engine, Some(column), &Cell::Text(raw.to_owned()))),
        FilterOp::Lt => format!("{ident} < {}", sql_cell(engine, Some(column), &Cell::Text(raw.to_owned()))),
        FilterOp::Null => format!("{ident} IS NULL"),
        FilterOp::NotNull => format!("{ident} IS NOT NULL"),
        FilterOp::Empty => format!("{ident} = ''"),
    }
}

pub fn qualified_name(schema: &TableSchema) -> String {
    let table = quote_ident(schema.engine, &schema.name);
    if schema.schema.is_empty() {
        table
    } else {
        format!("{}.{}", quote_ident(schema.engine, &schema.schema), table)
    }
}

pub fn compile_where(schema: &TableSchema, filters: &[Filter]) -> Result<String, String> {
    let mut parts = Vec::new();
    for filter in filters {
        let Some(column) = schema.columns.iter().find(|column| column.name == filter.column) else {
            return Err(format!("找不到列 {}，不能拼进筛选条件", filter.column));
        };
        if filter.op.needs_value() && filter.value.as_deref().unwrap_or("").is_empty() {
            return Err("筛选值不能为空".to_owned());
        }
        parts.push(filter_clause(schema.engine, column, filter));
    }
    Ok(parts.join(" AND "))
}

pub fn browse_sql(schema: &TableSchema, filters: &[Filter], sort: Option<&Sort>, limit: u64, offset: u64) -> Result<String, String> {
    let where_sql = compile_where(schema, filters)?;
    let clause = if where_sql.is_empty() { String::new() } else { format!(" WHERE {where_sql}") };
    let order = sort
        .map(|item| format!(" ORDER BY {} {}", quote_ident(schema.engine, &item.column), if item.descending { "DESC" } else { "ASC" }))
        .unwrap_or_default();
    let tail = if offset == 0 { format!(" LIMIT {limit}") } else { format!(" LIMIT {limit} OFFSET {offset}") };
    Ok(format!("SELECT * FROM {}{clause}{order}{tail}", qualified_name(schema)))
}

pub fn select_export_gate(sql: &str) -> Result<String, String> {
    select_export_gate_for("postgres", sql)
}

/// 导出闸门走 query 的语句扫描，并带上引擎。
pub fn select_export_gate_for(engine: &str, sql: &str) -> Result<String, String> {
    let report = crate::query::split_sql(engine, sql);
    if let Some(reason) = report.refusal {
        return Err(reason);
    }
    let statements = report
        .pieces
        .into_iter()
        .map(|piece| piece.text.trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    if statements.is_empty() {
        return Err("没有可导出的语句".to_owned());
    }
    if statements.len() > 1 {
        return Err("完整导出只接受单条 SELECT 语句".to_owned());
    }
    let statement = statements[0].clone();
    let keyword = statement.chars().take_while(|ch| ch.is_ascii_alphabetic()).collect::<String>().to_ascii_uppercase();
    if keyword != "SELECT" {
        return Err("只有 SELECT 可以重新执行并完整导出".to_owned());
    }
    Ok(statement)
}

/// 网格不再保留自己的词法，只转发 query::split_sql。
pub fn split_sql(engine: &str, sql: &str) -> Vec<String> {
    crate::query::split_sql(engine, sql)
        .pieces
        .into_iter()
        .map(|piece| piece.text)
        .filter(|text| !text.trim().is_empty())
        .collect()
}

pub fn update_sql(schema: &TableSchema, sets: &[(String, Cell)], locator: &[(String, Cell)]) -> String {
    let assignments = sets
        .iter()
        .map(|(name, value)| {
            let column = schema.columns.iter().find(|column| column.name == *name);
            format!("{} = {}", quote_ident(schema.engine, name), sql_cell(schema.engine, column, value))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("UPDATE {} SET {assignments} WHERE {}", qualified_name(schema), locator_sql(schema, locator))
}

pub fn delete_sql(schema: &TableSchema, locator: &[(String, Cell)]) -> String {
    format!("DELETE FROM {} WHERE {}", qualified_name(schema), locator_sql(schema, locator))
}

pub fn insert_sql(schema: &TableSchema, values: &[(String, Cell)]) -> String {
    if values.is_empty() {
        return if matches!(schema.engine, Engine::Mysql | Engine::Mariadb) {
            format!("INSERT INTO {} () VALUES ()", qualified_name(schema))
        } else {
            format!("INSERT INTO {} DEFAULT VALUES", qualified_name(schema))
        };
    }
    let names = values.iter().map(|(name, _)| quote_ident(schema.engine, name)).collect::<Vec<_>>().join(", ");
    let literals = values
        .iter()
        .map(|(name, value)| sql_cell(schema.engine, schema.columns.iter().find(|column| column.name == *name), value))
        .collect::<Vec<_>>()
        .join(", ");
    format!("INSERT INTO {} ({names}) VALUES ({literals})", qualified_name(schema))
}

pub fn reads_full_values(schema: &TableSchema, filters: &[Filter]) -> bool {
    if schema.kind != "table" || schema.key.is_empty() {
        return true;
    }
    schema.key.iter().all(|name| {
        filters.iter().any(|filter| filter.column == *name && filter.op == FilterOp::Eq && filter.value.as_deref().is_some_and(|value| !value.is_empty()))
    })
}

/// 能定位的表行只投影预览和字节数。整数等短列仍用原来的 ::text / CAST。
pub fn select_list(schema: &TableSchema, full: bool) -> String {
    schema.columns.iter().map(|column| project_column(schema, column, full)).collect::<Vec<_>>().join(", ")
}

fn project_column(schema: &TableSchema, column: &Column, full: bool) -> String {
    let ident = quote_ident(schema.engine, &column.name);
    let plain = plain_cast(schema.engine, &ident);
    match value_family(&column.type_name) {
        ValueFamily::Binary if full || schema.key.is_empty() => binary_hex(schema.engine, &ident),
        ValueFamily::Binary => {
            let guard = null_key_guard(schema);
            format!("CASE WHEN {guard} THEN {} ELSE {} END", binary_hex(schema.engine, &ident), binary_length(schema.engine, &ident))
        }
        ValueFamily::Text if !full && !schema.key.is_empty() => {
            let guard = null_key_guard(schema);
            format!("CASE WHEN {guard} THEN {plain} ELSE {} END", text_preview(schema.engine, &plain))
        }
        _ => plain,
    }
}

fn null_key_guard(schema: &TableSchema) -> String {
    schema.key.iter().map(|name| format!("{} IS NULL", quote_ident(schema.engine, name))).collect::<Vec<_>>().join(" OR ")
}

fn plain_cast(engine: Engine, ident: &str) -> String {
    match engine {
        Engine::Postgres => format!("{ident}::text"),
        Engine::Mysql | Engine::Mariadb => format!("CAST({ident} AS CHAR)"),
        Engine::Sqlite => format!("CAST({ident} AS TEXT)"),
    }
}

fn text_preview(engine: Engine, plain: &str) -> String {
    match engine {
        Engine::Postgres => format!("CASE WHEN octet_length({plain}) > 1024 THEN chr(1) || 't:' || octet_length({plain})::text || ':' || left({plain}, 1024) ELSE {plain} END"),
        Engine::Mysql | Engine::Mariadb => format!("CASE WHEN LENGTH({plain}) > 1024 THEN CONCAT(CHAR(1), 't:', LENGTH({plain}), ':', LEFT({plain}, 1024)) ELSE {plain} END"),
        Engine::Sqlite => format!("CASE WHEN length(CAST({plain} AS BLOB)) > 1024 THEN char(1) || 't:' || length(CAST({plain} AS BLOB)) || ':' || substr({plain}, 1, 1024) ELSE {plain} END"),
    }
}

fn binary_length(engine: Engine, ident: &str) -> String {
    match engine {
        Engine::Postgres => format!("CASE WHEN {ident} IS NULL THEN NULL ELSE chr(1) || 'b:' || octet_length({ident})::text END"),
        Engine::Mysql | Engine::Mariadb => format!("CASE WHEN {ident} IS NULL THEN NULL ELSE CONCAT(CHAR(1), 'b:', LENGTH({ident})) END"),
        Engine::Sqlite => format!("CASE WHEN {ident} IS NULL THEN NULL ELSE char(1) || 'b:' || length({ident}) END"),
    }
}

fn binary_hex(engine: Engine, ident: &str) -> String {
    let hex = match engine {
        Engine::Postgres => format!("encode({ident}, 'hex')"),
        Engine::Mysql | Engine::Mariadb => format!("LOWER(HEX({ident}))"),
        Engine::Sqlite => format!("lower(hex({ident}))"),
    };
    format!("CASE WHEN {ident} IS NULL THEN NULL ELSE {hex} END")
}

fn locator_sql(schema: &TableSchema, locator: &[(String, Cell)]) -> String {
    locator
        .iter()
        .map(|(name, value)| {
            let column = schema.columns.iter().find(|column| column.name == *name);
            format!("{} = {}", quote_ident(schema.engine, name), sql_cell(schema.engine, column, value))
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueFamily {
    Unknown,
    Bool,
    Integer,
    Decimal,
    Float,
    Date,
    Time,
    Datetime,
    Binary,
    Geometry,
    Text,
    Raw,
}

fn value_family(type_name: &str) -> ValueFamily {
    let name = base_type(type_name);
    if name.is_empty() || name == "类型未知" {
        return ValueFamily::Unknown;
    }
    if name == "bool" || name == "boolean" {
        return ValueFamily::Bool;
    }
    if is_binary_name(&name) {
        return ValueFamily::Binary;
    }
    if is_geometry_name(&name) {
        return ValueFamily::Geometry;
    }
    if name == "date" {
        return ValueFamily::Date;
    }
    if name == "datetime" || name == "timestamptz" || name.starts_with("timestamp") {
        return ValueFamily::Datetime;
    }
    if name == "time" || name.starts_with("time ") {
        return ValueFamily::Time;
    }
    if is_integer_name(&name) {
        return ValueFamily::Integer;
    }
    if name == "numeric" || name == "decimal" || name == "number" {
        return ValueFamily::Decimal;
    }
    if is_float_name(&name) {
        return ValueFamily::Float;
    }
    if is_text_name(&name) {
        return ValueFamily::Text;
    }
    ValueFamily::Raw
}

fn base_type(type_name: &str) -> String {
    let lower = type_name.trim().to_ascii_lowercase();
    let mut without = String::new();
    let mut depth = 0i32;
    for ch in lower.chars() {
        if ch == '(' {
            depth += 1;
            continue;
        }
        if ch == ')' {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth == 0 {
            without.push(ch);
        }
    }
    without.split_whitespace().filter(|part| !matches!(*part, "unsigned" | "signed" | "zerofill")).collect::<Vec<_>>().join(" ")
}

fn bool_literal(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "t" | "true" | "1" | "yes" => Some("TRUE".to_owned()),
        "f" | "false" | "0" | "no" => Some("FALSE".to_owned()),
        _ => None,
    }
}

fn parse_hex(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let text = if let Some(rest) = trimmed.strip_prefix("\\x").or_else(|| trimmed.strip_prefix("\\X")) {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        rest
    } else if (trimmed.starts_with("x'") || trimmed.starts_with("X'")) && trimmed.ends_with('\'') {
        &trimmed[2..trimmed.len() - 1]
    } else {
        trimmed
    };
    if text.is_empty() {
        return Some(String::new());
    }
    if text.len() % 2 != 0 || !text.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    Some(text.to_ascii_lowercase())
}

fn binary_literal(engine: Engine, hex: &str) -> String {
    match engine {
        Engine::Postgres => format!("'\\x{hex}'"),
        Engine::Mysql | Engine::Mariadb | Engine::Sqlite => format!("X'{hex}'"),
    }
}

fn is_binary_name(name: &str) -> bool {
    matches!(name, "bytea" | "blob" | "binary" | "varbinary" | "image" | "raw" | "byte") || name.ends_with("blob")
}

fn is_geometry_name(name: &str) -> bool {
    matches!(name, "geometry" | "geography" | "point" | "line" | "lseg" | "box" | "path" | "polygon" | "circle" | "multipoint" | "multilinestring" | "multipolygon" | "geometrycollection")
        || name.starts_with("geometry ")
        || name.starts_with("geography ")
}

fn is_integer_name(name: &str) -> bool {
    matches!(name, "tinyint" | "smallint" | "mediumint" | "bigint" | "integer" | "int" | "int2" | "int4" | "int8" | "serial" | "bigserial" | "smallserial" | "year")
}

fn is_float_name(name: &str) -> bool {
    matches!(name, "real" | "float" | "float4" | "float8" | "double" | "double precision") || name.starts_with("float ") || name.starts_with("double ")
}

fn is_text_name(name: &str) -> bool {
    matches!(
        name,
        "text" | "tinytext" | "mediumtext" | "longtext" | "clob" | "nclob" | "varchar" | "nvarchar" | "char" | "nchar" | "character" | "character varying" | "varchar2" | "nvarchar2" | "string" | "json" | "jsonb" | "uuid" | "enum" | "set" | "xml" | "citext" | "name" | "bpchar" | "varying"
    ) || name.starts_with("character ")
}
