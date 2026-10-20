use std::collections::BTreeMap;

use super::sql::{browse_sql, delete_sql, insert_sql, update_sql};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Postgres,
    Mysql,
    Mariadb,
    Sqlite,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cell {
    Null,
    Text(String),
}

#[derive(Clone, Debug)]
pub struct Column {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub primary: bool,
    pub unique: bool,
    pub auto: bool,
}

#[derive(Clone, Debug)]
pub struct TableSchema {
    pub engine: Engine,
    pub schema: String,
    pub name: String,
    pub kind: &'static str,
    pub columns: Vec<Column>,
    pub key: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locator {
    pub columns: Vec<String>,
    pub values: Vec<Cell>,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    pub cells: BTreeMap<String, Cell>,
    pub locator: Option<Locator>,
    pub located: bool,
}

#[derive(Clone, Debug)]
pub struct TableStore {
    pub schema: TableSchema,
    pub rows: Vec<Row>,
    pub read_error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterOp {
    Eq,
    Ne,
    Contains,
    Starts,
    Gt,
    Lt,
    Between,
    In,
    Null,
    NotNull,
    Empty,
}

impl FilterOp {
    pub fn needs_value(self) -> bool {
        !matches!(self, Self::Null | Self::NotNull | Self::Empty)
    }
}

#[derive(Clone, Debug)]
pub struct Filter {
    pub column: String,
    pub op: FilterOp,
    pub value: Option<String>,
    pub second: Option<String>,
    pub values: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Sort {
    pub column: String,
    pub descending: bool,
}

#[derive(Clone, Debug)]
pub struct Page {
    pub rows: Vec<Row>,
    pub matched: usize,
    pub truncated: bool,
    pub sql: String,
}

#[derive(Clone, Debug)]
pub enum Change {
    Update { id: String, row_id: String, sets: BTreeMap<String, Cell> },
    Insert { id: String, values: BTreeMap<String, Option<Cell>> },
    Delete { id: String, row_id: String },
}

#[derive(Clone, Debug)]
pub struct Applied {
    pub id: String,
    pub sql: String,
}

#[derive(Clone, Debug)]
pub struct Failed {
    pub id: String,
    pub sql: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct CommitReport {
    pub applied: Vec<Applied>,
    pub failed: Option<Failed>,
    pub pending_ids: Vec<String>,
    pub rolled_back: bool,
    pub rule: &'static str,
}

pub const COMMIT_RULE: &str = "按顺序逐条提交，遇到错误即停止；已成功的语句不撤销。";

pub fn touch_api() -> usize {
    let engines = [Engine::Postgres, Engine::Mysql, Engine::Mariadb, Engine::Sqlite];
    let ops = [
        FilterOp::Eq,
        FilterOp::Ne,
        FilterOp::Contains,
        FilterOp::Starts,
        FilterOp::Gt,
        FilterOp::Lt,
        FilterOp::Between,
        FilterOp::In,
        FilterOp::Null,
        FilterOp::NotNull,
        FilterOp::Empty,
    ];
    let page = Page { rows: Vec::new(), matched: 0, truncated: false, sql: String::new() };
    let applied = Applied { id: String::new(), sql: String::new() };
    let failed = Failed { id: String::new(), sql: String::new(), message: String::new() };
    let report = CommitReport { applied: Vec::new(), failed: None, pending_ids: Vec::new(), rolled_back: false, rule: COMMIT_RULE };
    let _changes = [
        Change::Update { id: String::new(), row_id: String::new(), sets: BTreeMap::new() },
        Change::Insert { id: String::new(), values: BTreeMap::new() },
        Change::Delete { id: String::new(), row_id: String::new() },
    ];
    engines.len()
        + ops.len()
        + page.rows.len()
        + page.matched
        + usize::from(page.truncated)
        + page.sql.len()
        + applied.id.len()
        + applied.sql.len()
        + failed.id.len()
        + failed.sql.len()
        + failed.message.len()
        + usize::from(report.rolled_back)
        + report.pending_ids.len()
        + report.applied.len()
        + usize::from(report.failed.is_none())
        + report.rule.len()
}

fn column(name: &str, type_name: &str, nullable: bool, primary: bool, unique: bool, auto: bool) -> Column {
    Column { name: name.to_owned(), type_name: type_name.to_owned(), nullable, primary, unique, auto }
}

fn text_cell(value: &str) -> Cell {
    Cell::Text(value.to_owned())
}

pub fn customers_fixture(count: usize) -> TableStore {
    let schema = TableSchema {
        engine: Engine::Postgres,
        schema: "public".to_owned(),
        name: "customers".to_owned(),
        kind: "table",
        key: vec!["id".to_owned()],
        columns: vec![
            column("id", "integer", false, true, true, true),
            column("name", "text", false, false, false, false),
            column("email", "text", false, false, true, false),
            column("note", "text", true, false, false, false),
        ],
    };
    let mut rows = Vec::new();
    for index in 0..count {
        let id = (index + 1).to_string();
        let note = if index == 0 {
            Cell::Null
        } else if index == 1 {
            text_cell("")
        } else if index == 3 {
            text_cell("他说\"你好\"，\n下一行")
        } else {
            text_cell(&format!("备注{id}"))
        };
        let name = if index + 1 == count { "000-first" } else if index % 17 == 0 { "小河" } else { "" };
        let name = if name.is_empty() { format!("客户{id}") } else { name.to_owned() };
        let mut cells = BTreeMap::new();
        cells.insert("id".to_owned(), text_cell(&id));
        cells.insert("name".to_owned(), text_cell(&name));
        cells.insert("email".to_owned(), text_cell(&format!("u{id}@example.com")));
        cells.insert("note".to_owned(), note);
        rows.push(Row { id, cells, locator: None, located: false });
    }
    refresh_located(&schema, &mut rows);
    TableStore { schema, rows, read_error: None }
}

pub fn nokey_fixture() -> TableStore {
    let schema = TableSchema {
        engine: Engine::Sqlite,
        schema: String::new(),
        name: "notes".to_owned(),
        kind: "table",
        key: Vec::new(),
        columns: vec![column("label", "text", true, false, false, false), column("note", "text", true, false, false, false)],
    };
    let mut rows = Vec::new();
    for (id, label, note) in [("a", "重复", Cell::Null), ("b", "重复", text_cell("")), ("c", "唯一", text_cell("中文"))] {
        let mut cells = BTreeMap::new();
        cells.insert("label".to_owned(), text_cell(label));
        cells.insert("note".to_owned(), note);
        rows.push(Row { id: id.to_owned(), cells, locator: None, located: false });
    }
    refresh_located(&schema, &mut rows);
    TableStore { schema, rows, read_error: None }
}

fn locator_from(schema: &TableSchema, cells: &BTreeMap<String, Cell>) -> Option<Locator> {
    if schema.kind != "table" || schema.key.is_empty() {
        return None;
    }
    let mut values = Vec::new();
    for name in &schema.key {
        match cells.get(name) {
            Some(Cell::Text(value)) => values.push(Cell::Text(value.clone())),
            _ => return None,
        }
    }
    Some(Locator { columns: schema.key.clone(), values })
}

fn same_locator(left: &Locator, right: &Locator) -> bool {
    left.columns == right.columns && left.values == right.values
}

pub fn refresh_located(schema: &TableSchema, rows: &mut [Row]) {
    let locators: Vec<Option<Locator>> = rows.iter().map(|row| locator_from(schema, &row.cells)).collect();
    for (index, row) in rows.iter_mut().enumerate() {
        let Some(locator) = locators[index].clone() else {
            row.locator = None;
            row.located = false;
            continue;
        };
        let copies = locators.iter().filter(|item| item.as_ref().is_some_and(|candidate| same_locator(candidate, &locator))).count();
        row.located = copies == 1;
        row.locator = if copies == 1 { Some(locator) } else { None };
    }
}

fn cell_of(row: &Row, name: &str) -> Cell {
    row.cells.get(name).cloned().unwrap_or(Cell::Null)
}

fn order_text(type_name: &str, left: &str, right: &str) -> std::cmp::Ordering {
    let numeric = ["int", "numeric", "decimal", "real", "double", "float", "serial", "number"].iter().any(|token| type_name.to_ascii_lowercase().contains(token));
    if numeric {
        if let (Ok(a), Ok(b)) = (left.parse::<f64>(), right.parse::<f64>()) {
            return a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal);
        }
    }
    left.cmp(right)
}

pub(crate) fn matches(column: &Column, value: &Cell, filter: &Filter) -> bool {
    match filter.op {
        FilterOp::Null => value == &Cell::Null,
        FilterOp::NotNull => value != &Cell::Null,
        FilterOp::Empty => value == &Cell::Text(String::new()),
        FilterOp::Contains => match value {
            Cell::Text(text) => text.to_lowercase().contains(&filter.value.clone().unwrap_or_default().to_lowercase()),
            Cell::Null => false,
        },
        FilterOp::Starts => match value {
            Cell::Text(text) => text.to_lowercase().starts_with(&filter.value.clone().unwrap_or_default().to_lowercase()),
            Cell::Null => false,
        },
        FilterOp::Between => {
            let Cell::Text(text) = value else { return false };
            let (Some(low), Some(high)) = (filter.value.as_deref(), filter.second.as_deref()) else {
                return false;
            };
            order_text(&column.type_name, text, low).is_ge() && order_text(&column.type_name, text, high).is_le()
        }
        FilterOp::In => match value {
            Cell::Text(text) => filter.values.iter().any(|item| item == text),
            Cell::Null => false,
        },
        FilterOp::Eq | FilterOp::Ne | FilterOp::Gt | FilterOp::Lt => {
            let Cell::Text(text) = value else { return false };
            let raw = filter.value.clone().unwrap_or_default();
            let order = order_text(&column.type_name, text, &raw);
            match filter.op {
                FilterOp::Eq => order.is_eq(),
                FilterOp::Ne => order.is_ne(),
                FilterOp::Gt => order.is_gt(),
                FilterOp::Lt => order.is_lt(),
                _ => false,
            }
        }
    }
}

fn compare_cells(type_name: &str, left: &Cell, right: &Cell) -> std::cmp::Ordering {
    match (left, right) {
        (Cell::Null, Cell::Null) => std::cmp::Ordering::Equal,
        (Cell::Null, _) => std::cmp::Ordering::Greater,
        (_, Cell::Null) => std::cmp::Ordering::Less,
        (Cell::Text(a), Cell::Text(b)) => order_text(type_name, a, b),
    }
}

pub fn query_page(store: &TableStore, filters: &[Filter], sort: Option<&Sort>, offset: usize, limit: usize) -> Result<Page, (String, bool)> {
    let sql = browse_sql(&store.schema, filters, sort, limit as u64, offset as u64).unwrap_or_default();
    if let Some(error) = &store.read_error {
        return Err((error.clone(), false));
    }
    if let Err(error) = super::sql::compile_where(&store.schema, filters) {
        return Err((error, false));
    }
    let mut rows: Vec<Row> = store
        .rows
        .iter()
        .filter(|row| {
            filters.iter().all(|filter| {
                let Some(column) = store.schema.columns.iter().find(|column| column.name == filter.column) else {
                    return false;
                };
                matches(column, &cell_of(row, &filter.column), filter)
            })
        })
        .cloned()
        .collect();
    if let Some(sort) = sort {
        let type_name = store.schema.columns.iter().find(|column| column.name == sort.column).map(|column| column.type_name.clone()).unwrap_or_default();
        rows.sort_by(|left, right| {
            let order = compare_cells(&type_name, &cell_of(left, &sort.column), &cell_of(right, &sort.column));
            if sort.descending { order.reverse() } else { order }
        });
    }
    let matched = rows.len();
    let slice = rows.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
    let truncated = offset + slice.len() < matched;
    Ok(Page { rows: slice, matched, truncated, sql })
}

fn fail(applied: Vec<Applied>, id: String, sql: String, message: String, pending: Vec<String>) -> CommitReport {
    CommitReport { applied, failed: Some(Failed { id, sql, message }), pending_ids: pending, rolled_back: false, rule: COMMIT_RULE }
}

fn unique_conflict(schema: &TableSchema, rows: &[Row], cells: &BTreeMap<String, Cell>, ignore: Option<&str>) -> Option<String> {
    for column in schema.columns.iter().filter(|column| column.unique || column.primary) {
        let Some(Cell::Text(value)) = cells.get(&column.name) else { continue };
        if rows.iter().any(|row| Some(row.id.as_str()) != ignore && matches!(row.cells.get(&column.name), Some(Cell::Text(existing)) if existing == value)) {
            return Some(format!("duplicate key value violates unique constraint \"{}_{}_key\"", schema.name, column.name));
        }
    }
    None
}

fn next_auto(rows: &[Row], name: &str) -> String {
    let max = rows.iter().filter_map(|row| match row.cells.get(name) {
        Some(Cell::Text(value)) => value.parse::<i64>().ok(),
        _ => None,
    }).max().unwrap_or(0);
    (max + 1).to_string()
}

pub fn commit_changes(store: &mut TableStore, changes: &[Change]) -> CommitReport {
    let mut applied = Vec::new();
    for (index, change) in changes.iter().enumerate() {
        let pending = changes[index..].iter().map(|item| match item {
            Change::Update { id, .. } | Change::Insert { id, .. } | Change::Delete { id, .. } => id.clone(),
        }).collect::<Vec<_>>();
        if store.schema.kind != "table" {
            return fail(applied, pending[0].clone(), String::new(), "只有数据表可以新增、修改或删除，视图不行".to_owned(), pending);
        }
        match change {
            Change::Update { id, row_id, sets } => {
                let Some(position) = store.rows.iter().position(|row| row.id == *row_id) else {
                    return fail(applied, id.clone(), String::new(), "无法唯一确定这一行，不能修改".to_owned(), pending);
                };
                if !store.rows[position].located || store.rows[position].locator.is_none() {
                    return fail(applied, id.clone(), String::new(), "无法唯一确定这一行，不能修改".to_owned(), pending);
                }
                let locator = store.rows[position].locator.clone().unwrap();
                let hits = store.rows.iter().filter(|row| row.locator.as_ref().is_some_and(|item| same_locator(item, &locator))).count();
                let pairs = locator.columns.iter().cloned().zip(locator.values.iter().cloned()).collect::<Vec<_>>();
                let set_pairs = sets.iter().map(|(key, value)| (key.clone(), value.clone())).collect::<Vec<_>>();
                let sql = update_sql(&store.schema, &set_pairs, &pairs);
                if hits != 1 {
                    return fail(applied, id.clone(), sql, format!("影响 {hits} 行，已停止。已成功的语句不撤销。"), pending);
                }
                let mut next = store.rows[position].cells.clone();
                for (key, value) in sets {
                    next.insert(key.clone(), value.clone());
                }
                if let Some(message) = unique_conflict(&store.schema, &store.rows, &next, Some(row_id)) {
                    return fail(applied, id.clone(), sql, message, pending);
                }
                store.rows[position].cells = next;
                let schema = store.schema.clone();
                refresh_located(&schema, &mut store.rows);
                applied.push(Applied { id: id.clone(), sql });
            }
            Change::Delete { id, row_id } => {
                let Some(position) = store.rows.iter().position(|row| row.id == *row_id) else {
                    return fail(applied, id.clone(), String::new(), "无法唯一确定这一行，不能删除".to_owned(), pending);
                };
                if !store.rows[position].located || store.rows[position].locator.is_none() {
                    return fail(applied, id.clone(), String::new(), "无法唯一确定这一行，不能删除".to_owned(), pending);
                }
                let locator = store.rows[position].locator.clone().unwrap();
                let hits = store.rows.iter().filter(|row| row.locator.as_ref().is_some_and(|item| same_locator(item, &locator))).count();
                let pairs = locator.columns.iter().cloned().zip(locator.values.clone()).collect::<Vec<_>>();
                let sql = delete_sql(&store.schema, &pairs);
                if hits != 1 {
                    return fail(applied, id.clone(), sql, format!("影响 {hits} 行，已停止。已成功的语句不撤销。"), pending);
                }
                store.rows.retain(|row| row.id != *row_id);
                let schema = store.schema.clone();
                refresh_located(&schema, &mut store.rows);
                applied.push(Applied { id: id.clone(), sql });
            }
            Change::Insert { id, values } => {
                let mut draft = values.clone();
                for column in &store.schema.columns {
                    if column.auto && !draft.contains_key(&column.name) {
                        draft.insert(column.name.clone(), Some(Cell::Text(next_auto(&store.rows, &column.name))));
                    }
                }
                for column in &store.schema.columns {
                    if !column.nullable && !column.auto && !draft.contains_key(&column.name) {
                        let pairs = draft.iter().filter_map(|(name, value)| value.clone().map(|cell| (name.clone(), cell))).collect::<Vec<_>>();
                        return fail(applied, id.clone(), insert_sql(&store.schema, &pairs), format!("新增行缺少必填字段 {}", column.name), pending);
                    }
                }
                let mut cells = BTreeMap::new();
                for column in &store.schema.columns {
                    cells.insert(column.name.clone(), draft.get(&column.name).and_then(|value| value.clone()).unwrap_or(Cell::Null));
                }
                if let Some(message) = unique_conflict(&store.schema, &store.rows, &cells, None) {
                    let pairs = cells.iter().map(|(name, value)| (name.clone(), value.clone())).collect::<Vec<_>>();
                    return fail(applied, id.clone(), insert_sql(&store.schema, &pairs), message, pending);
                }
                let pairs = draft.iter().filter_map(|(name, value)| value.clone().map(|cell| (name.clone(), cell))).collect::<Vec<_>>();
                let sql = insert_sql(&store.schema, &pairs);
                store.rows.push(Row { id: id.clone(), cells, locator: None, located: false });
                let schema = store.schema.clone();
                refresh_located(&schema, &mut store.rows);
                applied.push(Applied { id: id.clone(), sql });
            }
        }
    }
    CommitReport { applied, failed: None, pending_ids: Vec::new(), rolled_back: false, rule: COMMIT_RULE }
}
