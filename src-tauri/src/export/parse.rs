//! CSV / JSON 导入解析。无引号空字段是空值，引号空字段是空字符串。

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedTable {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

#[derive(Clone, Debug)]
pub struct TableField {
    pub name: String,
    pub nullable: bool,
    pub auto: bool,
}

struct Field {
    value: Option<String>,
    quoted: bool,
}

pub fn parse_csv(text: &str, delimiter: char) -> Result<ParsedTable, String> {
    let table = parse_csv_fields(text, delimiter);
    if table.is_empty() || table[0].is_empty() {
        return Err("文件不是可用的 CSV。".to_owned());
    }
    let mut columns = Vec::new();
    for field in &table[0] {
        let name = field.value.as_deref().unwrap_or("").trim();
        if name.is_empty() {
            return Err("CSV 缺少列名。".to_owned());
        }
        if columns.iter().any(|column: &String| column == name) {
            return Err(format!("CSV 列名重复：{name}"));
        }
        columns.push(name.to_owned());
    }
    let mut rows = Vec::new();
    for row in table.iter().skip(1) {
        if row.iter().all(|field| field.value.is_none() && !field.quoted) {
            continue;
        }
        let mut cells = Vec::with_capacity(columns.len());
        for index in 0..columns.len() {
            cells.push(row.get(index).and_then(|field| field.value.clone()));
        }
        rows.push(cells);
    }
    Ok(ParsedTable { columns, rows })
}

pub fn parse_json(text: &str) -> Result<ParsedTable, String> {
    let source = text.strip_prefix('\u{feff}').unwrap_or(text);
    if source.trim().is_empty() {
        return Err("JSON 必须是由对象组成的数组。".to_owned());
    }
    let parsed: serde_json::Value = serde_json::from_str(source).map_err(|_| "JSON 必须是由对象组成的数组。".to_owned())?;
    let Some(items) = parsed.as_array() else {
        return Err("JSON 必须是由对象组成的数组。".to_owned());
    };
    let mut columns = Vec::new();
    let mut rows = Vec::new();
    for item in items {
        let Some(object) = item.as_object() else {
            return Err("JSON 必须是由对象组成的数组。".to_owned());
        };
        let mut cells = Vec::new();
        for (key, value) in object {
            if !columns.iter().any(|column: &String| column == key) {
                columns.push(key.clone());
            }
            cells.push((key.clone(), json_cell(value)?));
        }
        rows.push(cells);
    }
    let rows = rows
        .into_iter()
        .map(|cells| {
            columns
                .iter()
                .map(|name| cells.iter().find(|(key, _)| key == name).and_then(|(_, value)| value.clone()))
                .collect()
        })
        .collect();
    Ok(ParsedTable { columns, rows })
}

pub fn suggest_mapping(fields: &[TableField], file_columns: &[String]) -> BTreeMap<String, String> {
    let mut mapping = BTreeMap::new();
    for field in fields {
        let exact = file_columns.iter().find(|name| *name == &field.name);
        let loose = file_columns.iter().find(|name| name.eq_ignore_ascii_case(&field.name));
        mapping.insert(field.name.clone(), exact.or(loose).cloned().unwrap_or_default());
    }
    mapping
}

pub fn required_gaps(fields: &[TableField], mapping: &BTreeMap<String, String>) -> Vec<String> {
    fields
        .iter()
        .filter(|field| !field.nullable && !field.auto && mapping.get(&field.name).is_none_or(String::is_empty))
        .map(|field| field.name.clone())
        .collect()
}

fn json_cell(value: &serde_json::Value) -> Result<Option<String>, String> {
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(text) => Ok(Some(text.clone())),
        serde_json::Value::Number(number) => Ok(Some(number.to_string())),
        serde_json::Value::Bool(flag) => Ok(Some(if *flag { "true" } else { "false" }.to_owned())),
        _ => Err("JSON 字段只能是字符串、数字、布尔或空值。".to_owned()),
    }
}

fn parse_csv_fields(text: &str, delimiter: char) -> Vec<Vec<Field>> {
    let source = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut saw_quote = false;
    let chars: Vec<char> = source.chars().collect();
    let mut index = 0;
    let push_field = |row: &mut Vec<Field>, field: &mut String, saw_quote: &mut bool| {
        let value = if !*saw_quote && field.is_empty() { None } else { Some(std::mem::take(field)) };
        row.push(Field { value, quoted: *saw_quote });
        *saw_quote = false;
    };
    while index < chars.len() {
        let char = chars[index];
        if quoted {
            if char == '"' {
                if chars.get(index + 1) == Some(&'"') {
                    field.push('"');
                    index += 2;
                    continue;
                }
                quoted = false;
            } else {
                field.push(char);
            }
            index += 1;
            continue;
        }
        if char == '"' {
            quoted = true;
            saw_quote = true;
            index += 1;
            continue;
        }
        if char == delimiter {
            push_field(&mut row, &mut field, &mut saw_quote);
            index += 1;
            continue;
        }
        if char == '\n' {
            if field.ends_with('\r') {
                field.pop();
            }
            push_field(&mut row, &mut field, &mut saw_quote);
            rows.push(std::mem::take(&mut row));
            index += 1;
            continue;
        }
        field.push(char);
        index += 1;
    }
    if !field.is_empty() || saw_quote || !row.is_empty() {
        if field.ends_with('\r') {
            field.pop();
        }
        push_field(&mut row, &mut field, &mut saw_quote);
        rows.push(row);
    }
    rows
}
