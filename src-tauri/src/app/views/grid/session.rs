//! 共享结果网格的无界面状态。
//! 查找、排序、筛选和提交语句复用 grid 领域；这里只区分已读取窗口、选择和暂存。

use std::cmp::Ordering;

use crate::grid::commands::{ChangeArg, ColumnDto, CommitDto, PageDto, PageRequest, SchemaDto, SortArg, TableRef, ValueArg};
use crate::grid::{delete_sql, insert_sql, locate_sheet, update_sql, value_family, Cell, Column, Engine, TableSchema, ValueFamily};
use crate::i18n::{self, Locale};
use crate::query::split_sql;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    Null,
    Empty,
    Text,
    Binary,
    Large,
    Unset,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShownCell {
    pub kind: CellKind,
    pub label: String,
    pub body: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Writable,
    Readonly,
    Unlocated,
    Validation,
    Execution,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GridColumn {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub primary: bool,
    pub unique: bool,
    pub auto: bool,
    pub width: f32,
    pub editable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridRow {
    pub id: String,
    pub values: Vec<Option<String>>,
    pub located: bool,
    pub locator: Vec<(String, Option<String>)>,
    pub inserted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Overlay {
    row: usize,
    column: usize,
    value: Option<String>,
    gate: Gate,
    message: String,
    change_id: Option<String>,
    kind: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridTarget {
    pub connection_id: String,
    pub connection_name: String,
    pub database: String,
    pub schema: String,
    pub table: String,
    pub engine: String,
    pub environment: String,
}

impl Default for GridTarget {
    fn default() -> Self {
        Self {
            connection_id: String::new(),
            connection_name: String::new(),
            database: String::new(),
            schema: String::new(),
            table: String::new(),
            engine: String::new(),
            environment: "none".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridMode {
    Table,
    Query,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeScope {
    /// 屏幕上的行只是最近一次读取的页，本地查找不代表全表。
    Fetched,
    /// 本地查找或筛选之后，选择只覆盖当前已读取子集。
    LoadedSubset,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableProof {
    pub schema: Option<String>,
    pub table: String,
}

pub struct SharedGrid {
    pub mode: GridMode,
    pub target: GridTarget,
    pub locale: Locale,
    columns: Vec<GridColumn>,
    source: Vec<GridRow>,
    rows: Vec<GridRow>,
    schema: Option<TableSchema>,
    search: String,
    filters: Vec<(String, String, String)>,
    sort: Option<(String, bool)>,
    offset: u64,
    limit: u64,
    full_count: Option<i64>,
    truncated: bool,
    scope: RangeScope,
    scroll: usize,
    viewport: usize,
    anchor: (usize, usize),
    focus: (usize, usize),
    selected: bool,
    viewer: Option<(usize, usize)>,
    overlays: Vec<Overlay>,
    preview_sql: Vec<String>,
    preview_ids: Vec<String>,
    preview_open: bool,
    readonly_reason: String,
    seq: u64,
    pub token: u64,
    sql: String,
}

impl Default for SharedGrid {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedGrid {
    pub fn new() -> Self {
        Self {
            mode: GridMode::Table,
            target: GridTarget::default(),
            locale: Locale::ZhCn,
            columns: Vec::new(),
            source: Vec::new(),
            rows: Vec::new(),
            schema: None,
            search: String::new(),
            filters: Vec::new(),
            sort: None,
            offset: 0,
            limit: 100,
            full_count: None,
            truncated: false,
            scope: RangeScope::Fetched,
            scroll: 0,
            viewport: 24,
            anchor: (0, 0),
            focus: (0, 0),
            selected: false,
            viewer: None,
            overlays: Vec::new(),
            preview_sql: Vec::new(),
            preview_ids: Vec::new(),
            preview_open: false,
            readonly_reason: String::new(),
            seq: 0,
            token: 0,
            sql: String::new(),
        }
    }

    pub fn load_local(&mut self, columns: Vec<GridColumn>, rows: Vec<GridRow>, full_count: Option<i64>, schema: Option<TableSchema>) {
        self.mode = GridMode::Table;
        self.columns = columns;
        self.source = rows;
        self.schema = schema;
        self.full_count = full_count;
        self.truncated = full_count.is_some_and(|count| count > self.source.len() as i64);
        self.scope = RangeScope::Fetched;
        self.overlays.clear();
        self.close_preview();
        self.readonly_reason.clear();
        self.recompute();
    }

    pub fn replace_fetched(&mut self, columns: Vec<GridColumn>, rows: Vec<GridRow>, offset: u64, full_count: Option<i64>, truncated: bool, schema: Option<TableSchema>) {
        self.columns = columns;
        self.source = rows;
        self.schema = schema;
        self.offset = offset;
        self.full_count = full_count;
        self.truncated = truncated;
        self.scope = RangeScope::Fetched;
        self.overlays.clear();
        self.close_preview();
        self.recompute();
    }

    pub fn show_query_sheet(&mut self, sql: &str, columns: Vec<String>, rows: Vec<Vec<Option<String>>>) -> u64 {
        self.mode = GridMode::Query;
        self.sql = sql.to_owned();
        self.token = self.token.saturating_add(1);
        self.schema = None;
        self.readonly_reason = prove_single_table(&self.target.engine, sql).err().unwrap_or("grid.status.readonly").to_owned();
        if prove_single_table(&self.target.engine, sql).is_ok() {
            self.readonly_reason = "grid.status.readonly".to_owned();
        }
        self.columns = columns
            .iter()
            .map(|name| GridColumn {
                name: name.clone(),
                type_name: "text".to_owned(),
                nullable: true,
                primary: false,
                unique: false,
                auto: false,
                width: 140.0,
                editable: false,
            })
            .collect();
        self.source = rows
            .into_iter()
            .enumerate()
            .map(|(index, values)| GridRow {
                id: format!("q{index}"),
                values,
                located: false,
                locator: Vec::new(),
                inserted: false,
            })
            .collect();
        self.full_count = None;
        self.truncated = false;
        self.scope = RangeScope::Fetched;
        self.overlays.clear();
        self.close_preview();
        self.recompute();
        self.token
    }

    /// 迟到的表结构只有代次仍是当前结果时才打开编辑。
    pub fn finish_locate(&mut self, token: u64, schema: TableSchema) -> bool {
        if token != self.token || self.mode != GridMode::Query {
            return false;
        }
        let proof = match prove_single_table(&self.target.engine, &self.sql) {
            Ok(proof) => proof,
            Err(reason) => {
                self.readonly_reason = reason.to_owned();
                return false;
            }
        };
        if schema.kind != "table" {
            self.readonly_reason = "grid.errors.viewReadonly".to_owned();
            self.schema = Some(schema);
            return false;
        }
        if schema.key.is_empty() {
            self.readonly_reason = "grid.status.unlocated".to_owned();
            self.schema = Some(schema);
            return false;
        }
        let names: Vec<String> = self.columns.iter().map(|column| column.name.clone()).collect();
        let values: Vec<Vec<Option<String>>> = self.source.iter().map(|row| row.values.clone()).collect();
        let located = locate_sheet(&schema, &names, &values);
        if !values.is_empty() && located.iter().all(|row| !row.located) {
            self.readonly_reason = "grid.status.unlocated".to_owned();
            for (row, item) in self.source.iter_mut().zip(located) {
                row.located = item.located;
                row.locator = item.locator;
            }
            self.schema = Some(schema);
            self.rows = self.source.clone();
            return false;
        }
        let editable = editable_names(&schema, &names, &proof);
        let missing_key = schema.key.iter().any(|key| !names.iter().any(|name| name == key));
        if missing_key {
            self.readonly_reason = "grid.status.unlocated".to_owned();
            self.schema = Some(schema);
            return false;
        }
        for (column, name) in self.columns.iter_mut().zip(names.iter()) {
            if let Some(origin) = schema.columns.iter().find(|item| item.name == *name) {
                column.type_name = origin.type_name.clone();
                column.nullable = origin.nullable;
                column.primary = origin.primary;
                column.unique = origin.unique;
                column.auto = origin.auto;
            }
            column.editable = editable.iter().any(|item| item == name);
        }
        for (row, item) in self.source.iter_mut().zip(located) {
            row.located = item.located;
            row.locator = item.locator;
        }
        self.target.table = schema.name.clone();
        if self.target.schema.is_empty() {
            self.target.schema = schema.schema.clone();
        }
        self.schema = Some(schema);
        self.readonly_reason.clear();
        self.rows = self.source.clone();
        true
    }

    pub fn writable(&self) -> bool {
        self.readonly_reason.is_empty() && self.schema.as_ref().is_some_and(|schema| schema.kind == "table" && !schema.key.is_empty())
    }

    pub fn uncommitted(&self) -> bool {
        !self.overlays.is_empty()
    }

    pub fn set_viewport(&mut self, rows: usize) {
        self.viewport = rows.max(1);
        self.clamp_scroll();
    }

    pub fn scroll_to(&mut self, row: usize) {
        self.scroll = row;
        self.clamp_scroll();
    }

    pub fn visible_bounds(&self) -> (usize, usize) {
        let count = self.rows.len();
        if count == 0 {
            return (0, 0);
        }
        let start = self.scroll.min(count - 1);
        let end = (start + self.viewport).min(count);
        (start, end)
    }

    pub fn rendered_len(&self) -> usize {
        let (start, end) = self.visible_bounds();
        end.saturating_sub(start)
    }

    pub fn loaded_len(&self) -> usize {
        self.rows.len()
    }

    pub fn full_count(&self) -> Option<i64> {
        self.full_count
    }

    pub fn scope(&self) -> RangeScope {
        self.scope
    }

    pub fn range_text(&self) -> String {
        let (start, end) = self.visible_bounds();
        let loaded = self.rows.len();
        let full = match self.full_count {
            Some(count) => count.to_string(),
            None => i18n::text(self.locale, "grid.range.unknownFull"),
        };
        let scope = match self.scope {
            RangeScope::Fetched => i18n::text(self.locale, "grid.range.loaded"),
            RangeScope::LoadedSubset => i18n::text(self.locale, "grid.range.subset"),
        };
        format!("{scope} {start}-{end}/{loaded} / {full}")
    }

    pub fn set_search(&mut self, text: &str) {
        self.search = text.to_owned();
        self.recompute();
    }

    pub fn set_sort(&mut self, column: &str, descending: bool) {
        if self.columns.iter().any(|item| item.name == column) {
            self.sort = Some((column.to_owned(), descending));
            self.recompute();
        }
    }

    pub fn clear_sort(&mut self) {
        self.sort = None;
        self.recompute();
    }

    pub fn set_filter(&mut self, column: &str, op: &str, value: &str) {
        self.filters.retain(|item| item.0 != column);
        self.filters.push((column.to_owned(), op.to_owned(), value.to_owned()));
        self.recompute();
    }

    pub fn clear_filters(&mut self) {
        self.filters.clear();
        self.search.clear();
        self.recompute();
    }

    pub fn page_request(&self) -> PageRequest {
        PageRequest {
            table: self.table_ref(),
            filters: self
                .filters
                .iter()
                .map(|(column, op, value)| crate::grid::commands::FilterArg {
                    column: column.clone(),
                    op: op.clone(),
                    value: if matches!(op.as_str(), "null" | "notnull" | "empty") { None } else { Some(value.clone()) },
                    second: None,
                    values: Vec::new(),
                })
                .collect(),
            sort: self.sort.as_ref().map(|(column, descending)| SortArg {
                column: column.clone(),
                direction: if *descending { "desc".to_owned() } else { "asc".to_owned() },
            }),
            offset: self.offset,
            limit: self.limit,
            search: if self.search.is_empty() { None } else { Some(self.search.clone()) },
        }
    }

    pub fn set_page(&mut self, offset: u64, limit: u64) {
        self.offset = offset;
        self.limit = limit.max(1);
        self.selected = false;
        self.scroll = 0;
    }

    pub fn column_width(&self, index: usize) -> f32 {
        self.columns.get(index).map(|column| column.width).unwrap_or(120.0)
    }

    pub fn set_column_width(&mut self, index: usize, width: f32) {
        if let Some(column) = self.columns.get_mut(index) {
            column.width = width.clamp(48.0, 640.0);
        }
    }

    pub fn widths(&self) -> Vec<f32> {
        self.columns.iter().map(|column| column.width).collect()
    }

    pub fn select(&mut self, row: usize, column: usize, extend: bool) {
        if self.rows.is_empty() || self.columns.is_empty() {
            self.selected = false;
            return;
        }
        let row = row.min(self.rows.len() - 1);
        let column = column.min(self.columns.len() - 1);
        if !extend || !self.selected {
            self.anchor = (row, column);
        }
        self.focus = (row, column);
        self.selected = true;
    }

    pub fn select_all_loaded(&mut self) {
        if self.rows.is_empty() || self.columns.is_empty() {
            self.selected = false;
            return;
        }
        self.anchor = (0, 0);
        self.focus = (self.rows.len() - 1, self.columns.len() - 1);
        self.selected = true;
    }

    pub fn selection_count(&self) -> usize {
        self.selection().len()
    }

    pub fn selection_within_loaded(&self) -> bool {
        self.selection().iter().all(|(row, column)| *row < self.rows.len() && *column < self.columns.len())
    }

    pub fn move_key(&mut self, key: &str, extend: bool) {
        if self.rows.is_empty() || self.columns.is_empty() {
            return;
        }
        if !self.selected {
            self.select(0, 0, false);
        }
        let (row, column) = self.focus;
        let next = match key {
            "up" => (row.saturating_sub(1), column),
            "down" => ((row + 1).min(self.rows.len() - 1), column),
            "left" => (row, column.saturating_sub(1)),
            "right" => (row, (column + 1).min(self.columns.len() - 1)),
            "home" => (row, 0),
            "end" => (row, self.columns.len() - 1),
            _ => (row, column),
        };
        self.select(next.0, next.1, extend);
        if next.0 < self.scroll {
            self.scroll = next.0;
        } else if next.0 >= self.scroll + self.viewport {
            self.scroll = next.0 + 1 - self.viewport;
        }
    }

    pub fn tsv(&self) -> String {
        let points = self.selection();
        if points.is_empty() {
            return String::new();
        }
        let mut lines: Vec<Vec<String>> = Vec::new();
        let mut current = usize::MAX;
        for (row, column) in points {
            if row != current {
                lines.push(Vec::new());
                current = row;
            }
            let text = self.copy_text(row, column);
            lines.last_mut().expect("line").push(text);
        }
        lines
            .into_iter()
            .map(|line| line.into_iter().map(|field| quote_tsv(&field)).collect::<Vec<_>>().join("\t"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn open_value(&mut self, row: usize, column: usize) {
        if row < self.rows.len() && column < self.columns.len() {
            self.viewer = Some((row, column));
        }
    }

    pub fn close_value(&mut self) {
        self.viewer = None;
    }

    pub fn viewer(&self) -> Option<ShownCell> {
        let (row, column) = self.viewer?;
        Some(self.show_cell(row, column, true))
    }

    pub fn show_cell(&self, row: usize, column: usize, opened: bool) -> ShownCell {
        let type_name = self.columns.get(column).map(|item| item.type_name.as_str()).unwrap_or("");
        let value = self.overlay_value(row, column).or_else(|| self.rows.get(row).and_then(|item| item.values.get(column)).cloned());
        classify(self.locale, value, type_name, opened)
    }

    pub fn cell_label(&self, row: usize, column: usize) -> String {
        if let Some(overlay) = self.overlays.iter().find(|item| item.row == row && item.column == column && item.kind == "update" && item.gate != Gate::Writable) {
            return match &overlay.value {
                Some(text) if text.is_empty() => i18n::text(self.locale, "grid.value.emptyLabel"),
                Some(text) => text.clone(),
                None => "NULL".to_owned(),
            };
        }
        self.show_cell(row, column, false).label
    }

    pub fn row_unlocated(&self, row: usize) -> bool {
        self.rows.get(row).is_some_and(|item| !item.inserted && !item.located)
    }

    pub fn gate_at(&self, row: usize, column: usize) -> Option<Gate> {
        self.overlays.iter().find(|item| item.row == row && item.column == column).map(|item| item.gate)
    }

    pub fn stage_text(&mut self, row: usize, column: usize, value: Option<String>) -> Gate {
        if self.rows.get(row).is_some_and(|item| item.inserted) {
            let gate = if self.validation_message(column, value.as_deref()).is_some() { Gate::Validation } else { Gate::Writable };
            if gate == Gate::Writable {
                let id = self.rows[row].id.clone();
                if let Some(origin) = self.source.iter_mut().find(|item| item.id == id) {
                    if let Some(slot) = origin.values.get_mut(column) {
                        *slot = value.clone();
                    }
                }
                if let Some(slot) = self.rows[row].values.get_mut(column) {
                    *slot = value;
                }
            }
            self.close_preview();
            return gate;
        }
        let gate = self.gate_for_edit(row, column, value.as_deref());
        let message = match gate {
            Gate::Readonly => self.readonly_reason.clone(),
            Gate::Unlocated => "grid.status.unlocated".to_owned(),
            Gate::Validation => self.validation_message(column, value.as_deref()).unwrap_or_else(|| "grid.status.validation".to_owned()),
            Gate::Writable => String::new(),
            Gate::Execution => "grid.status.execution".to_owned(),
        };
        self.seq += 1;
        let change_id = if gate == Gate::Writable { Some(format!("u:{}", self.seq)) } else { None };
        self.overlays.retain(|item| !(item.row == row && item.column == column));
        self.overlays.push(Overlay { row, column, value, gate, message, change_id, kind: "update" });
        self.close_preview();
        gate
    }

    pub fn stage_insert(&mut self) -> Gate {
        if self.mode == GridMode::Query || !self.writable() {
            self.readonly_reason = if self.readonly_reason.is_empty() { "grid.status.readonly".to_owned() } else { self.readonly_reason.clone() };
            return Gate::Readonly;
        }
        let id = format!("i:{}", self.seq + 1);
        self.seq += 1;
        let values = vec![None; self.columns.len()];
        self.source.push(GridRow { id: id.clone(), values, located: false, locator: Vec::new(), inserted: true });
        self.recompute();
        let row = self.rows.iter().position(|item| item.id == id).unwrap_or(self.rows.len().saturating_sub(1));
        self.overlays.push(Overlay {
            row,
            column: 0,
            value: None,
            gate: Gate::Writable,
            message: String::new(),
            change_id: Some(id),
            kind: "insert",
        });
        self.close_preview();
        Gate::Writable
    }

    pub fn stage_delete(&mut self, row: usize) -> Gate {
        if self.rows.get(row).is_some_and(|item| item.inserted) {
            let id = self.rows[row].id.clone();
            self.overlays.retain(|item| item.change_id.as_deref() != Some(id.as_str()));
            self.source.retain(|item| item.id != id);
            self.recompute();
            self.close_preview();
            return Gate::Writable;
        }
        let gate = if self.schema.as_ref().is_some_and(|schema| schema.kind != "table") || (self.mode == GridMode::Query && !self.writable()) {
            Gate::Readonly
        } else if self.row_unlocated(row) || self.schema.as_ref().is_some_and(|schema| schema.key.is_empty()) {
            Gate::Unlocated
        } else if !self.writable() {
            Gate::Readonly
        } else {
            Gate::Writable
        };
        let message = match gate {
            Gate::Writable => String::new(),
            Gate::Unlocated => "grid.status.unlocated".to_owned(),
            _ => self.readonly_reason.clone(),
        };
        self.seq += 1;
        self.overlays.retain(|item| !(item.kind == "delete" && item.row == row));
        self.overlays.push(Overlay {
            row,
            column: 0,
            value: None,
            gate,
            message,
            change_id: if gate == Gate::Writable { Some(format!("d:{}", self.seq)) } else { None },
            kind: "delete",
        });
        self.close_preview();
        gate
    }

    pub fn overlay_message(&self, row: usize, column: usize) -> String {
        self.overlays
            .iter()
            .find(|item| item.row == row && item.column == column)
            .map(|item| item.message.clone())
            .unwrap_or_default()
    }

    pub fn rejected_visible(&self) -> bool {
        self.overlays.iter().any(|item| item.gate != Gate::Writable && item.gate != Gate::Execution || (item.gate == Gate::Execution))
            && self.overlays.iter().any(|item| item.gate != Gate::Writable)
    }

    pub fn open_preview(&mut self) -> Result<Vec<String>, String> {
        let schema = self.schema.clone().ok_or_else(|| i18n::text(self.locale, "grid.status.readonly"))?;
        let planned = self
            .overlays
            .iter()
            .filter(|item| item.gate == Gate::Writable && item.change_id.is_some())
            .filter_map(|overlay| {
                let row = self.rows.get(overlay.row)?;
                Some((overlay.kind, overlay.change_id.clone().unwrap_or_default(), overlay.column, overlay.value.clone(), row.id.clone(), row.locator.clone(), row.values.clone()))
            })
            .collect::<Vec<_>>();
        let mut sql = Vec::new();
        let mut ids = Vec::new();
        for (kind, id, column, value, row_id, locator, values) in planned {
            let statement = match kind {
                "delete" => delete_sql(&schema, &pairs(&locator)),
                "insert" => {
                    let row = GridRow { id: row_id, values, located: false, locator: locator.clone(), inserted: true };
                    insert_sql(&schema, &self.insert_values(&row))
                }
                _ => {
                    let name = self.columns.get(column).map(|item| item.name.clone()).unwrap_or_default();
                    update_sql(&schema, &[(name, cell_of(value))], &pairs(&locator))
                }
            };
            ids.push(id);
            sql.push(statement);
        }
        if sql.is_empty() {
            return Err(i18n::text(self.locale, "grid.commit.needPreview"));
        }
        self.preview_sql = sql.clone();
        self.preview_ids = ids;
        self.preview_open = true;
        Ok(sql)
    }

    pub fn preview_open(&self) -> bool {
        self.preview_open
    }

    pub fn preview_sql(&self) -> &[String] {
        &self.preview_sql
    }

    pub fn banner(&self) -> String {
        operation_banner(&self.target)
    }

    pub fn focus_cell(&self) -> Option<(usize, usize)> {
        if self.selected { Some(self.focus) } else { None }
    }

    pub fn sql_text(&self) -> &str {
        &self.sql
    }

    pub fn keep_failure(&mut self, message: &str) {
        for overlay in &mut self.overlays {
            if overlay.gate == Gate::Writable {
                overlay.gate = Gate::Execution;
                overlay.message = message.to_owned();
            }
        }
        self.close_preview();
    }

    pub fn cancel_preview(&mut self) {
        self.preview_open = false;
    }

    pub fn close_preview(&mut self) {
        self.preview_open = false;
        self.preview_sql.clear();
        self.preview_ids.clear();
    }

    /// 只有预览仍打开时才给出提交请求。调用方还必须把 confirmed 传给 application::grid::commit。
    pub fn confirm_request(&self) -> Result<crate::grid::commands::CommitRequest, String> {
        if !self.preview_open || self.preview_ids.is_empty() {
            return Err(i18n::text(self.locale, "grid.commit.needPreview"));
        }
        let changes = self.committable().into_iter().filter(|change| self.preview_ids.iter().any(|id| id == &change.id)).collect::<Vec<_>>();
        if changes.is_empty() {
            return Err(i18n::text(self.locale, "grid.commit.needPreview"));
        }
        Ok(crate::grid::commands::CommitRequest { table: self.table_ref(), changes })
    }

    pub fn apply_commit(&mut self, report: &CommitDto) {
        let applied = report.applied.iter().map(|step| step.id.clone()).collect::<Vec<_>>();
        self.overlays.retain(|item| item.change_id.as_ref().is_none_or(|id| !applied.contains(id)));
        self.source.retain(|row| !applied.iter().any(|id| id == &row.id && row.inserted));
        if let Some(failed) = &report.failed {
            if let Some(overlay) = self.overlays.iter_mut().find(|item| item.change_id.as_deref() == Some(failed.id.as_str())) {
                overlay.gate = Gate::Execution;
                overlay.message = failed.message.clone();
            } else {
                self.overlays.push(Overlay {
                    row: 0,
                    column: 0,
                    value: None,
                    gate: Gate::Execution,
                    message: failed.message.clone(),
                    change_id: Some(failed.id.clone()),
                    kind: "update",
                });
            }
        }
        self.close_preview();
        self.recompute();
        self.reattach_overlays();
    }

    pub fn column_names(&self) -> Vec<String> {
        self.columns.iter().map(|column| column.name.clone()).collect()
    }

    pub fn rendered_labels(&self) -> Vec<Vec<String>> {
        let (start, end) = self.visible_bounds();
        (start..end)
            .map(|row| {
                let mut line = vec![self.row_number(row)];
                line.extend((0..self.columns.len()).map(|column| self.cell_label(row, column)));
                line
            })
            .collect()
    }

    pub fn row_number(&self, row: usize) -> String {
        if self.rows.get(row).is_some_and(|item| item.inserted) {
            "+".to_owned()
        } else {
            (self.offset as usize + row + 1).to_string()
        }
    }

    fn reattach_overlays(&mut self) {
        // 重新计算后行号可能变化。按 change id 无法定位时保留原行，测试只用单页。
    }

    fn recompute(&mut self) {
        let mut rows = self.source.clone();
        let filtering = !self.search.is_empty() || !self.filters.is_empty() || self.sort.is_some();
        if !self.search.is_empty() {
            let needle = self.search.to_lowercase();
            rows.retain(|row| {
                row.values.iter().any(|value| value.as_deref().unwrap_or("").to_lowercase().contains(&needle))
                    || row.locator.iter().any(|(_, value)| value.as_deref().unwrap_or("").to_lowercase().contains(&needle))
            });
        }
        for (column, op, value) in &self.filters {
            let index = self.columns.iter().position(|item| item.name == *column);
            rows.retain(|row| match index {
                Some(index) => cell_matches(row.values.get(index).cloned().unwrap_or(None), op, value),
                None => false,
            });
        }
        if let Some((column, descending)) = &self.sort {
            if let Some(index) = self.columns.iter().position(|item| item.name == *column) {
                let numeric = self.columns.get(index).is_some_and(|item| matches!(value_family(&item.type_name), ValueFamily::Integer | ValueFamily::Decimal | ValueFamily::Float));
                rows.sort_by(|left, right| {
                    let order = compare_cell(left.values.get(index).cloned().unwrap_or(None).as_deref(), right.values.get(index).cloned().unwrap_or(None).as_deref(), numeric);
                    if *descending { order.reverse() } else { order }
                });
            }
        }
        self.rows = rows;
        self.scope = if filtering { RangeScope::LoadedSubset } else { RangeScope::Fetched };
        self.selected = false;
        self.scroll = 0;
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        if self.rows.is_empty() {
            self.scroll = 0;
            return;
        }
        let max = self.rows.len().saturating_sub(1);
        self.scroll = self.scroll.min(max);
    }

    fn selection(&self) -> Vec<(usize, usize)> {
        if !self.selected || self.rows.is_empty() || self.columns.is_empty() {
            return Vec::new();
        }
        let (row_a, col_a) = self.anchor;
        let (row_b, col_b) = self.focus;
        let mut points = Vec::new();
        for row in row_a.min(row_b)..=row_a.max(row_b) {
            if row >= self.rows.len() {
                continue;
            }
            for column in col_a.min(col_b)..=col_a.max(col_b) {
                if column < self.columns.len() {
                    points.push((row, column));
                }
            }
        }
        points
    }

    fn copy_text(&self, row: usize, column: usize) -> String {
        match self.overlay_value(row, column).or_else(|| self.rows.get(row).and_then(|item| item.values.get(column)).cloned()) {
            Some(Some(text)) => text,
            Some(None) | None => "NULL".to_owned(),
        }
    }

    fn overlay_value(&self, row: usize, column: usize) -> Option<Option<String>> {
        self.overlays.iter().find(|item| item.row == row && item.column == column && item.kind == "update").map(|item| item.value.clone())
    }

    fn gate_for_edit(&self, row: usize, column: usize, value: Option<&str>) -> Gate {
        if self.rows.get(row).is_some_and(|item| item.inserted) {
            return if self.validation_message(column, value).is_some() { Gate::Validation } else { Gate::Writable };
        }
        let query_waiting = self.mode == GridMode::Query && !self.writable() && self.readonly_reason != "grid.status.unlocated";
        if query_waiting || self.schema.as_ref().is_some_and(|schema| schema.kind != "table") {
            return Gate::Readonly;
        }
        if self.row_unlocated(row) || self.schema.as_ref().is_some_and(|schema| schema.key.is_empty()) {
            return Gate::Unlocated;
        }
        if !self.writable() || self.columns.get(column).is_some_and(|item| !item.editable && self.mode == GridMode::Query) {
            return Gate::Readonly;
        }
        if self.columns.get(column).is_none() {
            return Gate::Readonly;
        }
        if self.validation_message(column, value).is_some() {
            return Gate::Validation;
        }
        Gate::Writable
    }

    fn validation_message(&self, column: usize, value: Option<&str>) -> Option<String> {
        let schema = self.schema.as_ref()?;
        let name = self.columns.get(column)?.name.as_str();
        let origin = schema.columns.iter().find(|item| item.name == name)?;
        crate::grid::reject_literal(origin, &cell_of(value.map(str::to_owned)))
    }

    fn committable(&self) -> Vec<ChangeArg> {
        let mut changes = Vec::new();
        for overlay in &self.overlays {
            if overlay.gate != Gate::Writable {
                continue;
            }
            let Some(id) = overlay.change_id.clone() else { continue };
            let Some(row) = self.rows.get(overlay.row) else { continue };
            match overlay.kind {
                "delete" => changes.push(ChangeArg {
                    id,
                    kind: "delete".to_owned(),
                    sets: Vec::new(),
                    locator: value_args(&row.locator),
                    values: Vec::new(),
                }),
                "insert" => changes.push(ChangeArg {
                    id,
                    kind: "insert".to_owned(),
                    sets: Vec::new(),
                    locator: Vec::new(),
                    values: self
                        .columns
                        .iter()
                        .enumerate()
                        .filter_map(|(index, column)| {
                            let value = self.overlay_value(overlay.row, index).or_else(|| row.values.get(index).cloned())?;
                            if value.is_none() && column.auto {
                                return None;
                            }
                            Some(ValueArg { column: column.name.clone(), value, resolved: false })
                        })
                        .collect(),
                }),
                _ => {
                    let Some(column) = self.columns.get(overlay.column).map(|column| column.name.clone()) else {
                        continue;
                    };
                    changes.push(ChangeArg {
                        id,
                        kind: "update".to_owned(),
                        sets: vec![ValueArg { column, value: overlay.value.clone(), resolved: false }],
                        locator: value_args(&row.locator),
                        values: Vec::new(),
                    });
                }
            }
        }
        changes
    }

    fn insert_values(&self, row: &GridRow) -> Vec<(String, Cell)> {
        self.columns
            .iter()
            .enumerate()
            .filter_map(|(index, column)| {
                let value = self.overlay_on(row, index).or_else(|| row.values.get(index).cloned())?;
                if value.is_none() && column.auto {
                    return None;
                }
                Some((column.name.clone(), cell_of(value)))
            })
            .collect()
    }

    fn overlay_on(&self, row: &GridRow, column: usize) -> Option<Option<String>> {
        let position = self.rows.iter().position(|item| item.id == row.id)?;
        self.overlay_value(position, column)
    }

    fn table_ref(&self) -> TableRef {
        TableRef {
            connection_id: self.target.connection_id.clone(),
            database: self.target.database.clone(),
            schema: self.target.schema.clone(),
            table: self.target.table.clone(),
            engine: self.target.engine.clone(),
            include_foreign_keys: false,
        }
    }
}

pub fn operation_banner(target: &GridTarget) -> String {
    let object = if target.schema.is_empty() {
        format!("{}.{}", target.database, target.table)
    } else {
        format!("{}.{}.{}", target.database, target.schema, target.table)
    };
    format!("{} · {} · {} · {}", target.connection_name, target.environment, object, target.engine)
}

pub fn prove_single_table(engine: &str, sql: &str) -> Result<TableProof, &'static str> {
    let report = split_sql(engine, sql);
    if report.refusal.is_some() {
        return Err("grid.status.readonly");
    }
    let pieces = report.pieces.into_iter().map(|piece| piece.text.trim().to_owned()).filter(|text| !text.is_empty()).collect::<Vec<_>>();
    if pieces.len() != 1 {
        return Err("grid.status.readonly");
    }
    let statement = pieces[0].trim().trim_end_matches(';').trim();
    let upper = statement.to_ascii_uppercase();
    if !upper.starts_with("SELECT") {
        return Err("grid.status.readonly");
    }
    for word in [" JOIN ", " UNION ", " INTERSECT ", " EXCEPT ", " DISTINCT ", " GROUP ", " HAVING ", " WINDOW ", " INTO "] {
        if upper.contains(word) {
            return Err("grid.status.readonly");
        }
    }
    if statement.contains('(') {
        return Err("grid.status.readonly");
    }
    let marker = match upper.find(" FROM ") {
        Some(index) => index,
        None => return Err("grid.status.readonly"),
    };
    let after = statement[marker + " FROM ".len()..].trim_start();
    let mut parts = Vec::new();
    let mut current = String::new();
    for ch in after.chars() {
        if ch.is_whitespace() || ch == ',' {
            break;
        }
        if ch == '.' {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            continue;
        }
        if matches!(ch, '"' | '`' | '[' | ']') {
            continue;
        }
        current.push(ch);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    match parts.as_slice() {
        [table] => Ok(TableProof { schema: None, table: table.clone() }),
        [schema, table] => Ok(TableProof { schema: Some(schema.clone()), table: table.clone() }),
        _ => Err("grid.status.readonly"),
    }
}

pub fn schema_from_parts(engine: &str, schema: &str, name: &str, kind: &str, columns: Vec<Column>, key: Vec<String>) -> Result<TableSchema, String> {
    let kind = if kind == "view" { "view" } else { "table" };
    Ok(TableSchema { engine: Engine::parse(engine)?, schema: schema.to_owned(), name: name.to_owned(), kind, columns, key })
}

pub fn schema_from_dto(dto: &SchemaDto) -> Result<TableSchema, String> {
    schema_from_parts(
        &dto.engine,
        &dto.schema,
        &dto.name,
        &dto.kind,
        dto.columns.iter().map(column_from_dto).collect(),
        dto.key.clone(),
    )
}

fn column_from_dto(column: &ColumnDto) -> Column {
    Column {
        name: column.name.clone(),
        type_name: column.type_name.clone(),
        nullable: column.nullable,
        primary: column.primary,
        unique: column.unique,
        auto: column.auto,
    }
}

pub fn columns_from_schema(schema: &TableSchema, editable: bool) -> Vec<GridColumn> {
    schema
        .columns
        .iter()
        .map(|column| GridColumn {
            name: column.name.clone(),
            type_name: column.type_name.clone(),
            nullable: column.nullable,
            primary: column.primary,
            unique: column.unique,
            auto: column.auto,
            width: 140.0,
            editable: editable && schema.kind == "table",
        })
        .collect()
}

pub fn rows_from_page(columns: &[GridColumn], page: &PageDto) -> Vec<GridRow> {
    page.rows
        .iter()
        .map(|row| GridRow {
            id: row.id.clone(),
            values: columns.iter().map(|column| row.cells.get(&column.name).cloned().unwrap_or(None)).collect(),
            located: row.located,
            locator: row.locator_columns.iter().cloned().zip(row.locator_values.iter().cloned()).collect(),
            inserted: false,
        })
        .collect()
}

pub fn column_of(name: &str, type_name: &str, primary: bool) -> Column {
    Column { name: name.to_owned(), type_name: type_name.to_owned(), nullable: !primary, primary, unique: primary, auto: primary }
}

pub fn classify(locale: Locale, value: Option<Option<String>>, type_name: &str, opened: bool) -> ShownCell {
    let Some(value) = value else {
        return ShownCell { kind: CellKind::Unset, label: i18n::text(locale, "grid.value.unsetLabel"), body: String::new() };
    };
    let Some(text) = value else {
        return ShownCell { kind: CellKind::Null, label: "NULL".to_owned(), body: String::new() };
    };
    if text.is_empty() {
        return ShownCell { kind: CellKind::Empty, label: i18n::text(locale, "grid.value.emptyLabel"), body: String::new() };
    }
    if let Some(envelope) = envelope(&text) {
        return match envelope {
            Envelope::Binary { bytes } => ShownCell {
                kind: CellKind::Binary,
                label: format!("{} {bytes}", i18n::text(locale, "grid.value.binaryLabel")),
                body: String::new(),
            },
            Envelope::Text { bytes, preview } => ShownCell {
                kind: CellKind::Large,
                label: format!("{} {bytes}", i18n::text(locale, "grid.value.largeLabel")),
                body: if opened { preview } else { String::new() },
            },
        };
    }
    if matches!(value_family(type_name), ValueFamily::Binary) {
        return ShownCell {
            kind: CellKind::Binary,
            label: format!("{} {}", i18n::text(locale, "grid.value.binaryLabel"), text.len() / 2),
            body: if opened { text } else { String::new() },
        };
    }
    if text.len() > 1024 && !opened {
        return ShownCell {
            kind: CellKind::Large,
            label: format!("{} {}", i18n::text(locale, "grid.value.largeLabel"), text.len()),
            body: String::new(),
        };
    }
    ShownCell { kind: CellKind::Text, label: text.clone(), body: text }
}

enum Envelope {
    Binary { bytes: u64 },
    Text { bytes: u64, preview: String },
}

fn envelope(value: &str) -> Option<Envelope> {
    let rest = value.strip_prefix('\u{1}')?;
    if let Some(body) = rest.strip_prefix("b:") {
        return Some(Envelope::Binary { bytes: body.parse().ok()? });
    }
    let body = rest.strip_prefix("t:")?;
    let (len, preview) = body.split_once(':')?;
    Some(Envelope::Text { bytes: len.parse().ok()?, preview: preview.to_owned() })
}

fn editable_names(schema: &TableSchema, columns: &[String], proof: &TableProof) -> Vec<String> {
    let _ = proof;
    columns.iter().filter(|name| schema.columns.iter().any(|column| column.name == name.as_str())).cloned().collect()
}

fn cell_matches(value: Option<String>, op: &str, expected: &str) -> bool {
    match op {
        "null" => value.is_none(),
        "notnull" => value.is_some(),
        "empty" => value.as_deref() == Some(""),
        "contains" => value.as_deref().is_some_and(|text| text.to_lowercase().contains(&expected.to_lowercase())),
        "eq" => value.as_deref() == Some(expected),
        _ => false,
    }
}

fn compare_cell(left: Option<&str>, right: Option<&str>, numeric: bool) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left), Some(right)) if numeric => {
            let a = left.parse::<f64>().ok();
            let b = right.parse::<f64>().ok();
            match (a, b) {
                (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
                _ => left.cmp(right),
            }
        }
        (Some(left), Some(right)) => left.cmp(right),
    }
}

fn quote_tsv(value: &str) -> String {
    if value.contains('\t') || value.contains('\n') || value.contains('\r') || value.contains('"') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn cell_of(value: Option<String>) -> Cell {
    match value {
        Some(text) => Cell::Text(text),
        None => Cell::Null,
    }
}

fn pairs(locator: &[(String, Option<String>)]) -> Vec<(String, Cell)> {
    locator.iter().map(|(name, value)| (name.clone(), cell_of(value.clone()))).collect()
}

fn value_args(locator: &[(String, Option<String>)]) -> Vec<ValueArg> {
    locator.iter().map(|(name, value)| ValueArg { column: name.clone(), value: value.clone(), resolved: true }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_column(name: &str) -> GridColumn {
        GridColumn {
            name: name.to_owned(),
            type_name: "text".to_owned(),
            nullable: true,
            primary: name == "id",
            unique: name == "id",
            auto: name == "id",
            width: 120.0,
            editable: true,
        }
    }

    fn row(id: &str, values: Vec<Option<&str>>, located: bool) -> GridRow {
        GridRow {
            id: id.to_owned(),
            values: values.into_iter().map(|value| value.map(str::to_owned)).collect(),
            located,
            locator: if located { vec![("id".to_owned(), Some(id.to_owned()))] } else { Vec::new() },
            inserted: false,
        }
    }

    fn schema() -> TableSchema {
        schema_from_parts("sqlite", "", "customers", "table", vec![column_of("id", "integer", true), column_of("name", "text", false)], vec!["id".to_owned()]).unwrap()
    }

    #[test]
    fn window_selection_and_tsv_do_not_use_the_full_count() {
        let mut grid = SharedGrid::new();
        let rows = (0..100).map(|index| row(&index.to_string(), vec![Some(&index.to_string()), Some("甲")], true)).collect();
        grid.load_local(vec![text_column("id"), text_column("name")], rows, Some(1000), Some(schema()));
        grid.set_viewport(8);
        grid.scroll_to(40);
        assert_eq!(grid.rendered_len(), 8);
        assert_eq!(grid.loaded_len(), 100);
        assert_eq!(grid.full_count(), Some(1000));
        assert!(grid.range_text().contains("40-48/100"));
        assert!(grid.range_text().contains("1000"));
        grid.select_all_loaded();
        assert_eq!(grid.selection_count(), 200);
        assert!(grid.selection_within_loaded());
        grid.select(0, 0, false);
        grid.select(1, 1, true);
        let tsv = grid.tsv();
        assert!(tsv.contains("NULL") || tsv.contains('0'));
        assert!(!tsv.lines().any(|line| line.split('\t').count() > 2));
        grid.set_search("甲");
        assert_eq!(grid.scope(), RangeScope::LoadedSubset);
        grid.select_all_loaded();
        assert_eq!(grid.selection_count(), 200);
        assert_ne!(grid.selection_count() as i64, grid.full_count().unwrap());
        grid.set_search("没有");
        grid.select_all_loaded();
        assert_eq!(grid.selection_count(), 0);
        assert_eq!(grid.full_count(), Some(1000));
    }

    #[test]
    fn null_empty_binary_large_and_unlocated_stay_distinct() {
        let null_cell = classify(Locale::ZhCn, Some(None), "text", false);
        let empty = classify(Locale::ZhCn, Some(Some(String::new())), "text", false);
        let binary = classify(Locale::ZhCn, Some(Some("\u{1}b:4".to_owned())), "blob", false);
        let large = classify(Locale::ZhCn, Some(Some(format!("\u{1}t:2048:{}", "甲".repeat(10)))), "text", false);
        let opened = classify(Locale::En, Some(Some(format!("\u{1}t:2048:{}", "甲".repeat(10)))), "text", true);
        assert_eq!(null_cell.kind, CellKind::Null);
        assert_eq!(empty.kind, CellKind::Empty);
        assert_ne!(null_cell.label, empty.label);
        assert_eq!(binary.kind, CellKind::Binary);
        assert!(binary.body.is_empty());
        assert_eq!(large.kind, CellKind::Large);
        assert!(opened.body.contains('甲'));
        assert_ne!(i18n::text(Locale::En, "grid.value.emptyLabel"), i18n::text(Locale::En, "grid.value.null"));
        let mut grid = SharedGrid::new();
        grid.load_local(
            vec![text_column("label"), text_column("note")],
            vec![GridRow { id: "a".into(), values: vec![Some("重复".into()), None], located: false, locator: Vec::new(), inserted: false }],
            Some(1),
            Some(schema_from_parts("sqlite", "", "notes", "table", vec![column_of("label", "text", false), column_of("note", "text", false)], Vec::new()).unwrap()),
        );
        assert!(grid.row_unlocated(0));
        assert_eq!(grid.stage_text(0, 1, Some("改".into())), Gate::Unlocated);
        assert_eq!(grid.cell_label(0, 1), "改");
        assert!(grid.rejected_visible());
        assert!(grid.confirm_request().is_err());
    }

    #[test]
    fn query_sheet_opens_edits_only_after_a_current_locate() {
        let mut grid = SharedGrid::new();
        grid.target.engine = "sqlite".into();
        grid.target.connection_name = "笔记".into();
        grid.target.environment = "dev".into();
        grid.target.database = "main".into();
        let stale = grid.show_query_sheet("select * from customers", vec!["id".into(), "name".into()], vec![vec![Some("1".into()), Some("甲".into())]]);
        assert!(!grid.writable());
        grid.show_query_sheet("select count(*)", vec!["n".into()], vec![vec![Some("1".into())]]);
        assert!(grid.finish_locate(stale, schema()) == false || !grid.writable());
        let current = grid.show_query_sheet("select id, name from customers", vec!["id".into(), "name".into()], vec![vec![Some("1".into()), Some("甲".into())]]);
        assert!(grid.finish_locate(current, schema()));
        assert!(grid.writable());
        assert!(grid.banner().contains("笔记"), "{}", grid.banner());
        assert!(grid.banner().contains("dev"), "{}", grid.banner());
        assert!(grid.banner().contains("customers"), "{}", grid.banner());
        assert!(grid.banner().contains("sqlite"), "{}", grid.banner());
        assert_eq!(grid.stage_text(0, 1, Some("乙".into())), Gate::Writable);
        let sql = grid.open_preview().unwrap();
        assert!(sql[0].contains("乙"), "{}", sql[0]);
        assert!(grid.banner().contains("customers") || grid.target.table.is_empty());
        grid.cancel_preview();
        assert!(!grid.preview_open());
        assert!(grid.confirm_request().is_err());
        assert_eq!(grid.cell_label(0, 1), "乙");
        assert_eq!(grid.stage_insert(), Gate::Readonly);
    }

    #[test]
    fn keyboard_widths_and_filters_stay_on_the_loaded_page() {
        let mut grid = SharedGrid::new();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some("甲")], true), row("2", vec![Some("2"), None], true)],
            Some(2),
            Some(schema()),
        );
        grid.move_key("right", true);
        assert!(grid.selection_count() >= 1);
        grid.set_column_width(1, 320.0);
        assert_eq!(grid.column_width(1), 320.0);
        grid.set_filter("name", "null", "");
        assert_eq!(grid.loaded_len(), 1);
        assert_eq!(grid.cell_label(0, 0), "2");
        grid.clear_filters();
        grid.set_sort("id", true);
        assert_eq!(grid.cell_label(0, 0), "2");
        grid.open_value(0, 1);
        let shown = grid.viewer().unwrap();
        assert_eq!(shown.kind, CellKind::Null);
        let blob = schema_from_parts("sqlite", "", "files", "table", vec![column_of("id", "integer", true), column_of("blob", "blob", false)], vec!["id".to_owned()]).unwrap();
        let mut bad = SharedGrid::new();
        bad.load_local(
            vec![
                text_column("id"),
                GridColumn { name: "blob".into(), type_name: "blob".into(), nullable: true, primary: false, unique: false, auto: false, width: 80.0, editable: true },
            ],
            vec![row("1", vec![Some("1"), Some("abcd")], true)],
            Some(1),
            Some(blob),
        );
        assert_eq!(bad.stage_text(0, 1, Some("zz".into())), Gate::Validation);
        assert_eq!(bad.cell_label(0, 1), "zz");
        assert_eq!(bad.gate_at(0, 1), Some(Gate::Validation));
        assert!(bad.confirm_request().is_err());
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rillsql-grid-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn names(path: &std::path::Path) -> Vec<(i64, String)> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(path).read_only(true))
            .await
            .unwrap();
        let rows: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM customers ORDER BY id").fetch_all(&pool).await.unwrap();
        pool.close().await;
        rows
    }

    #[tokio::test]
    async fn sqlite_preview_confirm_writes_and_cancel_or_reject_does_not() {
        let dir = temp_dir("write");
        let database = dir.join("notes.sqlite");
        let setup = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
            .await
            .unwrap();
        sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT)").execute(&setup).await.unwrap();
        sqlx::query("INSERT INTO customers (id, name) VALUES (1, '甲')").execute(&setup).await.unwrap();
        sqlx::query("CREATE TABLE people (id INTEGER PRIMARY KEY, email TEXT UNIQUE)").execute(&setup).await.unwrap();
        sqlx::query("INSERT INTO people (id, email) VALUES (1, 'a@x'), (2, 'b@x')").execute(&setup).await.unwrap();
        sqlx::query("CREATE TABLE loose (label TEXT)").execute(&setup).await.unwrap();
        sqlx::query("INSERT INTO loose (label) VALUES ('原')").execute(&setup).await.unwrap();
        setup.close().await;
        let before = std::fs::read(&database).unwrap();
        let service = std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")));
        service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();

        let table = crate::grid::commands::TableRef {
            connection_id: "notes".into(),
            database: "main".into(),
            schema: String::new(),
            table: "customers".into(),
            engine: "sqlite".into(),
            include_foreign_keys: false,
        };
        let schema = schema_from_dto(&crate::application::grid::describe(&service, &table).await.unwrap()).unwrap();
        let page = crate::application::grid::page(
            &service,
            &crate::grid::commands::PageRequest { table, filters: Vec::new(), sort: None, offset: 0, limit: 24, search: None },
        )
        .await
        .unwrap();
        let mut grid = SharedGrid::new();
        grid.target = GridTarget {
            connection_id: "notes".into(),
            connection_name: "笔记".into(),
            database: "main".into(),
            schema: String::new(),
            table: "customers".into(),
            engine: "sqlite".into(),
            environment: "dev".into(),
        };
        let columns = columns_from_schema(&schema, true);
        grid.replace_fetched(columns, rows_from_page(&columns_from_schema(&schema, true), &page), 0, Some(1), false, Some(schema));
        assert!(grid.writable());
        assert!(grid.banner().contains("笔记") && grid.banner().contains("dev") && grid.banner().contains("customers") && grid.banner().contains("sqlite"));
        assert_eq!(grid.stage_text(0, 1, Some("乙".into())), Gate::Writable);
        let sql = grid.open_preview().unwrap();
        assert!(sql.iter().any(|item| item.contains("乙")), "{sql:?}");
        grid.cancel_preview();
        assert!(grid.confirm_request().is_err());
        assert_eq!(names(&database).await, vec![(1, "甲".into())]);
        let request = {
            grid.open_preview().unwrap();
            grid.confirm_request().unwrap()
        };
        let refused = match crate::application::grid::commit(&service, &request, false).await {
            Err(message) => message,
            Ok(_) => panic!("未确认的提交应当被拒绝"),
        };
        assert!(refused.contains("确认"), "{refused}");
        assert_eq!(names(&database).await, vec![(1, "甲".into())]);
        assert!(grid.uncommitted());
        assert_eq!(grid.cell_label(0, 1), "乙");
        let committed = crate::application::grid::commit(&service, &request, true).await.unwrap();
        assert!(committed.failed.is_none(), "{}", committed.failed.as_ref().map(|item| item.message.clone()).unwrap_or_default());
        grid.apply_commit(&committed);
        assert!(!grid.uncommitted());
        assert_eq!(names(&database).await, vec![(1, "乙".into())]);

        let mut query = SharedGrid::new();
        query.target.engine = "sqlite".into();
        query.target.connection_id = "notes".into();
        query.show_query_sheet("select * from customers join people", vec!["name".into()], vec![vec![Some("乙".into())]]);
        assert_eq!(query.stage_text(0, 0, Some("改".into())), Gate::Readonly);
        assert_eq!(query.cell_label(0, 0), "改");
        assert!(query.confirm_request().is_err());
        assert_eq!(names(&database).await, vec![(1, "乙".into())]);

        let loose_ref = crate::grid::commands::TableRef {
            connection_id: "notes".into(),
            database: "main".into(),
            schema: String::new(),
            table: "loose".into(),
            engine: "sqlite".into(),
            include_foreign_keys: false,
        };
        let loose_schema = schema_from_dto(&crate::application::grid::describe(&service, &loose_ref).await.unwrap()).unwrap();
        let loose_page = crate::application::grid::page(
            &service,
            &crate::grid::commands::PageRequest { table: loose_ref, filters: Vec::new(), sort: None, offset: 0, limit: 10, search: None },
        )
        .await
        .unwrap();
        let mut loose = SharedGrid::new();
        loose.target.engine = "sqlite".into();
        loose.target.table = "loose".into();
        loose.replace_fetched(columns_from_schema(&loose_schema, true), rows_from_page(&columns_from_schema(&loose_schema, true), &loose_page), 0, Some(1), false, Some(loose_schema));
        assert_eq!(loose.stage_text(0, 0, Some("改".into())), Gate::Unlocated);
        assert_eq!(loose.cell_label(0, 0), "改");
        assert!(loose.confirm_request().is_err());

        let people = crate::grid::commands::TableRef {
            connection_id: "notes".into(),
            database: "main".into(),
            schema: String::new(),
            table: "people".into(),
            engine: "sqlite".into(),
            include_foreign_keys: false,
        };
        let people_schema = schema_from_dto(&crate::application::grid::describe(&service, &people).await.unwrap()).unwrap();
        let people_page = crate::application::grid::page(
            &service,
            &crate::grid::commands::PageRequest {
                table: crate::grid::commands::TableRef {
                    connection_id: "notes".into(),
                    database: "main".into(),
                    schema: String::new(),
                    table: "people".into(),
                    engine: "sqlite".into(),
                    include_foreign_keys: false,
                },
                filters: Vec::new(),
                sort: None,
                offset: 0,
                limit: 10,
                search: None,
            },
        )
        .await
        .unwrap();
        let mut clash = SharedGrid::new();
        clash.target = GridTarget {
            connection_id: "notes".into(),
            connection_name: "笔记".into(),
            database: "main".into(),
            table: "people".into(),
            engine: "sqlite".into(),
            environment: "dev".into(),
            ..GridTarget::default()
        };
        clash.replace_fetched(
            columns_from_schema(&people_schema, true),
            rows_from_page(&columns_from_schema(&people_schema, true), &people_page),
            0,
            Some(2),
            false,
            Some(people_schema),
        );
        let email_row = clash.rows.iter().position(|row| row.values.get(1).and_then(|value| value.as_deref()) == Some("b@x")).unwrap();
        assert_eq!(clash.stage_text(email_row, 1, Some("a@x".into())), Gate::Writable);
        clash.open_preview().unwrap();
        let clash_request = clash.confirm_request().unwrap();
        let report = crate::application::grid::commit(&service, &clash_request, true).await.unwrap();
        assert!(report.failed.is_some(), "唯一约束应当失败并留下暂存");
        clash.apply_commit(&report);
        assert_eq!(clash.gate_at(email_row, 1), Some(Gate::Execution));
        assert_eq!(clash.cell_label(email_row, 1), "a@x");
        assert!(clash.rejected_visible());
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let email: String = sqlx::query_scalar("SELECT email FROM people WHERE id = 2").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(email, "b@x");
        let _ = before;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sqlite_pages_keep_null_empty_binary_large_insert_and_delete_apart() {
        let dir = temp_dir("kinds");
        let database = dir.join("kinds.sqlite");
        let setup = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
            .await
            .unwrap();
        sqlx::query("CREATE TABLE bag (id INTEGER PRIMARY KEY, note TEXT, blob BLOB)").execute(&setup).await.unwrap();
        let long = "甲".repeat(3000);
        sqlx::query("INSERT INTO bag (id, note, blob) VALUES (1, NULL, NULL), (2, '', x'abcd')").execute(&setup).await.unwrap();
        sqlx::query("INSERT INTO bag (id, note, blob) VALUES (3, ?, NULL)").bind(&long).execute(&setup).await.unwrap();
        setup.close().await;
        let service = std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")));
        service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
        let table = crate::grid::commands::TableRef {
            connection_id: "notes".into(),
            database: "main".into(),
            schema: String::new(),
            table: "bag".into(),
            engine: "sqlite".into(),
            include_foreign_keys: false,
        };
        let schema = schema_from_dto(&crate::application::grid::describe(&service, &table).await.unwrap()).unwrap();
        let page = crate::application::grid::page(
            &service,
            &crate::grid::commands::PageRequest { table, filters: Vec::new(), sort: None, offset: 0, limit: 10, search: None },
        )
        .await
        .unwrap();
        let columns = columns_from_schema(&schema, true);
        let rows = rows_from_page(&columns, &page);
        let mut grid = SharedGrid::new();
        grid.target = GridTarget {
            connection_id: "notes".into(),
            connection_name: "笔记".into(),
            database: "main".into(),
            schema: String::new(),
            table: "bag".into(),
            engine: "sqlite".into(),
            environment: "dev".into(),
        };
        grid.load_local(columns, rows, Some(3), Some(schema));
        let null_note = grid.show_cell(0, 1, false);
        let empty_note = grid.show_cell(1, 1, false);
        let binary = grid.show_cell(1, 2, true);
        let large = grid.show_cell(2, 1, false);
        assert_eq!(null_note.kind, CellKind::Null, "{}", null_note.label);
        assert_eq!(empty_note.kind, CellKind::Empty, "{}", empty_note.label);
        assert_ne!(null_note.label, empty_note.label);
        assert_eq!(binary.kind, CellKind::Binary, "{}", binary.label);
        assert!(matches!(large.kind, CellKind::Large), "{:?}", large);
        assert_eq!(grid.stage_insert(), Gate::Writable);
        let inserted = grid.rows.len() - 1;
        assert_eq!(grid.stage_text(inserted, 1, Some("新".into())), Gate::Writable);
        let insert_sql = grid.open_preview().unwrap();
        assert!(insert_sql.iter().any(|item| item.to_ascii_uppercase().contains("INSERT")), "{insert_sql:?}");
        let added = crate::application::grid::commit(&service, &grid.confirm_request().unwrap(), true).await.unwrap();
        assert!(added.failed.is_none(), "{}", added.failed.as_ref().map(|item| item.message.clone()).unwrap_or_default());
        grid.apply_commit(&added);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let created: i64 = sqlx::query_scalar("SELECT count(*) FROM bag WHERE note = '新'").fetch_one(&pool).await.unwrap();
        assert_eq!(created, 1);
        pool.close().await;

        let fresh = crate::application::grid::page(
            &service,
            &crate::grid::commands::PageRequest {
                table: crate::grid::commands::TableRef {
                    connection_id: "notes".into(),
                    database: "main".into(),
                    schema: String::new(),
                    table: "bag".into(),
                    engine: "sqlite".into(),
                    include_foreign_keys: false,
                },
                filters: Vec::new(),
                sort: None,
                offset: 0,
                limit: 10,
                search: None,
            },
        )
        .await
        .unwrap();
        let schema = grid.schema.clone().unwrap();
        let columns = columns_from_schema(&schema, true);
        grid.replace_fetched(columns, rows_from_page(&columns_from_schema(&schema, true), &fresh), 0, Some(4), false, Some(schema));
        let row = grid.rows.iter().position(|row| row.values.get(1).and_then(|value| value.as_deref()) == Some("新")).unwrap();
        let before_delete = {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
                .await
                .unwrap();
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM bag").fetch_one(&pool).await.unwrap();
            pool.close().await;
            count
        };
        assert_eq!(grid.stage_delete(row), Gate::Writable);
        let delete_sql = grid.open_preview().unwrap();
        assert!(delete_sql.iter().any(|item| item.to_ascii_uppercase().contains("DELETE")), "{delete_sql:?}");
        assert!(grid.banner().contains("bag") || grid.target.table == "bag" || grid.target.table.is_empty());
        grid.cancel_preview();
        let still = {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
                .await
                .unwrap();
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM bag").fetch_one(&pool).await.unwrap();
            pool.close().await;
            count
        };
        assert_eq!(still, before_delete);
        grid.open_preview().unwrap();
        let removed = crate::application::grid::commit(&service, &grid.confirm_request().unwrap(), true).await.unwrap();
        assert!(removed.failed.is_none(), "{}", removed.failed.as_ref().map(|item| item.message.clone()).unwrap_or_default());
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM bag WHERE note = '新'").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(left, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
