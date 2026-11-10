use super::store::{Cell, Column, Engine, Filter, FilterOp, Sort, TableSchema};

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
        // T-SQL 的括号标识符转义是 ]]。
        Engine::Sqlserver => format!("[{}]", name.replace(']', "]]")),
        // Redis 没有 SQL 表网格；TableSchema 不会带这个引擎（live::describe 已拒绝）。
        Engine::Redis | Engine::Mongodb => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

pub fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// T-SQL 的 Unicode 字符串字面量。其余引擎与 quote_literal 相同。
pub fn text_literal(engine: Engine, value: &str) -> String {
    if engine == Engine::Sqlserver {
        format!("N'{}'", value.replace('\'', "''"))
    } else {
        quote_literal(value)
    }
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
            Some(ValueFamily::Bool) => bool_literal(text).unwrap_or_else(|| text_literal(engine, text)),
            Some(ValueFamily::Binary) => parse_hex(text).map(|hex| binary_literal(engine, &hex)).unwrap_or_else(|| text_literal(engine, text)),
            _ if column.is_some_and(|item| is_numeric(&item.type_name) && number_literal(text)) => text.clone(),
            _ => text_literal(engine, text),
        },
    }
}

pub fn reject_literal(column: &Column, value: &Cell) -> Option<String> {
    reject_literal_with(column, value, false)
}

/// resolved 为真表示这段文本来自已经完成的主键另读，即使形状像预览也是原值。
pub fn reject_literal_with(column: &Column, value: &Cell, resolved: bool) -> Option<String> {
    let family = value_family(&column.type_name);
    match family {
        ValueFamily::Unknown => Some("类型未知，不能编辑".to_owned()),
        ValueFamily::Geometry => Some("几何类型不能从格子保存".to_owned()),
        ValueFamily::Raw => Some("不能判断写回方式，不能编辑".to_owned()),
        _ => match value {
            Cell::Null => None,
            Cell::Text(text) if !resolved && blocks_preview(family, text) => Some("预览内容不能写回".to_owned()),
            Cell::Text(text) if family == ValueFamily::Binary && parse_hex(text).is_none() => Some("这不是十六进制".to_owned()),
            Cell::Text(text) if family == ValueFamily::Bool && bool_literal(text).is_none() => Some("不能写成布尔字面量".to_owned()),
            Cell::Text(_) => None,
        },
    }
}

/// 二进制预览，或页查询才会发出的长文本预览。短文本碰巧写成协议形状时不是预览。
fn blocks_preview(family: ValueFamily, value: &str) -> bool {
    if family == ValueFamily::Binary {
        return is_preview_envelope(value);
    }
    is_synthetic_text_preview(value)
}

/// 页查询只在文本超过 1024 字节时包信封，预览最多 1024 个字符，因此整段一定长于 1024 字节。
fn is_synthetic_text_preview(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('\u{1}') else {
        return false;
    };
    let Some(body) = rest.strip_prefix("t:") else {
        return false;
    };
    let Some((count, preview)) = body.split_once(':') else {
        return false;
    };
    let Ok(bytes) = count.parse::<u64>() else {
        return false;
    };
    // 信封本身是协议标记：只要前缀、数字字节数和分隔冒号成立就视为预览，
    // 不再用字节数与字符数比较，避免多字节文本被误判为可写回。
    let _ = preview;
    // 小于 1 KB 的 t: 文本可能是用户字面量；协议只在大值上生成信封。
    bytes > 1024
}

/// 与前端预览协议一致：U+0001 开头的 t:字节数:预览 或 b:字节数。
fn is_preview_envelope(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('\u{1}') else {
        return false;
    };
    if let Some(body) = rest.strip_prefix("b:") {
        return !body.is_empty() && body.bytes().all(|byte| byte.is_ascii_digit());
    }
    if let Some(body) = rest.strip_prefix("t:") {
        let Some((count, _)) = body.split_once(':') else {
            return false;
        };
        return !count.is_empty() && count.bytes().all(|byte| byte.is_ascii_digit());
    }
    false
}

