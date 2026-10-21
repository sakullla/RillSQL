//! 按主键或唯一键生成数据差异脚本。字面量走网格已有的写入规则，不另做转义，也不执行。

use std::collections::BTreeMap;

use crate::grid::{delete_sql, insert_sql, reject_literal, update_sql, Cell, Column, Engine as GridEngine, TableSchema};

use super::model::{DataDiff, DataInput, DataKey, DataReport, DataTable, Engine};

pub fn diff_data(input: &DataInput) -> DataReport {
    if Engine::parse(&input.baseline_engine).ok() != Engine::parse(&input.target_engine).ok() || Engine::parse(&input.target_engine).is_err() {
        return DataReport {
            refused: Some("两侧引擎不同。MySQL 与 MariaDB 是不同引擎，不生成脚本。".to_owned()),
            script: String::new(),
            incomplete: false,
            notes: Vec::new(),
            diffs: Vec::new(),
        };
    }
    let engine = GridEngine::parse(&input.target_engine).expect("engine checked");
    let mut notes = Vec::new();
    let mut diffs = Vec::new();
    let mut statements = Vec::new();
    let mut incomplete = input.cancelled;
    if input.cancelled {
        notes.push("对比已取消，未完成的结果不是完整脚本。".to_owned());
    }
    if input.tables.is_empty() {
        return DataReport {
            refused: Some("没有选择要对比的表，不生成脚本。".to_owned()),
            script: String::new(),
            incomplete: false,
            notes: Vec::new(),
            diffs: Vec::new(),
        };
    }
    for table in &input.tables {
        if let Some(side) = &table.only_side {
            notes.push(format!("表 {} 只在{side}，这是结构差异，不编造行。", table.name));
            diffs.push(DataDiff { table: table.name.clone(), action: "skipped".to_owned(), key: String::new(), detail: format!("只在{side}") });
            continue;
        }
        if let Some(message) = &table.read_error {
            incomplete = true;
            notes.push(format!("表 {} 读取失败：{message}", table.name));
            if table.baseline.is_empty() && table.target.is_empty() {
                continue;
            }
        }
        if table.keys_unknown {
            notes.push(format!("表 {} 的索引或主键没有读到，不生成行变更。", table.name));
            diffs.push(DataDiff { table: table.name.clone(), action: "skipped".to_owned(), key: String::new(), detail: "索引或主键没有读到".to_owned() });
            continue;
        }
        match resolve_key(&table.keys, table.chosen_key.as_deref()) {
            KeyUse::Ready { columns, label } => {
                if let Some(label) = label {
                    notes.push(format!("表 {} 使用唯一键 {label}。", table.name));
                }
                diff_table(engine, table, &columns, &mut statements, &mut diffs, &mut notes, &mut incomplete);
            }
            KeyUse::None => {
                notes.push(format!("表 {} 没有主键或唯一键，无法对应行，不生成行变更。", table.name));
                diffs.push(DataDiff { table: table.name.clone(), action: "skipped".to_owned(), key: String::new(), detail: "无法对应行".to_owned() });
            }
            KeyUse::Choose => {
                notes.push(format!("表 {} 有多个唯一键，选定前不生成行变更。", table.name));
                diffs.push(DataDiff { table: table.name.clone(), action: "skipped".to_owned(), key: String::new(), detail: "尚未选定唯一键".to_owned() });
            }
        }
    }
    notes.sort();
    notes.dedup();
    DataReport {
        refused: None,
        script: render(&notes, &statements, incomplete),
        incomplete,
        notes,
        diffs,
    }
}

enum KeyUse {
    Ready { columns: Vec<String>, label: Option<String> },
    None,
    Choose,
}

fn resolve_key(keys: &[DataKey], chosen: Option<&str>) -> KeyUse {
    if let Some(primary) = keys.iter().find(|key| key.primary && !key.columns.is_empty()) {
        return KeyUse::Ready { columns: primary.columns.clone(), label: None };
    }
    let unique = keys.iter().filter(|key| !key.primary && !key.columns.is_empty()).collect::<Vec<_>>();
    if unique.is_empty() {
        return KeyUse::None;
    }
    if unique.len() == 1 {
        return KeyUse::Ready { columns: unique[0].columns.clone(), label: Some(unique[0].name.clone()) };
    }
    let Some(chosen) = chosen else {
        return KeyUse::Choose;
    };
    match unique.iter().find(|key| key.name == chosen) {
        Some(key) => KeyUse::Ready { columns: key.columns.clone(), label: Some(key.name.clone()) },
        None => KeyUse::Choose,
    }
}

