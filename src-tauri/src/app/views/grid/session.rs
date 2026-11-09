//! 共享结果网格的无界面状态。
//! 查找、排序、筛选和提交语句复用 grid 领域；这里只区分已读取窗口、选择和暂存。

use std::cmp::Ordering;

use crate::grid::commands::{ChangeArg, ColumnDto, CommitDto, PageDto, PageRequest, SchemaDto, SortArg, TableRef, ValueArg};
use crate::grid::{delete_sql, insert_sql, locate_sheet, sql_cell, update_sql, value_family, Cell, Column, Engine, TableSchema, ValueFamily};
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
    /// 当前显示行号，只用于把暂存值画回格子；重排后由 reattach_overlays 更新。
    row: usize,
    /// 可视行号会随排序和筛选重排，挂回和预览都用这个稳定 id。
    row_id: String,
    /// 当前显示列号，同样只用于绘制。
    column: usize,
    /// 列顺序变化时按名字重新定位暂存目标。
    column_name: String,
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

/// 查询页交给网格的结果集：截断与完整行数随结果一起给，网格不能自己猜。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuerySheet {
    pub sql: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    /// 语句的读取上限命中了，结果还有没读到的行；网格不会重新执行语句来延续。
    pub truncated: bool,
    /// 完整行数：结果未被截断时就是本次返回的行数，被截断时未知。
    pub full_count: Option<i64>,
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

/// 查找只覆盖已经读取的行；服务器范围才把关键字下发到数据库。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindScope {
    Loaded,
    Server,
}

/// 空网格的原因：表本来没有行、结果集本来没有行，还是当前条件没有命中。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetEmptiness {
    Rows,
    TableEmpty,
    /// 查询结果的空结果集：语句成功但没有返回行，与"表为空"不是一回事。
    ResultEmpty,
    NoMatch,
}

/// 还能不能继续读。加载中、还有更多、已完整、以及网格不能自己延续的结果要分开。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Continuation {
    Loading,
    More,
    Complete,
    /// 查询结果不能由网格重新执行来延续；写语句和多语句更不能。
    Unavailable,
}

/// 后端已经支持、并且可以出现在筛选条里的条件。
pub const FILTER_OPS: [&str; 11] = ["eq", "ne", "contains", "starts", "gt", "lt", "between", "in", "null", "notnull", "empty"];