fn like_literal(engine: Engine, value: &str, contains: bool) -> String {
    let escaped = value.replace('!', "!!").replace('%', "!%").replace('_', "!_").replace('\'', "''");
    let prefix = if engine == Engine::Sqlserver { "N" } else { "" };
    if contains {
        format!("{prefix}'%{escaped}%'")
    } else {
        format!("{prefix}'{escaped}%'")
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
            format!("{ident} {op} {} ESCAPE '!'", like_literal(engine, raw, true))
        }
        FilterOp::Starts => format!("{ident} LIKE {} ESCAPE '!'", like_literal(engine, raw, false)),
        FilterOp::Gt => format!("{ident} > {}", sql_cell(engine, Some(column), &Cell::Text(raw.to_owned()))),
        FilterOp::Lt => format!("{ident} < {}", sql_cell(engine, Some(column), &Cell::Text(raw.to_owned()))),
        FilterOp::Between => {
            let high = filter.second.as_deref().unwrap_or("");
            format!("{ident} BETWEEN {} AND {}", sql_cell(engine, Some(column), &Cell::Text(raw.to_owned())), sql_cell(engine, Some(column), &Cell::Text(high.to_owned())))
        }
        FilterOp::In => {
            let list = filter.values.iter().map(|item| sql_cell(engine, Some(column), &Cell::Text(item.clone()))).collect::<Vec<_>>().join(", ");
            format!("{ident} IN ({list})")
        }
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
        if filter.op == FilterOp::Between {
            if filter.value.as_deref().unwrap_or("").is_empty() || filter.second.as_deref().unwrap_or("").is_empty() {
                return Err("between 需要两个值".to_owned());
            }
        } else if filter.op == FilterOp::In {
            if filter.values.is_empty() {
                return Err("IN 筛选至少要一个值".to_owned());
            }
        } else if filter.op.needs_value() && filter.value.as_deref().unwrap_or("").is_empty() {
            return Err("筛选值不能为空".to_owned());
        }
        parts.push(filter_clause(schema.engine, column, filter));
    }
    Ok(parts.join(" AND "))
}

/// 表数据网格的整表关键字搜索（R3）：跨可作为文本匹配的列生成 OR-LIKE。
/// `%`/`_`/`!` 经现有 like_literal 的 ESCAPE '!' 字面转义；postgres 用 ILIKE、其余引擎 LIKE，与筛选 contains 同方言。
/// 二进制、几何与类型未知的列不参与；全部列被排除时给出恒假子句，让“无匹配”空态仍然成立。
/// 关键字去空白后为空时返回 None，表示没有搜索。
pub fn search_clause(engine: Engine, columns: &[Column], keyword: &str) -> Option<String> {
    let keyword = keyword.trim();
    if keyword.is_empty() {
        return None;
    }
    let op = if engine == Engine::Postgres { "ILIKE" } else { "LIKE" };
    let pattern = like_literal(engine, keyword, true);
    let parts = columns
        .iter()
        .filter(|column| searchable_family(&column.type_name))
        .map(|column| format!("{} {op} {pattern} ESCAPE '!'", plain_cast(engine, &quote_ident(engine, &column.name))))
        .collect::<Vec<_>>();
    Some(if parts.is_empty() { "1 = 0".to_owned() } else { format!("({})", parts.join(" OR ")) })
}

/// 与网格投影同源的文本形态判定：二进制、几何与未知类型之外的列都能按文本匹配（数字/日期等先转文本再比）。
fn searchable_family(type_name: &str) -> bool {
    !matches!(value_family(type_name), ValueFamily::Binary | ValueFamily::Geometry | ValueFamily::Unknown)
}