fn diff_table(engine: GridEngine, table: &DataTable, key: &[String], statements: &mut Vec<String>, diffs: &mut Vec<DataDiff>, notes: &mut Vec<String>, incomplete: &mut bool) {
    let schema = grid_schema(engine, table, key);
    let left = group_rows(&table.baseline, key);
    let right = group_rows(&table.target, key);
    if left.missing_key || right.missing_key {
        notes.push(format!("表 {} 的键列没有同时出现在两侧行中，无法对应行，不生成行变更。", table.name));
        diffs.push(DataDiff { table: table.name.clone(), action: "skipped".to_owned(), key: String::new(), detail: "键列没有同时出现在两侧行中".to_owned() });
        return;
    }
    if left.duplicate || right.duplicate {
        *incomplete = true;
        notes.push(format!("表 {} 有重复键，这些键不生成语句。", table.name));
    }
    let mut tokens = std::collections::BTreeSet::new();
    tokens.extend(left.rows.keys().cloned());
    tokens.extend(right.rows.keys().cloned());
    for token in tokens {
        if left.duplicates.contains(&token) || right.duplicates.contains(&token) {
            continue;
        }
        match (left.rows.get(&token), right.rows.get(&token)) {
            (Some(base), None) => push_insert(&schema, table, key, base, statements, diffs, notes),
            (None, Some(other)) => push_delete(&schema, table, key, other, statements, diffs, notes),
            (Some(base), Some(other)) => push_update(&schema, table, key, base, other, statements, diffs, notes),
            _ => {}
        }
    }
}

struct Grouped {
    rows: BTreeMap<String, BTreeMap<String, Cell>>,
    duplicates: std::collections::BTreeSet<String>,
    duplicate: bool,
    /// 已读到的行里没有选定的键列。不能把这行当成不存在。
    missing_key: bool,
}

fn group_rows(rows: &[BTreeMap<String, Cell>], key: &[String]) -> Grouped {
    let mut grouped = Grouped { rows: BTreeMap::new(), duplicates: std::collections::BTreeSet::new(), duplicate: false, missing_key: false };
    for row in rows {
        let Some(token) = key_token(row, key) else {
            grouped.missing_key = true;
            continue;
        };
        if grouped.rows.contains_key(&token) {
            grouped.duplicate = true;
            grouped.duplicates.insert(token);
            continue;
        }
        grouped.rows.insert(token, row.clone());
    }
    for token in &grouped.duplicates {
        grouped.rows.remove(token);
    }
    grouped
}

fn key_token(row: &BTreeMap<String, Cell>, key: &[String]) -> Option<String> {
    let mut parts = Vec::new();
    for column in key {
        let cell = row.get(column)?;
        parts.push(match cell {
            Cell::Null => "n:".to_owned(),
            Cell::Text(text) => format!("t:{text}"),
        });
    }
    Some(parts.join("\u{1e}"))
}

