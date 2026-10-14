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

pub fn sql_cell(column: Option<&Column>, value: &Cell) -> String {
    match value {
        Cell::Null => "NULL".to_owned(),
        Cell::Text(text) => {
            if column.is_some_and(|item| is_numeric(&item.type_name) && number_literal(text)) {
                text.clone()
            } else {
                quote_literal(text)
            }
        }
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
        FilterOp::Eq => format!("{ident} = {}", sql_cell(Some(column), &Cell::Text(raw.to_owned()))),
        FilterOp::Ne => format!("{ident} <> {}", sql_cell(Some(column), &Cell::Text(raw.to_owned()))),
        FilterOp::Contains => {
            let op = if engine == Engine::Postgres { "ILIKE" } else { "LIKE" };
            format!("{ident} {op} {} ESCAPE '!'", like_literal(raw, true))
        }
        FilterOp::Starts => format!("{ident} LIKE {} ESCAPE '!'", like_literal(raw, false)),
        FilterOp::Gt => format!("{ident} > {}", sql_cell(Some(column), &Cell::Text(raw.to_owned()))),
        FilterOp::Lt => format!("{ident} < {}", sql_cell(Some(column), &Cell::Text(raw.to_owned()))),
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
    let statements = split_sql(sql).into_iter().map(|item| item.trim().to_owned()).filter(|item| !item.is_empty()).collect::<Vec<_>>();
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

pub fn split_sql(sql: &str) -> Vec<String> {
    let chars: Vec<char> = sql.chars().collect();
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut mode = "";
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        let next = chars.get(index + 1).copied();
        match mode {
            "line" => {
                if ch == '\n' {
                    mode = "";
                }
            }
            "block" => {
                if ch == '*' && next == Some('/') {
                    mode = "";
                    index += 1;
                }
            }
            "single" => {
                current.push(ch);
                if ch == '\'' && next == Some('\'') {
                    current.push('\'');
                    index += 1;
                } else if ch == '\'' {
                    mode = "";
                }
            }
            "double" => {
                current.push(ch);
                if ch == '"' && next == Some('"') {
                    current.push('"');
                    index += 1;
                } else if ch == '"' {
                    mode = "";
                }
            }
            "tick" => {
                current.push(ch);
                if ch == '`' && next == Some('`') {
                    current.push('`');
                    index += 1;
                } else if ch == '`' {
                    mode = "";
                }
            }
            _ => {
                if ch == '-' && next == Some('-') {
                    mode = "line";
                    index += 1;
                } else if ch == '/' && next == Some('*') {
                    mode = "block";
                    index += 1;
                } else if ch == '\'' {
                    mode = "single";
                    current.push(ch);
                } else if ch == '"' {
                    mode = "double";
                    current.push(ch);
                } else if ch == '`' {
                    mode = "tick";
                    current.push(ch);
                } else if ch == ';' {
                    statements.push(std::mem::take(&mut current));
                } else {
                    current.push(ch);
                }
            }
        }
        index += 1;
    }
    if !current.trim().is_empty() {
        statements.push(current);
    }
    statements
}

pub fn update_sql(schema: &TableSchema, sets: &[(String, Cell)], locator: &[(String, Cell)]) -> String {
    let assignments = sets
        .iter()
        .map(|(name, value)| {
            let column = schema.columns.iter().find(|column| column.name == *name);
            format!("{} = {}", quote_ident(schema.engine, name), sql_cell(column, value))
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
        .map(|(name, value)| sql_cell(schema.columns.iter().find(|column| column.name == *name), value))
        .collect::<Vec<_>>()
        .join(", ");
    format!("INSERT INTO {} ({names}) VALUES ({literals})", qualified_name(schema))
}

fn locator_sql(schema: &TableSchema, locator: &[(String, Cell)]) -> String {
    locator
        .iter()
        .map(|(name, value)| {
            let column = schema.columns.iter().find(|column| column.name == *name);
            format!("{} = {}", quote_ident(schema.engine, name), sql_cell(column, value))
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}