/// 一条筛选条件草稿。文本保留用户的原文，只有"应用"时才编译成请求参数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterDraft {
    pub column: String,
    pub op: String,
    pub value: String,
    /// between 的上界。
    pub second: String,
    /// in 的集合输入，逗号或换行分隔。
    pub values: String,
    /// 还在编辑、尚未应用的草稿不进请求，也不参与本地筛选。
    pub dirty: bool,
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
    find_scope: FindScope,
    filters: Vec<FilterDraft>,
    /// 最近一次应用筛选时的非法输入原因；不解成请求，也不改已有结果。
    filter_error: Option<String>,
    /// 最近一次替换/追加读取失败的原因；旧行列仍保留，界面据此提示重试。
    read_failure: Option<String>,
    /// 换了目标表但还没读到新数据：这时读取失败才丢弃旧表快照。
    target_changed: bool,
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
    /// 选择的锚点与焦点用稳定行 id 记住，重排后按 id 找回显示行号。
    anchor_id: String,
    focus_id: String,
    selected: bool,
    /// 完整值查看同样记住行 id，滚动或重排后仍指向原来那一行。
    viewer: Option<(String, usize)>,
    overlays: Vec<Overlay>,
    /// 已经离开当前读取窗口、但仍被暂存引用的行；翻页后预览和提交仍用它们。
    retained: Vec<GridRow>,
    /// 没有对应暂存的失败（例如提交报告给出未知 id）单独记账，不伪造格子。
    failures: Vec<(String, String)>,
    preview_sql: Vec<String>,
    preview_ids: Vec<String>,
    preview_open: bool,
    /// 大字段预览的截断上限（字节）；来自设置 preview_kb。
    preview_bytes: usize,
    /// TSV 里数据库 NULL 的显示文本；SQL 字面量始终是 NULL。
    null_copy: String,
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
            find_scope: FindScope::Loaded,
            filters: Vec::new(),
            filter_error: None,
            read_failure: None,
            target_changed: false,
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
            anchor_id: String::new(),
            focus_id: String::new(),
            selected: false,
            viewer: None,
            overlays: Vec::new(),
            retained: Vec::new(),
            failures: Vec::new(),
            preview_sql: Vec::new(),
            preview_ids: Vec::new(),
            preview_open: false,
            preview_bytes: 1024,
            null_copy: "NULL".to_owned(),
            readonly_reason: String::new(),
            seq: 0,
            token: 0,
            sql: String::new(),
        }
    }

    pub fn load_local(&mut self, columns: Vec<GridColumn>, rows: Vec<GridRow>, full_count: Option<i64>, schema: Option<TableSchema>) {
        self.mode = GridMode::Table;
        self.columns = merge_widths(&self.columns, columns);
        self.source = rows;
        self.schema = schema;
        self.full_count = full_count;
        self.truncated = full_count.is_some_and(|count| count > self.source.len() as i64);
        self.scope = RangeScope::Fetched;
        self.overlays.clear();
        self.retained.clear();
        self.failures.clear();
        self.selected = false;
        self.viewer = None;
        self.read_failure = None;
        self.target_changed = false;
        self.close_preview();
        self.refresh_readonly_reason();
        self.recompute();
    }

    /// 同一张表翻页或刷新时替换已读取窗口：暂存仍按行 id 挂回，列宽按列名保留。
    /// 选择和完整值查看也按行 id 重新挂回；只有那一行真的不在新窗口里才消失。
    pub fn replace_fetched(&mut self, columns: Vec<GridColumn>, rows: Vec<GridRow>, offset: u64, full_count: Option<i64>, truncated: bool, schema: Option<TableSchema>) {
        self.retain_staged_rows();
        self.columns = merge_widths(&self.columns, columns);
        self.source = rows;
        self.schema = schema;
        self.offset = offset;
        self.full_count = full_count;
        self.truncated = truncated;
        self.scope = RangeScope::Fetched;
        self.read_failure = None;
        self.target_changed = false;
        self.close_preview();
        self.refresh_readonly_reason();
        self.recompute();
    }

    /// 查询结果进入网格：截断与完整行数由查询页给出，网格据此区分"已读到末尾"
    /// 和"这个结果不能由网格继续"，不再把两者都写成完成。
    pub fn show_query_sheet(&mut self, sql: &str, columns: Vec<String>, rows: Vec<Vec<Option<String>>>, truncated: bool, full_count: Option<i64>) -> u64 {
        self.mode = GridMode::Query;
        self.sql = sql.to_owned();
        self.token = self.token.saturating_add(1);
        self.schema = None;
        // 单表 SELECT 只在结构证明成功后短暂只读；联接、聚合或多语句从一开始就说明原因。
        self.readonly_reason = match prove_single_table(&self.target.engine, sql) {
            Ok(_) => "grid.status.readonly".to_owned(),
            Err(reason) => reason.to_owned(),
        };
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
        self.full_count = full_count;
        self.truncated = truncated;
        self.offset = 0;
        self.scope = RangeScope::Fetched;
        self.overlays.clear();
        self.retained.clear();
        self.failures.clear();
        self.selected = false;
        self.viewer = None;
        self.read_failure = None;
        self.target_changed = false;
        self.find_scope = FindScope::Loaded;
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

    /// 只读时给出的具体原因；可写时为空。界面用它解释禁写，而不是只画一个“只读”。
    pub fn readonly_reason(&self) -> &str {
        &self.readonly_reason
    }

    pub fn uncommitted(&self) -> bool {
        !self.overlays.is_empty()
    }

    pub fn failures(&self) -> &[(String, String)] {
        &self.failures
    }

    /// 换到另一张表时清掉旧表的筛选、排序、选择和暂存；同一张表翻页不算换表。
    /// 返回值表示目标是否真的换了：换了以后读取失败才丢弃旧表快照。
    pub fn switch_target(&mut self, target: GridTarget) -> bool {
        let same = self.mode == GridMode::Table
            && self.target.connection_id == target.connection_id
            && self.target.database == target.database
            && self.target.schema == target.schema
            && self.target.table == target.table;
        if !same {
            self.overlays.clear();
            self.retained.clear();
            self.failures.clear();
            self.search.clear();
            self.find_scope = FindScope::Loaded;
            self.filters.clear();
            self.filter_error = None;
            self.read_failure = None;
            self.sort = None;
            self.selected = false;
            self.viewer = None;
            self.anchor_id.clear();
            self.focus_id.clear();
            self.close_preview();
        }
        self.target_changed = !same;
        self.target = target;
        !same
    }

    /// 当前快照是否属于另一个还没读到的目标；读取失败时用它决定丢弃还是保留。
    pub fn stale_target(&self) -> bool {
        self.target_changed
    }

    pub fn read_failure(&self) -> Option<&str> {
        self.read_failure.as_deref()
    }

    pub fn clear_read_failure(&mut self) {
        self.read_failure = None;
    }

    /// 同目标的刷新、追加、排序、筛选或翻页失败：只记原因，行列、条件、选择、滚动和暂存都保留。
    pub fn keep_after_failure(&mut self, message: &str) {
        self.read_failure = Some(message.to_owned());
    }

    /// 表模式下只读原因由结构决定：视图、无唯一键或不可写都由这里统一给出。
    fn refresh_readonly_reason(&mut self) {
        if self.mode == GridMode::Query {
            return;
        }
        self.readonly_reason = match &self.schema {
            Some(schema) if schema.kind != "table" => "grid.errors.viewReadonly".to_owned(),
            Some(schema) if schema.key.is_empty() => "grid.status.unlocated".to_owned(),
            _ => String::new(),
        };
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

    pub fn find_text(&self) -> &str {
        &self.search
    }

    pub fn find_scope(&self) -> FindScope {
        self.find_scope
    }

    /// 查找只筛当前已读取的行，不发查询。
    pub fn set_search(&mut self, text: &str) {
        self.search = text.to_owned();
        self.recompute();
    }

    /// 服务器范围筛选要求这个网格能重新读取某张表才能成立。
    /// 查询结果已由语句返回、没有目标表，都不该给出一个点了却不发请求的开关。
    pub fn find_scope_block(&self) -> Option<&'static str> {
        if self.mode == GridMode::Query {
            return Some("grid.find.scopeQueryOnly");
        }
        if self.target.connection_id.is_empty() || self.target.table.is_empty() {
            return Some("grid.find.scopeNoTable");
        }
        None
    }

    /// 切到服务器范围表示"在整表里筛选"，调用方需要重新读取；切回来同样重读。
    /// 不可用时返回原因，范围保持原样，关键字仍只在已读取行里生效。
    pub fn set_find_scope(&mut self, scope: FindScope) -> Result<(), &'static str> {
        if self.find_scope == scope {
            return Ok(());
        }
        if scope == FindScope::Server {
            if let Some(reason) = self.find_scope_block() {
                return Err(reason);
            }
        }
        self.find_scope = scope;
        self.recompute();
        Ok(())
    }

    /// 只有服务器范围的非空关键字才作为整表条件下发。
    pub fn server_search(&self) -> Option<String> {
        (self.find_scope == FindScope::Server && !self.search.trim().is_empty()).then(|| self.search.clone())
    }

    pub fn filters(&self) -> &[FilterDraft] {
        &self.filters
    }

    pub fn filter_error(&self) -> Option<&str> {
        self.filter_error.as_deref()
    }

    /// 这一列适用的条件：数字/日期给范围，文本给包含，二进制只留相等与 NULL。
    pub fn filter_ops_for(&self, column: &str) -> Vec<&'static str> {
        let family = self.columns.iter().find(|item| item.name == column).map(|item| value_family(&item.type_name)).unwrap_or(ValueFamily::Unknown);
        match family {
            ValueFamily::Integer | ValueFamily::Decimal | ValueFamily::Float | ValueFamily::Date | ValueFamily::Time | ValueFamily::Datetime => {
                vec!["eq", "ne", "gt", "lt", "between", "in", "null", "notnull"]
            }
            ValueFamily::Text => vec!["eq", "ne", "contains", "starts", "in", "null", "notnull", "empty"],
            _ => vec!["eq", "ne", "null", "notnull"],
        }
    }

    pub fn op_label(&self, op: &str) -> String {
        i18n::text(self.locale, &format!("grid.op.{op}"))
    }

    fn draft_index(&self, column: &str) -> Option<usize> {
        self.filters.iter().position(|item| item.column == column)
    }

    fn filter_values(draft: &FilterDraft) -> Vec<String> {
        draft.values.split([',', '\n']).map(str::trim).filter(|item| !item.is_empty()).map(str::to_owned).collect()
    }

    /// 按列替换一条条件并立即应用；保持既有"设了就生效"的用法。
    pub fn set_filter(&mut self, column: &str, op: &str, value: &str) {
        self.set_filter_full(column, op, value, "", "");
    }

    pub fn set_filter_full(&mut self, column: &str, op: &str, value: &str, second: &str, values: &str) {
        let draft = FilterDraft {
            column: column.to_owned(),
            op: op.to_owned(),
            value: value.to_owned(),
            second: second.to_owned(),
            values: values.to_owned(),
            dirty: false,
        };
        match self.draft_index(column) {
            Some(index) => self.filters[index] = draft,
            None => self.filters.push(draft),
        }
        self.filter_error = None;
        self.recompute();
    }

    /// 为一列新增一条待编辑条件；这一列已有条件就改成编辑已有的那条。
    pub fn add_filter(&mut self, column: &str) -> usize {
        if let Some(index) = self.draft_index(column) {
            self.filters[index].dirty = true;
            return index;
        }
        let op = self.filter_ops_for(column).first().copied().unwrap_or("eq").to_owned();
        self.filters.push(FilterDraft { column: column.to_owned(), op, value: String::new(), second: String::new(), values: String::new(), dirty: true });
        self.filters.len() - 1
    }

    pub fn set_filter_column(&mut self, index: usize, column: &str) -> bool {
        let ops = self.filter_ops_for(column);
        let Some(draft) = self.filters.get_mut(index) else {
            return false;
        };
        draft.column = column.to_owned();
        if !ops.iter().any(|op| *op == draft.op) {
            draft.op = ops.first().copied().unwrap_or("eq").to_owned();
            draft.second.clear();
            draft.values.clear();
        }
        draft.dirty = true;
        true
    }

    pub fn set_filter_op(&mut self, index: usize, op: &str) -> bool {
        let Some(column) = self.filters.get(index).map(|item| item.column.clone()) else {
            return false;
        };
        if !self.filter_ops_for(&column).contains(&op) {
            return false;
        }
        let Some(draft) = self.filters.get_mut(index) else {
            return false;
        };
        draft.op = op.to_owned();
        if op != "between" {
            draft.second.clear();
        }
        if op != "in" {
            draft.values.clear();
        }
        draft.dirty = true;
        true
    }

    /// 循环到这一列的下一个适用条件，返回新条件。
    pub fn cycle_filter_op(&mut self, index: usize) -> Option<&'static str> {
        let column = self.filters.get(index)?.column.clone();
        let ops = self.filter_ops_for(&column);
        if ops.is_empty() {
            return None;
        }
        let current = self.filters.get(index)?.op.clone();
        let position = ops.iter().position(|op| *op == current).unwrap_or(usize::MAX);
        let next = ops[position.wrapping_add(1) % ops.len()];
        self.set_filter_op(index, next).then_some(next)
    }

    pub fn update_filter(&mut self, index: usize, op: &str, value: &str, second: &str, values: &str) -> bool {
        let Some(draft) = self.filters.get_mut(index) else {
            return false;
        };
        draft.op = op.to_owned();
        draft.value = value.to_owned();
        draft.second = second.to_owned();
        draft.values = values.to_owned();
        draft.dirty = true;
        true
    }

    /// 编辑输入框的逐字更新：只改草稿文本，不应用、不发查询。
    pub fn update_filter_value(&mut self, index: usize, slot: usize, value: &str) -> bool {
        let Some(draft) = self.filters.get_mut(index) else {
            return false;
        };
        match slot {
            0 => draft.value = value.to_owned(),
            1 => draft.second = value.to_owned(),
            _ => draft.values = value.to_owned(),
        }
        draft.dirty = true;
        true
    }

    /// 界面上的取值输入：按当前条件决定这是值、上界还是集合。
    pub fn update_filter_text(&mut self, index: usize, first: bool, text: &str) -> bool {
        let Some(draft) = self.filters.get_mut(index) else {
            return false;
        };
        if first {
            if draft.op == "in" {
                draft.values = text.to_owned();
            } else {
                draft.value = text.to_owned();
            }
        } else {
            draft.second = text.to_owned();
        }
        draft.dirty = true;
        true
    }

    pub fn remove_filter(&mut self, index: usize) -> bool {
        if index >= self.filters.len() {
            return false;
        }
        self.filters.remove(index);
        self.filter_error = None;
        self.recompute();
        true
    }

    /// 全清：条件和查找一起清掉。
    pub fn clear_filters(&mut self) {
        self.filters.clear();
        self.filter_error = None;
        self.search.clear();
        self.recompute();
    }

    fn compile_draft(&self, draft: &FilterDraft) -> Result<crate::grid::commands::FilterArg, String> {
        let op = draft.op.as_str();
        if !FILTER_OPS.contains(&op) {
            return Err(i18n::text(self.locale, "grid.filter.unknownOp"));
        }
        let mut value = None;
        let mut second = None;
        let mut values = Vec::new();
        match op {
            "null" | "notnull" | "empty" => {}
            "between" => {
                if draft.value.trim().is_empty() || draft.second.trim().is_empty() {
                    return Err(i18n::text(self.locale, "grid.filter.needRange"));
                }
                value = Some(draft.value.clone());
                second = Some(draft.second.clone());
            }
            "in" => {
                values = Self::filter_values(draft);
                if values.is_empty() {
                    return Err(i18n::text(self.locale, "grid.filter.needList"));
                }
            }
            _ => {
                if draft.value.trim().is_empty() {
                    return Err(i18n::text(self.locale, "grid.filter.needValue"));
                }
                value = Some(draft.value.clone());
            }
        }
        Ok(crate::grid::commands::FilterArg { column: draft.column.clone(), op: op.to_owned(), value, second, values })
    }

    /// 已应用条件的请求参数；还在编辑的草稿不进请求。
    pub fn filter_args(&self) -> Result<Vec<crate::grid::commands::FilterArg>, String> {
        self.filters.iter().filter(|draft| !draft.dirty).map(|draft| self.compile_draft(draft)).collect()
    }

    /// 校验全部条件（含正在编辑的），非法时给出原因。
    pub fn validate_filters(&self) -> Result<(), String> {
        for draft in &self.filters {
            self.compile_draft(draft)?;
        }
        Ok(())
    }

    /// 应用全部条件。非法输入只记录原因：不发查询，已有结果和条件输入都保留。
    pub fn apply_filters(&mut self) -> Result<(), String> {
        if let Err(error) = self.validate_filters() {
            self.filter_error = Some(error.clone());
            return Err(error);
        }
        for draft in &mut self.filters {
            draft.dirty = false;
        }
        self.filter_error = None;
        self.recompute();
        Ok(())
    }

    pub fn sort_state(&self) -> Option<(&str, bool)> {
        self.sort.as_ref().map(|(column, descending)| (column.as_str(), *descending))
    }

    /// 还有正在编辑、尚未应用的筛选条件：界面据此标出"未应用"，避免把旧结果当成新条件的结果。
    pub fn unapplied_filters(&self) -> bool {
        self.filters.iter().any(|draft| draft.dirty)
    }

    /// 排序列在数据列里的下标；表头用它标出方向，和焦点无关。
    pub fn sort_index(&self) -> Option<(usize, bool)> {
        let (column, descending) = self.sort.as_ref()?;
        self.columns.iter().position(|item| item.name == *column).map(|index| (index, *descending))
    }

    /// 当前排序列与方向的文字；没有排序时为空。它不依赖焦点，状态栏因此始终可读。
    pub fn sort_text(&self) -> Option<String> {
        let (column, descending) = self.sort.as_ref()?;
        let arrow = if *descending { "↓" } else { "↑" };
        Some(i18n::fill(&i18n::text(self.locale, "grid.state.sorted"), &[("column", column), ("arrow", arrow)]))
    }

    /// 这一列当前的方向；没有排序时为 None。
    pub fn sort_for(&self, column: &str) -> Option<bool> {
        self.sort.as_ref().filter(|(name, _)| name == column).map(|(_, descending)| *descending)
    }

    /// 任意列升序 → 降序 → 取消排序，返回新的方向。
    pub fn toggle_sort(&mut self, column: &str) -> Option<bool> {
        if !self.columns.iter().any(|item| item.name == column) {
            return None;
        }
        let next = match self.sort_for(column) {
            None => Some(false),
            Some(false) => Some(true),
            Some(true) => None,
        };
        self.sort = next.map(|descending| (column.to_owned(), descending));
        self.recompute();
        next
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

    /// 空网格到底是表为空、结果集为空还是条件没命中。
    pub fn emptiness(&self) -> SheetEmptiness {
        if !self.rows.is_empty() {
            return SheetEmptiness::Rows;
        }
        let narrowing = !self.search.trim().is_empty() || self.filters.iter().any(|draft| !draft.dirty);
        if narrowing {
            return SheetEmptiness::NoMatch;
        }
        if self.mode == GridMode::Query {
            SheetEmptiness::ResultEmpty
        } else {
            SheetEmptiness::TableEmpty
        }
    }

    /// 还能不能继续读；查询结果不允许网格用重新执行来延续。
    pub fn continuation(&self, loading: bool) -> Continuation {
        if loading {
            return Continuation::Loading;
        }
        if self.mode == GridMode::Query {
            return if self.truncated { Continuation::Unavailable } else { Continuation::Complete };
        }
        if self.has_more_rows() {
            Continuation::More
        } else {
            Continuation::Complete
        }
    }

    /// 状态栏一句话：读取失败、无匹配、表/结果集为空、加载中、还有更多、已到末尾各自可辨认。
    /// 加载优先于"空"：首屏、换表和替换读取还没回来时不能说表里没有行。
    pub fn state_text(&self, loading: bool) -> String {
        if let Some(failure) = &self.read_failure {
            return failure.clone();
        }
        if loading && self.rows.is_empty() {
            return i18n::text(self.locale, "grid.state.loading");
        }
        match self.emptiness() {
            SheetEmptiness::NoMatch => return i18n::text(self.locale, "grid.state.noMatch"),
            SheetEmptiness::TableEmpty => return i18n::text(self.locale, "grid.state.tableEmpty"),
            SheetEmptiness::ResultEmpty => return i18n::text(self.locale, "grid.state.resultEmpty"),
            SheetEmptiness::Rows => {}
        }
        match self.continuation(loading) {
            Continuation::Loading => i18n::text(self.locale, "grid.state.loading"),
            Continuation::More => i18n::text(self.locale, "grid.state.more"),
            Continuation::Complete => i18n::text(self.locale, "grid.state.complete"),
            Continuation::Unavailable => i18n::text(self.locale, "grid.state.noContinuation"),
        }
    }

    /// 读取请求。条件非法时返回原因：调用方不能发出这条查询，也不能清掉旧结果。
    pub fn page_request(&self) -> Result<PageRequest, String> {
        let filters = self.filter_args()?;
        Ok(PageRequest {
            table: self.table_ref(),
            filters,
            sort: self.sort.as_ref().map(|(column, descending)| SortArg {
                column: column.clone(),
                direction: if *descending { "desc".to_owned() } else { "asc".to_owned() },
            }),
            offset: self.offset,
            limit: self.limit,
            preview_bytes: self.preview_bytes as u64,
            search: self.server_search(),
        })
    }

    /// 条件变化后回到第一页，但保留按行 id 记住的选择与完整值查看。
    pub fn set_page_keep_identity(&mut self, offset: u64, limit: u64) {
        self.offset = offset;
        self.limit = limit.max(1);
    }

    pub fn set_page(&mut self, offset: u64, limit: u64) {
        self.offset = offset;
        self.limit = limit.max(1);
        self.clear_selection();
        self.viewer = None;
        self.scroll = 0;
    }

    /// 还能不能翻页。查询结果和没有目标表的对象都不能；读取失败后偏移仍停在上次成功的页上，
    /// 所以重试还是同一页，不会跳过失败页，也不会因为旧页没有更多就报告"已读完"。
    pub fn can_page(&self, forward: bool) -> bool {
        if self.mode != GridMode::Table || self.target.connection_id.is_empty() || self.target.table.is_empty() {
            return false;
        }
        if forward {
            self.has_more_rows()
        } else {
            self.offset > 0
        }
    }

    /// 新的一页真的读回来了才换窗口：旧页的选择和完整值查看不属于这一页。
    pub fn reset_page_view(&mut self) {
        self.clear_selection();
        self.viewer = None;
        self.scroll = 0;
    }

    pub fn page_offset(&self) -> u64 {
        self.offset
    }

    pub fn page_limit(&self) -> u64 {
        self.limit
    }

    /// 大字段预览的截断上限（字节）；默认 1024，由设置 preview_kb 下发。
    pub fn preview_bytes(&self) -> usize {
        self.preview_bytes
    }

    pub fn set_preview_bytes(&mut self, bytes: usize) {
        self.preview_bytes = bytes.max(1);
    }

    /// TSV 里数据库 NULL 的显示文本。
    pub fn null_copy(&self) -> &str {
        &self.null_copy
    }

    pub fn set_null_copy(&mut self, token: &str) {
        self.null_copy = token.to_owned();
    }

    pub fn clear_selection(&mut self) {
        self.selected = false;
        self.anchor_id.clear();
        self.focus_id.clear();
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
            self.clear_selection();
            return;
        }
        let row = row.min(self.rows.len() - 1);
        let column = column.min(self.columns.len() - 1);
        if !extend || !self.selected {
            self.anchor = (row, column);
            self.anchor_id = self.rows[row].id.clone();
        }
        self.focus = (row, column);
        self.focus_id = self.rows[row].id.clone();
        self.selected = true;
    }

    pub fn select_all_loaded(&mut self) {
        if self.rows.is_empty() || self.columns.is_empty() {
            self.clear_selection();
            return;
        }
        self.anchor = (0, 0);
        self.anchor_id = self.rows[0].id.clone();
        self.focus = (self.rows.len() - 1, self.columns.len() - 1);
        self.focus_id = self.rows[self.rows.len() - 1].id.clone();
        self.selected = true;
    }

    /// 当前选择焦点对应的稳定行 id。排序、筛选和翻页后它就是“同一行”的判据。
    pub fn focused_row_id(&self) -> Option<&str> {
        if self.selected && !self.focus_id.is_empty() {
            Some(self.focus_id.as_str())
        } else {
            None
        }
    }

    pub fn row_id(&self, row: usize) -> Option<&str> {
        self.rows.get(row).map(|item| item.id.as_str())
    }

    fn row_index(&self, row_id: &str) -> Option<usize> {
        self.rows.iter().position(|row| row.id == row_id)
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

    /// TSV 复制：数据库 NULL 写成不带引号的 NULL，字符串 "NULL" 写成带引号的 "NULL"，
    /// 空字符串保持空字段；制表符、换行和引号按 TSV 规则转义。
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
            lines.last_mut().expect("line").push(self.tsv_field(row, column));
        }
        lines.into_iter().map(|line| line.join("\t")).collect::<Vec<_>>().join("\n")
    }

    /// SQL 值复制：按目标引擎写成字面量，NULL 与 'NULL' 在文本上也可区分。
    pub fn sql_values(&self) -> String {
        let points = self.selection();
        if points.is_empty() {
            return String::new();
        }
        let engine = Engine::parse(&self.target.engine).unwrap_or(Engine::Sqlite);
        let mut lines: Vec<Vec<String>> = Vec::new();
        let mut current = usize::MAX;
        for (row, column) in points {
            if row != current {
                lines.push(Vec::new());
                current = row;
            }
            let cell = match self.value_at(row, column) {
                Some(Some(text)) => Cell::Text(text),
                _ => Cell::Null,
            };
            let origin = self.columns.get(column).map(domain_column);
            lines.last_mut().expect("line").push(sql_cell(engine, origin.as_ref(), &cell));
        }
        lines.into_iter().map(|line| line.join(", ")).collect::<Vec<_>>().join("\n")
    }

    pub fn open_value(&mut self, row: usize, column: usize) {
        if row < self.rows.len() && column < self.columns.len() {
            self.viewer = Some((self.rows[row].id.clone(), column));
        }
    }

    pub fn close_value(&mut self) {
        self.viewer = None;
    }

    pub fn viewer(&self) -> Option<ShownCell> {
        let (row, column) = self.viewer_cell()?;
        Some(self.show_cell(row, column, true))
    }

    /// 完整值查看当前落在哪一格；行已不在当前快照时返回 None，而不是指向别的行。
    pub fn viewer_cell(&self) -> Option<(usize, usize)> {
        let (row_id, column) = self.viewer.as_ref()?;
        let row = self.row_index(row_id)?;
        (*column < self.columns.len()).then_some((row, *column))
    }

    pub fn viewer_reason(&self) -> String {
        match self.viewer_cell() {
            Some((row, column)) => self.write_reason(row, column),
            None => String::new(),
        }
    }

    pub fn show_cell(&self, row: usize, column: usize, opened: bool) -> ShownCell {
        let type_name = self.columns.get(column).map(|item| item.type_name.as_str()).unwrap_or("");
        classify_with_limit(self.locale, self.value_at(row, column), type_name, opened, self.preview_bytes)
    }

    pub fn cell_label(&self, row: usize, column: usize) -> String {
        if let Some(overlay) = self.overlay_at(row, column).filter(|item| item.kind == "update" && item.gate != Gate::Writable) {
            return match &overlay.value {
                Some(text) if text.is_empty() => i18n::text(self.locale, "grid.value.emptyLabel"),
                Some(text) => text.clone(),
                None => "NULL".to_owned(),
            };
        }
        self.show_cell(row, column, false).label
    }

    /// 当前格子的原始值，供编辑栏判断“NULL 未改动”和抽取文本。
    pub fn raw_value(&self, row: usize, column: usize) -> Option<Option<String>> {
        self.value_at(row, column)
    }

    /// 进入文本编辑时使用的原值：数据库 NULL 与空字符串保持不同。
    pub fn edit_text(&self, row: usize, column: usize) -> String {
        match self.value_at(row, column) {
            Some(Some(text)) => text,
            _ => String::new(),
        }
    }

    /// 当前格子是否真的可写；不可写时给出具体原因。只看结构，不看已存值。
    pub fn write_reason(&self, row: usize, column: usize) -> String {
        self.gate_text(self.structural_gate(Some(row), column))
    }

    /// 把 Gate 翻成界面文案；可写时为空。
    pub fn gate_text(&self, gate: Gate) -> String {
        let key = match gate {
            Gate::Writable => return String::new(),
            Gate::Readonly => {
                if self.readonly_reason.is_empty() {
                    "grid.status.readonly"
                } else {
                    self.readonly_reason.as_str()
                }
            }
            Gate::Unlocated => "grid.status.unlocated",
            Gate::Validation => "grid.status.validation",
            Gate::Execution => "grid.status.execution",
        };
        i18n::text(self.locale, key)
    }

    pub fn row_unlocated(&self, row: usize) -> bool {
        self.rows.get(row).is_some_and(|item| !item.inserted && !item.located)
    }

    pub fn gate_at(&self, row: usize, column: usize) -> Option<Gate> {
        self.overlay_at(row, column).map(|item| item.gate)
    }

    /// 按当前显示位置暂存一次编辑。位置先解析成稳定行 id，再交给 stage_text_by_id。
    pub fn stage_text(&mut self, row: usize, column: usize, value: Option<String>) -> Gate {
        let Some(row_id) = self.rows.get(row).map(|item| item.id.clone()) else {
            return Gate::Readonly;
        };
        self.stage_text_by_id(&row_id, column, value)
    }

    /// 按稳定行 id 暂存编辑：排序、筛选或翻页改变显示行号后仍落在同一行。
    pub fn stage_text_by_id(&mut self, row_id: &str, column: usize, value: Option<String>) -> Gate {
        let display = self.row_index(row_id);
        let Some(column_name) = self.columns.get(column).map(|item| item.name.clone()) else {
            return Gate::Readonly;
        };
        let Some(row) = self.any_row(row_id).cloned() else {
            return Gate::Readonly;
        };
        let draw_row = display.unwrap_or(usize::MAX);
        if row.inserted {
            let message = self.validation_message(column, value.as_deref());
            let gate = if message.is_some() { Gate::Validation } else { Gate::Writable };
            self.overlays.retain(|item| !(item.kind == "update" && item.row_id == row_id && item.column_name == column_name));
            if gate == Gate::Writable {
                if let Some(origin) = self.source.iter_mut().find(|item| item.id == row_id) {
                    if let Some(slot) = origin.values.get_mut(column) {
                        *slot = value.clone();
                    }
                }
                if let Some(slot) = display.and_then(|row| self.rows.get_mut(row)).and_then(|item| item.values.get_mut(column)) {
                    *slot = value;
                }
            } else {
                self.overlays.push(overlay(draw_row, row_id.to_owned(), column, column_name, value, Gate::Validation, message.unwrap_or_else(|| "grid.status.validation".to_owned()), None, "update"));
            }
            self.close_preview();
            return gate;
        }
        let gate = self.gate_for_edit(display, column, value.as_deref());
        let message = match gate {
            Gate::Readonly => self.readonly_reason.clone(),
            Gate::Unlocated => "grid.status.unlocated".to_owned(),
            Gate::Validation => self.validation_message(column, value.as_deref()).unwrap_or_else(|| "grid.status.validation".to_owned()),
            Gate::Writable => String::new(),
            Gate::Execution => "grid.status.execution".to_owned(),
        };
        // 只读或无法定位的格子不制造“看起来已修改”的暂存；界面用 gate_text 解释原因。
        if !matches!(gate, Gate::Writable | Gate::Validation) {
            return gate;
        }
        self.seq += 1;
        let change_id = if gate == Gate::Writable { Some(format!("u:{}", self.seq)) } else { None };
        self.overlays.retain(|item| !(item.kind == "update" && item.row_id == row_id && item.column_name == column_name));
        self.overlays.push(overlay(draw_row, row_id.to_owned(), column, column_name, value, gate, message, change_id, "update"));
        self.close_preview();
        gate
    }

    /// 撤销最近一次暂存的修改（更新、插入或删除）。没有可撤销的内容时返回 false。
    pub fn undo_last_stage(&mut self) -> bool {
        let Some(last) = self.overlays.pop() else {
            return false;
        };
        if last.kind == "insert" {
            if let Some(id) = last.change_id.as_deref() {
                self.source.retain(|item| item.id != id);
                self.overlays.retain(|item| item.row_id != last.row_id);
                self.recompute();
            }
        }
        self.prune_retained();
        self.close_preview();
        true
    }

    pub fn stage_insert(&mut self) -> Gate {
        if self.mode == GridMode::Query || !self.writable() {
            if self.readonly_reason.is_empty() {
                self.readonly_reason = "grid.status.readonly".to_owned();
            }
            return Gate::Readonly;
        }
        let id = format!("i:{}", self.seq + 1);
        self.seq += 1;
        let values = vec![None; self.columns.len()];
        self.source.push(GridRow { id: id.clone(), values, located: false, locator: Vec::new(), inserted: true });
        self.recompute();
        // 新增行在筛选/查找下可能不在显示快照里，身份必须始终用新行自己的 id，
        // 不能退化成最后一行，否则预览会把别人的行当成新增内容。
        let row = self.rows.iter().position(|item| item.id == id).unwrap_or(self.rows.len());
        self.overlays.push(overlay(row, id.clone(), 0, String::new(), None, Gate::Writable, String::new(), Some(id), "insert"));
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
        // 删除针对整行，只需要判断行是否可定位，与具体列无关。
        let gate = if self.schema.as_ref().is_some_and(|schema| schema.kind != "table") || (self.mode == GridMode::Query && !self.writable()) {
            Gate::Readonly
        } else if self.row_unlocated(row) || self.schema.as_ref().is_some_and(|schema| schema.key.is_empty()) {
            Gate::Unlocated
        } else if !self.writable() {
            Gate::Readonly
        } else {
            Gate::Writable
        };
        // 只读或无法定位的行不接受删除暂存，也不该把网格标成未提交。
        if gate != Gate::Writable {
            return gate;
        }
        self.seq += 1;
        let row_id = self.rows.get(row).map(|item| item.id.clone()).unwrap_or_default();
        self.overlays.retain(|item| !(item.kind == "delete" && item.row_id == row_id));
        self.overlays.push(overlay(row, row_id, 0, String::new(), None, Gate::Writable, String::new(), Some(format!("d:{}", self.seq)), "delete"));
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
        self.overlays.iter().any(|item| item.gate != Gate::Writable) || !self.failures.is_empty()
    }

    pub fn open_preview(&mut self) -> Result<Vec<String>, String> {
        let schema = self.schema.clone().ok_or_else(|| i18n::text(self.locale, "grid.status.readonly"))?;
        let staged = self.overlays.clone();
        let planned = staged
            .iter()
            .filter(|item| item.gate == Gate::Writable && item.change_id.is_some())
            .filter_map(|overlay| {
                let row = self.row_for(overlay)?;
                Some((
                    overlay.kind,
                    overlay.change_id.clone().unwrap_or_default(),
                    overlay.column_name.clone(),
                    overlay.value.clone(),
                    row.id.clone(),
                    row.locator.clone(),
                    row.values.clone(),
                ))
            })
            .collect::<Vec<_>>();
        let mut sql = Vec::new();
        let mut ids = Vec::new();
        for (kind, id, column_name, value, row_id, locator, values) in planned {
            let statement = match kind {
                "delete" => delete_sql(&schema, &pairs(&locator)),
                "insert" => {
                    let row = GridRow { id: row_id, values, located: false, locator: locator.clone(), inserted: true };
                    insert_sql(&schema, &self.insert_values(&row))
                }
                _ => update_sql(&schema, &[(column_name, cell_of(value))], &pairs(&locator)),
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

    /// 把提交结果如实落回显示：成功项写回已读取行，失败项保留在原来的格子上并写明原因。
    pub fn apply_commit(&mut self, report: &CommitDto) {
        for step in &report.applied {
            let Some(index) = self.overlays.iter().position(|item| item.change_id.as_deref() == Some(step.id.as_str())) else {
                continue;
            };
            let applied = self.overlays.remove(index);
            match applied.kind {
                "insert" => {
                    self.source.retain(|row| row.id != applied.row_id);
                    self.retained.retain(|row| row.id != applied.row_id);
                }
                "delete" => {
                    self.source.retain(|row| row.id != applied.row_id);
                    self.rows.retain(|row| row.id != applied.row_id);
                    self.retained.retain(|row| row.id != applied.row_id);
                }
                _ => self.set_row_value(&applied.row_id, &applied.column_name, applied.value.clone()),
            }
        }
        if let Some(failed) = &report.failed {
            if let Some(overlay) = self.overlays.iter_mut().find(|item| item.change_id.as_deref() == Some(failed.id.as_str())) {
                overlay.gate = Gate::Execution;
                overlay.message = failed.message.clone();
            } else {
                // 未知 id 没有对应格子，只记录失败本身，不在首格伪造一个值。
                self.failures.push((failed.id.clone(), failed.message.clone()));
            }
        }
        self.prune_retained();
        self.close_preview();
        self.recompute();
    }

    pub fn column_names(&self) -> Vec<String> {
        self.columns.iter().map(|column| column.name.clone()).collect()
    }

    /// 当前已读取的表、列和行。没有连接或表名时不给出导入目标。
    #[allow(clippy::type_complexity)]
    pub fn exchange_sheet(&self) -> (Option<TableRef>, Vec<String>, Vec<Vec<Option<String>>>, bool) {
        let table = if self.target.connection_id.is_empty() || self.target.table.is_empty() { None } else { Some(self.table_ref()) };
        // 暂存的新增行还没写进数据库，交换与导出都不能把它们当成已读取内容。
        let rows = self.source.iter().filter(|row| !row.inserted).map(|row| row.values.clone()).collect();
        (table, self.column_names(), rows, self.has_more_rows())
    }

    pub fn rendered_labels(&self) -> Vec<Vec<String>> {
        let (start, end) = self.visible_bounds();
        (start..end).map(|row| self.sheet_line(row)).collect()
    }

    /// DataTable 自己虚拟化。这里交出全部已加载行，行号仍按页偏移计算。
    pub fn sheet_labels(&self) -> Vec<Vec<String>> {
        (0..self.rows.len()).map(|row| self.sheet_line(row)).collect()
    }

    /// 换了目标表又读失败时丢弃上一张表的快照，避免旧行被当成新表的数据导出。
    /// 同一张表的刷新/追加失败不走这里，走 keep_after_failure。
    pub fn drop_stale_sheet(&mut self) {
        self.columns.clear();
        self.source.clear();
        self.rows.clear();
        self.schema = None;
        self.full_count = None;
        self.truncated = false;
        self.filters.clear();
        self.filter_error = None;
        self.search.clear();
        self.find_scope = FindScope::Loaded;
        self.sort = None;
        self.selected = false;
        self.viewer = None;
        self.scroll = 0;
        self.retained.clear();
        self.read_failure = None;
        self.target_changed = false;
        self.close_preview();
    }

    /// 已读取的服务器行数；未提交的新增行不计入分页偏移。
    pub fn source_len(&self) -> usize {
        self.source.iter().filter(|row| !row.inserted).count()
    }

    /// 测试用：只改声明的完整行数，模拟服务器上还有没读到的行。
    #[cfg(test)]
    pub fn set_total_rows_for_test(&mut self, total: i64) {
        self.full_count = Some(total);
    }

    pub fn has_more_rows(&self) -> bool {
        if let Some(count) = self.full_count {
            (self.offset as i64) + (self.source_len() as i64) < count
        } else {
            self.truncated
        }
    }

    /// 追加下一页。只对表数据成立；查询结果延续由查询页自己负责，网格不重新执行 SQL。
    /// 已经读到的稳定行 id 不再追加第二份，返回真正新增的行数。
    pub fn append_page(&mut self, rows: Vec<GridRow>, full_count: Option<i64>, truncated: bool) -> usize {
        if self.mode != GridMode::Table {
            return 0;
        }
        let mut added = 0;
        for row in rows {
            if self.source.iter().any(|item| item.id == row.id) {
                continue;
            }
            self.source.push(row);
            added += 1;
        }
        if full_count.is_some() {
            self.full_count = full_count;
        }
        self.truncated = truncated;
        self.read_failure = None;
        let scroll = self.scroll;
        // 选择按行 id 重新挂回，不按刚才的显示行号硬塞回去。
        self.recompute();
        self.scroll = scroll;
        self.clamp_scroll();
        added
    }

    pub fn copy_row(&mut self) -> String {
        let Some((row, _)) = self.focus_cell() else {
            return String::new();
        };
        let last = self.columns.len().saturating_sub(1);
        self.select(row, 0, false);
        self.select(row, last, true);
        self.tsv()
    }

    pub fn copy_column(&mut self) -> String {
        let Some((_, column)) = self.focus_cell() else {
            return String::new();
        };
        if self.rows.is_empty() {
            return String::new();
        }
        self.select(0, column, false);
        self.select(self.rows.len() - 1, column, true);
        self.tsv()
    }

    pub fn filter_focused_value(&mut self) {
        let Some((row, column)) = self.focus_cell() else {
            return;
        };
        let Some(name) = self.columns.get(column).map(|item| item.name.clone()) else {
            return;
        };
        match self.value_at(row, column).unwrap_or(None) {
            None => self.set_filter(&name, "null", ""),
            Some(text) if text.is_empty() => self.set_filter(&name, "empty", ""),
            Some(text) => self.set_filter(&name, "eq", &text),
        }
    }

    fn sheet_line(&self, row: usize) -> Vec<String> {
        let mut line = vec![self.row_number(row)];
        line.extend((0..self.columns.len()).map(|column| self.cell_label(row, column)));
        line
    }

    pub fn row_number(&self, row: usize) -> String {
        if self.rows.get(row).is_some_and(|item| item.inserted) {
            "+".to_owned()
        } else {
            (self.offset as usize + row + 1).to_string()
        }
    }

    /// 暂存只认稳定身份：行按 id、列按列名挂回当前显示位置。
    fn reattach_overlays(&mut self) {
        let column_names: Vec<String> = self.columns.iter().map(|column| column.name.clone()).collect();
        let row_ids: Vec<String> = self.rows.iter().map(|row| row.id.clone()).collect();
        for item in &mut self.overlays {
            if !item.column_name.is_empty() {
                if let Some(index) = column_names.iter().position(|name| name == &item.column_name) {
                    item.column = index;
                }
            }
            item.row = if item.row_id.is_empty() { usize::MAX } else { row_ids.iter().position(|id| id == &item.row_id).unwrap_or(usize::MAX) };
        }
    }

    fn any_row(&self, row_id: &str) -> Option<&GridRow> {
        self.rows
            .iter()
            .find(|row| row.id == row_id)
            .or_else(|| self.source.iter().find(|row| row.id == row_id))
            .or_else(|| self.retained.iter().find(|row| row.id == row_id))
    }

    fn row_for(&self, overlay: &Overlay) -> Option<&GridRow> {
        self.any_row(&overlay.row_id)
    }

    /// 行离开当前读取窗口时，把仍被暂存引用的行留在 retained，预览和提交仍能定位它。
    fn retain_staged_rows(&mut self) {
        let mut keep = Vec::new();
        for overlay in &self.overlays {
            if overlay.change_id.is_none() {
                continue;
            }
            if let Some(row) = self.any_row(&overlay.row_id) {
                if !keep.iter().any(|item: &GridRow| item.id == row.id) {
                    keep.push(row.clone());
                }
            }
        }
        self.retained = keep;
    }

    fn prune_retained(&mut self) {
        self.retained.retain(|row| self.overlays.iter().any(|overlay| overlay.row_id == row.id));
    }

    /// 提交成功后把新值写回三个行来源，界面不再显示提交前的旧值。
    fn set_row_value(&mut self, row_id: &str, column_name: &str, value: Option<String>) {
        let Some(column) = self.columns.iter().position(|item| item.name == column_name) else {
            return;
        };
        for row in self.source.iter_mut().chain(self.rows.iter_mut()).chain(self.retained.iter_mut()).filter(|row| row.id == row_id) {
            if let Some(slot) = row.values.get_mut(column) {
                *slot = value.clone();
            }
            if let Some(slot) = row.locator.iter_mut().find(|(name, _)| name == column_name) {
                slot.1 = value.clone();
            }
        }
    }

    fn recompute(&mut self) {
        let mut rows = self.source.clone();
        let applied = self.filters.iter().filter(|draft| !draft.dirty).count();
        let filtering = !self.search.is_empty() || applied > 0 || self.sort.is_some();
        // 已读取范围的查找在本地筛；服务器范围的行已经是服务器筛过的，不再重复筛。
        if self.find_scope == FindScope::Loaded && !self.search.is_empty() {
            let needle = self.search.to_lowercase();
            rows.retain(|row| {
                row.values.iter().any(|value| value.as_deref().unwrap_or("").to_lowercase().contains(&needle))
                    || row.locator.iter().any(|(_, value)| value.as_deref().unwrap_or("").to_lowercase().contains(&needle))
            });
        }
        // 还在编辑、没有应用的草稿不改变可见行；非法条件也不改变可见行，
        // 否则一个漏填的上界会让整张表突然变空。
        let drafts: Vec<FilterDraft> = self
            .filters
            .iter()
            .filter(|draft| !draft.dirty && self.compile_draft(draft).is_ok())
            .cloned()
            .collect();
        if !drafts.is_empty() {
            let indices: Vec<Option<usize>> = drafts.iter().map(|draft| self.columns.iter().position(|item| item.name == draft.column)).collect();
            rows.retain(|row| {
                drafts.iter().zip(indices.iter()).all(|(draft, index)| match index {
                    Some(index) => draft_matches(row.values.get(*index).cloned().unwrap_or(None), self.columns.get(*index), draft),
                    None => false,
                })
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
        self.scroll = 0;
        self.reattach_overlays();
        self.reattach_selection();
        // 重排后让焦点行仍在可见范围内；选择被筛掉时退回顶部。
        if let Some((focus, _)) = self.focus_cell() {
            let (start, end) = self.visible_bounds();
            if focus < start || focus >= end {
                self.scroll = focus;
            }
        }
        self.clamp_scroll();
    }

    /// 选择的锚点和焦点按行 id 找回；原来那一行真的消失时才清空选择。
    fn reattach_selection(&mut self) {
        if !self.selected {
            self.anchor_id.clear();
            self.focus_id.clear();
            return;
        }
        let anchor = self.row_index(&self.anchor_id);
        let focus = self.row_index(&self.focus_id);
        match (anchor, focus) {
            (Some(anchor), Some(focus)) => {
                self.anchor.0 = anchor;
                self.focus.0 = focus;
            }
            (None, Some(focus)) => {
                self.anchor = (focus, self.anchor.1);
                self.focus.0 = focus;
            }
            (Some(_), None) | (None, None) => {
                self.clear_selection();
                return;
            }
        }
        let last_column = self.columns.len().saturating_sub(1);
        self.anchor.1 = self.anchor.1.min(last_column);
        self.focus.1 = self.focus.1.min(last_column);
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

    /// 行内当前值（含暂存修改），按显示位置读取；越界返回 None。
    fn value_at(&self, row: usize, column: usize) -> Option<Option<String>> {
        if let Some(overlay) = self.overlay_at(row, column).filter(|item| item.kind == "update") {
            return Some(overlay.value.clone());
        }
        self.rows.get(row).and_then(|item| item.values.get(column).cloned())
    }

    fn tsv_field(&self, row: usize, column: usize) -> String {
        match self.value_at(row, column) {
            Some(Some(text)) if text == "NULL" => quote_tsv(&text, true),
            Some(Some(text)) => quote_tsv(&text, false),
            // 数据库 NULL 的显示文本来自设置；字符串 "NULL" 仍然强制加引号。
            _ => self.null_copy.clone(),
        }
    }

    fn overlay_at(&self, row: usize, column: usize) -> Option<&Overlay> {
        self.overlays.iter().find(|item| item.row == row && item.column == column && item.kind == "update")
    }

    /// 结构上是否允许写这一格：不看已存值，所以预览信封里的旧值仍可被替换。
    fn structural_gate(&self, row: Option<usize>, column: usize) -> Gate {
        if row.is_some_and(|row| self.rows.get(row).is_some_and(|item| item.inserted)) {
            return Gate::Writable;
        }
        let query_waiting = self.mode == GridMode::Query && !self.writable() && self.readonly_reason != "grid.status.unlocated";
        if query_waiting || self.schema.as_ref().is_some_and(|schema| schema.kind != "table") {
            return Gate::Readonly;
        }
        if row.is_some_and(|row| self.row_unlocated(row)) || self.schema.as_ref().is_some_and(|schema| schema.key.is_empty()) {
            return Gate::Unlocated;
        }
        if !self.writable() || self.columns.get(column).is_none() {
            return Gate::Readonly;
        }
        if self.mode == GridMode::Query && self.columns.get(column).is_some_and(|item| !item.editable) {
            return Gate::Readonly;
        }
        Gate::Writable
    }

    fn gate_for_edit(&self, row: Option<usize>, column: usize, value: Option<&str>) -> Gate {
        match self.structural_gate(row, column) {
            Gate::Writable if self.validation_message(column, value).is_some() => Gate::Validation,
            other => other,
        }
    }

    fn validation_message(&self, column: usize, value: Option<&str>) -> Option<String> {
        let schema = self.schema.as_ref()?;
        let name = self.columns.get(column)?.name.as_str();
        let origin = schema.columns.iter().find(|item| item.name == name)?;
        crate::grid::reject_literal(origin, &cell_of(value.map(str::to_owned)))
    }

    fn committable(&self) -> Vec<ChangeArg> {
        let mut changes = Vec::new();
        let staged = self.overlays.clone();
        for overlay in &staged {
            if overlay.gate != Gate::Writable {
                continue;
            }
            let Some(id) = overlay.change_id.clone() else { continue };
            let Some(row) = self.row_for(overlay).cloned() else { continue };
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
                            let value = self.writable_overlay(&row.id, &column.name).or_else(|| row.values.get(index).cloned())?;
                            if value.is_none() && column.auto {
                                return None;
                            }
                            Some(ValueArg { column: column.name.clone(), value, resolved: false })
                        })
                        .collect(),
                }),
                _ => {
                    if overlay.column_name.is_empty() {
                        continue;
                    }
                    changes.push(ChangeArg {
                        id,
                        kind: "update".to_owned(),
                        sets: vec![ValueArg { column: overlay.column_name.clone(), value: overlay.value.clone(), resolved: false }],
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
                let value = self.writable_overlay(&row.id, &column.name).or_else(|| row.values.get(index).cloned())?;
                if value.is_none() && column.auto {
                    return None;
                }
                Some((column.name.clone(), cell_of(value)))
            })
            .collect()
    }

    fn writable_overlay(&self, row_id: &str, column_name: &str) -> Option<Option<String>> {
        self.overlays
            .iter()
            .find(|item| item.row_id == row_id && item.column_name == column_name && item.kind == "update" && item.gate == Gate::Writable)
            .map(|item| item.value.clone())
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

#[allow(dead_code)]
pub fn column_of(name: &str, type_name: &str, primary: bool) -> Column {
    Column { name: name.to_owned(), type_name: type_name.to_owned(), nullable: !primary, primary, unique: primary, auto: primary }
}

#[allow(dead_code)]
pub fn classify(locale: Locale, value: Option<Option<String>>, type_name: &str, opened: bool) -> ShownCell {
    classify_with_limit(locale, value, type_name, opened, 1024)
}

/// 与 classify 相同，但用设置里的预览上限判断“大字段”；默认仍是 1024 字节。
pub fn classify_with_limit(
    locale: Locale,
    value: Option<Option<String>>,
    type_name: &str,
    opened: bool,
    preview_bytes: usize,
) -> ShownCell {
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
    if text.len() > preview_bytes.max(1) && !opened {
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

/// 本地在已读取行上复算同一条条件；NULL 不参与相等、范围和集合的命中，与 SQL 语义一致。
fn draft_matches(value: Option<String>, column: Option<&GridColumn>, draft: &FilterDraft) -> bool {
    let numeric = column.is_some_and(|item| matches!(value_family(&item.type_name), ValueFamily::Integer | ValueFamily::Decimal | ValueFamily::Float));
    match draft.op.as_str() {
        "null" => value.is_none(),
        "notnull" => value.is_some(),
        "empty" => value.as_deref() == Some(""),
        "contains" => value.as_deref().is_some_and(|text| text.to_lowercase().contains(&draft.value.to_lowercase())),
        "starts" => value.as_deref().is_some_and(|text| text.to_lowercase().starts_with(&draft.value.to_lowercase())),
        "eq" => value.as_deref() == Some(draft.value.as_str()),
        // SQL 里 NULL <> 值 不成立，因此 NULL 不算命中"不等于"。
        "ne" => value.is_some() && value.as_deref() != Some(draft.value.as_str()),
        "gt" | "lt" => {
            let Some(text) = value.as_deref() else {
                return false;
            };
            let order = compare_cell(Some(text), Some(draft.value.as_str()), numeric);
            if draft.op == "gt" {
                order.is_gt()
            } else {
                order.is_lt()
            }
        }
        "between" => {
            let Some(text) = value.as_deref() else {
                return false;
            };
            compare_cell(Some(text), Some(draft.value.as_str()), numeric).is_ge()
                && compare_cell(Some(text), Some(draft.second.as_str()), numeric).is_le()
        }
        "in" => match value.as_deref() {
            Some(text) => SharedGrid::filter_values(draft).iter().any(|item| item == text),
            None => false,
        },
        _ => true,
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

fn quote_tsv(value: &str, force: bool) -> String {
    if force || value.contains('\t') || value.contains('\n') || value.contains('\r') || value.contains('"') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// 列宽跟着列名保留，翻页或重新读取时不会回到默认宽度。
fn merge_widths(previous: &[GridColumn], mut columns: Vec<GridColumn>) -> Vec<GridColumn> {
    for column in &mut columns {
        if let Some(width) = previous.iter().find(|item| item.name == column.name).map(|item| item.width) {
            column.width = width;
        }
    }
    columns
}

fn domain_column(column: &GridColumn) -> Column {
    Column {
        name: column.name.clone(),
        type_name: column.type_name.clone(),
        nullable: column.nullable,
        primary: column.primary,
        unique: column.unique,
        auto: column.auto,
    }
}

#[allow(clippy::too_many_arguments)]
fn overlay(
    row: usize,
    row_id: String,
    column: usize,
    column_name: String,
    value: Option<String>,
    gate: Gate,
    message: String,
    change_id: Option<String>,
    kind: &'static str,
) -> Overlay {
    Overlay { row, row_id, column, column_name, value, gate, message, change_id, kind }
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

    fn int_column(name: &str) -> GridColumn {
        GridColumn { type_name: "integer".to_owned(), ..text_column(name) }
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
        assert_eq!(grid.sheet_labels().len(), 100);
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
    fn preview_limit_follows_the_setting_and_changes_the_label() {
        let long = "a".repeat(2048);
        let mut grid = SharedGrid::new();
        grid.load_local(
            vec![text_column("id"), text_column("note")],
            vec![row("1", vec![Some("1"), Some(long.as_str())], true)],
            Some(1),
            Some(schema()),
        );
        let large = format!("{} 2048", i18n::text(Locale::ZhCn, "grid.value.largeLabel"));
        assert_eq!(grid.cell_label(0, 1), large, "默认 1 KB 上限下显示大字段");
        grid.set_preview_bytes(4 * 1024);
        assert_eq!(grid.cell_label(0, 1), long, "4 KB 上限下同一值直接显示");
        assert_eq!(grid.page_request().unwrap().preview_bytes, 4096);
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
        // 语义变化：无法定位的行不再把输入值画成“已修改”，而是拒绝暂存并给出原因。
        assert_eq!(grid.stage_text(0, 1, Some("改".into())), Gate::Unlocated);
        assert_eq!(grid.cell_label(0, 1), "NULL");
        assert!(!grid.uncommitted(), "无法定位的编辑不应把网格标成未提交");
        assert_eq!(grid.gate_text(Gate::Unlocated), i18n::text(Locale::ZhCn, "grid.status.unlocated"));
        assert!(!grid.gate_text(Gate::Unlocated).is_empty());
        assert!(grid.confirm_request().is_err());
        // 复制仍然可用，且拿到的是数据库里的真实值。
        grid.select(0, 1, false);
        assert_eq!(grid.tsv(), "NULL");
    }

    #[test]
    fn query_sheet_opens_edits_only_after_a_current_locate() {
        let mut grid = SharedGrid::new();
        grid.target.engine = "sqlite".into();
        grid.target.connection_name = "笔记".into();
        grid.target.environment = "dev".into();
        grid.target.database = "main".into();
        let stale = grid.show_query_sheet("select * from customers", vec!["id".into(), "name".into()], vec![vec![Some("1".into()), Some("甲".into())]], false, Some(1));
        assert!(!grid.writable());
        grid.show_query_sheet("select count(*)", vec!["n".into()], vec![vec![Some("1".into())]], false, Some(1));
        assert!(!grid.finish_locate(stale, schema()) || !grid.writable());
        let current = grid.show_query_sheet("select id, name from customers", vec!["id".into(), "name".into()], vec![vec![Some("1".into()), Some("甲".into())]], false, Some(1));
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

    #[test]
    fn display_targets_follow_row_ids_across_sort_filter_page_and_append() {
        let mut grid = SharedGrid::new();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("10", vec![Some("10"), Some("甲")], true), row("11", vec![Some("11"), Some("乙")], true), row("12", vec![Some("12"), Some("丙")], true)],
            Some(3),
            Some(schema()),
        );
        grid.set_viewport(2);
        grid.select(1, 1, false);
        assert_eq!(grid.focused_row_id(), Some("11"));

        // 排序后焦点行号变了，但身份和查看目标不变。
        grid.set_sort("id", true);
        assert_eq!(grid.cell_label(0, 0), "12");
        assert_eq!(grid.focused_row_id(), Some("11"));
        let (display_row, column) = grid.focus_cell().expect("focus");
        assert_eq!(grid.row_id(display_row), Some("11"));
        assert_eq!(grid.cell_label(display_row, column), "乙");

        // 编辑目标由显示行号解析成行 id：降序显示第 0 行是 id 12，不是旧的焦点行。
        assert_eq!(grid.stage_text(0, 1, Some("改".into())), Gate::Writable);
        let sql = grid.open_preview().unwrap();
        assert!(sql[0].contains("\"id\" = 12"), "{sql:?}");
        assert!(!sql[0].contains("\"id\" = 11"), "{sql:?}");
        assert_eq!(grid.confirm_request().unwrap().changes[0].locator[0].value.as_deref(), Some("12"));

        // 完整值查看同样按行 id，重新排序后仍读同一行。
        grid.open_value(display_row, column);
        grid.set_sort("id", false);
        assert_eq!(grid.viewer().map(|shown| shown.label), Some("乙".to_owned()), "完整值查看应跟随行 id");

        // 筛选把焦点行排除后选择清空，而不是落到另一行。
        let mut filtered = SharedGrid::new();
        filtered.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("10", vec![Some("10"), Some("甲")], true), row("11", vec![Some("11"), Some("乙")], true)],
            Some(2),
            Some(schema()),
        );
        filtered.select(1, 1, false);
        filtered.set_filter("name", "eq", "甲");
        assert_eq!(filtered.loaded_len(), 1);
        assert!(filtered.focused_row_id().is_none());
        assert!(filtered.focus_cell().is_none());
        filtered.clear_filters();
        assert_eq!(filtered.loaded_len(), 2);
        filtered.select(1, 1, false);
        filtered.set_filter("name", "eq", "乙");
        assert_eq!(filtered.focused_row_id(), Some("11"), "筛掉其他行后焦点仍指向同一行");
        assert_eq!(filtered.focus_cell().map(|(row, _)| row), Some(0));

        // 追加读取不动选择，翻页替换窗口后暂存仍按原行 id 预览和提交。
        let mut paged = SharedGrid::new();
        paged.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("20", vec![Some("20"), Some("甲")], true), row("21", vec![Some("21"), Some("乙")], true)],
            Some(4),
            Some(schema()),
        );
        paged.set_viewport(2);
        paged.set_column_width(1, 320.0);
        paged.select(0, 1, false);
        assert_eq!(paged.stage_text(0, 1, Some("改".into())), Gate::Writable);
        let columns = vec![text_column("id"), text_column("name")];
        paged.append_page(
            vec![row("30", vec![Some("30"), Some("丙")], true), row("31", vec![Some("31"), Some("丁")], true)],
            Some(4),
            false,
        );
        assert_eq!(paged.loaded_len(), 4);
        assert_eq!(paged.focused_row_id(), Some("20"), "追加读取后选择仍指向原来那一行");
        paged.replace_fetched(
            columns,
            vec![row("40", vec![Some("40"), Some("戊")], true), row("41", vec![Some("41"), Some("己")], true)],
            2,
            Some(4),
            true,
            Some(schema()),
        );
        assert_eq!(paged.cell_label(0, 0), "40");
        assert!(paged.uncommitted(), "翻页不应丢掉上一页的暂存");
        assert_eq!(paged.column_width(1), 320.0, "列宽按列名在翻页后保留");
        let sql = paged.open_preview().unwrap();
        assert!(sql[0].contains("\"id\" = 20"), "{sql:?}");
        assert_eq!(paged.confirm_request().unwrap().changes[0].locator[0].value.as_deref(), Some("20"));
    }

    #[test]
    fn explicit_null_and_text_null_stay_apart_in_preview_and_copy() {
        let mut grid = SharedGrid::new();
        grid.target.engine = "sqlite".into();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some("NULL")], true), row("2", vec![Some("2"), None], true)],
            Some(2),
            Some(schema()),
        );
        // 字符串 "NULL" 改成数据库 NULL；数据库 NULL 改成字符串 "NULL"。
        assert_eq!(grid.stage_text(0, 1, None), Gate::Writable);
        assert_eq!(grid.stage_text(1, 1, Some("NULL".into())), Gate::Writable);
        let sql = grid.open_preview().unwrap();
        assert_eq!(sql.len(), 2);
        assert!(sql[0].contains("= NULL"), "{sql:?}");
        assert!(!sql[0].contains("'NULL'"), "{sql:?}");
        assert!(sql[1].contains("'NULL'"), "{sql:?}");
        assert!(!sql[1].contains("= NULL"), "{sql:?}");
        assert_eq!(grid.cell_label(0, 1), "NULL");
        assert_eq!(grid.show_cell(0, 1, false).kind, CellKind::Null);
        assert_eq!(grid.show_cell(1, 1, false).kind, CellKind::Text);

        // 复制时 NULL、字符串 "NULL" 和空字符串三种写法可区分。
        let mut copy = SharedGrid::new();
        copy.target.engine = "sqlite".into();
        copy.load_local(
            vec![text_column("name")],
            vec![
                GridRow { id: "a".into(), values: vec![Some("NULL".into())], located: false, locator: Vec::new(), inserted: false },
                GridRow { id: "b".into(), values: vec![None], located: false, locator: Vec::new(), inserted: false },
                GridRow { id: "c".into(), values: vec![Some(String::new())], located: false, locator: Vec::new(), inserted: false },
            ],
            Some(3),
            None,
        );
        copy.select_all_loaded();
        // split 保留末尾空字段，证明空字符串和 NULL 不会在最后一行被混同。
        assert_eq!(copy.tsv().split('\n').collect::<Vec<_>>(), vec!["\"NULL\"", "NULL", ""]);
        assert_eq!(copy.sql_values().split('\n').collect::<Vec<_>>(), vec!["'NULL'", "NULL", "''"]);
    }

    #[test]
    fn exchange_and_export_never_see_staged_rows_or_values() {
        let mut grid = SharedGrid::new();
        grid.target.connection_id = "notes".into();
        grid.target.table = "customers".into();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some("甲")], true)],
            Some(1),
            Some(schema()),
        );
        assert_eq!(grid.stage_text(0, 1, Some("改".into())), Gate::Writable);
        assert_eq!(grid.stage_insert(), Gate::Writable);
        let (table, columns, rows, _) = grid.exchange_sheet();
        assert!(table.is_some());
        assert_eq!(columns, vec!["id".to_owned(), "name".to_owned()]);
        assert_eq!(rows.len(), 1, "未提交的新增行不能进入交换或导出");
        assert_eq!(rows[0][1].as_deref(), Some("甲"), "未提交的修改不能进入交换或导出");
    }

    #[test]
    fn multi_cell_selection_copies_the_same_rectangle_after_scrolling() {
        let mut grid = SharedGrid::new();
        let rows = (0..50)
            .map(|index| row(&index.to_string(), vec![Some(&index.to_string()), Some(if index % 2 == 0 { "偶" } else { "奇" })], true))
            .collect();
        grid.load_local(vec![text_column("id"), text_column("name")], rows, Some(50), Some(schema()));
        grid.set_viewport(5);
        grid.select(2, 0, false);
        grid.move_key("down", true);
        grid.move_key("right", true);
        assert_eq!(grid.selection_count(), 4);
        let before = grid.tsv();
        assert_eq!(before.lines().count(), 2);
        assert!(before.lines().all(|line| line.split('\t').count() == 2));
        grid.scroll_to(40);
        assert_eq!(grid.tsv(), before, "虚拟滚动只改变呈现，不改变选择");
        grid.set_column_width(0, 260.0);
        grid.scroll_to(0);
        assert_eq!(grid.column_width(0), 260.0);
    }

    #[test]
    fn undo_drops_the_last_staged_change() {
        let mut grid = SharedGrid::new();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some("甲")], true), row("2", vec![Some("2"), Some("乙")], true)],
            Some(2),
            Some(schema()),
        );
        assert!(!grid.undo_last_stage());
        assert_eq!(grid.stage_text(0, 1, Some("改".into())), Gate::Writable);
        assert_eq!(grid.cell_label(0, 1), "改");
        assert!(grid.undo_last_stage());
        assert_eq!(grid.cell_label(0, 1), "甲");
        assert!(!grid.uncommitted());
        let before = grid.rendered_len();
        assert_eq!(grid.stage_insert(), Gate::Writable);
        assert_eq!(grid.rendered_len(), before + 1);
        assert!(grid.undo_last_stage());
        assert_eq!(grid.rendered_len(), before);
        assert!(!grid.uncommitted());
    }

    #[test]
    fn staged_edit_keeps_the_original_key_after_sort_or_filter() {
        let mut grid = SharedGrid::new();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some("甲")], true), row("2", vec![Some("2"), Some("乙")], true)],
            Some(2),
            Some(schema()),
        );
        assert_eq!(grid.stage_text(0, 1, Some("改".into())), Gate::Writable);
        grid.set_sort("id", true);
        assert_eq!(grid.cell_label(0, 0), "2");
        assert_eq!(grid.cell_label(1, 1), "改");
        let sql = grid.open_preview().unwrap();
        assert!(sql[0].contains("改"), "{sql:?}");
        assert!(sql[0].contains("\"id\" = 1"), "{sql:?}");
        assert!(!sql[0].contains("\"id\" = 2"), "{sql:?}");
        let request = grid.confirm_request().unwrap();
        assert_eq!(request.changes[0].locator[0].value.as_deref(), Some("1"));

        let mut filtered = SharedGrid::new();
        filtered.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some("甲")], true), row("2", vec![Some("2"), Some("乙")], true)],
            Some(2),
            Some(schema()),
        );
        assert_eq!(filtered.stage_text(0, 1, Some("改".into())), Gate::Writable);
        filtered.set_filter("name", "eq", "乙");
        assert_eq!(filtered.cell_label(0, 0), "2");
        let sql = filtered.open_preview().unwrap();
        assert!(sql[0].contains("\"id\" = 1"), "{sql:?}");
        assert!(!sql[0].contains("\"id\" = 2"), "{sql:?}");
        assert_eq!(filtered.confirm_request().unwrap().changes[0].locator[0].value.as_deref(), Some("1"));
    }

    #[test]
    fn failed_refresh_keeps_rows_conditions_selection_scroll_and_staging() {
        let mut grid = SharedGrid::new();
        grid.target.connection_id = "notes".into();
        grid.target.table = "customers".into();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some("甲")], true), row("2", vec![Some("2"), Some("乙")], true), row("3", vec![Some("3"), Some("丙")], true)],
            Some(3),
            Some(schema()),
        );
        grid.set_viewport(2);
        grid.set_column_width(1, 320.0);
        assert_eq!(grid.stage_text(0, 1, Some("改".into())), Gate::Writable);
        // 条件会重排可见行并把滚动拉回顶部，所以选择和滚动在条件之后再定。
        grid.set_filter("name", "ne", "丁");
        grid.select(1, 1, false);
        grid.scroll_to(1);
        // 同一张表的刷新/翻页失败：行列、条件、选择、滚动和暂存都保留，只多一条失败原因。
        grid.keep_after_failure("连接已断开");
        assert_eq!(grid.loaded_len(), 3);
        assert_eq!(grid.column_names(), vec!["id".to_owned(), "name".to_owned()]);
        assert_eq!(grid.column_width(1), 320.0);
        assert!(grid.uncommitted(), "失败不能丢掉暂存");
        assert_eq!(grid.focused_row_id(), Some("2"));
        assert_eq!(grid.scroll, 1);
        assert_eq!(grid.filters().len(), 1);
        assert_eq!(grid.read_failure(), Some("连接已断开"));
        assert_eq!(grid.state_text(false), "连接已断开");
        // 重试成功后失败提示消失，其他内容不动。
        grid.clear_read_failure();
        assert!(grid.read_failure().is_none());
        assert_eq!(grid.loaded_len(), 3);
        assert!(grid.uncommitted());
    }

    #[test]
    fn failed_read_after_switching_target_drops_the_old_sheet() {
        let mut grid = SharedGrid::new();
        grid.target.connection_id = "notes".into();
        grid.target.table = "customers".into();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some("甲")], true)],
            Some(1),
            Some(schema()),
        );
        let changed = grid.switch_target(GridTarget { connection_id: "notes".into(), table: "people".into(), ..Default::default() });
        assert!(changed);
        assert!(grid.stale_target(), "换表后快照属于旧表");
        grid.drop_stale_sheet();
        assert_eq!(grid.loaded_len(), 0, "旧表行不能继续展示");
        assert_eq!(grid.exchange_sheet().1.len(), 0, "旧列不能进入交换来源");
        assert!(!grid.has_more_rows());
        assert!(!grid.stale_target());
        // 同一张表再次打开不算换表，读取失败就不会丢快照。
        let mut same = SharedGrid::new();
        same.target.connection_id = "notes".into();
        same.target.table = "customers".into();
        same.load_local(vec![text_column("id")], vec![row("1", vec![Some("1")], true)], Some(1), Some(schema()));
        assert!(!same.switch_target(same.target.clone()));
        assert!(!same.stale_target());
    }

    #[test]
    fn find_scope_separates_loaded_rows_from_the_server_keyword() {
        let mut grid = SharedGrid::new();
        grid.target.connection_id = "notes".into();
        grid.target.table = "customers".into();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some("甲")], true), row("2", vec![Some("2"), Some("乙")], true)],
            Some(2),
            Some(schema()),
        );
        // 默认查找只筛已读取行，不下发关键字。
        grid.set_search("乙");
        assert_eq!(grid.find_scope(), FindScope::Loaded);
        assert_eq!(grid.loaded_len(), 1);
        assert_eq!(grid.scope(), RangeScope::LoadedSubset);
        assert_eq!(grid.page_request().unwrap().search, None);
        // 无命中不是表为空。
        grid.set_search("没有");
        assert_eq!(grid.emptiness(), SheetEmptiness::NoMatch);
        assert_eq!(grid.state_text(false), i18n::text(Locale::ZhCn, "grid.state.noMatch"));
        let mut empty = SharedGrid::new();
        empty.load_local(vec![text_column("id")], Vec::new(), Some(0), Some(schema()));
        assert_eq!(empty.emptiness(), SheetEmptiness::TableEmpty);
        assert_eq!(empty.state_text(false), i18n::text(Locale::ZhCn, "grid.state.tableEmpty"));
        // 切到整表筛选：关键字随请求下发，本地不再重复筛同一段文本。
        grid.set_search("乙");
        assert!(grid.set_find_scope(FindScope::Server).is_ok());
        assert_eq!(grid.page_request().unwrap().search.as_deref(), Some("乙"));
        assert_eq!(grid.loaded_len(), 2, "服务器范围的行已经是服务器筛过的");
        // 切回来又变成本地查找。
        assert!(grid.set_find_scope(FindScope::Loaded).is_ok());
        assert_eq!(grid.page_request().unwrap().search, None);
        assert_eq!(grid.loaded_len(), 1);
    }

    #[test]
    fn any_column_sorts_ascending_descending_and_back_to_none() {
        let mut grid = SharedGrid::new();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some("c")], true), row("2", vec![Some("2"), Some("a")], true), row("3", vec![Some("3"), Some("b")], true)],
            Some(3),
            Some(schema()),
        );
        let names = |grid: &SharedGrid| (0..grid.loaded_len()).map(|row| grid.cell_label(row, 1)).collect::<Vec<_>>();
        assert_eq!(grid.sort_state(), None);
        // 非首列升序 → 降序 → 取消，取消后回到原始顺序而不是停在最后一次视觉位置。
        assert_eq!(grid.toggle_sort("name"), Some(false));
        assert_eq!(names(&grid), vec!["a", "b", "c"]);
        assert_eq!(grid.toggle_sort("name"), Some(true));
        assert_eq!(names(&grid), vec!["c", "b", "a"]);
        assert_eq!(grid.toggle_sort("name"), None);
        assert_eq!(grid.sort_state(), None);
        assert_eq!(names(&grid), vec!["c", "a", "b"]);
        // 不存在的列不能排。
        assert_eq!(grid.toggle_sort("missing"), None);
        // 请求里的排序列与方向跟着当前列走。
        grid.set_sort("id", true);
        let request = grid.page_request().unwrap();
        assert_eq!(request.sort.map(|sort| (sort.column, sort.direction)), Some(("id".to_owned(), "desc".to_owned())));
        grid.clear_sort();
        assert!(grid.page_request().unwrap().sort.is_none());
    }

    #[test]
    fn filters_cover_range_set_null_and_keep_invalid_input_out_of_the_query() {
        let mut grid = SharedGrid::new();
        grid.target.connection_id = "notes".into();
        grid.target.table = "customers".into();
        grid.load_local(
            vec![int_column("id"), text_column("name")],
            vec![
                row("1", vec![Some("1"), Some("甲")], true),
                row("2", vec![Some("2"), None], true),
                row("3", vec![Some("3"), Some("丙")], true),
                row("4", vec![Some("4"), Some("")], true),
            ],
            Some(4),
            Some(schema()),
        );
        // 数字列给范围，文本列给包含与空串。
        assert!(grid.filter_ops_for("id").contains(&"between"));
        assert!(!grid.filter_ops_for("id").contains(&"contains"));
        assert!(grid.filter_ops_for("name").contains(&"contains"));
        assert!(grid.filter_ops_for("name").contains(&"empty"));
        // 范围。
        grid.set_filter_full("id", "between", "2", "3", "");
        assert_eq!(grid.loaded_len(), 2);
        let request = grid.page_request().unwrap();
        assert_eq!(request.filters[0].op, "between");
        assert_eq!(request.filters[0].value.as_deref(), Some("2"));
        assert_eq!(request.filters[0].second.as_deref(), Some("3"));
        // 集合。
        grid.set_filter_full("id", "in", "", "", "1, 4");
        assert_eq!(grid.loaded_len(), 2);
        let request = grid.page_request().unwrap();
        assert_eq!(request.filters[0].values, vec!["1".to_owned(), "4".to_owned()]);
        // NULL 与非空。
        grid.clear_filters();
        grid.set_filter("name", "null", "");
        assert_eq!(grid.loaded_len(), 1);
        grid.set_filter("name", "notnull", "");
        assert_eq!(grid.loaded_len(), 3);
        grid.set_filter("name", "empty", "");
        assert_eq!(grid.loaded_len(), 1);
        // 非法输入：不进请求，不改已有结果，并保留原因。
        grid.clear_filters();
        grid.set_filter_full("id", "between", "2", "", "");
        assert_eq!(grid.loaded_len(), 4, "缺上界的条件不能让表变空");
        let error = grid.page_request().unwrap_err();
        assert_eq!(error, i18n::text(Locale::ZhCn, "grid.filter.needRange"));
        grid.set_filter_full("id", "in", "", "", " , ");
        assert_eq!(grid.page_request().unwrap_err(), i18n::text(Locale::ZhCn, "grid.filter.needList"));
        grid.set_filter_full("id", "gt", "  ", "", "");
        assert_eq!(grid.page_request().unwrap_err(), i18n::text(Locale::ZhCn, "grid.filter.needValue"));
        // 还在编辑的草稿同样不发查询。
        grid.clear_filters();
        let index = grid.add_filter("name");
        assert!(grid.update_filter_text(index, true, "甲"));
        assert_eq!(grid.loaded_len(), 4, "未应用的草稿不改变可见行");
        assert!(grid.page_request().unwrap().filters.is_empty());
        assert!(grid.apply_filters().is_ok());
        assert_eq!(grid.loaded_len(), 1);
        // 应用失败时条件与结果都保留，可以改好再应用。
        assert!(grid.update_filter_text(index, true, ""));
        let reason = i18n::text(Locale::ZhCn, "grid.filter.needValue");
        assert_eq!(grid.apply_filters().unwrap_err(), reason);
        assert_eq!(grid.filter_error(), Some(reason.as_str()));
        assert_eq!(grid.loaded_len(), 1);
        // 单删与全清。
        assert!(grid.remove_filter(index));
        assert!(grid.filters().is_empty());
        assert_eq!(grid.loaded_len(), 4);
        grid.set_filter("name", "eq", "甲");
        grid.clear_filters();
        assert!(grid.filters().is_empty());
        assert_eq!(grid.loaded_len(), 4);
    }

    #[test]
    fn append_skips_rows_already_read_and_query_results_never_replay() {
        let mut grid = SharedGrid::new();
        grid.target.connection_id = "notes".into();
        grid.target.table = "customers".into();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some("甲")], true), row("2", vec![Some("2"), Some("乙")], true)],
            Some(4),
            Some(schema()),
        );
        assert_eq!(grid.continuation(false), Continuation::More);
        let added = grid.append_page(vec![row("2", vec![Some("2"), Some("乙")], true), row("3", vec![Some("3"), Some("丙")], true)], Some(4), true);
        assert_eq!(added, 1, "已读过的稳定行不再追加第二份");
        assert_eq!(grid.loaded_len(), 3);
        assert_eq!(grid.continuation(true), Continuation::Loading);
        assert_eq!(grid.continuation(false), Continuation::More);
        grid.append_page(vec![row("4", vec![Some("4"), Some("丁")], true)], Some(4), false);
        assert_eq!(grid.continuation(false), Continuation::Complete);
        // 查询结果不在这里延续：网格从不重新执行语句。
        let mut query = SharedGrid::new();
        query.target.engine = "sqlite".into();
        query.show_query_sheet("select id from customers", vec!["id".into()], vec![vec![Some("1".into())]], false, Some(1));
        assert_eq!(query.continuation(false), Continuation::Complete);
        assert_eq!(query.append_page(vec![row("q9", vec![Some("9")], false)], None, true), 0);
        assert_eq!(query.loaded_len(), 1);
    }

    #[test]
    fn insert_stays_bound_to_its_own_row_when_filter_hides_it() {
        let mut grid = SharedGrid::new();
        grid.target.connection_id = "notes".into();
        grid.target.table = "customers".into();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some("甲")], true), row("2", vec![Some("2"), Some("乙")], true)],
            Some(2),
            Some(schema()),
        );
        grid.set_filter("name", "eq", "甲");
        assert_eq!(grid.loaded_len(), 1, "筛选后只剩一行可见");
        assert_eq!(grid.stage_insert(), Gate::Writable);
        assert_eq!(grid.loaded_len(), 1, "新增行被筛选隐藏，不能顶替可见行");

        let sql = grid.open_preview().expect("新增行应能预览");
        assert!(sql[0].to_uppercase().starts_with("INSERT"), "{sql:?}");
        assert!(!sql[0].contains("甲") && !sql[0].contains("乙"), "不得复制可见行作为新增内容：{sql:?}");
    }

    #[test]
    fn rejected_insert_keeps_the_typed_value_without_writing_it() {
        let blob = schema_from_parts("sqlite", "", "files", "table", vec![column_of("id", "integer", true), column_of("payload", "blob", false)], vec!["id".to_owned()]).unwrap();
        let mut grid = SharedGrid::new();
        grid.load_local(
            vec![
                text_column("id"),
                GridColumn { name: "payload".into(), type_name: "blob".into(), nullable: true, primary: false, unique: false, auto: false, width: 80.0, editable: true },
            ],
            vec![row("1", vec![Some("1"), Some("abcd")], true)],
            Some(1),
            Some(blob),
        );
        assert_eq!(grid.stage_insert(), Gate::Writable);
        let inserted = grid.loaded_len() - 1;
        assert_eq!(grid.stage_text(inserted, 1, Some("zz".into())), Gate::Validation);
        assert_eq!(grid.cell_label(inserted, 1), "zz");
        assert_eq!(grid.gate_at(inserted, 1), Some(Gate::Validation));
        assert!(grid.rejected_visible());
        assert!(!grid.overlay_message(inserted, 1).is_empty());
        if let Ok(sql) = grid.open_preview() { assert!(sql.iter().all(|item| !item.contains("zz")), "{sql:?}") }
        if let Ok(request) = grid.confirm_request() {
            assert!(request.changes.iter().all(|change| {
                change.sets.iter().all(|item| item.value.as_deref() != Some("zz")) && change.values.iter().all(|item| item.value.as_deref() != Some("zz"))
            }));
        }
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
            &crate::grid::commands::PageRequest { table, filters: Vec::new(), sort: None, offset: 0, limit: 24, preview_bytes: 1024, search: None },
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
        // 提交成功后显示的是新值，而不是暂存移除后回退到旧值。
        assert_eq!(grid.cell_label(0, 1), "乙");
        assert_eq!(names(&database).await, vec![(1, "乙".into())]);

        let mut query = SharedGrid::new();
        query.target.engine = "sqlite".into();
        query.target.connection_id = "notes".into();
        query.show_query_sheet("select * from customers join people", vec!["name".into()], vec![vec![Some("乙".into())]], false, Some(1));
        // 语义变化：只读结果的写尝试被拒绝并说明原因，格子继续显示数据库里的原值。
        assert_eq!(query.stage_text(0, 0, Some("改".into())), Gate::Readonly);
        assert_eq!(query.cell_label(0, 0), "乙");
        assert!(!query.uncommitted());
        assert_eq!(query.gate_text(Gate::Readonly), i18n::text(Locale::ZhCn, "grid.status.readonly"));
        assert!(query.confirm_request().is_err());
        // 只读时仍可选择、复制和查看完整值。
        query.select(0, 0, false);
        assert_eq!(query.tsv(), "乙");
        query.open_value(0, 0);
        assert_eq!(query.viewer().map(|shown| shown.label), Some("乙".to_owned()));
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
            &crate::grid::commands::PageRequest { table: loose_ref, filters: Vec::new(), sort: None, offset: 0, limit: 10, preview_bytes: 1024, search: None },
        )
        .await
        .unwrap();
        let mut loose = SharedGrid::new();
        loose.target.engine = "sqlite".into();
        loose.target.table = "loose".into();
        loose.replace_fetched(columns_from_schema(&loose_schema, true), rows_from_page(&columns_from_schema(&loose_schema, true), &loose_page), 0, Some(1), false, Some(loose_schema));
        assert_eq!(loose.stage_text(0, 0, Some("改".into())), Gate::Unlocated);
        assert_eq!(loose.cell_label(0, 0), "原");
        assert!(!loose.uncommitted());
        assert_eq!(loose.gate_text(Gate::Unlocated), i18n::text(Locale::ZhCn, "grid.status.unlocated"));
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
                preview_bytes: 1024,
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
    async fn sqlite_writes_database_null_and_the_text_null_separately() {
        let dir = temp_dir("marks");
        let database = dir.join("marks.sqlite");
        let setup = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
            .await
            .unwrap();
        sqlx::query("CREATE TABLE marks (id INTEGER PRIMARY KEY, note TEXT)").execute(&setup).await.unwrap();
        sqlx::query("INSERT INTO marks (id, note) VALUES (1, 'NULL'), (2, NULL)").execute(&setup).await.unwrap();
        setup.close().await;
        let service = std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")));
        service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
        let table = crate::grid::commands::TableRef {
            connection_id: "notes".into(),
            database: "main".into(),
            schema: String::new(),
            table: "marks".into(),
            engine: "sqlite".into(),
            include_foreign_keys: false,
        };
        let schema = schema_from_dto(&crate::application::grid::describe(&service, &table).await.unwrap()).unwrap();
        let page = crate::application::grid::page(
            &service,
            &crate::grid::commands::PageRequest { table, filters: Vec::new(), sort: None, offset: 0, limit: 10, preview_bytes: 1024, search: None },
        )
        .await
        .unwrap();
        let columns = columns_from_schema(&schema, true);
        let mut grid = SharedGrid::new();
        grid.target = GridTarget {
            connection_id: "notes".into(),
            connection_name: "笔记".into(),
            database: "main".into(),
            schema: String::new(),
            table: "marks".into(),
            engine: "sqlite".into(),
            environment: "dev".into(),
        };
        grid.load_local(columns_from_schema(&schema, true), rows_from_page(&columns, &page), Some(2), Some(schema));
        let first = grid.rows.iter().position(|row| row.values.first().and_then(|value| value.as_deref()) == Some("1")).expect("row 1");
        let second = grid.rows.iter().position(|row| row.values.first().and_then(|value| value.as_deref()) == Some("2")).expect("row 2");
        // 字符串 "NULL" 显式改成数据库 NULL；数据库 NULL 改成同样的字符串 "NULL"。
        assert_eq!(grid.stage_text(first, 1, None), Gate::Writable);
        assert_eq!(grid.stage_text(second, 1, Some("NULL".into())), Gate::Writable);
        let sql = grid.open_preview().unwrap();
        assert!(sql.iter().any(|item| item.contains("= NULL") && !item.contains("'NULL'")), "{sql:?}");
        assert!(sql.iter().any(|item| item.contains("= 'NULL'")), "{sql:?}");
        let report = crate::application::grid::commit(&service, &grid.confirm_request().unwrap(), true).await.unwrap();
        assert!(report.failed.is_none(), "{}", report.failed.as_ref().map(|item| item.message.clone()).unwrap_or_default());
        grid.apply_commit(&report);
        assert_eq!(grid.show_cell(first, 1, false).kind, CellKind::Null);
        assert_eq!(grid.show_cell(second, 1, false).kind, CellKind::Text);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let first_is_null: i64 = sqlx::query_scalar("SELECT note IS NULL FROM marks WHERE id = 1").fetch_one(&pool).await.unwrap();
        let second_value: Option<String> = sqlx::query_scalar("SELECT note FROM marks WHERE id = 2").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(first_is_null, 1, "第一行应写入数据库 NULL");
        assert_eq!(second_value.as_deref(), Some("NULL"), "第二行应写入字符串 NULL");
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
            &crate::grid::commands::PageRequest { table, filters: Vec::new(), sort: None, offset: 0, limit: 10, preview_bytes: 1024, search: None },
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
                preview_bytes: 1024,
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