fn key_label(row: &BTreeMap<String, Cell>, key: &[String]) -> String {
    key.iter()
        .map(|column| match row.get(column) {
            Some(Cell::Text(text)) => text.clone(),
            Some(Cell::Null) => "NULL".to_owned(),
            None => "?".to_owned(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn grid_schema(engine: GridEngine, table: &DataTable, key: &[String]) -> TableSchema {
    TableSchema {
        engine,
        schema: table.qualify.clone(),
        name: table.name.clone(),
        kind: "table",
        columns: table
            .columns
            .iter()
            .map(|column| Column {
                name: column.name.clone(),
                type_name: column.type_name.clone(),
                nullable: column.nullable,
                primary: key.iter().any(|item| item == &column.name),
                unique: false,
                auto: false,
            })
            .collect(),
        key: key.to_vec(),
    }
}

fn locator(row: &BTreeMap<String, Cell>, key: &[String]) -> Vec<(String, Cell)> {
    key.iter().map(|column| (column.clone(), row.get(column).cloned().unwrap_or(Cell::Null))).collect()
}

fn values_of(row: &BTreeMap<String, Cell>, columns: &[Column]) -> Vec<(String, Cell)> {
    columns.iter().map(|column| (column.name.clone(), row.get(&column.name).cloned().unwrap_or(Cell::Null))).collect()
}

fn rejection(schema: &TableSchema, pairs: &[(String, Cell)]) -> Option<String> {
    for (name, value) in pairs {
        let Some(column) = schema.columns.iter().find(|column| column.name == *name) else {
            continue;
        };
        if let Some(message) = reject_literal(column, value) {
            return Some(message);
        }
    }
    None
}

fn push_insert(schema: &TableSchema, table: &DataTable, key: &[String], row: &BTreeMap<String, Cell>, statements: &mut Vec<String>, diffs: &mut Vec<DataDiff>, notes: &mut Vec<String>) {
    let values = values_of(row, &schema.columns);
    let label = key_label(row, key);
    if let Some(message) = rejection(schema, &values) {
        notes.push(format!("表 {} 键 {label} 未能生成语句：{message}", table.name));
        diffs.push(DataDiff { table: table.name.clone(), action: "rejected".to_owned(), key: label, detail: message });
        return;
    }
    statements.push(insert_sql(schema, &values));
    diffs.push(DataDiff { table: table.name.clone(), action: "insert".to_owned(), key: label, detail: "只在基准".to_owned() });
}

fn push_delete(schema: &TableSchema, table: &DataTable, key: &[String], row: &BTreeMap<String, Cell>, statements: &mut Vec<String>, diffs: &mut Vec<DataDiff>, notes: &mut Vec<String>) {
    let locator = locator(row, key);
    let label = key_label(row, key);
    if let Some(message) = rejection(schema, &locator) {
        notes.push(format!("表 {} 键 {label} 未能生成语句：{message}", table.name));
        diffs.push(DataDiff { table: table.name.clone(), action: "rejected".to_owned(), key: label, detail: message });
        return;
    }
    statements.push(delete_sql(schema, &locator));
    diffs.push(DataDiff { table: table.name.clone(), action: "delete".to_owned(), key: label, detail: "只在对照".to_owned() });
}

fn push_update(
    schema: &TableSchema,
    table: &DataTable,
    key: &[String],
    baseline: &BTreeMap<String, Cell>,
    target: &BTreeMap<String, Cell>,
    statements: &mut Vec<String>,
    diffs: &mut Vec<DataDiff>,
    notes: &mut Vec<String>,
) {
    let mut sets = Vec::new();
    for column in &schema.columns {
        if key.iter().any(|item| item == &column.name) {
            continue;
        }
        let left = baseline.get(&column.name).cloned().unwrap_or(Cell::Null);
        let right = target.get(&column.name).cloned().unwrap_or(Cell::Null);
        if left != right {
            sets.push((column.name.clone(), left));
        }
    }
    if sets.is_empty() {
        return;
    }
    let label = key_label(baseline, key);
    if let Some(message) = rejection(schema, &sets) {
        notes.push(format!("表 {} 键 {label} 未能生成语句：{message}", table.name));
        diffs.push(DataDiff { table: table.name.clone(), action: "rejected".to_owned(), key: label, detail: message });
        return;
    }
    statements.push(update_sql(schema, &sets, &locator(baseline, key)));
    diffs.push(DataDiff { table: table.name.clone(), action: "update".to_owned(), key: label, detail: "键相同，其他值不同".to_owned() });
}

fn render(notes: &[String], statements: &[String], incomplete: bool) -> String {
    let mut lines = vec!["-- 数据差异脚本。生成时没有执行。".to_owned()];
    if incomplete {
        lines.push("-- 脚本不完整。".to_owned());
    }
    for note in notes {
        lines.push(format!("-- {note}"));
    }
    if statements.is_empty() {
        if incomplete {
            lines.push("-- 没有可放入完整脚本的行变更。".to_owned());
        } else if notes.iter().any(|note| note.contains("无法对应") || note.contains("选定前") || note.contains("结构差异") || note.contains("未能生成") || note.contains("没有读到")) {
            lines.push("-- 没有可生成的行变更。".to_owned());
        } else {
            lines.push("-- 没有数据差异。".to_owned());
        }
    }
    for statement in statements {
        let text = statement.trim().trim_end_matches(';').trim();
        if !text.is_empty() {
            lines.push(format!("{text};"));
        }
    }
    lines.join("\n")
}

trait ParseGridEngine {
    fn parse(value: &str) -> Result<Self, String>
    where
        Self: Sized;
}

impl ParseGridEngine for GridEngine {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "postgres" => Ok(Self::Postgres),
            "mysql" => Ok(Self::Mysql),
            "mariadb" => Ok(Self::Mariadb),
            "sqlite" => Ok(Self::Sqlite),
            other => Err(format!("不支持的数据库 {other}")),
        }
    }
}