/// compile_where 的搜索叠加形态：筛选条件与整表关键字 AND 组合，分页与计数共用。
pub fn compile_where_with_search(schema: &TableSchema, filters: &[Filter], search: Option<&str>) -> Result<String, String> {
    let mut where_sql = compile_where(schema, filters)?;
    if let Some(clause) = search.and_then(|keyword| search_clause(schema.engine, &schema.columns, keyword)) {
        if where_sql.is_empty() {
            where_sql = clause;
        } else {
            where_sql = format!("{where_sql} AND {clause}");
        }
    }
    Ok(where_sql)
}

pub fn browse_sql(schema: &TableSchema, filters: &[Filter], sort: Option<&Sort>, limit: u64, offset: u64) -> Result<String, String> {
    browse_sql_with_search(schema, filters, None, sort, limit, offset)
}

pub fn browse_sql_with_search(schema: &TableSchema, filters: &[Filter], search: Option<&str>, sort: Option<&Sort>, limit: u64, offset: u64) -> Result<String, String> {
    let where_sql = compile_where_with_search(schema, filters, search)?;
    let clause = if where_sql.is_empty() { String::new() } else { format!(" WHERE {where_sql}") };
    let order = sort
        .map(|item| format!(" ORDER BY {} {}", quote_ident(schema.engine, &item.column), if item.descending { "DESC" } else { "ASC" }))
        .unwrap_or_default();
    let tail = page_tail(schema.engine, limit, offset, !order.is_empty());
    Ok(format!("SELECT * FROM {}{clause}{order}{tail}", qualified_name(schema)))
}

/// 分页尾段。T-SQL 的 OFFSET/FETCH 必须跟在 ORDER BY 后面，没有排序时补一个恒定序。
pub fn page_tail(engine: Engine, limit: u64, offset: u64, ordered: bool) -> String {
    match engine {
        Engine::Sqlserver => {
            if ordered {
                format!(" OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY")
            } else {
                format!(" ORDER BY (SELECT NULL) OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY")
            }
        }
        _ if offset == 0 => format!(" LIMIT {limit}"),
        _ => format!(" LIMIT {limit} OFFSET {offset}"),
    }
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
#[allow(dead_code)]
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

/// 能定位的表行只对非主键的长文本和二进制投影预览。主键始终是完整文本或完整十六进制。
pub fn select_list(schema: &TableSchema, full: bool) -> String {
    select_list_preview(schema, full, 1024)
}

/// 与 select_list 相同，但预览截断上限来自设置；缺省调用仍按 1024 字节。
pub fn select_list_preview(schema: &TableSchema, full: bool, preview_bytes: usize) -> String {
    schema.columns.iter().map(|column| project_column(schema, column, full, preview_bytes)).collect::<Vec<_>>().join(", ")
}

fn project_column(schema: &TableSchema, column: &Column, full: bool, preview_bytes: usize) -> String {
    let ident = quote_ident(schema.engine, &column.name);
    let plain = plain_cast(schema.engine, &ident);
    let key_column = schema.key.iter().any(|name| name == &column.name);
    match value_family(&column.type_name) {
        ValueFamily::Binary if full || schema.key.is_empty() || key_column => binary_hex(schema.engine, &ident),
        ValueFamily::Binary => {
            let guard = null_key_guard(schema);
            format!("CASE WHEN {guard} THEN {} ELSE {} END", binary_hex(schema.engine, &ident), binary_length(schema.engine, &ident))
        }
        ValueFamily::Text if !full && !schema.key.is_empty() && !key_column => {
            let guard = null_key_guard(schema);
            let preview = text_preview(schema.engine, &plain, preview_bytes.max(1));
            format!("CASE WHEN {guard} THEN {plain} ELSE {preview} END")
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
        // NVARCHAR(MAX) 承接任意长度的文本形态，日期/数字等隐式转换后参与 LIKE 匹配。
        Engine::Sqlserver => format!("CAST({ident} AS NVARCHAR(MAX))"),
        // Redis 不会进入 SQL 编译（live::describe 已拒绝），占位保持穷尽。
        Engine::Redis | Engine::Mongodb => ident.to_owned(),
    }
}

fn text_preview(engine: Engine, plain: &str, preview_bytes: usize) -> String {
    let cap = preview_bytes.max(1);
    match engine {
        Engine::Postgres => format!("CASE WHEN octet_length({plain}) > {cap} THEN chr(1) || 't:' || octet_length({plain})::text || ':' || left({plain}, {cap}) ELSE {plain} END"),
        Engine::Mysql | Engine::Mariadb => format!("CASE WHEN LENGTH({plain}) > {cap} THEN CONCAT(CHAR(1), 't:', LENGTH({plain}), ':', LEFT({plain}, {cap})) ELSE {plain} END"),
        Engine::Sqlite => format!("CASE WHEN length(CAST({plain} AS BLOB)) > {cap} THEN char(1) || 't:' || length(CAST({plain} AS BLOB)) || ':' || substr({plain}, 1, {cap}) ELSE {plain} END"),
        // LEN 按字符计；预览协议的字节数对文本形态按字符数呈现，超长截断按设置的字符数。
        Engine::Sqlserver => format!("CASE WHEN LEN({plain}) > {cap} THEN CHAR(1) + N't:' + CAST(LEN({plain}) AS NVARCHAR(20)) + N':' + LEFT({plain}, {cap}) ELSE {plain} END"),
        // Redis 不会进入 SQL 编译（live::describe 已拒绝），占位保持穷尽。
        Engine::Redis | Engine::Mongodb => plain.to_owned(),
    }
}

fn binary_length(engine: Engine, ident: &str) -> String {
    match engine {
        Engine::Postgres => format!("CASE WHEN {ident} IS NULL THEN NULL ELSE chr(1) || 'b:' || octet_length({ident})::text END"),
        Engine::Mysql | Engine::Mariadb => format!("CASE WHEN {ident} IS NULL THEN NULL ELSE CONCAT(CHAR(1), 'b:', LENGTH({ident})) END"),
        Engine::Sqlite => format!("CASE WHEN {ident} IS NULL THEN NULL ELSE char(1) || 'b:' || length({ident}) END"),
        Engine::Sqlserver => format!("CASE WHEN {ident} IS NULL THEN NULL ELSE CHAR(1) + N'b:' + CAST(DATALENGTH({ident}) AS NVARCHAR(20)) END"),
        // Redis 不会进入 SQL 编译（live::describe 已拒绝），占位保持穷尽。
        Engine::Redis | Engine::Mongodb => format!("CASE WHEN {ident} IS NULL THEN NULL ELSE {ident} END"),
    }
}

fn binary_hex(engine: Engine, ident: &str) -> String {
    let hex = match engine {
        Engine::Postgres => format!("encode({ident}, 'hex')"),
        Engine::Mysql | Engine::Mariadb => format!("LOWER(HEX({ident}))"),
        Engine::Sqlite => format!("lower(hex({ident}))"),
        // CONVERT 样式 2 输出不带 0x 前缀的小写十六进制。
        Engine::Sqlserver => format!("LOWER(CONVERT(VARCHAR(MAX), {ident}, 2))"),
        // Redis 不会进入 SQL 编译（live::describe 已拒绝），占位保持穷尽。
        Engine::Redis | Engine::Mongodb => ident.to_owned(),
    };
    format!("CASE WHEN {ident} IS NULL THEN NULL ELSE {hex} END")
}

pub(crate) fn locator_sql(schema: &TableSchema, locator: &[(String, Cell)]) -> String {
    locator
        .iter()
        .map(|(name, value)| {
            let column = schema.columns.iter().find(|column| column.name == *name);
            format!("{} = {}", quote_ident(schema.engine, name), sql_cell(schema.engine, column, value))
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// 列类型的值家族。迁移（R8）按两侧家族给出兼容提示，与字面量生成共用这一张表。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ValueFamily {
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

pub fn value_family(type_name: &str) -> ValueFamily {
    let name = base_type(type_name);
    if name.is_empty() || name == "类型未知" {
        return ValueFamily::Unknown;
    }
    // bit 是 T-SQL 的布尔类型；MySQL 的 bit 约定同走布尔家族（TRUE/FALSE 字面量两者都接受）。
    if name == "bool" || name == "boolean" || name == "bit" {
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
    if name == "datetime" || name == "datetime2" || name == "datetimeoffset" || name == "smalldatetime" || name == "timestamptz" || name.starts_with("timestamp") {
        return ValueFamily::Datetime;
    }
    if name == "time" || name.starts_with("time ") {
        return ValueFamily::Time;
    }
    if is_integer_name(&name) {
        return ValueFamily::Integer;
    }
    if name == "numeric" || name == "decimal" || name == "number" || name == "money" || name == "smallmoney" {
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
        // T-SQL 的二进制字面量是 0x 前缀（空二进制是 0x）。
        Engine::Sqlserver => format!("0x{hex}"),
        // Redis 不会进入 SQL 编译（live::describe 已拒绝），占位保持穷尽。
        Engine::Redis | Engine::Mongodb => format!("X'{hex}'"),
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
pub(crate) mod value_family_test {
    use super::value_family;

    pub fn test_names() -> std::collections::HashMap<&'static str, super::ValueFamily> {
        let mut map = std::collections::HashMap::new();
        for name in [
            "datetime2", "datetimeoffset", "smalldatetime", "money", "smallmoney", "bit",
            "uniqueidentifier", "nvarchar", "varbinary", "int", "float", "date", "text",
        ] {
            map.insert(name, value_family(name));
        }
        map
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
        "text" | "tinytext" | "mediumtext" | "longtext" | "clob" | "nclob" | "varchar" | "nvarchar" | "char" | "nchar" | "character" | "character varying" | "varchar2" | "nvarchar2" | "string" | "json" | "jsonb" | "uuid" | "uniqueidentifier" | "ntext" | "enum" | "set" | "xml" | "citext" | "name" | "bpchar" | "varying"
    ) || name.starts_with("character ")
}

/// 预览信封：读取大字段时数据库就地截断出来的替身，不是原值。
/// 查看、复制、编辑、导出和 WHERE 都必须先按定位读取原值，绝不把它当数据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewValue {
    Binary { bytes: u64 },
    Text { bytes: u64, preview: String },
}

/// 权威判断：只有按列类型和预览上限真正会产出信封的形态才算信封。
/// 声明长度必须真的超过上限；形状相同但没被截断的普通文本仍是原值。
pub fn preview_envelope(value: &str, type_name: &str, preview_bytes: usize) -> Option<PreviewValue> {
    let rest = value.strip_prefix('\u{1}')?;
    if let Some(body) = rest.strip_prefix("b:") {
        if value_family(type_name) != ValueFamily::Binary {
            return None;
        }
        return Some(PreviewValue::Binary { bytes: body.parse().ok()? });
    }
    let body = rest.strip_prefix("t:")?;
    if value_family(type_name) != ValueFamily::Text {
        return None;
    }
    let (bytes, preview) = body.split_once(':')?;
    let bytes: u64 = bytes.parse().ok()?;
    // 投影只在超过上限时套信封；声明长度没超过上限的按原值处理。
    if bytes <= preview_bytes.max(1) as u64 {
        return None;
    }
    Some(PreviewValue::Text { bytes, preview: preview.to_owned() })
}

/// 导出前的结构检查：值还带着预览信封的形状时不能写成文件。
/// 文本只认页查询真的会截断的形态（声明长度超过 1 KB），
/// 二进制只认 `b:<字节>`；短小但形状相同的普通文本仍按原值处理。
pub fn looks_like_preview(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('\u{1}') else {
        return false;
    };
    if let Some(body) = rest.strip_prefix("b:") {
        return !body.is_empty() && body.bytes().all(|byte| byte.is_ascii_digit());
    }
    is_synthetic_text_preview(value)
}

