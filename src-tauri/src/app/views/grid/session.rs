//! 共享结果网格的无界面状态。
//! 查找、排序、筛选和提交语句复用 grid 领域；这里只区分已读取窗口、选择和暂存。

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use crate::grid::commands::{ChangeArg, ColumnDto, CommitDto, PageDto, PageRequest, SchemaDto, SortArg, TableRef, ValueArg};
use crate::grid::{delete_sql, insert_sql, locate_sheet, qualified_name, quote_ident, sql_cell, update_sql, value_family, Cell, Column, Engine, TableSchema, ValueFamily};
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
    /// 这一格显示的只是预览信封里的片段，不是数据库原值。
    pub preview: bool,
}

/// 完整值的四种状态。预览、加载、完整和错误必须彼此可分：
/// 预览不是原值，加载中不能说成已读取，失败也不能退回预览冒充原值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellPhase {
    Preview,
    Loading,
    Full,
    Error,
}

/// 完整值查看的定位：稳定行 id + 列名，重排、翻页和选择移动都不改变它。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerState {
    pub row_id: String,
    pub column_name: String,
    pub phase: CellPhase,
    pub message: String,
}

/// 一次按定位读取完整值的请求；调用方执行后把结果交回 apply_full。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FullRead {
    pub row_id: String,
    pub column_name: String,
    pub locator: Vec<(String, Option<String>)>,
    /// 发起读取时的目标与快照代次：读回来时目标或行列已经换了，这次结果就不能落到当前格子上。
    pub target: GridTarget,
    pub token: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FullState {
    Loading,
    Ready(Option<String>),
    Failed(String),
}

/// 一格值能不能直接当原值用。需要完整值时先读一次，读不到就明确拒绝。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CellRead {
    Ready(Option<String>),
    /// 读取请求比普通值大得多，装进 Box 以免整个枚举被撑大。
    NeedsFull(Box<FullRead>),
    /// 没定位（无唯一键、未定位行或外键缺列），不能读取也不能当原值。
    Unavailable(&'static str),
}

/// 当前表的外键；列、引用模式/表和引用列都来自真实结构。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignKeyInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

/// 外键导航目标：连接、库、模式、表与条件值都在这里一次算清。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub connection_id: String,
    pub database: String,
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    pub values: Vec<Option<String>>,
    pub sql: String,
}

/// 会读到原值的动作。查看、复制、编辑、导出和 WHERE 都先按这些格子读完整值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridAction {
    Value,
    CopyRegion,
    CopySqlValues,
    CopyRow,
    CopyColumn,
    CopyCell,
    WhereCell,
    WhereRow,
    WhereColumn,
    Edit,
    FilterValue,
    Reference,
    /// 导出前把当前窗口里所有还是预览的格子读成原值。
    ExportValues,
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
    /// 同一次批量暂存（粘贴、选区设值）共用的编号；撤销时整批一起回退。
    batch: Option<u64>,
}

/// "复制为"支持的格式。除带表头的 TSV 外都复用 export::write 的写法。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyFormat {
    TsvHeader,
    Csv,
    Json,
    Markdown,
    Insert,
}

impl CopyFormat {
    pub const ALL: [CopyFormat; 5] = [CopyFormat::TsvHeader, CopyFormat::Csv, CopyFormat::Json, CopyFormat::Markdown, CopyFormat::Insert];

    pub fn command(self) -> &'static str {
        match self {
            CopyFormat::TsvHeader => "grid-copy-as-tsv",
            CopyFormat::Csv => "grid-copy-as-csv",
            CopyFormat::Json => "grid-copy-as-json",
            CopyFormat::Markdown => "grid-copy-as-markdown",
            CopyFormat::Insert => "grid-copy-as-insert",
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            CopyFormat::TsvHeader => "grid.copyAs.tsv",
            CopyFormat::Csv => "grid.copyAs.csv",
            CopyFormat::Json => "grid.copyAs.json",
            CopyFormat::Markdown => "grid.copyAs.markdown",
            CopyFormat::Insert => "grid.copyAs.insert",
        }
    }

    pub fn from_command(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|format| format.command() == id)
    }
}

/// 复制出来的文本；placeholder 为真表示 INSERT 用了占位表名，界面要告诉用户。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopiedText {
    pub text: String,
    pub placeholder: bool,
}

/// 复制停下的原因：没有选区，或选区里还有只读到预览、拿不到原值的格子。
/// 两种情况调用方都不写剪贴板。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyBlock {
    Empty,
    Missing,
}

/// 一次批量暂存被整体拒绝的原因。拒绝时不写任何覆盖层，已有暂存保持不变。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchReject {
    /// 剪贴板为空或不是文本。
    Empty,
    /// 没有焦点格或选区，不知道从哪里开始。
    NoFocus,
    /// 超出可见列或已加载行；不会自动新增行。
    OutOfRange,
    /// 整个网格只读，原因沿用现有的只读文案。
    Readonly(String),
    /// 目标区域含主键或自增列。
    KeyColumn(String),
    /// 某一格被现有写入门控拒绝（不可写、未定位或校验失败）。
    Blocked { column: String, reason: String },
}

impl BatchReject {
    pub fn message(&self, locale: Locale) -> String {
        match self {
            BatchReject::Empty => i18n::text(locale, "grid.batch.empty"),
            BatchReject::NoFocus => i18n::text(locale, "grid.batch.noFocus"),
            BatchReject::OutOfRange => i18n::text(locale, "grid.batch.outOfRange"),
            BatchReject::Readonly(reason) => i18n::fill(&i18n::text(locale, "grid.batch.readonly"), &[("reason", reason)]),
            BatchReject::KeyColumn(column) => i18n::fill(&i18n::text(locale, "grid.batch.keyColumn"), &[("column", column)]),
            BatchReject::Blocked { column, reason } => {
                i18n::fill(&i18n::text(locale, "grid.batch.blocked"), &[("column", column), ("reason", reason)])
            }
        }
    }
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

/// 复制范围：当前格、整行、整列。行按稳定 id、列按列名取身份，视觉重排不改变逻辑列。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyScope {
    Cell,
    Row,
    Column,
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

/// 一个矩形选区（行起、列起、行止、列止），始终归一化成 起 <= 止。
pub type Rect = (usize, usize, usize, usize);

fn rect_of(a: (usize, usize), b: (usize, usize)) -> Rect {
    (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
}

fn rect_has(rect: Rect, row: usize, column: usize) -> bool {
    row >= rect.0 && row <= rect.2 && column >= rect.1 && column <= rect.3
}

fn overlaps(left: Rect, right: Rect) -> bool {
    left.0 <= right.2 && right.0 <= left.2 && left.1 <= right.3 && right.1 <= left.3
}

/// 正在用 Ctrl/Cmd 拖出来的区域：按下时就知道这一拖是加还是减，松开才落定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RegionDrag {
    anchor: (usize, usize),
    subtract: bool,
    /// 减选时要跟着指针改写的 minus 下标；加选时为 None，主矩形本身就是那一块。
    slot: Option<usize>,
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
    // 来源与排序/筛选视图共享行；只有修改已读原值时才写时复制。
    source: Vec<Arc<GridRow>>,
    rows: Vec<Arc<GridRow>>,
    schema: Option<TableSchema>,
    search: String,
    find_scope: FindScope,
    /// 目标表所在连接是否仍然可用（已打开且未失联）；会话自己拿不到服务，由 GridPane 同步。
    target_live: bool,
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
    /// 选区的全部矩形：普通单击/拖选只有一块，Ctrl/Cmd 加选会往后面追加区域。
    /// 这块表是选择的唯一来源，高亮、复制、统计和批量动作都从它算起。
    regions: Vec<Rect>,
    /// 被 Ctrl/Cmd 从选择里减掉的矩形；加色区域减去这里才是真正的选区。
    minus: Vec<Rect>,
    /// 正在用 Ctrl/Cmd 拖选的区域：按下时已经决定这一拖是加还是减，松开时才落定。
    region_drag: Option<RegionDrag>,
    /// 完整值查看同样记住行 id + 列名，滚动、重排或改变列顺序后仍指向原来那一格。
    viewer: Option<ViewerState>,
    /// 按 (行 id, 列名) 缓存的完整原值：查看、复制、编辑、导出和 WHERE 共用同一次读取。
    full: HashMap<String, HashMap<String, FullState>>,
    /// 当前表的真实外键；只有读到结构时才可能有内容。
    keys: Vec<ForeignKeyInfo>,
    overlays: Vec<Overlay>,
    /// 已经离开当前读取窗口、但仍被暂存引用的行；翻页后预览和提交仍用它们。
    retained: Vec<Arc<GridRow>>,
    /// 没有对应暂存的失败（例如提交报告给出未知 id）单独记账，不伪造格子。
    failures: Vec<(String, String)>,
    preview_sql: Vec<String>,
    preview_ids: Vec<String>,
    preview_open: bool,
    /// 大字段预览的截断上限（字节）；来自设置 preview_kb，只用于下一次读取。
    preview_bytes: usize,
    /// 当前这份行列读出来时用的预览上限。判断预览信封、导出和编辑都用它，
    /// 设置改了也不会把旧快照按新上限重新解释。
    sheet_preview_bytes: usize,
    /// 快照代次：每次载入或替换行列都加一。按定位读完整值的批次带着它，
    /// 目标、行列一变，迟到的读取结果就会被丢弃。
    sheet_serial: u64,
    export_revision: u64,
    /// TSV 里数据库 NULL 的显示文本；SQL 字面量始终是 NULL。
    null_copy: String,
    readonly_reason: String,
    seq: u64,
    pub token: u64,
    sql: String,
    /// 只影响显示的隐藏列（按列名）；复制、导出、筛选和排序仍按全部逻辑列。
    hidden: BTreeSet<String>,
    /// 冻结到哪一列（按列名）；它和它左边的可见列固定在左侧。
    frozen: Option<String>,
    /// 批量暂存替换掉的旧覆盖层及其移除时的下标：撤销整批时按原位置放回去，保持暂存顺序。
    batches: Vec<(u64, Vec<(usize, Overlay)>)>,
    /// 由"复制为新行"产生的新增行：它们的主键和自增列留空时不写进 INSERT。
    cloned: Vec<String>,
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
            target_live: true,
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
            regions: Vec::new(),
            minus: Vec::new(),
            region_drag: None,
            viewer: None,
            full: HashMap::new(),
            keys: Vec::new(),
            overlays: Vec::new(),
            retained: Vec::new(),
            failures: Vec::new(),
            preview_sql: Vec::new(),
            preview_ids: Vec::new(),
            preview_open: false,
            preview_bytes: crate::grid::DEFAULT_PREVIEW_BYTES,
            sheet_preview_bytes: crate::grid::DEFAULT_PREVIEW_BYTES,
            sheet_serial: 0,
            export_revision: 0,
            null_copy: "NULL".to_owned(),
            readonly_reason: String::new(),
            seq: 0,
            token: 0,
            sql: String::new(),
            hidden: BTreeSet::new(),
            frozen: None,
            batches: Vec::new(),
            cloned: Vec::new(),
        }
    }

    pub fn load_local(&mut self, columns: Vec<GridColumn>, rows: Vec<GridRow>, full_count: Option<i64>, schema: Option<TableSchema>) {
        self.mode = GridMode::Table;
        self.columns = merge_widths(&self.columns, columns);
        self.source = rows.into_iter().map(Arc::new).collect();
        self.schema = schema;
        self.full_count = full_count;
        self.truncated = full_count.is_some_and(|count| count > self.source.len() as i64);
        self.scope = RangeScope::Fetched;
        self.overlays.clear();
        self.batches.clear();
        self.cloned.clear();
        self.retained.clear();
        self.failures.clear();
        self.selected = false;
        // 这份行列是这一次读取的结果：预览上限随快照一起记下来，旧原值缓存作废。
        self.record_sheet_limit();
        self.viewer = None;
        self.full.clear();
        self.keys.clear();
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
        self.source = rows.into_iter().map(Arc::new).collect();
        self.schema = schema;
        self.offset = offset;
        self.full_count = full_count;
        self.truncated = truncated;
        self.scope = RangeScope::Fetched;
        self.read_failure = None;
        self.target_changed = false;
        self.record_sheet_limit();
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
            // 结构上像单表 SELECT，但唯一键还没读到：原因写清楚，而不是一句"只读"。
            Ok(_) => "grid.readonly.locatePending".to_owned(),
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
            .map(|(index, values)| Arc::new(GridRow {
                id: format!("q{index}"),
                values,
                located: false,
                locator: Vec::new(),
                inserted: false,
            }))
            .collect();
        self.full_count = full_count;
        self.truncated = truncated;
        self.offset = 0;
        self.scope = RangeScope::Fetched;
        self.overlays.clear();
        self.batches.clear();
        self.cloned.clear();
        self.retained.clear();
        self.failures.clear();
        self.selected = false;
        self.record_sheet_limit();
        self.viewer = None;
        self.full.clear();
        self.keys.clear();
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
        let located = locate_sheet(&schema, &names, self.source.iter().map(|row| &row.values));
        if !self.source.is_empty() && located.iter().all(|row| !row.located) {
            self.readonly_reason = "grid.status.unlocated".to_owned();
            self.rows.clear();
            for (row, item) in self.source.iter_mut().zip(located) {
                let row = Arc::make_mut(row);
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
        self.rows.clear();
        for (row, item) in self.source.iter_mut().zip(located) {
            let row = Arc::make_mut(row);
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

    /// 读这份行列时用的预览上限；查看、复制、编辑、WHERE、导出和外键判断都按它比较。
    pub fn sheet_preview_bytes(&self) -> usize {
        self.sheet_preview_bytes
    }

    /// 当前快照代次；按定位读取的批次带着它，目标或行列一变就作废。
    pub fn sheet_serial(&self) -> u64 {
        self.sheet_serial
    }

    fn record_sheet_limit(&mut self) {
        self.sheet_preview_bytes = self.preview_bytes.max(1);
        self.sheet_serial = self.sheet_serial.saturating_add(1);
    }

    /// 预览上限变了：按旧上限得到的原值缓存和查看面板都不再可信。
    /// 返回 true 表示上限真的变了，调用方应重读行列（不能重读时至少清空派生状态）。
    pub fn set_preview_bytes(&mut self, bytes: usize) -> bool {
        let bytes = bytes.max(1);
        if bytes == self.preview_bytes {
            return false;
        }
        self.preview_bytes = bytes;
        // 迟到的完整值读取带着旧代次回来，这里换代后一律丢弃。
        self.invalidate_previews();
        true
    }

    /// 清空按定位读取的原值缓存并关闭查看面板；代次加一，在路上读取的结果随之作废。
    pub fn invalidate_previews(&mut self) {
        self.full.clear();
        self.viewer = None;
        self.sheet_serial = self.sheet_serial.saturating_add(1);
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
            self.batches.clear();
            self.cloned.clear();
            // 换了表：隐藏和冻结属于旧表的列，不带到新表上。
            self.hidden.clear();
            self.frozen = None;
            self.retained.clear();
            self.failures.clear();
            self.search.clear();
            self.find_scope = FindScope::Loaded;
            self.filters.clear();
            self.filter_error = None;
            self.read_failure = None;
            self.sort = None;
            self.selected = false;
            // 换了目标：查看面板和按定位读到的原值都不属于新表，代次一并换代。
            self.invalidate_previews();
            self.keys.clear();
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

    /// 行数摘要：完整行数已知时写“起–止 / 总数 行”（读完时只写总数），未知时只写已读行数。
    pub fn range_text(&self) -> String {
        let loaded = self.rows.len();
        let unit = i18n::text(self.locale, "ui.grid.rows");
        let first = self.offset + 1;
        let last = self.offset + loaded as u64;
        match self.full_count {
            Some(count) if self.offset == 0 && loaded as i64 >= count => format!("{} {unit}", group_digits(count)),
            Some(count) if loaded == 0 => format!("0 / {} {unit}", group_digits(count)),
            Some(count) => format!("{}–{} / {} {unit}", group_digits(first as i64), group_digits(last as i64), group_digits(count)),
            None if self.offset > 0 && loaded > 0 => format!("{}–{} {unit}", group_digits(first as i64), group_digits(last as i64)),
            None => format!("{} {unit}", group_digits(loaded as i64)),
        }
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

    /// 目标连接是否仍可用：只有它为真时整表筛选才可能重新读表。
    pub fn target_live(&self) -> bool {
        self.target_live
    }

    /// 连接已打开、失联或关闭会随时变化；界面每次操作和每帧都把真实状态同步进来。
    pub fn set_target_live(&mut self, live: bool) {
        self.target_live = live;
    }

    /// 服务器范围筛选要求这个网格能重新读取某张表才能成立。
    /// 查询结果已由语句返回、没有目标表、目标连接已关闭或失联，都不该给出一个点了却不发请求的开关。
    pub fn find_scope_block(&self) -> Option<&'static str> {
        if self.mode == GridMode::Query {
            return Some("grid.find.scopeQueryOnly");
        }
        if self.target.connection_id.is_empty() || self.target.table.is_empty() {
            return Some("grid.find.scopeNoTable");
        }
        // 表目标还在，但连接不能重新读取：切到服务器范围只会让关键字静默失效。
        if !self.target_live {
            return Some("grid.find.scopeOffline");
        }
        None
    }

    /// 整表筛选生效期间连接丢失：退回已读取行，关键字继续在本地筛；返回是否真的退回了。
    pub fn demote_server_find_scope(&mut self) -> bool {
        if self.find_scope != FindScope::Server || self.find_scope_block().is_none() {
            return false;
        }
        self.find_scope = FindScope::Loaded;
        self.recompute();
        true
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
        // 还有更多、已到末尾、不能续读都不是需要用户处理的事：行数摘要和翻页按钮已经说明，
        // 这里不再常驻一句解释。
        match self.continuation(loading) {
            Continuation::Loading => i18n::text(self.locale, "grid.state.loading"),
            Continuation::More | Continuation::Complete | Continuation::Unavailable => String::new(),
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

    /// 下一次读取要用的大字段预览上限（字节）；由设置 preview_kb 下发。
    pub fn preview_bytes(&self) -> usize {
        self.preview_bytes
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
        self.regions.clear();
        self.minus.clear();
        self.region_drag = None;
    }

    pub fn column_width(&self, index: usize) -> f32 {
        self.columns.get(index).map(|column| column.width).unwrap_or(120.0)
    }

    pub fn set_column_width(&mut self, index: usize, width: f32) {
        if let Some(column) = self.columns.get_mut(index) {
            column.width = width.clamp(COLUMN_MIN_WIDTH, COLUMN_MAX_WIDTH);
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
        if !extend || !self.selected || self.regions.is_empty() {
            self.anchor = (row, column);
            self.anchor_id = self.rows[row].id.clone();
            self.regions.clear();
            self.regions.push((row, column, row, column));
            self.minus.clear();
        } else {
            // Shift 扩展改的是最后一块区域；Ctrl/Cmd 加进来其它区域保持不动。
            let rect = rect_of(self.anchor, (row, column));
            let last = self.regions.len() - 1;
            self.regions[last] = rect;
            self.minus.clear();
        }
        self.region_drag = None;
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
        self.regions.clear();
        self.regions.push((0, 0, self.rows.len() - 1, self.columns.len() - 1));
        self.minus.clear();
        self.region_drag = None;
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

    pub fn row_index(&self, row_id: &str) -> Option<usize> {
        self.rows.iter().position(|row| row.id == row_id)
    }

    /// 选区的整体边界（行起、列起、行止、列止）。非连续选择取全部区域的包围盒；
    /// 逐格的判据是 selection_regions + selection_holes，这里是给"整行/整列"判断用的摘要。
    pub fn selection_bounds(&self) -> Option<(usize, usize, usize, usize)> {
        if !self.selected || self.rows.is_empty() || self.columns.is_empty() {
            return None;
        }
        let mut bounds: Option<Rect> = None;
        for rect in self.regions.iter().copied() {
            bounds = Some(match bounds {
                Some(current) => (
                    current.0.min(rect.0).min(self.rows.len() - 1),
                    current.1.min(rect.1).min(self.columns.len() - 1),
                    current.2.max(rect.2).min(self.rows.len() - 1),
                    current.3.max(rect.3).min(self.columns.len() - 1),
                ),
                None => (rect.0.min(self.rows.len() - 1), rect.1.min(self.columns.len() - 1), rect.2.min(self.rows.len() - 1), rect.3.min(self.columns.len() - 1)),
            });
        }
        bounds
    }

    /// 选择锚点与焦点各自对应的稳定行 id；滚动、排序、筛选和翻页后仍是同两行。
    pub fn selection_ids(&self) -> Option<(&str, &str)> {
        if !self.selected || self.anchor_id.is_empty() || self.focus_id.is_empty() {
            return None;
        }
        Some((self.anchor_id.as_str(), self.focus_id.as_str()))
    }

    /// 选中的格子数：非连续选择把各区域并起来再减掉洞，不是包围盒的面积。
    pub fn selection_count(&self) -> usize {
        self.selection().len()
    }

    pub fn selection_within_loaded(&self) -> bool {
        self.selection().iter().all(|(row, column)| *row < self.rows.len() && *column < self.columns.len())
    }

    /// Ctrl/Cmd 单击：已选中的格子从选择里减掉，没选中的加成一块新区域。
    /// 拖选时从按下这一格开始扩成矩形，松开才落定（见 toggle_drag）。
    pub fn toggle_press(&mut self, row: usize, column: usize) {
        if self.rows.is_empty() || self.columns.is_empty() {
            self.clear_selection();
            return;
        }
        let row = row.min(self.rows.len() - 1);
        let column = column.min(self.columns.len() - 1);
        let already = self.is_selected(row, column);
        self.anchor = (row, column);
        self.anchor_id = self.rows[row].id.clone();
        self.focus = (row, column);
        self.focus_id = self.rows[row].id.clone();
        self.selected = true;
        if already {
            // 减选：这一格先挖掉，拖动时把洞扩成矩形。
            self.minus.push((row, column, row, column));
            let slot = self.minus.len() - 1;
            self.region_drag = Some(RegionDrag { anchor: (row, column), subtract: true, slot: Some(slot) });
        } else {
            self.regions.push((row, column, row, column));
            let slot = self.regions.len() - 1;
            self.region_drag = Some(RegionDrag { anchor: (row, column), subtract: false, slot: Some(slot) });
        }
    }

    /// Ctrl/Cmd 拖过另一格：把按下时那一块区域换成从按下格到当前格的矩形；
    /// 减选时改的是洞，原来的区域保持不动。
    pub fn toggle_drag(&mut self, row: usize, column: usize) {
        let Some(drag) = self.region_drag else {
            return;
        };
        if self.rows.is_empty() || self.columns.is_empty() {
            return;
        }
        let row = row.min(self.rows.len() - 1);
        let column = column.min(self.columns.len() - 1);
        let rect = rect_of(drag.anchor, (row, column));
        if drag.subtract {
            if let Some(slot) = drag.slot {
                self.minus[slot] = rect;
            }
        } else if let Some(slot) = drag.slot {
            self.regions[slot] = rect;
        }
        self.focus = (row, column);
        self.focus_id = self.rows[row].id.clone();
    }

    /// 松开左键：这一段拖选结束了。整块被洞盖住的区域直接收掉，不留空区域。
    pub fn finish_region(&mut self) {
        self.region_drag = None;
        let holes = self.minus.clone();
        self.regions.retain(|rect| !holes.iter().any(|hole| hole == rect));
        if self.regions.is_empty() {
            self.minus.clear();
            self.selected = false;
            self.anchor_id.clear();
            self.focus_id.clear();
        } else {
            let regions = self.regions.clone();
            // 洞只在还和某块区域相交时才有意义。
            self.minus.retain(|hole| regions.iter().any(|rect| overlaps(*rect, *hole)));
        }
    }

    /// 这一格现在算不算选中：落在任何区域里、并且没有被减选的洞挖掉。
    pub fn is_selected(&self, row: usize, column: usize) -> bool {
        if !self.selected {
            return false;
        }
        let hit = self.regions.iter().any(|rect| rect_has(*rect, row, column));
        hit && !self.minus.iter().any(|hole| rect_has(*hole, row, column))
    }

    /// 选择的全部矩形（Ctrl/Cmd 加进来的区域也在内）。高亮按它逐格判断，
    /// 和复制、统计、批量动作读的是同一份。
    pub fn selection_regions(&self) -> Vec<Rect> {
        if !self.selected {
            return Vec::new();
        }
        self.regions.clone()
    }

    /// 减选的洞：这些矩形里的格子即使落在区域里也不算选中。
    pub fn selection_holes(&self) -> Vec<Rect> {
        if !self.selected {
            return Vec::new();
        }
        self.minus.clone()
    }

    /// 选区是不是由多块区域拼起来的（Ctrl/Cmd 加选过）。
    pub fn has_extra_regions(&self) -> bool {
        self.selected && self.regions.len() > 1
    }

    /// 有没有真的选中格子。工具条每帧都问，所以只看区域表，不展开成格子。
    pub fn has_selection(&self) -> bool {
        self.selected && !self.regions.is_empty()
    }

    pub fn move_key(&mut self, key: &str, extend: bool) {
        if self.rows.is_empty() || self.columns.is_empty() {
            return;
        }
        if !self.selected {
            self.select(0, 0, false);
        }
        let (row, column) = self.focus;
        // 左右和 Home/End 只在可见列之间移动；隐藏列不会接住焦点。
        let visible = self.visible_columns();
        let position = visible.iter().position(|item| *item >= column).unwrap_or(visible.len().saturating_sub(1));
        let at = |index: usize| visible.get(index).copied().unwrap_or(column);
        // 翻页按当前可视行数整页走；可视行数由外面按窗口高度发布，边界处停在首/末行。
        let page = self.viewport.max(1);
        let next = match key {
            "up" => (row.saturating_sub(1), column),
            "down" => ((row + 1).min(self.rows.len() - 1), column),
            "left" => (row, at(position.saturating_sub(1))),
            "right" => (row, at((position + 1).min(visible.len().saturating_sub(1)))),
            "home" => (row, at(0)),
            "end" => (row, at(visible.len().saturating_sub(1))),
            "pagedown" => ((row + page).min(self.rows.len() - 1), column),
            "pageup" => (row.saturating_sub(page), column),
            _ => (row, column),
        };
        self.select(next.0, next.1, extend);
        self.follow_focus();
    }

    /// Tab / Shift+Tab：在可见列之间逐格移动，走到行首/行尾就换到上一行或下一行，
    /// 首格和末格处不动。隐藏列不接住 Tab，和方向键的可见列范围一致。
    /// 返回是否真的移动了：停在边界时调用方不再拦 Tab，键盘用户可以从网格两端用 Tab 离开。
    pub fn move_tab(&mut self, forward: bool) -> bool {
        if self.rows.is_empty() || self.columns.is_empty() {
            return false;
        }
        if !self.selected {
            self.select(0, 0, false);
            return true;
        }
        let (row, column) = self.focus;
        let visible = self.visible_columns();
        let Some(position) = visible.iter().position(|item| *item == column) else {
            self.select(row, visible[0], false);
            return true;
        };
        let next = if forward {
            match visible.get(position + 1) {
                Some(column) => Some((row, *column)),
                None if row + 1 < self.rows.len() => Some((row + 1, visible[0])),
                None => None,
            }
        } else if position > 0 {
            Some((row, visible[position - 1]))
        } else if row > 0 {
            Some((row - 1, visible[visible.len() - 1]))
        } else {
            None
        };
        let Some((row, column)) = next else {
            return false;
        };
        self.select(row, column, false);
        self.follow_focus();
        true
    }

    /// 可视行数：翻页步长和“屏幕上的行”都用它，由网格按窗口高度发布。
    pub fn viewport_rows(&self) -> usize {
        self.viewport.max(1)
    }

    /// 键盘移动后让滚动跟着焦点行走：焦点在窗口上方就顶到顶，在下方就滚到底。横向不动。
    fn follow_focus(&mut self) {
        let row = self.focus.0;
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll + self.viewport {
            self.scroll = row + 1 - self.viewport;
        }
        self.clamp_scroll();
    }

    /// 选区按块拼成的 TSV 正文（不含表头）：数据库 NULL 写成不带引号的 NULL，
    /// 字符串 "NULL" 写成带引号的 "NULL"，空字符串保持空字段；制表符、换行和引号按 TSV 规则转义。
    /// 非连续选择里没选中的格子留空，行内从最小列排到最大列。
    pub fn tsv(&self) -> String {
        let points = self.selection();
        if self.missing_full(&points) {
            return String::new();
        }
        self.tsv_grid(&points)
    }

    /// 复制到剪贴板的文本：单格复制值本身，多格复制含表头的 TSV。
    /// 多格式复制（grid-copy-as-*）仍然各写各的格式，长值和二进制的原值保护照旧。
    pub fn copy_text(&self) -> String {
        let Some((rows, column_start, column_end)) = self.selection_block() else {
            return String::new();
        };
        let points = self.selection();
        if self.missing_full(&points) {
            return String::new();
        }
        if rows.len() == 1 && column_start == column_end {
            return self.tsv_field(rows[0], column_start);
        }
        format!("{}\n{}", self.header_tsv(column_start, column_end), self.tsv_grid(&points))
    }

    /// 复制块的表头：列名按逻辑列顺序，和正文同一套 TSV 转义。
    fn header_tsv(&self, column_start: usize, column_end: usize) -> String {
        (column_start..=column_end.min(self.columns.len().saturating_sub(1)))
            .filter_map(|column| self.columns.get(column).map(|item| quote_tsv(&item.name, false)))
            .collect::<Vec<_>>()
            .join("\t")
    }

    /// SQL 值复制：按目标引擎写成字面量，NULL 与 'NULL' 在文本上也可区分。
    pub fn sql_values(&self) -> String {
        let points = self.selection();
        if self.missing_full(&points) {
            return String::new();
        }
        self.sql_grid(&points)
    }

    /// 一组单元格（显示行列）拼成多行 TSV；行内顺序就是传入顺序（逻辑列顺序）。
    fn tsv_grid(&self, points: &[(usize, usize)]) -> String {
        if points.is_empty() {
            return String::new();
        }
        // 块按整段列范围对齐：非连续选择里没选中的格子留空字段，每行宽度一致。
        let column_start = points.iter().map(|(_, column)| *column).min().unwrap_or(0);
        let column_end = points.iter().map(|(_, column)| *column).max().unwrap_or(0);
        let width = column_end.saturating_sub(column_start) + 1;
        let finish = |line: Vec<Option<String>>| line.into_iter().map(|field| field.unwrap_or_default()).collect::<Vec<_>>().join("\t");
        let mut lines: Vec<String> = Vec::new();
        let mut line: Vec<Option<String>> = vec![None; width];
        let mut current = usize::MAX;
        for (row, column) in points.iter().copied() {
            if row != current {
                if current != usize::MAX {
                    lines.push(finish(line));
                }
                line = vec![None; width];
                current = row;
            }
            if let Some(slot) = line.get_mut(column.saturating_sub(column_start)) {
                *slot = Some(self.tsv_field(row, column));
            }
        }
        lines.push(finish(line));
        lines.join("\n")
    }

    fn sql_grid(&self, points: &[(usize, usize)]) -> String {
        if points.is_empty() {
            return String::new();
        }
        let engine = Engine::parse(&self.target.engine).unwrap_or(Engine::Sqlite);
        let mut lines: Vec<Vec<String>> = Vec::new();
        let mut current = usize::MAX;
        for (row, column) in points.iter().copied() {
            if row != current {
                lines.push(Vec::new());
                current = row;
            }
            let cell = match self.original(row, column) {
                Some(Some(text)) => Cell::Text(text),
                _ => Cell::Null,
            };
            let origin = self.columns.get(column).map(domain_column);
            lines.last_mut().expect("line").push(sql_cell(engine, origin.as_ref(), &cell));
        }
        lines.into_iter().map(|line| line.join(", ")).collect::<Vec<_>>().join("\n")
    }

    /// 打开完整值查看：先给预览，再按定位读取原值。普通选择与键盘移动不会走到这里。
    pub fn open_value(&mut self, row: usize, column: usize) {
        let (Some(row_id), Some(name)) = (self.rows.get(row).map(|item| item.id.clone()), self.columns.get(column).map(|item| item.name.clone()))
        else {
            return;
        };
        let (phase, message) = match self.full_state(&row_id, &name) {
            Some(FullState::Ready(_)) => (CellPhase::Full, String::new()),
            Some(FullState::Loading) => (CellPhase::Loading, String::new()),
            Some(FullState::Failed(message)) => (CellPhase::Error, message.clone()),
            None => (CellPhase::Preview, String::new()),
        };
        self.viewer = Some(ViewerState { row_id, column_name: name, phase, message });
    }

    pub fn close_value(&mut self) {
        self.viewer = None;
    }

    /// 查看面板当前的定位与状态；界面据此区分预览、加载、完整和错误。
    pub fn viewer_state(&self) -> Option<&ViewerState> {
        self.viewer.as_ref()
    }

    pub fn viewer(&self) -> Option<ShownCell> {
        let (row, column) = self.viewer_cell()?;
        Some(self.show_cell(row, column, true))
    }

    /// 完整值查看当前落在哪一格；行或列已不在当前快照时返回 None，而不是指向别的格子。
    pub fn viewer_cell(&self) -> Option<(usize, usize)> {
        let state = self.viewer.as_ref()?;
        let row = self.row_index(&state.row_id)?;
        let column = self.columns.iter().position(|item| item.name == state.column_name)?;
        Some((row, column))
    }

    pub fn viewer_reason(&self) -> String {
        match self.viewer_cell() {
            Some((row, column)) => self.write_reason(row, column),
            None => String::new(),
        }
    }

    pub fn show_cell(&self, row: usize, column: usize, opened: bool) -> ShownCell {
        let type_name = self.columns.get(column).map(|item| item.type_name.as_str()).unwrap_or("");
        // 已经读到原值就显示原值；否则显示预览信封里的片段，并把它标成预览。
        if let Some(Some(text)) = self.full_text_at(row, column) {
            return classify_with_limit(self.locale, Some(Some(text)), type_name, true, self.sheet_preview_bytes);
        }
        let mut shown = classify_with_limit(self.locale, self.value_at(row, column), type_name, opened, self.sheet_preview_bytes);
        shown.preview = self.needs_full(row, column);
        shown
    }

    fn full_state(&self, row_id: &str, column_name: &str) -> Option<&FullState> {
        self.full.get(row_id)?.get(column_name)
    }

    /// 这一格的定位列（唯一键整组）；未定位的行或没有唯一键的表给不出定位。
    fn cell_locator(&self, row: usize) -> Vec<(String, Option<String>)> {
        let Some(item) = self.rows.get(row) else {
            return Vec::new();
        };
        if item.inserted || !item.located || item.locator.is_empty() {
            return Vec::new();
        }
        item.locator.clone()
    }

    /// 已知行直接借用当前值，避免在逐格扫描时反复按 id 查找行、复制长文本。
    fn value_in_row<'a>(&'a self, row: &'a GridRow, column: usize) -> Option<Option<&'a str>> {
        let column_name = self.columns.get(column)?.name.as_str();
        if let Some(item) = self.overlays.iter().find(|item| item.row_id == row.id && item.column_name == column_name && item.kind == "update") {
            return Some(item.value.as_deref());
        }
        row.values.get(column).map(|value| value.as_deref())
    }

    fn row_needs_full(&self, row: &GridRow, column: usize) -> bool {
        let Some(Some(text)) = self.value_in_row(row, column) else {
            return false;
        };
        let type_name = self.columns.get(column).map(|item| item.type_name.as_str()).unwrap_or("");
        crate::grid::preview_envelope(text, type_name, self.sheet_preview_bytes).is_some()
    }

    /// 这一格（按行 id）显示的是否只是预览信封：长文本被截断或二进制只有长度时成立。
    pub fn needs_full_of(&self, row_id: &str, column: usize) -> bool {
        self.any_row(row_id).is_some_and(|row| self.row_needs_full(row, column))
    }

    /// 这一格（按行 id）已缓存的原值；仍在预览阶段、加载或失败时为空，绝不拿预览充当原值。
    pub fn full_text_of(&self, row_id: &str, column: usize) -> Option<Option<String>> {
        let column_name = self.columns.get(column)?.name.as_str();
        match self.full_state(row_id, column_name) {
            Some(FullState::Ready(value)) => Some(value.clone()),
            _ => None,
        }
    }

    /// 这一格显示的是否只是预览信封：长文本被截断或二进制只有长度时成立。
    pub fn needs_full(&self, row: usize, column: usize) -> bool {
        self.rows.get(row).is_some_and(|row| self.row_needs_full(row, column))
    }

    /// 已缓存的原值；仍在预览阶段、加载或失败时为空，绝不拿预览充当原值。
    pub fn full_text_at(&self, row: usize, column: usize) -> Option<Option<String>> {
        let row_id = self.rows.get(row)?.id.as_str();
        self.full_text_of(row_id, column)
    }

    /// 一格值按原值处理还是需要先读取；未定位时明确说明读不了。
    /// 测试用：这一格现在需要的那次按定位读取。
    #[cfg(test)]
    pub fn pending_read(&self, row: usize, column: usize) -> Option<FullRead> {
        match self.cell_read(row, column) {
            CellRead::NeedsFull(read) => Some(*read),
            _ => None,
        }
    }

    pub fn cell_read(&self, row: usize, column: usize) -> CellRead {
        if self.needs_full(row, column) {
            if let Some(ready) = self.full_text_at(row, column) {
                return CellRead::Ready(ready);
            }
            let locator = self.cell_locator(row);
            if locator.is_empty() {
                return CellRead::Unavailable("grid.value.noLocator");
            }
            let Some(row_id) = self.rows.get(row).map(|item| item.id.clone()) else {
                return CellRead::Unavailable("grid.value.noLocator");
            };
            let Some(column_name) = self.columns.get(column).map(|item| item.name.clone()) else {
                return CellRead::Unavailable("grid.value.noLocator");
            };
            return CellRead::NeedsFull(Box::new(FullRead { row_id, column_name, locator, target: self.target.clone(), token: self.sheet_serial }));
        }
        match self.value_at(row, column) {
            Some(value) => CellRead::Ready(value),
            None => CellRead::Unavailable("grid.value.noLocator"),
        }
    }

    /// 开始读取这个格子的完整值；已经在读、已经读到或本来就不需要时返回 None。
    /// 上次失败的读取在这里重新发起，失败原因不留在缓存里冒充结果。
    pub fn begin_full(&mut self, row: usize, column: usize) -> Option<FullRead> {
        let row_id = self.rows.get(row)?.id.clone();
        self.begin_full_of(&row_id, column)
    }

    /// 按稳定行 id 开始读取完整值：导出扫描要读被本地查找/筛选隐藏的行，
    /// 这些行没有显示位置，只能靠行 id 和它们自己的定位列发起读取。
    pub fn begin_full_of(&mut self, row_id: &str, column: usize) -> Option<FullRead> {
        if !self.needs_full_of(row_id, column) {
            return None;
        }
        let column_name = self.columns.get(column)?.name.clone();
        match self.full_state(row_id, &column_name) {
            Some(FullState::Ready(_)) | Some(FullState::Loading) => return None,
            _ => {}
        }
        let locator = {
            let row = self.any_row(row_id)?;
            if row.inserted || !row.located || row.locator.is_empty() {
                return None;
            }
            row.locator.clone()
        };
        let read = FullRead {
            row_id: row_id.to_owned(),
            column_name: column_name.clone(),
            locator,
            target: self.target.clone(),
            token: self.sheet_serial,
        };
        self.full.entry(row_id.to_owned()).or_default().insert(column_name.clone(), FullState::Loading);
        if let Some(state) = self.viewer.as_mut().filter(|state| state.row_id == row_id && state.column_name == column_name) {
            state.phase = CellPhase::Loading;
            state.message.clear();
        }
        Some(read)
    }

    /// 原值读回来了。查看面板同时从加载状态转到完整状态。
    /// 读取结果只在同一目标、同一快照代次时落到格子上；换了表、翻了页或改了预览上限，
    /// 迟到的结果直接丢弃，绝不写到同 id 的另一行上。返回值表示是否采纳。
    pub fn apply_full(&mut self, read: &FullRead, value: Option<String>) -> bool {
        if !self.accepts_full(read) {
            return false;
        }
        self.store_full(&read.row_id, &read.column_name, FullState::Ready(value), CellPhase::Full, String::new());
        true
    }

    /// 完整值读取失败：保留预览可见，但标记为错误并给原因，绝不当成原值。过期失败同样丢弃。
    pub fn fail_full(&mut self, read: &FullRead, message: &str) -> bool {
        if !self.accepts_full(read) {
            return false;
        }
        self.store_full(&read.row_id, &read.column_name, FullState::Failed(message.to_owned()), CellPhase::Error, message.to_owned());
        true
    }

    /// 这次读取是否还属于当前目标与当前行列：代次一致，行和列也还在。
    fn accepts_full(&self, read: &FullRead) -> bool {
        if read.token != self.sheet_serial {
            return false;
        }
        if read.target.connection_id != self.target.connection_id
            || read.target.database != self.target.database
            || read.target.schema != self.target.schema
            || read.target.table != self.target.table
        {
            return false;
        }
        let row_known = self.rows.iter().chain(self.source.iter()).chain(self.retained.iter()).any(|row| row.id == read.row_id);
        row_known && self.columns.iter().any(|column| column.name == read.column_name)
    }

    fn store_full(&mut self, row_id: &str, column_name: &str, state: FullState, phase: CellPhase, message: String) {
        self.export_revision = self.export_revision.wrapping_add(1);
        self.full.entry(row_id.to_owned()).or_default().insert(column_name.to_owned(), state);
        if let Some(viewer) = self.viewer.as_mut().filter(|viewer| viewer.row_id == row_id && viewer.column_name == column_name) {
            viewer.phase = phase;
            viewer.message = message;
        }
    }

    /// 还没有原值的格子（行 id + 列序号）：扫描范围与 exchange_sheet 给出的导出行完全一致，
    /// 只看当前来源行、跳过还没写库的新增行，并且包含被本地查找/筛选隐藏的行。
    /// 导出会写出隐藏行，所以它们也必须算进"还有多少预览"，否则导出被挡而网格没有读取入口。
    fn unresolved_source_iter(&self) -> impl Iterator<Item = (&str, usize)> {
        self.source.iter().filter(|row| !row.inserted).flat_map(move |row| {
            self.columns.iter().enumerate().filter_map(move |(column, origin)| {
                (self.row_needs_full(row, column) && !matches!(self.full_state(&row.id, &origin.name), Some(FullState::Ready(_))))
                    .then_some((row.id.as_str(), column))
            })
        })
    }

    fn unresolved_source_cells(&self) -> Vec<(String, usize)> {
        self.unresolved_source_iter().map(|(row_id, column)| (row_id.to_owned(), column)).collect()
    }

    /// 仅计数不分配行 id，也不复制已经缓存的完整值（包括数据库 NULL）。
    pub fn unresolved_previews(&self) -> usize {
        self.unresolved_source_iter().count()
    }

    /// 当前视图里能看到的未读格子（显示行列）：界面用它说明预览位置。
    pub fn unresolved_cells(&self) -> Vec<(usize, usize)> {
        self.unresolved_source_cells()
            .into_iter()
            .filter_map(|(row_id, column)| self.rows.iter().position(|row| row.id == row_id).map(|row| (row, column)))
            .collect()
    }

    /// 导出前按定位读取所有还没有原值的格子；隐藏行也在这里读到，读回的原值随后共享给所有入口。
    pub fn begin_unresolved_full(&mut self) -> Vec<FullRead> {
        let mut reads: Vec<FullRead> = Vec::new();
        for (row_id, column) in self.unresolved_source_cells() {
            if let Some(read) = self.begin_full_of(&row_id, column) {
                if !reads.iter().any(|item| item.row_id == read.row_id && item.column_name == read.column_name) {
                    reads.push(read);
                }
            }
        }
        reads
    }

    /// 这些导出行里还有拿不到原值的格子：没定位、加载中或上次失败都给出原因，动作必须停在这里。
    pub fn unresolved_block(&self) -> Option<String> {
        for (row_id, column) in self.unresolved_source_cells() {
            let Some(column_name) = self.columns.get(column).map(|item| item.name.clone()) else {
                continue;
            };
            match self.full_state(&row_id, &column_name) {
                Some(FullState::Ready(_)) => {}
                Some(FullState::Failed(message)) => return Some(message.clone()),
                Some(FullState::Loading) => return Some(i18n::text(self.locale, "grid.value.loadingFull")),
                _ => {
                    // 连读取都没能发起：这一行没有唯一键定位，原因比"没有原值"更具体。
                    let locatable = self.any_row(&row_id).is_some_and(|row| !row.inserted && row.located && !row.locator.is_empty());
                    return Some(i18n::text(self.locale, if locatable { "grid.value.noOriginal" } else { "grid.value.noLocator" }));
                }
            }
        }
        None
    }

    /// 当前焦点格是否适用外键导航：这一列有真实外键，行也定位到具体一行。
    pub fn has_reference(&self) -> bool {
        let Some((row, column)) = self.focus_cell() else {
            return false;
        };
        let Some(name) = self.columns.get(column).map(|item| item.name.clone()) else {
            return false;
        };
        self.foreign_key_for(&name).is_some() && !self.cell_locator(row).is_empty() && self.rows.get(row).is_some_and(|item| item.located)
    }

    /// 一次动作会读到哪些格子：查看、复制、编辑、WHERE、导出和外键导航共用这张表。
    pub fn action_cells(&self, action: GridAction) -> Vec<(usize, usize)> {
        match action {
            GridAction::Value | GridAction::CopyCell | GridAction::WhereCell | GridAction::Edit | GridAction::FilterValue => {
                self.focus_cell().into_iter().collect()
            }
            GridAction::CopyRegion | GridAction::CopySqlValues => self.selection(),
            GridAction::CopyRow => self.scope_points(CopyScope::Row).unwrap_or_default(),
            GridAction::CopyColumn => self.scope_points(CopyScope::Column).unwrap_or_default(),
            GridAction::WhereRow => self.scope_points(CopyScope::Row).unwrap_or_default(),
            GridAction::WhereColumn => self.scope_points(CopyScope::Column).unwrap_or_default(),
            GridAction::ExportValues => self.unresolved_cells(),
            // 外键条件要读齐外键的每一列；单列外键就是当前这一列。
            GridAction::Reference => match self.focus_cell() {
                Some((row, column)) => {
                    let names = self.columns.get(column).and_then(|item| self.foreign_key_for(&item.name)).map(|key| key.columns.clone());
                    match names {
                        Some(names) => names.iter().filter_map(|name| self.columns.iter().position(|item| item.name == *name)).map(|index| (row, index)).collect(),
                        None => vec![(row, column)],
                    }
                }
                None => Vec::new(),
            },
        }
    }

    /// 这些格子里还没拿到原值的读取请求；调用方一次发起，读回来后共用。
    pub fn begin_full_cells(&mut self, cells: &[(usize, usize)]) -> Vec<FullRead> {
        let mut reads = Vec::new();
        for (row, column) in cells.iter().copied() {
            if let Some(read) = self.begin_full(row, column) {
                if !reads.iter().any(|item: &FullRead| item.row_id == read.row_id && item.column_name == read.column_name) {
                    reads.push(read);
                }
            }
        }
        reads
    }

    /// 这些格子里还有没有拿不到原值的；有就返回原因，动作必须停在这里。
    pub fn cell_block(&self, cells: &[(usize, usize)]) -> Option<String> {
        for (row, column) in cells.iter().copied() {
            let row_id = self.rows.get(row).map(|item| item.id.clone());
            let column_name = self.columns.get(column).map(|item| item.name.clone());
            let (Some(row_id), Some(column_name)) = (row_id, column_name) else {
                continue;
            };
            match self.full_state(&row_id, &column_name) {
                Some(FullState::Ready(_)) => {}
                Some(FullState::Failed(message)) => return Some(message.clone()),
                Some(FullState::Loading) => return Some(i18n::text(self.locale, "grid.value.loadingFull")),
                _ => {
                    if self.needs_full(row, column) {
                        return Some(i18n::text(self.locale, "grid.value.noOriginal"));
                    }
                }
            }
            if let CellRead::Unavailable(key) = self.cell_read(row, column) {
                return Some(i18n::text(self.locale, key));
            }
        }
        None
    }

    /// 缓存只保留当前快照里还有的行；翻页和重新读取后不会留下过期原值。
    fn prune_full(&mut self) {
        if self.full.is_empty() && self.viewer.is_none() {
            return;
        }
        let ids: HashSet<&str> = self.rows.iter().chain(self.source.iter()).chain(self.retained.iter()).map(|row| row.id.as_str()).collect();
        self.full.retain(|row_id, _| ids.contains(row_id.as_str()));
        if let Some(viewer) = &self.viewer {
            if !ids.contains(viewer.row_id.as_str()) {
                self.viewer = None;
            }
        }
    }

    /// 当前表结构（引擎、模式、唯一键）；完整值读取按它定位和引用标识符。
    pub fn schema(&self) -> Option<&TableSchema> {
        self.schema.as_ref()
    }

    pub fn set_foreign_keys(&mut self, keys: Vec<ForeignKeyInfo>) {
        self.keys = keys;
    }

    pub fn foreign_keys(&self) -> &[ForeignKeyInfo] {
        &self.keys
    }

    /// 这一列属于哪个外键；不是外键列时为空，菜单据此只在适用行出现。
    pub fn foreign_key_for(&self, column: &str) -> Option<&ForeignKeyInfo> {
        self.keys.iter().find(|key| key.columns.iter().any(|name| name == column))
    }

    /// 按外键算出引用目标与条件：复合外键必须列齐，NULL、缺列和预览都拒绝。
    pub fn reference(&self, row: usize, column: usize) -> Result<Reference, &'static str> {
        if self.mode != GridMode::Table || self.schema.is_none() {
            return Err("grid.fk.needTable");
        }
        let name = self.columns.get(column).map(|item| item.name.clone()).ok_or("grid.fk.none")?;
        let key = self.foreign_key_for(&name).ok_or("grid.fk.none")?.clone();
        let mut values = Vec::new();
        let mut origins: Vec<Option<&GridColumn>> = Vec::new();
        for fk_column in &key.columns {
            let index = self.columns.iter().position(|item| item.name == *fk_column).ok_or("grid.fk.missingColumn")?;
            match self.cell_read(row, index) {
                CellRead::Ready(Some(text)) => {
                    values.push(Some(text));
                    origins.push(self.columns.get(index));
                }
                CellRead::Ready(None) => return Err("grid.fk.null"),
                CellRead::NeedsFull(_) => return Err("grid.fk.preview"),
                CellRead::Unavailable(_) => return Err("grid.fk.missingColumn"),
            }
        }
        if key.ref_columns.len() != key.columns.len() || key.ref_table.is_empty() {
            return Err("grid.fk.missingColumn");
        }
        let engine = Engine::parse(&self.target.engine).unwrap_or(Engine::Sqlite);
        // 与网格读取同样的限定方式：会话已经绑定库，引用部分只带模式（模式为空就只带表）。
        let table = if key.ref_schema.is_empty() {
            quote_ident(engine, &key.ref_table)
        } else {
            format!("{}.{}", quote_ident(engine, &key.ref_schema), quote_ident(engine, &key.ref_table))
        };
        let conditions = key
            .ref_columns
            .iter()
            .zip(values.iter())
            .zip(origins.iter())
            .map(|((ref_column, value), origin)| {
                // 值按本表这一列的类型写成字面量，和复制 WHERE 用的是同一套字面量规则。
                format!("{} = {}", quote_ident(engine, ref_column), self.literal(*origin, value.clone()))
            })
            .collect::<Vec<_>>()
            .join(" AND ");
        let list = key.ref_columns.iter().map(|column| quote_ident(engine, column)).collect::<Vec<_>>().join(", ");
        Ok(Reference {
            connection_id: self.target.connection_id.clone(),
            database: self.target.database.clone(),
            schema: key.ref_schema.clone(),
            table: key.ref_table.clone(),
            columns: key.ref_columns.clone(),
            values,
            sql: format!("SELECT {list} FROM {table} WHERE {conditions}"),
        })
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

    /// 这一格要按哪种语义画：NULL、空字符串、普通文本、大值预览各自不同。
    /// 与 `cell_label` 同一套判据（先看被拦下的暂存值，再看原值/预览信封），
    /// 界面不再拿显示文本和 "NULL" 字面量比较 —— 真正存着 "NULL" 的字符串不能被画成空值。
    pub fn cell_kind_at(&self, row: usize, column: usize) -> CellKind {
        if let Some(overlay) = self.overlay_at(row, column).filter(|item| item.kind == "update" && item.gate != Gate::Writable) {
            return match &overlay.value {
                Some(text) if text.is_empty() => CellKind::Empty,
                Some(_) => CellKind::Text,
                None => CellKind::Null,
            };
        }
        self.show_cell(row, column, false).kind
    }

    /// 当前格子的原始值，供编辑栏判断“NULL 未改动”和抽取文本。
    /// 预览信封不是原值：还没读到完整值时为空，编辑必须先读完整值。
    pub fn raw_value(&self, row: usize, column: usize) -> Option<Option<String>> {
        self.original(row, column)
    }

    /// 进入文本编辑时使用的原值：数据库 NULL 与空字符串保持不同。
    pub fn edit_text(&self, row: usize, column: usize) -> String {
        match self.original(row, column) {
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
        let Some(inserted) = self.any_row(row_id).map(|row| row.inserted) else {
            return Gate::Readonly;
        };
        let draw_row = display.unwrap_or(usize::MAX);
        if inserted {
            let message = self.validation_message(column, value.as_deref());
            let gate = if message.is_some() { Gate::Validation } else { Gate::Writable };
            self.overlays.retain(|item| !(item.kind == "update" && item.row_id == row_id && item.column_name == column_name));
            if gate == Gate::Writable {
                if let Some(origin) = self.source.iter_mut().find(|item| item.id == row_id) {
                    if let Some(slot) = Arc::make_mut(origin).values.get_mut(column) {
                        *slot = value.clone();
                    }
                }
                if let Some(slot) = display.and_then(|row| self.rows.get_mut(row)).and_then(|item| Arc::make_mut(item).values.get_mut(column)) {
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
        // 批量暂存整批回退：同批的其它格子一起去掉，被它替换的旧暂存放回来。
        if let Some(batch) = last.batch {
            self.overlays.retain(|item| item.batch != Some(batch));
            if let Some(index) = self.batches.iter().position(|(id, _)| *id == batch) {
                let (_, replaced) = self.batches.remove(index);
                // 按移除的逆序插回各自原下标，恢复批量暂存前的顺序，下一次撤销仍按原暂存顺序。
                for (position, item) in replaced.into_iter().rev() {
                    let position = position.min(self.overlays.len());
                    self.overlays.insert(position, item);
                }
            }
            self.reattach_overlays();
        }
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
            // 查询结果本身不能新增行：即使定位成功，也只读。原因要具体，不能退回一句"只读"。
            if self.readonly_reason.is_empty() {
                self.readonly_reason = "grid.readonly.queryResult".to_owned();
            }
            return Gate::Readonly;
        }
        let id = format!("i:{}", self.seq + 1);
        self.seq += 1;
        let values = vec![None; self.columns.len()];
        self.source.push(Arc::new(GridRow { id: id.clone(), values, located: false, locator: Vec::new(), inserted: true }));
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
            // 新增行上的批量值也挂在这一行上，行没了它们一起去掉。
            self.overlays.retain(|item| item.change_id.as_deref() != Some(id.as_str()) && item.row_id != id);
            self.cloned.retain(|item| item != &id);
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
        Ok(crate::grid::commands::CommitRequest {
            table: self.table_ref(),
            changes,
            // 写回前的信封判断用读出这些值时的上限，和查看/复制/导出比较的是同一个值。
            preview_bytes: self.sheet_preview_bytes as u64,
        })
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
                    // 新增行上没有提交编号的批量值已经随 INSERT 写入，不能留下来冒充未提交。
                    self.overlays.retain(|item| item.row_id != applied.row_id);
                    self.cloned.retain(|item| item != &applied.row_id);
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
        // 提交之后剩下的暂存逐条撤销：批次里已有一部分写库，不能再按整批放回旧值。
        if !report.applied.is_empty() {
            self.batches.clear();
            for item in &mut self.overlays {
                item.batch = None;
            }
        }
        // 写库成功后原值可能已经变了：缓存的完整值和查看面板显示的状态都不能再用。
        self.invalidate_previews();
        self.close_preview();
        self.recompute();
    }

    pub fn column_names(&self) -> Vec<String> {
        self.columns.iter().map(|column| column.name.clone()).collect()
    }

    /// 列的声明类型；记录视图把它写在列名旁边。
    pub fn column_type(&self, column: usize) -> Option<&str> {
        self.columns.get(column).map(|item| item.type_name.as_str())
    }

    pub fn exchange_stamp(&self) -> (u64, usize, u64, bool) {
        (self.sheet_serial, self.source.len(), self.export_revision, self.has_more_rows())
    }

    pub fn exchange_metadata(&self) -> (Option<TableRef>, Vec<String>, bool) {
        let table = if self.target.connection_id.is_empty() || self.target.table.is_empty() { None } else { Some(self.table_ref()) };
        (table, self.column_names(), self.has_more_rows())
    }

    /// 当前已读取的表、列和行。没有连接或表名时不给出导入目标。
    #[allow(clippy::type_complexity)]
    pub fn exchange_sheet(&self) -> (Option<TableRef>, Vec<String>, Vec<Vec<Option<String>>>, bool) {
        let table = if self.target.connection_id.is_empty() || self.target.table.is_empty() { None } else { Some(self.table_ref()) };
        // 暂存的新增行还没写进数据库，交换与导出都不能把它们当成已读取内容。
        // 已经读到完整值的格子交原值；仍是预览的格子交给导出前的形状检查挡住。
        let names = self.column_names();
        let rows = self
            .source
            .iter()
            .filter(|row| !row.inserted)
            .map(|row| {
                row.values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| match names.get(index) {
                        Some(name) => match self.full_state(&row.id, name) {
                            Some(FullState::Ready(full)) => full.clone(),
                            _ => value.clone(),
                        },
                        None => value.clone(),
                    })
                    .collect()
            })
            .collect();
        (table, names, rows, self.has_more_rows())
    }

    pub fn rendered_labels(&self) -> Vec<Vec<String>> {
        let (start, end) = self.visible_bounds();
        (start..end).map(|row| self.sheet_line(row)).collect()
    }

    /// 自适应列宽只保留最长的 64 项；相同字符数保持行顺序，与稳定排序后取前 64 项一致。
    pub fn fit_column_labels(&self, column: usize) -> Vec<String> {
        if column >= self.columns.len() {
            return Vec::new();
        }
        let mut candidates: Vec<(usize, String)> = Vec::with_capacity(65);
        for row in 0..self.rows.len() {
            let text = self.cell_label(row, column);
            let length = text.chars().count();
            let index = candidates.partition_point(|(other, _)| *other >= length);
            if index < 64 {
                candidates.insert(index, (length, text));
                candidates.truncate(64);
            }
        }
        candidates.into_iter().map(|(_, text)| text).collect()
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
        self.invalidate_previews();
        self.keys.clear();
        self.scroll = 0;
        self.hidden.clear();
        self.frozen = None;
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
        self.truncated = self.page_offset().saturating_add(self.source_len() as u64) < total.max(0) as u64;
    }

    pub fn set_full_count(&mut self, count: Option<i64>) { self.full_count = count.filter(|count| *count >= 0); }

    pub fn has_more_rows(&self) -> bool {
        // 是否还有页以最近一次服务器的 limit+1 探测为准，不能被过期计数驱动循环续读。
        self.truncated
    }

    /// 追加下一页。只对表数据成立；查询结果延续由查询页自己负责，网格不重新执行 SQL。
    /// 已经读到的稳定行 id 不再追加第二份，返回真正新增的行数。
    pub fn append_page(&mut self, rows: Vec<GridRow>, full_count: Option<i64>, truncated: bool) -> usize {
        if self.mode != GridMode::Table {
            return 0;
        }
        let mut added = 0;
        let mut ids: HashSet<String> = self.source.iter().map(|row| row.id.clone()).collect();
        for row in rows {
            if !ids.insert(row.id.clone()) {
                continue;
            }
            self.source.push(Arc::new(row));
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

    /// 整行复制：按焦点行的稳定 id 找到当前显示行，再按逻辑列顺序取全部列。
    /// 复制不改变选择，排序、筛选或翻页后仍对应同一行。
    pub fn copy_row(&mut self) -> String {
        self.copy_scope(CopyScope::Row, false)
    }

    pub fn copy_row_sql(&mut self) -> String {
        self.copy_scope(CopyScope::Row, true)
    }

    /// 整列复制：列按列名定位，视觉重排不改变逻辑列顺序。
    pub fn copy_column(&mut self) -> String {
        self.copy_scope(CopyScope::Column, false)
    }

    pub fn copy_column_sql(&mut self) -> String {
        self.copy_scope(CopyScope::Column, true)
    }

    /// 当前格复制：一格 TSV / 一格 SQL 字面量，与矩形选择的输入一致。
    pub fn copy_cell(&mut self) -> String {
        self.copy_scope(CopyScope::Cell, false)
    }

    pub fn copy_cell_sql(&mut self) -> String {
        self.copy_scope(CopyScope::Cell, true)
    }

    fn copy_scope(&mut self, scope: CopyScope, sql: bool) -> String {
        let Some(points) = self.scope_points(scope) else {
            return String::new();
        };
        if self.missing_full(&points) {
            return String::new();
        }
        if sql {
            self.sql_grid(&points)
        } else {
            self.tsv_grid(&points)
        }
    }

    /// 复制范围展开成显示行列：行取全部逻辑列，列取全部已读取行，格只取一格。
    fn scope_points(&self, scope: CopyScope) -> Option<Vec<(usize, usize)>> {
        let (row, column) = self.focus_cell()?;
        let points = match scope {
            CopyScope::Cell => vec![(row, column)],
            CopyScope::Row => (0..self.columns.len()).map(|column| (row, column)).collect(),
            CopyScope::Column => (0..self.rows.len()).map(|row| (row, column)).collect(),
        };
        Some(points)
    }

    /// WHERE 复制：行用稳定定位列（没有定位列时退回全部列），列用该列在选区里的
    /// 去重值集合；条件里的列名来自列身份，不依赖当前显示位置。
    pub fn copy_where(&mut self, scope: CopyScope) -> String {
        self.where_text(scope)
    }

    pub fn where_text(&self, scope: CopyScope) -> String {
        let Some((row, column)) = self.focus_cell() else {
            return String::new();
        };
        let cells = self.action_cells(match scope {
            CopyScope::Cell => GridAction::WhereCell,
            CopyScope::Row => GridAction::WhereRow,
            CopyScope::Column => GridAction::WhereColumn,
        });
        if self.missing_full(&cells) {
            return String::new();
        }
        let conditions = match scope {
            CopyScope::Cell => self.cell_conditions(row, column),
            CopyScope::Row => self.row_conditions(row),
            CopyScope::Column => self.column_conditions(column),
        };
        if conditions.is_empty() {
            return String::new();
        }
        format!("WHERE {}", conditions.join(" AND "))
    }

    fn cell_conditions(&self, row: usize, column: usize) -> Vec<String> {
        self.condition(row, column).into_iter().collect()
    }

    fn row_conditions(&self, row: usize) -> Vec<String> {
        let locator = self.rows.get(row).map(|item| item.locator.clone()).unwrap_or_default();
        if !locator.is_empty() {
            let conditions = locator
                .iter()
                .filter_map(|(name, value)| {
                    let column = self.columns.iter().position(|item| item.name == *name)?;
                    Some(self.condition_with(column, value.clone()))
                })
                .collect::<Vec<_>>();
            if !conditions.is_empty() {
                return conditions;
            }
        }
        (0..self.columns.len()).filter_map(|column| self.condition(row, column)).collect()
    }

    fn column_conditions(&self, column: usize) -> Vec<String> {
        let Some(name) = self.columns.get(column).map(|item| item.name.clone()) else {
            return Vec::new();
        };
        let mut values: Vec<String> = Vec::new();
        let mut has_null = false;
        for (row, cell_column) in self.selection() {
            if cell_column != column {
                continue;
            }
            match self.original(row, column) {
                Some(None) => has_null = true,
                Some(Some(text)) if !values.contains(&text) => values.push(text),
                _ => {}
            }
        }
        let ident = self.ident(&name);
        let origin = self.columns.get(column);
        let mut clauses = Vec::new();
        if !values.is_empty() {
            let literals = values.iter().map(|value| self.literal(origin, Some(value.clone()))).collect::<Vec<_>>();
            clauses.push(format!("{ident} IN ({})", literals.join(", ")));
        }
        if has_null {
            clauses.push(format!("{ident} IS NULL"));
        }
        if clauses.len() > 1 {
            vec![format!("({})", clauses.join(" OR "))]
        } else {
            clauses
        }
    }

    fn condition(&self, row: usize, column: usize) -> Option<String> {
        let value = self.original(row, column)?;
        Some(self.condition_with(column, value))
    }

    fn condition_with(&self, column: usize, value: Option<String>) -> String {
        let name = self.columns.get(column).map(|item| item.name.clone()).unwrap_or_default();
        let ident = self.ident(&name);
        match value {
            None => format!("{ident} IS NULL"),
            Some(text) => format!("{ident} = {}", self.literal(self.columns.get(column), Some(text))),
        }
    }

    /// 条件里的列名带表名前缀（有结构证明时），并按目标引擎引用。
    fn ident(&self, name: &str) -> String {
        let engine = Engine::parse(&self.target.engine).unwrap_or(Engine::Sqlite);
        match &self.schema {
            Some(schema) => format!("{}.{}", qualified_name(schema), quote_ident(engine, name)),
            None => quote_ident(engine, name),
        }
    }

    fn literal(&self, column: Option<&GridColumn>, value: Option<String>) -> String {
        let engine = Engine::parse(&self.target.engine).unwrap_or(Engine::Sqlite);
        sql_cell(engine, column.map(domain_column).as_ref(), &cell_of(value))
    }

    pub fn filter_focused_value(&mut self) {
        let Some((row, column)) = self.focus_cell() else {
            return;
        };
        let Some(name) = self.columns.get(column).map(|item| item.name.clone()) else {
            return;
        };
        // 按值筛选同样要原值：预览信封不能变成一条针对截断文本的条件。
        match self.original(row, column).unwrap_or(None) {
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
        self.shared_row(row_id).map(Arc::as_ref)
    }

    fn shared_row(&self, row_id: &str) -> Option<&Arc<GridRow>> {
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
            if let Some(row) = self.shared_row(&overlay.row_id) {
                if !keep.iter().any(|item: &Arc<GridRow>| item.id == row.id) {
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
            let row = Arc::make_mut(row);
            if let Some(slot) = row.values.get_mut(column) {
                *slot = value.clone();
            }
            if let Some(slot) = row.locator.iter_mut().find(|(name, _)| name == column_name) {
                slot.1 = value.clone();
            }
        }
    }

    fn recompute(&mut self) {
        self.prune_display();
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
                    Some(index) => draft_matches(row.values.get(*index).and_then(|value| value.as_deref()), self.columns.get(*index), draft),
                    None => false,
                })
            });
        }
        if let Some((column, descending)) = &self.sort {
            if let Some(index) = self.columns.iter().position(|item| item.name == *column) {
                let numeric = self.columns.get(index).is_some_and(|item| matches!(value_family(&item.type_name), ValueFamily::Integer | ValueFamily::Decimal | ValueFamily::Float));
                rows.sort_by(|left, right| {
                    let order = compare_cell(left.values.get(index).and_then(|value| value.as_deref()), right.values.get(index).and_then(|value| value.as_deref()), numeric);
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
        // 重排后不再存在的行不能留着它的完整值；查看面板也跟着关闭。
        self.prune_full();
    }

    /// 选择的锚点和焦点按行 id 找回；原来那一行真的消失时才清空选择。
    fn reattach_selection(&mut self) {
        if !self.selected {
            self.anchor_id.clear();
            self.focus_id.clear();
            self.regions.clear();
            self.minus.clear();
            self.region_drag = None;
            return;
        }
        // 区域和洞按显示行号记：重排、筛选换行后行号已经不是同一批人，
        // 无法按行身份还原，收拢成主矩形。主矩形按 id 重新定位。
        self.regions.truncate(1);
        self.minus.clear();
        self.region_drag = None;
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
        self.regions = vec![rect_of(self.anchor, self.focus)];
    }

    fn clamp_scroll(&mut self) {
        if self.rows.is_empty() {
            self.scroll = 0;
            return;
        }
        let max = self.rows.len().saturating_sub(1);
        self.scroll = self.scroll.min(max);
    }

    /// 选择展开成显示行列：各区域并起来、去掉减选的洞，按行优先排序。
    /// 复制、WHERE、批量动作和统计都读它，因此非连续选择在任何入口都是同一组格子。
    fn selection(&self) -> Vec<(usize, usize)> {
        if !self.selected || self.rows.is_empty() || self.columns.is_empty() {
            return Vec::new();
        }
        let mut points: BTreeSet<(usize, usize)> = BTreeSet::new();
        for (row_start, column_start, row_end, column_end) in self.regions.iter().copied() {
            for row in row_start..=row_end.min(self.rows.len() - 1) {
                for column in column_start..=column_end.min(self.columns.len() - 1) {
                    if !self.minus.iter().any(|hole| rect_has(*hole, row, column)) {
                        points.insert((row, column));
                    }
                }
            }
        }
        points.into_iter().collect()
    }

    /// 选区的复制块：参与的行（升序）、最小列、最大列。表头、TSV 和导出都按这个块对齐，
    /// 非连续选择里没选中的格子留空，块因此始终是矩形的。
    pub fn selection_block(&self) -> Option<(Vec<usize>, usize, usize)> {
        let points = self.selection();
        let first = *points.first()?;
        let mut rows: Vec<usize> = Vec::new();
        let mut last = usize::MAX;
        let (mut column_start, mut column_end) = (first.1, first.1);
        for (row, column) in points.iter().copied() {
            if row != last {
                rows.push(row);
                last = row;
            }
            column_start = column_start.min(column);
            column_end = column_end.max(column);
        }
        Some((rows, column_start, column_end))
    }

    /// 行内当前值（含暂存修改），按显示位置读取；越界返回 None。
    fn value_at(&self, row: usize, column: usize) -> Option<Option<String>> {
        if let Some(overlay) = self.overlay_at(row, column).filter(|item| item.kind == "update") {
            return Some(overlay.value.clone());
        }
        self.rows.get(row).and_then(|item| item.values.get(column).cloned())
    }

    /// 复制、导出和 WHERE 用的原值：已读到完整值时用它，预览信封绝不当原值。
    fn original(&self, row: usize, column: usize) -> Option<Option<String>> {
        if self.needs_full(row, column) {
            return self.full_text_at(row, column);
        }
        self.value_at(row, column)
    }

    /// 这些格子里还有只是预览、没有原值的：动作必须停下来先读完整值。
    fn missing_full(&self, points: &[(usize, usize)]) -> bool {
        points
            .iter()
            .any(|(row, column)| self.needs_full(*row, *column) && self.full_text_at(*row, *column).is_none())
    }

    fn tsv_field(&self, row: usize, column: usize) -> String {
        match self.original(row, column) {
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

    /// 暂存前的写回校验和查看/复制/WHERE/导出/commit 用同一个上限：
    /// 上限来自读出这份行列时的 preview_kb，协议形状但确实没被截断的文本因此还能保存。
    fn validation_message(&self, column: usize, value: Option<&str>) -> Option<String> {
        let schema = self.schema.as_ref()?;
        let name = self.columns.get(column)?.name.as_str();
        let origin = schema.columns.iter().find(|item| item.name == name)?;
        crate::grid::reject_literal_with(origin, &cell_of(value.map(str::to_owned)), false, self.sheet_preview_bytes)
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
                            if value.is_none() && self.unassigned(&row.id, column) {
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
                if value.is_none() && self.unassigned(&row.id, column) {
                    return None;
                }
                Some((column.name.clone(), cell_of(value)))
            })
            .collect()
    }

    /// 新增行里留空就不写进 INSERT 的列：自增列总是如此；"复制为新行"的主键列也一样，
    /// 由数据库给值或报错，而不是显式写一个 NULL 主键。
    fn unassigned(&self, row_id: &str, column: &GridColumn) -> bool {
        column.auto || (column.primary && self.cloned.iter().any(|item| item == row_id))
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

/// 列显示（隐藏、冻结、自适应）。只改变画哪些列、哪些列固定在左侧；
/// 复制、导出、筛选和排序一律按逻辑列，数据范围不变。
impl SharedGrid {
    /// 可见的逻辑列（按逻辑顺序）。界面的第 n 个数据列就是这里的第 n 项。
    pub fn visible_columns(&self) -> Vec<usize> {
        let visible: Vec<usize> = (0..self.columns.len()).filter(|index| !self.hidden.contains(&self.columns[*index].name)).collect();
        // 至少保留一个可见列；万一列变了导致全部被藏，就全部显示。
        if visible.is_empty() {
            (0..self.columns.len()).collect()
        } else {
            visible
        }
    }

    pub fn column_hidden(&self, column: usize) -> bool {
        self.columns.get(column).is_some_and(|item| self.hidden.contains(&item.name)) && self.visible_columns().len() < self.columns.len()
    }

    /// 只剩一列可见时不能再藏。
    pub fn can_hide(&self, column: usize) -> bool {
        !self.column_hidden(column) && column < self.columns.len() && self.visible_columns().len() > 1
    }

    pub fn hide_column(&mut self, column: usize) -> bool {
        if !self.can_hide(column) {
            return false;
        }
        let name = self.columns[column].name.clone();
        self.hidden.insert(name.clone());
        if self.frozen.as_deref() == Some(name.as_str()) {
            // 冻结边界列被藏起来时，边界退到它左边最近的可见列。
            self.frozen = self.visible_columns().into_iter().rfind(|index| *index < column).map(|index| self.columns[index].name.clone());
        }
        self.keep_selection_visible();
        true
    }

    pub fn show_column(&mut self, column: usize) -> bool {
        let Some(name) = self.columns.get(column).map(|item| item.name.clone()) else {
            return false;
        };
        self.hidden.remove(&name)
    }

    pub fn show_all_columns(&mut self) -> bool {
        let changed = !self.hidden.is_empty();
        self.hidden.clear();
        changed
    }

    /// 当前隐藏的列（逻辑列号、列名），表头菜单用它列出可恢复的列。
    pub fn hidden_columns(&self) -> Vec<(usize, String)> {
        (0..self.columns.len()).filter(|index| self.column_hidden(*index)).map(|index| (index, self.columns[index].name.clone())).collect()
    }

    pub fn freeze_to(&mut self, column: usize) -> bool {
        if self.column_hidden(column) {
            return false;
        }
        let Some(name) = self.columns.get(column).map(|item| item.name.clone()) else {
            return false;
        };
        self.frozen = Some(name);
        true
    }

    pub fn unfreeze(&mut self) -> bool {
        self.frozen.take().is_some()
    }

    /// 冻结边界的逻辑列。
    pub fn frozen_column(&self) -> Option<usize> {
        let name = self.frozen.as_deref()?;
        self.columns.iter().position(|item| item.name == name)
    }

    /// 前多少个可见数据列固定在左侧（不含行号列）。
    pub fn frozen_count(&self) -> usize {
        match self.frozen_column() {
            Some(frozen) => self.visible_columns().iter().filter(|index| **index <= frozen).count(),
            None => 0,
        }
    }

    /// 界面第 n 个数据列对应的逻辑列。
    pub fn display_to_logical(&self, display: usize) -> Option<usize> {
        self.visible_columns().get(display).copied()
    }

    /// 逻辑列在界面上是第几个数据列；被藏起来时没有位置。
    pub fn logical_to_display(&self, logical: usize) -> Option<usize> {
        self.visible_columns().iter().position(|index| *index == logical)
    }

    /// 列变了（新结果、列被删）时丢掉不再存在的隐藏和冻结设置。
    fn prune_display(&mut self) {
        let names: BTreeSet<String> = self.columns.iter().map(|item| item.name.clone()).collect();
        self.hidden.retain(|name| names.contains(name));
        if !self.columns.is_empty() && self.hidden.len() >= names.len() {
            self.hidden.clear();
        }
        if self.frozen.as_ref().is_some_and(|name| !names.contains(name)) {
            self.frozen = None;
        }
    }

    /// 选择的焦点或锚点落在被藏起来的列上时，挪到最近的可见列。
    fn keep_selection_visible(&mut self) {
        let visible = self.visible_columns();
        let nearest = |column: usize| -> usize {
            visible.iter().copied().find(|index| *index >= column).or_else(|| visible.last().copied()).unwrap_or(column)
        };
        if self.column_hidden(self.focus.1) {
            self.focus.1 = nearest(self.focus.1);
        }
        if self.column_hidden(self.anchor.1) {
            self.anchor.1 = nearest(self.anchor.1);
        }
        // 区域的列号也一起挪；挪不动的收拢成主矩形，免得高亮落在被藏起来的列上。
        if !self.regions.is_empty() {
            self.regions = vec![rect_of(self.anchor, self.focus)];
            self.minus.clear();
            self.region_drag = None;
        }
    }
}

/// 多格式复制。选区按逻辑行列拼成一张表（隐藏列也在内），再交给 export::write 写成各格式；
/// 和普通复制一样先要原值，预览信封绝不进剪贴板。
impl SharedGrid {
    pub fn copy_as(&self, format: CopyFormat) -> Result<CopiedText, CopyBlock> {
        let Some((block_rows, column_start, column_end)) = self.selection_block() else {
            return Err(CopyBlock::Empty);
        };
        let points = self.selection();
        if points.is_empty() {
            return Err(CopyBlock::Empty);
        }
        if self.missing_full(&points) {
            return Err(CopyBlock::Missing);
        }
        let columns: Vec<usize> = (column_start..=column_end).collect();
        let names: Vec<String> = columns.iter().map(|index| self.columns[*index].name.clone()).collect();
        // 非连续选择按块导出：没选中的格子留空，列名仍然覆盖整个块。
        let rows: Vec<Vec<Option<String>>> = block_rows
            .iter()
            .map(|row| {
                columns
                    .iter()
                    .map(|column| if self.is_selected(*row, *column) { self.original(*row, *column).unwrap_or(None) } else { None })
                    .collect()
            })
            .collect();
        let table = crate::export::ParsedTable { columns: names.clone(), rows };
        let render = |name: &str, null_token: &str| -> String {
            crate::export::render(name, &table, ',', null_token, false, "", "").map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default()
        };
        let mut placeholder = false;
        let text = match format {
            CopyFormat::TsvHeader => {
                let header = names.iter().map(|name| quote_tsv(name, false)).collect::<Vec<_>>().join("\t");
                format!("{header}\n{}", self.tsv_grid(&points))
            }
            CopyFormat::Csv => render("csv", &self.null_copy),
            CopyFormat::Json => render("json", ""),
            CopyFormat::Markdown => render("markdown", "NULL"),
            CopyFormat::Insert => {
                let engine = Engine::parse(&self.target.engine).unwrap_or(Engine::Sqlite);
                let (schema, table_name) = match (&self.schema, self.mode) {
                    (_, GridMode::Table) if !self.target.table.is_empty() => (self.target.schema.clone(), self.target.table.clone()),
                    // 查询结果只有在证明是单表并读到结构后才用真实表名。
                    (Some(schema), GridMode::Query) => (schema.schema.clone(), schema.name.clone()),
                    _ => {
                        placeholder = true;
                        (String::new(), PLACEHOLDER_TABLE.to_owned())
                    }
                };
                let origins: Vec<Column> = columns.iter().map(|index| domain_column(&self.columns[*index])).collect();
                crate::export::render_insert_for(engine, &schema, &table_name, &origins, &table)
            }
        };
        Ok(CopiedText { text, placeholder })
    }
}

/// INSERT 复制在查询结果没有定位到表时用的占位表名。
pub const PLACEHOLDER_TABLE: &str = "table_name";

const COLUMN_MIN_WIDTH: f32 = 48.0;
const COLUMN_MAX_WIDTH: f32 = 640.0;

/// 批量写入：复制为新行、粘贴 TSV、选区设值。全部只进暂存，写库仍走预览→确认→提交。
impl SharedGrid {
    /// 唯一的批量暂存入口：先把整批格子逐一过写入门控和校验，任何一格不行就整批拒绝，
    /// 不写覆盖层、不动已有暂存；全部通过才逐格暂存，同一批共用一个编号，撤销时整批回退。
    pub fn stage_batch(&mut self, cells: Vec<(String, usize, Option<String>)>) -> Result<usize, BatchReject> {
        if cells.is_empty() {
            return Err(BatchReject::NoFocus);
        }
        if !self.writable() {
            return Err(BatchReject::Readonly(self.gate_text(Gate::Readonly)));
        }
        for (row_id, column, value) in &cells {
            let Some(origin) = self.columns.get(*column) else {
                return Err(BatchReject::OutOfRange);
            };
            let Some(row) = self.any_row(row_id) else {
                return Err(BatchReject::OutOfRange);
            };
            if origin.primary || origin.auto {
                return Err(BatchReject::KeyColumn(origin.name.clone()));
            }
            if !origin.editable {
                return Err(BatchReject::Blocked { column: origin.name.clone(), reason: self.gate_text(Gate::Readonly) });
            }
            if row.inserted {
                if let Some(message) = self.validation_message(*column, value.as_deref()) {
                    return Err(BatchReject::Blocked { column: origin.name.clone(), reason: message });
                }
                continue;
            }
            match self.gate_for_edit(self.row_index(row_id), *column, value.as_deref()) {
                Gate::Writable => {}
                Gate::Validation => {
                    let reason = self.validation_message(*column, value.as_deref()).unwrap_or_else(|| self.gate_text(Gate::Validation));
                    return Err(BatchReject::Blocked { column: origin.name.clone(), reason });
                }
                other => return Err(BatchReject::Blocked { column: origin.name.clone(), reason: self.gate_text(other) }),
            }
        }
        self.seq += 1;
        let batch = self.seq;
        let mut replaced = Vec::new();
        let count = cells.len();
        for (row_id, column, value) in cells {
            let column_name = self.columns[column].name.clone();
            let inserted = self.any_row(&row_id).is_some_and(|row| row.inserted);
            // 同一格已有的暂存被这批替换；记下来，撤销整批时放回去。
            let mut index = 0;
            while index < self.overlays.len() {
                let item = &self.overlays[index];
                if item.kind == "update" && item.row_id == row_id && item.column_name == column_name {
                    replaced.push((index, self.overlays.remove(index)));
                } else {
                    index += 1;
                }
            }
            let draw_row = self.row_index(&row_id).unwrap_or(usize::MAX);
            // 新增行上的值不单独提交，由那条 INSERT 带上；已有行才各自形成一条 UPDATE。
            let change_id = if inserted {
                None
            } else {
                self.seq += 1;
                Some(format!("u:{}", self.seq))
            };
            let mut item = overlay(draw_row, row_id, column, column_name, value, Gate::Writable, String::new(), change_id, "update");
            item.batch = Some(batch);
            self.overlays.push(item);
        }
        self.batches.push((batch, replaced));
        self.close_preview();
        Ok(count)
    }

    /// 从焦点格开始按行列铺开剪贴板里的 TSV。只铺到可见列和已加载行上，越界就整次拒绝，不新增行。
    pub fn paste_tsv(&mut self, text: Option<&str>) -> Result<usize, BatchReject> {
        let Some(text) = text.filter(|text| !text.trim_matches(['\r', '\n']).is_empty()) else {
            return Err(BatchReject::Empty);
        };
        if !self.writable() {
            return Err(BatchReject::Readonly(self.gate_text(Gate::Readonly)));
        }
        let Some((anchor_row, anchor_column)) = self.focus_cell() else {
            return Err(BatchReject::NoFocus);
        };
        let block = parse_tsv(text, &self.null_copy);
        let visible = self.visible_columns();
        let Some(start) = visible.iter().position(|index| *index == anchor_column) else {
            return Err(BatchReject::NoFocus);
        };
        let mut cells = Vec::new();
        for (offset, line) in block.into_iter().enumerate() {
            let Some(row_id) = self.rows.get(anchor_row + offset).map(|row| row.id.clone()) else {
                return Err(BatchReject::OutOfRange);
            };
            for (step, value) in line.into_iter().enumerate() {
                let Some(column) = visible.get(start + step).copied() else {
                    return Err(BatchReject::OutOfRange);
                };
                cells.push((row_id.clone(), column, value));
            }
        }
        self.stage_batch(cells)
    }

    /// 选区里的每个可见格子暂存为同一个值（None 是数据库 NULL）。隐藏列不在写入范围里。
    pub fn stage_selection(&mut self, value: Option<String>) -> Result<usize, BatchReject> {
        if !self.writable() {
            return Err(BatchReject::Readonly(self.gate_text(Gate::Readonly)));
        }
        let cells = self.selection_targets().into_iter().map(|(row_id, column)| (row_id, column, value.clone())).collect::<Vec<_>>();
        if cells.is_empty() {
            return Err(BatchReject::NoFocus);
        }
        self.stage_batch(cells)
    }

    /// 选区里会被设值的格子（稳定行 id、逻辑列）；不含隐藏列。
    pub fn selection_targets(&self) -> Vec<(String, usize)> {
        self.selection()
            .into_iter()
            .filter(|(_, column)| !self.column_hidden(*column))
            .filter_map(|(row, column)| self.rows.get(row).map(|item| (item.id.clone(), column)))
            .collect()
    }

    /// 按焦点行的值暂存一条新行：主键和自增列留空，INSERT 里不写它们；其余列与原行一致。
    /// 原行里还是预览的格子要先读到原值，否则返回原因，不暂存。
    pub fn stage_clone(&mut self) -> Result<(), String> {
        if !self.writable() {
            return Err(self.gate_text(Gate::Readonly));
        }
        if self.mode == GridMode::Query {
            // 和“新增行”一致：定位成功的查询结果也不新增行，说明具体原因但不改写只读状态。
            return Err(i18n::text(self.locale, "grid.readonly.queryResult"));
        }
        let Some((row, _)) = self.focus_cell() else {
            return Err(i18n::text(self.locale, "grid.batch.noRow"));
        };
        let mut values = Vec::with_capacity(self.columns.len());
        for (column, origin) in self.columns.iter().enumerate() {
            if origin.primary || origin.auto {
                values.push(None);
                continue;
            }
            match self.original(row, column) {
                Some(value) => values.push(value),
                None => return Err(i18n::text(self.locale, "grid.value.noOriginal")),
            }
        }
        self.seq += 1;
        let id = format!("i:{}", self.seq);
        self.source.push(Arc::new(GridRow { id: id.clone(), values, located: false, locator: Vec::new(), inserted: true }));
        self.cloned.push(id.clone());
        self.recompute();
        let row = self.rows.iter().position(|item| item.id == id).unwrap_or(self.rows.len());
        self.overlays.push(overlay(row, id.clone(), 0, String::new(), None, Gate::Writable, String::new(), Some(id), "insert"));
        self.close_preview();
        Ok(())
    }

    /// 暂存里有多少条将要进预览的变更（按提交编号计）。
    pub fn staged_count(&self) -> usize {
        self.overlays.iter().filter(|item| item.gate == Gate::Writable && item.change_id.is_some()).count()
    }

    /// 新增行、修改格、删除行分开计数。只算能进预览的暂存。
    pub fn staged_summary(&self) -> StagedSummary {
        let mut inserts = BTreeSet::new();
        let mut updates = BTreeSet::new();
        let mut deletes = BTreeSet::new();
        for item in &self.overlays {
            if item.gate != Gate::Writable || item.change_id.is_none() {
                continue;
            }
            match item.kind {
                "insert" => {
                    inserts.insert(item.row_id.clone());
                }
                "delete" => {
                    deletes.insert(item.row_id.clone());
                }
                "update" if !item.column_name.is_empty() => {
                    updates.insert((item.row_id.clone(), item.column_name.clone()));
                }
                _ => {}
            }
        }
        StagedSummary { inserts: inserts.len(), updates: updates.len(), deletes: deletes.len() }
    }

    /// 这一格相对已读取原值的暂存状态。删除盖过修改，新增行整行都算新增。
    pub fn cell_dirty_state(&self, row: usize, column: usize) -> CellDirty {
        let Some(item) = self.rows.get(row) else {
            return CellDirty::Clean;
        };
        if self.overlays.iter().any(|overlay| overlay.kind == "delete" && overlay.row_id == item.id && overlay.gate == Gate::Writable) {
            return CellDirty::Deleted;
        }
        if item.inserted {
            return CellDirty::Inserted;
        }
        if self.overlay_at(row, column).is_some_and(|overlay| overlay.kind == "update" && overlay.gate == Gate::Writable) {
            return CellDirty::Updated;
        }
        CellDirty::Clean
    }

    /// 去掉这一格的暂存修改。新增行和删除标记不动。
    pub fn revert_cell(&mut self, row_id: &str, column: usize) -> bool {
        let Some(name) = self.columns.get(column).map(|item| item.name.clone()) else {
            return false;
        };
        let before = self.overlays.len();
        self.overlays.retain(|item| !(item.kind == "update" && item.row_id == row_id && item.column_name == name));
        if self.overlays.len() == before {
            return false;
        }
        self.close_preview();
        true
    }

    /// 去掉这一行上的全部暂存。新增行连同行一起丢掉。
    pub fn revert_row(&mut self, row_id: &str) -> bool {
        let inserted = self.any_row(row_id).is_some_and(|row| row.inserted) || self.overlays.iter().any(|item| item.kind == "insert" && item.row_id == row_id);
        let before = self.overlays.len();
        if inserted {
            self.source.retain(|item| item.id != row_id);
            self.overlays.retain(|item| item.row_id != row_id);
            self.cloned.retain(|item| item != row_id);
            self.recompute();
        } else {
            self.overlays.retain(|item| item.row_id != row_id);
            if self.overlays.len() == before {
                return false;
            }
        }
        self.close_preview();
        true
    }

    /// 丢掉全部暂存，包括还没写库的新增行。
    pub fn discard_all_staged(&mut self) {
        let inserted: Vec<String> = self
            .overlays
            .iter()
            .filter(|item| item.kind == "insert")
            .map(|item| item.row_id.clone())
            .chain(self.source.iter().filter(|row| row.inserted).map(|row| row.id.clone()))
            .collect();
        self.overlays.clear();
        self.batches.clear();
        if !inserted.is_empty() {
            self.source.retain(|item| !inserted.iter().any(|id| id == &item.id));
            self.cloned.retain(|item| !inserted.iter().any(|id| id == item));
            self.recompute();
        }
        self.close_preview();
    }

    /// 焦点之后的下一处暂存格；到末尾则回到第一处。
    pub fn next_staged_cell(&self) -> Option<(usize, usize)> {
        let mut cells = Vec::new();
        for item in &self.overlays {
            if item.gate != Gate::Writable || item.change_id.is_none() {
                continue;
            }
            let Some(row) = self.rows.iter().position(|row| row.id == item.row_id) else {
                continue;
            };
            let column = if item.kind == "update" {
                self.columns.iter().position(|column| column.name == item.column_name).unwrap_or(item.column)
            } else {
                self.visible_columns().first().copied().unwrap_or(0)
            };
            cells.push((row, column));
        }
        cells.sort_unstable();
        cells.dedup();
        let focus = self.focus_cell();
        if let Some(focus) = focus {
            if let Some(next) = cells.iter().copied().find(|cell| *cell > focus) {
                return Some(next);
            }
        }
        cells.first().copied()
    }

    /// 按焦点行生成 `SELECT * ... WHERE`。条件不完整或还是预览时为空，不把信封写进语句。
    pub fn select_statement(&self) -> String {
        let where_sql = self.where_text(CopyScope::Row);
        if where_sql.is_empty() || where_sql.contains('\u{1}') {
            return String::new();
        }
        format!("SELECT * FROM {} {where_sql}", self.statement_table())
    }

    /// 按焦点行生成 `UPDATE ... SET ... WHERE`。有暂存修改时 SET 用暂存值，否则用焦点格当前值。
    pub fn update_statement(&self) -> String {
        let Some((row, column)) = self.focus_cell() else {
            return String::new();
        };
        let where_sql = self.where_text(CopyScope::Row);
        if where_sql.is_empty() || where_sql.contains('\u{1}') {
            return String::new();
        }
        let row_id = self.rows.get(row).map(|item| item.id.clone()).unwrap_or_default();
        let engine = Engine::parse(&self.target.engine).unwrap_or(Engine::Sqlite);
        let mut sets = Vec::new();
        for item in &self.overlays {
            if item.kind != "update" || item.row_id != row_id || item.gate != Gate::Writable || item.column_name.is_empty() {
                continue;
            }
            if item.value.as_deref().is_some_and(|text| text.contains('\u{1}')) {
                return String::new();
            }
            let index = self.columns.iter().position(|column| column.name == item.column_name);
            let literal = self.literal(index.and_then(|index| self.columns.get(index)), item.value.clone());
            sets.push(format!("{} = {literal}", quote_ident(engine, &item.column_name)));
        }
        if sets.is_empty() {
            if self.needs_full(row, column) {
                return String::new();
            }
            let value = self.value_at(row, column).unwrap_or(None);
            if value.as_deref().is_some_and(|text| text.contains('\u{1}')) {
                return String::new();
            }
            let name = self.columns.get(column).map(|item| item.name.clone()).unwrap_or_default();
            let literal = self.literal(self.columns.get(column), value);
            sets.push(format!("{} = {literal}", quote_ident(engine, &name)));
        }
        format!("UPDATE {} SET {} {where_sql}", self.statement_table(), sets.join(", "))
    }

    fn statement_table(&self) -> String {
        if let Some(schema) = &self.schema {
            return qualified_name(schema);
        }
        let engine = Engine::parse(&self.target.engine).unwrap_or(Engine::Sqlite);
        if self.target.table.is_empty() {
            quote_ident(engine, PLACEHOLDER_TABLE)
        } else if self.target.schema.is_empty() {
            quote_ident(engine, &self.target.table)
        } else {
            format!("{}.{}", quote_ident(engine, &self.target.schema), quote_ident(engine, &self.target.table))
        }
    }

    /// 这一格还能不能按主键把预览读成原值。
    pub fn can_reread(&self, row: usize) -> bool {
        self.rows.get(row).is_some_and(|item| !item.inserted && item.located && !item.locator.is_empty())
    }

    /// 预览信封声明的原字节数。不是信封时为空。
    pub fn preview_total(&self, row: usize, column: usize) -> Option<u64> {
        let text = self.value_at(row, column).flatten()?;
        let type_name = self.column_type(column).unwrap_or("");
        match crate::grid::preview_envelope(&text, type_name, self.sheet_preview_bytes)? {
            crate::grid::PreviewValue::Binary { bytes } | crate::grid::PreviewValue::Text { bytes, .. } => Some(bytes),
        }
    }
}

/// 新增、修改、删除分开的暂存计数。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StagedSummary {
    pub inserts: usize,
    pub updates: usize,
    pub deletes: usize,
}

/// 格子相对已读取原值的暂存标记。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellDirty {
    Clean,
    Inserted,
    Updated,
    Deleted,
}

/// 表头要画的类型缩写、主键和外键。缩写是固定短码，不进词典。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnHeader {
    pub abbr: String,
    pub primary: bool,
    pub foreign: bool,
}

impl SharedGrid {
    pub fn column_header(&self, column: usize) -> Option<ColumnHeader> {
        let item = self.columns.get(column)?;
        Some(ColumnHeader {
            abbr: type_abbr(&item.type_name),
            primary: item.primary,
            foreign: self.foreign_key_for(&item.name).is_some(),
        })
    }
}

/// 字段类型缩成表头和记录徽标用的短码。
pub fn type_abbr(type_name: &str) -> String {
    let lower = type_name.trim().to_ascii_lowercase();
    let base = lower.split(['(', ' ']).next().unwrap_or("");
    match base {
        "int" | "integer" | "int2" | "int4" | "int8" | "bigint" | "smallint" | "tinyint" | "serial" | "bigserial" | "mediumint" | "smallserial" | "year" => "INT",
        "json" | "jsonb" => "JSON",
        "xml" => "XML",
        "bool" | "boolean" | "bit" => "BOOL",
        "blob" | "bytea" | "binary" | "varbinary" | "image" | "raw" | "byte" => "BLOB",
        "timestamp" | "timestamptz" | "datetime" | "datetime2" | "datetimeoffset" | "smalldatetime" => "TS",
        "date" => "DATE",
        "time" | "timetz" => "TIME",
        "text" | "varchar" | "nvarchar" | "char" | "nchar" | "clob" | "string" | "citext" | "tinytext" | "mediumtext" | "longtext" | "varchar2" | "nvarchar2" | "ntext" | "bpchar" | "name" | "uuid" | "uniqueidentifier" => {
            if base == "uuid" || base == "uniqueidentifier" {
                "UUID"
            } else {
                "TEXT"
            }
        }
        "float" | "float4" | "float8" | "double" | "real" => "FLOAT",
        "numeric" | "decimal" | "money" | "smallmoney" | "number" => "NUM",
        "" => "",
        other => {
            let short: String = other.chars().filter(|ch| ch.is_ascii_alphanumeric()).take(4).collect();
            return short.to_ascii_uppercase();
        }
    }
    .to_owned()
}

/// 把 TSV 文本拆成行列。引号包住的字段可以含制表符和换行，两个引号是一个引号；
/// 没加引号、正好等于 NULL 复制标记（非空时）的字段是数据库 NULL——和本网格复制出的 TSV 互为逆。
/// 行尾的 \r 去掉，末尾一个换行不算新的一行。
fn parse_tsv(text: &str, null_token: &str) -> Vec<Vec<Option<String>>> {
    let text = text.strip_suffix('\n').map(|rest| rest.strip_suffix('\r').unwrap_or(rest)).unwrap_or(text);
    let mut lines: Vec<Vec<Option<String>>> = Vec::new();
    let mut line: Vec<Option<String>> = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        // 一个字段。
        let mut field = String::new();
        let quoted = chars.peek() == Some(&'"');
        if quoted {
            chars.next();
            while let Some(ch) = chars.next() {
                if ch == '"' {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        break;
                    }
                } else {
                    field.push(ch);
                }
            }
            // 引号后面到分隔符之前的内容原样接上，不丢字。
            while let Some(ch) = chars.peek().copied() {
                if ch == '\t' || ch == '\n' {
                    break;
                }
                chars.next();
                if ch != '\r' {
                    field.push(ch);
                }
            }
        } else {
            while let Some(ch) = chars.peek().copied() {
                if ch == '\t' || ch == '\n' {
                    break;
                }
                chars.next();
                field.push(ch);
            }
            if field.ends_with('\r') {
                field.pop();
            }
        }
        let value = if !quoted && !null_token.is_empty() && field == null_token { None } else { Some(field) };
        line.push(value);
        match chars.next() {
            Some('\t') => continue,
            Some('\n') => lines.push(std::mem::take(&mut line)),
            _ => {
                lines.push(line);
                break;
            }
        }
    }
    lines
}

pub fn operation_banner(target: &GridTarget) -> String {
    let object = if target.schema.is_empty() {
        format!("{}.{}", target.database, target.table)
    } else {
        format!("{}.{}.{}", target.database, target.schema, target.table)
    };
    format!("{} · {} · {} · {}", target.connection_name, target.environment, object, target.engine)
}

/// 单表 SELECT 的结构证明。拒绝时返回具体原因键：联接、聚合、多语句、括号（子查询/派生表）
/// 和多表各有各的说法，界面直接显示这一条，而不是笼统的"只读"。
pub fn prove_single_table(engine: &str, sql: &str) -> Result<TableProof, &'static str> {
    let report = split_sql(engine, sql);
    if report.refusal.is_some() {
        return Err("grid.readonly.unparsed");
    }
    let pieces = report.pieces.into_iter().map(|piece| piece.text.trim().to_owned()).filter(|text| !text.is_empty()).collect::<Vec<_>>();
    if pieces.len() != 1 {
        return Err("grid.readonly.multiStatement");
    }
    let statement = pieces[0].trim().trim_end_matches(';').trim();
    let upper = statement.to_ascii_uppercase();
    if !upper.starts_with("SELECT") {
        return Err("grid.readonly.notSelect");
    }
    let padded = format!(" {} ", upper);
    if padded.contains(" JOIN ") {
        return Err("grid.readonly.join");
    }
    if padded.contains(" UNION ") || padded.contains(" INTERSECT ") || padded.contains(" EXCEPT ") {
        return Err("grid.readonly.setOperation");
    }
    // 聚合：显式分组，或结果里直接有聚合函数。括号里只有函数调用，不该被当成子查询。
    let aggregating = padded.contains(" GROUP ") || padded.contains(" HAVING ") || padded.contains(" WINDOW ") || has_aggregate_call(&upper);
    if aggregating {
        return Err("grid.readonly.aggregate");
    }
    if padded.contains(" INTO ") {
        return Err("grid.readonly.notSelect");
    }
    if statement.contains('(') {
        return Err("grid.readonly.parentheses");
    }
    let marker = match upper.find(" FROM ") {
        Some(index) => index,
        None => return Err("grid.readonly.notSelect"),
    };
    // FROM 之后、下一个子句之前出现逗号就是多表（隐式联接），不是一张表的定位范围。
    let tail = &upper[marker + " FROM ".len()..];
    let cut = [" WHERE ", " GROUP ", " HAVING ", " ORDER ", " LIMIT ", " OFFSET ", " FOR ", " UNION ", " WINDOW "]
        .iter()
        .filter_map(|word| tail.find(word))
        .min()
        .unwrap_or(tail.len());
    if tail[..cut].contains(',') {
        return Err("grid.readonly.multiTable");
    }
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
        _ => Err("grid.readonly.multiTable"),
    }
}

/// 结果里直接调用聚合函数：COUNT/SUM/AVG/MIN/MAX 后面紧跟括号，且前面不是标识符的一部分。
fn has_aggregate_call(upper: &str) -> bool {
    ["COUNT", "SUM", "AVG", "MIN", "MAX"].iter().any(|name| {
        let mut rest = upper;
        while let Some(index) = rest.find(name) {
            let before = rest[..index].chars().next_back();
            let after = rest[index + name.len()..].trim_start();
            if !before.is_some_and(|ch| ch.is_alphanumeric() || ch == '_') && after.starts_with('(') {
                return true;
            }
            rest = &rest[index + name.len()..];
        }
        false
    })
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
        return ShownCell { kind: CellKind::Unset, label: i18n::text(locale, "grid.value.unsetLabel"), body: String::new(), preview: false };
    };
    let Some(text) = value else {
        return ShownCell { kind: CellKind::Null, label: "NULL".to_owned(), body: String::new(), preview: false };
    };
    if text.is_empty() {
        return ShownCell { kind: CellKind::Empty, label: i18n::text(locale, "grid.value.emptyLabel"), body: String::new(), preview: false };
    }
    // 只有按列类型与预览上限真的会被截断的形态才算信封；形状相同的普通文本仍是原值。
    if let Some(envelope) = crate::grid::preview_envelope(&text, type_name, preview_bytes) {
        return match envelope {
            crate::grid::PreviewValue::Binary { bytes } => ShownCell {
                kind: CellKind::Binary,
                label: format!("{} {bytes}", i18n::text(locale, "grid.value.binaryLabel")),
                body: String::new(),
                preview: true,
            },
            crate::grid::PreviewValue::Text { bytes, preview } => ShownCell {
                kind: CellKind::Large,
                label: format!("{} {bytes}", i18n::text(locale, "grid.value.largeLabel")),
                body: if opened { preview } else { String::new() },
                preview: true,
            },
        };
    }
    if matches!(value_family(type_name), ValueFamily::Binary) {
        return ShownCell {
            kind: CellKind::Binary,
            label: format!("{} {}", i18n::text(locale, "grid.value.binaryLabel"), text.len() / 2),
            body: if opened { text } else { String::new() },
            preview: false,
        };
    }
    if text.len() > preview_bytes.max(1) && !opened {
        return ShownCell {
            kind: CellKind::Large,
            label: format!("{} {}", i18n::text(locale, "grid.value.largeLabel"), text.len()),
            body: String::new(),
            preview: false,
        };
    }
    ShownCell { kind: CellKind::Text, label: text.clone(), body: text, preview: false }
}

fn editable_names(schema: &TableSchema, columns: &[String], proof: &TableProof) -> Vec<String> {
    let _ = proof;
    columns.iter().filter(|name| schema.columns.iter().any(|column| column.name == name.as_str())).cloned().collect()
}

/// 本地在已读取行上复算同一条条件；NULL 不参与相等、范围和集合的命中，与 SQL 语义一致。
fn draft_matches(value: Option<&str>, column: Option<&GridColumn>, draft: &FilterDraft) -> bool {
    let numeric = column.is_some_and(|item| matches!(value_family(&item.type_name), ValueFamily::Integer | ValueFamily::Decimal | ValueFamily::Float));
    match draft.op.as_str() {
        "null" => value.is_none(),
        "notnull" => value.is_some(),
        "empty" => value == Some(""),
        "contains" => value.is_some_and(|text| text.to_lowercase().contains(&draft.value.to_lowercase())),
        "starts" => value.is_some_and(|text| text.to_lowercase().starts_with(&draft.value.to_lowercase())),
        "eq" => value == Some(draft.value.as_str()),
        // SQL 里 NULL <> 值 不成立，因此 NULL 不算命中"不等于"。
        "ne" => value.is_some() && value != Some(draft.value.as_str()),
        "gt" | "lt" => {
            let Some(text) = value else {
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
            let Some(text) = value else {
                return false;
            };
            compare_cell(Some(text), Some(draft.value.as_str()), numeric).is_ge()
                && compare_cell(Some(text), Some(draft.second.as_str()), numeric).is_le()
        }
        "in" => match value {
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
    Overlay { row, row_id, column, column_name, value, gate, message, change_id, kind, batch: None }
}

pub(crate) fn cell_of(value: Option<String>) -> Cell {
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

    // 旧统计路径用于语义对照和显式基准：按 id 找行，并复制值/完整值及未读行 id。
    fn legacy_unresolved_source_cells(grid: &SharedGrid) -> Vec<(String, usize)> {
        let mut cells = Vec::new();
        for row in grid.source.iter().filter(|row| !row.inserted) {
            for (column, origin) in grid.columns.iter().enumerate() {
                let value = grid.overlays.iter()
                    .find(|item| item.row_id == row.id && item.column_name == origin.name && item.kind == "update")
                    .map(|item| item.value.clone())
                    .or_else(|| grid.any_row(&row.id).and_then(|item| item.values.get(column).cloned()));
                if value.flatten().is_some_and(|text| crate::grid::preview_envelope(&text, &origin.type_name, grid.sheet_preview_bytes).is_some())
                    && grid.full_text_of(&row.id, column).is_none()
                {
                    cells.push((row.id.clone(), column));
                }
            }
        }
        cells
    }

    #[test]
    fn end_of_page_stops_continuation_even_when_a_cached_count_is_stale() {
        let mut grid = SharedGrid::new();
        grid.load_local(vec![text_column("id")], vec![row("1", vec![Some("1")], true)], Some(1000), None);
        assert!(grid.has_more_rows());
        grid.append_page(Vec::new(), None, false);
        assert!(!grid.has_more_rows(), "空的终页不能因旧计数继续自动请求同一页");
    }

    #[test]
    fn preview_scan_matches_original_with_overlays_hidden_rows_and_ready_null() {
        let envelope = "\u{1}t:3000:preview";
        let mut grid = SharedGrid::new();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            (1..=5).map(|id| row(&id.to_string(), vec![Some(&id.to_string()), Some(envelope)], true)).collect(),
            Some(5), Some(schema()),
        );
        let reads = grid.begin_unresolved_full();
        assert_eq!(reads.len(), 5);
        assert!(grid.apply_full(&reads[0], None), "缓存的数据库 NULL 是完整值");
        assert!(grid.apply_full(&reads[1], Some("完整值".repeat(10_000))));
        assert!(grid.fail_full(&reads[2], "retry"));
        assert_eq!(grid.stage_text(3, 1, Some("替换值".to_owned())), Gate::Writable);
        assert_eq!(grid.stage_insert(), Gate::Writable);
        grid.set_search("1");
        let expected = legacy_unresolved_source_cells(&grid);
        assert_eq!(expected, vec![("3".to_owned(), 1), ("5".to_owned(), 1)]);
        assert_eq!(grid.unresolved_source_cells(), expected);
        assert_eq!(grid.unresolved_previews(), expected.len());
        assert_eq!(grid.full_text_of("1", 1), Some(None));
        grid.revert_cell("4", 1);
        assert_eq!(grid.unresolved_source_cells(), legacy_unresolved_source_cells(&grid));
        assert_eq!(grid.unresolved_previews(), 3);
    }

    #[test]
    fn fit_candidates_match_stable_unicode_sort_and_keep_staged_values() {
        let mut grid = SharedGrid::new();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            (0..150).map(|id| {
                let value = if id % 3 == 0 { format!("中文{id:03}") } else { format!("ascii{id:03}") };
                row(&id.to_string(), vec![Some(&id.to_string()), Some(&value)], true)
            }).collect(),
            Some(150), Some(schema()),
        );
        assert_eq!(grid.stage_text(149, 1, Some("最长的暂存内容".repeat(40))), Gate::Writable);
        let mut expected: Vec<String> = (0..grid.loaded_len()).map(|row| grid.cell_label(row, 1)).collect();
        expected.sort_by_key(|text| std::cmp::Reverse(text.chars().count()));
        expected.truncate(64);
        assert_eq!(grid.fit_column_labels(1), expected, "包含同长度项的稳定顺序");
        assert!(grid.fit_column_labels(2).is_empty());
        grid.set_search("missing");
        assert!(grid.fit_column_labels(1).is_empty());
    }

    /// Run with --ignored --nocapture. Compares display/mark/preview preparation only;
    /// it does not measure TableState refresh, frame rendering, database I/O or process RSS.
    #[test]
    #[ignore = "manual grid preparation benchmark"]
    fn benchmark_grid_display_preparation() {
        use std::hint::black_box;
        use std::time::Instant;
        let mut grid = SharedGrid::new();
        let row_count = 3_000;
        let column_count = 8;
        let visible_rows = 30;
        let text = "x".repeat(128);
        let columns = (0..column_count).map(|column| text_column(&format!("c{column}"))).collect();
        let rows = (0..row_count).map(|id| row(&id.to_string(), vec![Some(&text); column_count], false)).collect();
        grid.load_local(columns, rows, Some(row_count as i64), None);
        grid.set_viewport(visible_rows);
        // Shared formatter keeps this comparison conservative: the legacy preparation also
        // benefits from the optimized direct-row preview check, but still materializes all cells.
        let legacy = || {
            let labels: Vec<Vec<String>> = (0..grid.loaded_len()).map(|row| grid.sheet_line(row)).collect();
            let marks: Vec<Vec<CellDirty>> = (0..grid.loaded_len())
                .map(|row| (0..column_count).map(|column| grid.cell_dirty_state(row, column)).collect()).collect();
            let unresolved = legacy_unresolved_source_cells(&grid);
            (labels, marks, unresolved)
        };
        let current = || {
            let labels = grid.rendered_labels();
            let marks: Vec<Vec<CellDirty>> = (0..visible_rows)
                .map(|row| (0..column_count).map(|column| grid.cell_dirty_state(row, column)).collect()).collect();
            (labels, marks, grid.unresolved_previews())
        };
        let baseline = legacy();
        let optimized = current();
        assert_eq!(baseline.0[..visible_rows], optimized.0);
        assert_eq!(baseline.1[..visible_rows], optimized.1);
        assert_eq!(baseline.2.len(), optimized.2);
        let payload_bytes: usize = baseline.0.iter().flatten().map(String::len).sum();
        let capacity_bytes: usize = baseline.0.iter().flatten().map(String::capacity).sum();
        let visible_payload: usize = optimized.0.iter().flatten().map(String::len).sum();
        drop((baseline, optimized));
        let rounds = 3;
        let started = Instant::now();
        for _ in 0..rounds { black_box(legacy()); }
        let legacy_elapsed = started.elapsed();
        let started = Instant::now();
        for _ in 0..rounds { black_box(current()); }
        let current_elapsed = started.elapsed();
        eprintln!("grid-preparation rows={row_count} columns={column_count} visible_rows={visible_rows} rounds={rounds} legacy_us={} current_us={} legacy_display_payload_bytes={payload_bytes} legacy_display_string_capacity_bytes={capacity_bytes} current_delegate_retained_display_bytes=0 visible_temporary_payload_bytes={visible_payload}", legacy_elapsed.as_micros(), current_elapsed.as_micros());
    }

    #[test]
    fn views_share_rows_across_sort_filter_and_retention_and_writes_are_isolated() {
        let mut grid = SharedGrid::new();
        grid.load_local(vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some("alpha")], true), row("2", vec![Some("2"), Some("beta")], true)], Some(2), Some(schema()));
        assert!(Arc::ptr_eq(&grid.source[0], &grid.rows[0]));
        let original = Arc::clone(&grid.source[0]);
        grid.sort = Some(("name".into(), true));
        grid.recompute();
        assert_eq!(grid.rows[0].id, "2");
        assert!(Arc::ptr_eq(&grid.source[1], &grid.rows[0]));
        grid.set_search("alpha");
        assert_eq!(grid.rows.len(), 1);
        assert!(Arc::ptr_eq(&original, &grid.rows[0]));
        assert_eq!(grid.stage_text_by_id("1", 1, Some("staged".into())), Gate::Writable);
        grid.retain_staged_rows();
        assert!(Arc::ptr_eq(&original, &grid.retained[0]));
        grid.set_row_value("1", "name", Some("committed".into()));
        assert_eq!(original.values[1].as_deref(), Some("alpha"), "共享的旧快照不被写回修改");
        assert_eq!(grid.retained[0].values[1].as_deref(), Some("committed"));
        grid.set_search("");
        for view in &grid.rows {
            let source = grid.source.iter().find(|row| row.id == view.id).unwrap();
            assert!(Arc::ptr_eq(source, view));
        }
    }

    #[test]
    fn indexed_full_cache_keeps_columns_independent_and_prunes_absent_rows() {
        let mut grid = SharedGrid::new();
        grid.load_local(vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some("alpha")], true), row("2", vec![Some("2"), Some("beta")], true)], Some(2), Some(schema()));
        grid.store_full("1", "id", FullState::Ready(None), CellPhase::Full, String::new());
        grid.store_full("1", "name", FullState::Ready(Some("full".into())), CellPhase::Full, String::new());
        grid.store_full("2", "name", FullState::Failed("retry".into()), CellPhase::Error, "retry".into());
        grid.set_search("alpha");
        assert_eq!(grid.full_state("1", "id"), Some(&FullState::Ready(None)));
        assert_eq!(grid.full_text_of("1", 1), Some(Some("full".into())));
        assert_eq!(grid.full_state("2", "name"), Some(&FullState::Failed("retry".into())), "隐藏来源行仍保留缓存");
        grid.source.retain(|row| row.id != "2");
        grid.recompute();
        assert!(!grid.full.contains_key("2"));
        grid.drop_stale_sheet();
        assert!(grid.full.is_empty());
    }

    #[test]
    #[ignore = "manual shared row allocation benchmark"]
    fn benchmark_grid_shared_rows() {
        use std::hint::black_box;
        use std::time::Instant;
        let mut grid = SharedGrid::new();
        let columns = (0..8).map(|i| text_column(&format!("c{i}"))).collect();
        let rows = (0..10_000).map(|i| GridRow {
            id: i.to_string(), values: (0..8).map(|_| Some("x".repeat(128))).collect(),
            located: false, locator: Vec::new(), inserted: false,
        }).collect();
        grid.load_local(columns, rows, Some(10_000), None);
        let payload: usize = grid.source.iter().flat_map(|r| &r.values).flatten().map(String::len).sum();
        for trial in 1..=3 {
            let start = Instant::now();
            let legacy: Vec<GridRow> = grid.source.iter().map(|r| r.as_ref().clone()).collect();
            black_box(&legacy);
            let clone_us = start.elapsed().as_micros();
            let start = Instant::now();
            grid.recompute();
            black_box(&grid.rows);
            let shared_us = start.elapsed().as_micros();
            assert!(legacy.iter().zip(&grid.rows).all(|(left, right)| left == right.as_ref()));
            assert!(grid.rows.iter().zip(&grid.source).all(|(left, right)| Arc::ptr_eq(left, right)));
            eprintln!("shared-rows trial={trial} rows=10000 columns=8 bytes_per_cell=128 legacy_clone_us={clone_us} shared_recompute_us={shared_us} avoided_view_cell_bytes={payload}");
        }
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
        // 行数摘要写已读取的行区间和完整行数，不随滚动窗口变化，数字带千分位。
        assert_eq!(grid.range_text(), "1–100 / 1,000 行");
        assert_eq!(group_digits(1234567), "1,234,567");
        assert_eq!(group_digits(-1000), "-1,000");
        assert_eq!(group_digits(999), "999");
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

    /// 导出扫描和 exchange_sheet 的范围必须一致：还没写库的新增行不在导出行里，
    /// 它上面的预览形状也就不能算进"还有多少预览要读"。
    #[test]
    fn unresolved_preview_scan_matches_the_exported_rows() {
        let envelope = format!("\u{1}t:3000:{}", "a".repeat(16));
        let mut grid = SharedGrid::new();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some(envelope.as_str())], true)],
            Some(1),
            Some(schema()),
        );
        assert!(grid.writable());
        // 新增行也带一个预览形状的值：它还没写库，导出根本不会写它。
        // 暂存会拒绝把信封写进编辑（见 preview 校验），这里直接把值放进新增行验证扫描范围。
        assert_eq!(grid.stage_insert(), Gate::Writable);
        assert!(grid.rows.iter().any(|item| item.inserted), "新增行");
        for item in grid.source.iter_mut().filter(|item| item.inserted) {
            Arc::make_mut(item).values[1] = Some(envelope.clone());
        }
        grid.recompute();
        assert_eq!(grid.unresolved_previews(), 1, "只算真正会被导出的那一格");
        let (_, _, rows, _) = grid.exchange_sheet();
        assert_eq!(rows.len(), 1, "导出行里没有未写入的新增行");
        assert!(rows[0][1].as_deref().is_some_and(|value| value.starts_with('\u{1}')));
    }

    /// 导出扫描和 exchange_sheet 的来源行一致：被本地查找/筛选隐藏的预览行也要算进未读预览，
    /// 并且能按它自己的定位读到原值——否则导出被挡，网格却没有能读到那一行的入口。
    #[test]
    fn unresolved_previews_cover_rows_hidden_by_local_search_and_filter() {
        let envelope = format!("\u{1}t:3000:{}", "a".repeat(16));
        let mut grid = SharedGrid::new();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![
                row("1", vec![Some("1"), Some(envelope.as_str())], true),
                row("2", vec![Some("2"), Some(envelope.as_str())], true),
            ],
            Some(2),
            Some(schema()),
        );
        grid.set_search("1");
        assert_eq!(grid.loaded_len(), 1, "本地查找只显示匹配行");
        assert_eq!(grid.unresolved_previews(), 2, "被隐藏的 id=2 也会被导出写出");
        assert_eq!(grid.unresolved_cells(), vec![(0, 1)], "视图里只看得到一行");
        let reads = grid.begin_unresolved_full();
        assert_eq!(reads.len(), 2, "隐藏行同样按定位发起读取");
        assert!(reads.iter().any(|read| read.row_id == "2"), "隐藏行有读取请求");
        for read in &reads {
            assert!(grid.apply_full(read, Some("正文".to_owned())), "同一目标同一代次采纳");
        }
        assert_eq!(grid.unresolved_previews(), 0);
        assert!(grid.unresolved_block().is_none(), "读到原值后不再挡导出");
        let (_, _, rows, _) = grid.exchange_sheet();
        assert_eq!(rows.len(), 2, "导出行仍包含被查找隐藏的那一行");
        assert!(rows.iter().all(|row| row[1].as_deref() == Some("正文")), "导出拿到原值而不是信封");
    }

    /// 迟到的完整值读取不能落到换了目标的同一格上：请求带着目标和快照代次回来对账。
    #[test]
    fn late_full_reads_are_discarded_after_the_target_changes() {
        let envelope = format!("\u{1}t:3000:{}", "a".repeat(16));
        let mut grid = SharedGrid::new();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some(envelope.as_str())], true)],
            Some(1),
            Some(schema()),
        );
        let read = grid.pending_read(0, 1).expect("预览格需要按定位读取");
        assert_eq!(read.target.connection_id, grid.target.connection_id);
        assert_eq!(read.token, grid.sheet_serial());

        // 换到另一张表后再按新快照载入同一行 id：旧的读取批次必须被丢弃。
        let other = GridTarget { connection_id: "other".to_owned(), table: "docs".to_owned(), ..GridTarget::default() };
        grid.switch_target(other);
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some(envelope.as_str())], true)],
            Some(1),
            Some(schema()),
        );
        assert!(!grid.apply_full(&read, Some("旧表的值".to_owned())), "过期读取不采纳");
        assert!(grid.full_text_at(0, 1).is_none(), "新表这一格没有被写入旧值");
        assert!(!grid.fail_full(&read, "旧表的失败"), "过期失败也不记到新表上");
        assert!(grid.cell_read(0, 1) != CellRead::Ready(Some("旧表的值".to_owned())));
    }

    /// 预览上限从 1 KB 变到 4 KB/64 KB：快照记住读出时的上限，旧行列仍按旧上限解释；
    /// 改上限会关掉查看面板并清空按定位读到的原值，按新上限重读后同一段文本就是原值。
    #[test]
    fn preview_limit_changes_reload_rows_and_close_the_viewer() {
        // 页查询在超过上限时发出的信封：声明 2048 字节，预览只有一段。
        let envelope = format!("\u{1}t:2048:{}", "a".repeat(64));
        let full = "a".repeat(2048);
        let mut grid = SharedGrid::new();
        let load = |grid: &mut SharedGrid, value: &str| {
            grid.load_local(
                vec![text_column("id"), text_column("note")],
                vec![row("1", vec![Some("1"), Some(value)], true)],
                Some(1),
                Some(schema()),
            );
        };
        load(&mut grid, &envelope);
        let large = format!("{} 2048", i18n::text(Locale::ZhCn, "grid.value.largeLabel"));
        assert_eq!(grid.sheet_preview_bytes(), 1024, "快照记住读出时的上限");
        assert_eq!(grid.cell_label(0, 1), large, "默认 1 KB 上限下显示大字段");
        assert!(grid.needs_full(0, 1), "信封不是原值");
        // 打开查看面板并读到一个原值，改上限后这些派生状态都要作废。
        grid.open_value(0, 1);
        let read = grid.pending_read(0, 1).expect("预览格需要按定位读取");
        assert!(grid.apply_full(&read, Some(full.clone())));
        assert_eq!(grid.viewer_state().map(|state| state.phase), Some(CellPhase::Full));

        assert!(grid.set_preview_bytes(4 * 1024), "上限真的变了");
        assert_eq!(grid.page_request().unwrap().preview_bytes, 4096, "下一次读取用新上限");
        assert!(grid.viewer().is_none(), "改上限后查看面板关闭");
        assert!(grid.full_text_at(0, 1).is_none(), "旧原值缓存清空");
        assert!(grid.needs_full(0, 1), "旧行列仍按 1 KB 上限判断成预览");
        assert!(grid.pending_read(0, 1).is_some(), "旧信封不会被新上限重新解释");

        // 按新上限重读后，同一段文本不再是信封：查看、复制、编辑都直接用它。
        load(&mut grid, &full);
        assert_eq!(grid.sheet_preview_bytes(), 4096);
        assert!(!grid.needs_full(0, 1), "4 KB 上限下 2048 字节是原值");
        assert_eq!(grid.cell_label(0, 1), full, "4 KB 上限下同一值直接显示");

        // 64 KB 上限下协议形状但没超过上限的文本仍是原值；超过 4 KB 的在 4 KB 上限下才是信封。
        let protocol_shaped = format!("\u{1}t:3000:{}", "b".repeat(16));
        load(&mut grid, &protocol_shaped);
        assert!(!grid.needs_full(0, 1), "4 KB 上限下 3000 字节没被截断，是原值");
        grid.select(0, 1, false);
        assert_eq!(grid.copy_cell(), protocol_shaped, "协议形状的普通文本按原值复制");
        // WHERE 也按原值生成条件，而不是拿信封当字面量。
        let where_sql = grid.where_text(CopyScope::Cell);
        assert!(where_sql.contains(&protocol_shaped), "{where_sql}");
        assert!(grid.set_preview_bytes(64 * 1024));
        load(&mut grid, &protocol_shaped);
        assert_eq!(grid.sheet_preview_bytes(), 64 * 1024);
        assert!(!grid.needs_full(0, 1), "64 KB 上限下 3000 字节是原值");
        load(&mut grid, &format!("\u{1}t:70000:{}", "c".repeat(16)));
        assert!(grid.needs_full(0, 1), "64 KB 上限下 70000 字节仍是信封");
    }

    /// 编辑暂存与复制/WHERE/导出/commit 比较同一个上限：4 KB 上限下 3000 字节的协议形状文本
    /// 没被截断，是原值，能复制也能保存；真的超过上限的信封仍被挡在暂存外，不会写进数据库。
    #[test]
    fn staging_edit_compares_protocol_shaped_text_against_the_sheet_limit() {
        let shaped = format!("\u{1}t:3000:{}", "b".repeat(16));
        let mut grid = SharedGrid::new();
        grid.set_preview_bytes(4 * 1024);
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![row("1", vec![Some("1"), Some(shaped.as_str())], true)],
            Some(1),
            Some(schema()),
        );
        assert_eq!(grid.sheet_preview_bytes(), 4096, "快照记住读出时的上限");
        assert!(!grid.needs_full(0, 1), "4 KB 上限下这段文本没被截断");
        grid.select(0, 1, false);
        assert_eq!(grid.copy_cell(), shaped, "复制拿到原值");
        // 暂存用同一个上限判断：这段原值能保存，不会被当成预览挡下。
        assert_eq!(grid.stage_text(0, 1, Some(shaped.clone())), Gate::Writable, "4 KB 上限下原值可编辑");
        assert_eq!(grid.overlay_message(0, 1), "");
        // 提交请求带着读出这份行列时的上限，写回前判断与暂存一致。
        let sql = grid.open_preview().expect("暂存的修改可以预览");
        assert!(sql.iter().any(|line| line.contains(&shaped)), "{sql:?}");
        let request = grid.confirm_request().expect("暂存的修改可以提交");
        assert_eq!(request.preview_bytes, 4096);
        assert_eq!(request.changes[0].sets[0].value.as_deref(), Some(shaped.as_str()), "写回的是原值");

        // 真的超过 4 KB 的预览信封仍不能暂存：信封不会写进数据库。
        let envelope = format!("\u{1}t:5000:{}", "c".repeat(16));
        assert_eq!(grid.stage_text(0, 1, Some(envelope)), Gate::Validation);
        assert_eq!(grid.overlay_message(0, 1), "预览内容不能写回");
        assert!(grid.open_preview().is_err(), "被挡下的信封不进提交预览");
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
        assert_eq!(SharedGrid::row_id(&grid, display_row), Some("11"));
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
        assert_eq!(query.gate_text(Gate::Readonly), i18n::text(Locale::ZhCn, "grid.readonly.join"));
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

    fn identity_grid() -> SharedGrid {
        let schema = schema_from_parts(
            "sqlite",
            "",
            "customers",
            "table",
            vec![column_of("id", "integer", true), column_of("name", "text", false), column_of("amount", "integer", false)],
            vec!["id".to_owned()],
        )
        .unwrap();
        let mut grid = SharedGrid::new();
        grid.target.engine = "sqlite".into();
        grid.target.table = "customers".into();
        grid.target.database = "main".into();
        grid.load_local(
            vec![int_column("id"), text_column("name"), int_column("amount")],
            vec![
                row("1", vec![Some("1"), Some("甲"), Some("10")], true),
                row("2", vec![Some("2"), Some("乙"), Some("20")], true),
                row("3", vec![Some("3"), Some("丙"), Some("30")], true),
            ],
            Some(3),
            Some(schema),
        );
        grid
    }

    #[test]
    fn copies_keep_row_and_column_identity_when_visual_order_changes() {
        let mut grid = identity_grid();
        // 视觉顺序改成 3,2,1；焦点行仍是 id=2。
        grid.set_sort("id", true);
        assert_eq!(grid.cell_label(0, 0), "3");
        grid.select(1, 1, false);
        assert_eq!(grid.focused_row_id(), Some("2"));
        assert_eq!(grid.copy_row(), "2\t乙\t20");
        assert_eq!(grid.copy_row_sql(), "2, '乙', 20");
        assert_eq!(grid.copy_column(), "丙\n乙\n甲");
        assert_eq!(grid.copy_column_sql(), "'丙'\n'乙'\n'甲'");
        assert_eq!(grid.copy_cell(), "乙");
        // 复制不改变选择：下一步的复制仍从同一格出发。
        assert_eq!(grid.focus_cell(), Some((1, 1)));

        assert_eq!(grid.where_text(CopyScope::Cell), "WHERE \"customers\".\"name\" = '乙'");
        assert_eq!(grid.where_text(CopyScope::Row), "WHERE \"customers\".\"id\" = 2");
        assert_eq!(grid.where_text(CopyScope::Column), "WHERE \"customers\".\"name\" IN ('乙')");

        // 筛选后同一行仍被找到，复制内容不漂移；筛掉后选择为空，不复制别行。
        grid.set_filter("name", "contains", "乙");
        assert_eq!(grid.focused_row_id(), Some("2"));
        assert_eq!(grid.copy_row(), "2\t乙\t20");
        assert_eq!(grid.copy_row_sql(), "2, '乙', 20");
        assert_eq!(grid.copy_column(), "乙");
        grid.clear_filters();
        grid.set_filter("name", "contains", "丙");
        assert_eq!(grid.focused_row_id(), None);
        assert_eq!(grid.copy_row(), "");
        assert_eq!(grid.where_text(CopyScope::Row), "");
    }

    #[test]
    fn where_copies_keep_null_and_staged_values_distinguishable() {
        let mut grid = identity_grid();
        grid.select(0, 1, false);
        assert_eq!(grid.where_text(CopyScope::Cell), "WHERE \"customers\".\"name\" = '甲'");
        assert_eq!(grid.stage_text(0, 1, None), Gate::Writable);
        assert_eq!(grid.where_text(CopyScope::Cell), "WHERE \"customers\".\"name\" IS NULL");
        assert_eq!(grid.stage_text(0, 1, Some("乙".into())), Gate::Writable);
        assert_eq!(grid.where_text(CopyScope::Cell), "WHERE \"customers\".\"name\" = '乙'");
        // 整列条件按选中行的去重值集合生成，NULL 单独写 IS NULL。
        grid.select(0, 1, false);
        grid.select(2, 1, true);
        let column = grid.where_text(CopyScope::Column);
        assert!(column.contains("IN (") && column.contains("'乙'") && column.contains("'丙'"), "{column}");
        assert!(!column.contains("'甲'"), "{column}");
        assert!(!column.contains("IS NULL"), "{column}");
    }

    /// 建库并执行建表/插入语句；返回目录与已打开会话的服务。
    async fn sqlite_fixture(name: &str, statements: &[&str]) -> (std::path::PathBuf, std::sync::Arc<crate::connections::Service>) {
        let dir = temp_dir(name);
        let database = dir.join("notes.sqlite");
        let setup = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
            .await
            .unwrap();
        for statement in statements {
            // 语句由测试自己给出，这里只是执行建表/插入，不接收用户输入。
            sqlx::query(sqlx::AssertSqlSafe((*statement).to_owned())).execute(&setup).await.unwrap();
        }
        setup.close().await;
        let service = std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")));
        service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
        (dir, service)
    }

    /// 按真实结构读一页到网格；外键同样来自这次 describe。
    async fn load_table(service: &crate::connections::Service, table: &str, preview_bytes: u64) -> SharedGrid {
        let request = crate::grid::commands::TableRef {
            connection_id: "notes".into(),
            database: "main".into(),
            schema: String::new(),
            table: table.into(),
            engine: "sqlite".into(),
            include_foreign_keys: true,
        };
        let dto = crate::application::grid::describe(service, &request).await.unwrap();
        let schema = schema_from_dto(&dto).unwrap();
        let page = crate::application::grid::page(
            service,
            &crate::grid::commands::PageRequest { table: request, filters: Vec::new(), sort: None, offset: 0, limit: 20, preview_bytes, search: None },
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
            table: table.into(),
            engine: "sqlite".into(),
            environment: "dev".into(),
        };
        grid.set_preview_bytes(preview_bytes as usize);
        grid.set_foreign_keys(crate::app::views::grid::foreign_keys_of(&dto));
        grid.replace_fetched(columns_from_schema(&schema, true), rows_from_page(&columns, &page), 0, Some(1), false, Some(schema));
        grid
    }

    fn locator_of(read: &FullRead) -> Vec<(String, Cell)> {
        read.locator.iter().map(|(name, value)| (name.clone(), cell_of(value.clone()))).collect()
    }

    #[tokio::test]
    async fn full_value_is_read_once_and_shared_by_view_copy_edit_export_and_where() {
        let group = "replace(hex(zeroblob(300)), '0', 'a')";
        let insert = format!("INSERT INTO docs (id, body) VALUES (1, {group}), (2, '短文本')");
        let (dir, service) = sqlite_fixture(
            "full-once",
            &["CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", insert.as_str()],
        )
        .await;
        let mut grid = load_table(&service, "docs", 64).await;
        assert!(grid.needs_full(0, 1), "600 字符的文本按 64 字节读了预览");
        let read = grid.begin_full(0, 1).expect("需要按定位读取完整值");
        assert_eq!(read.row_id, "k:1");
        assert_eq!(read.column_name, "body");
        // 同一定位只发起一次读取：再要不会产生第二个请求。
        assert!(grid.begin_full(0, 1).is_none(), "同一定位不能重复读取");

        let session = service.catalog_session("notes").unwrap();
        let schema = grid.schema().cloned().unwrap();
        let cell = crate::grid::read_full_cell(&session, &schema, &locator_of(&read), "body").await.unwrap();
        let full = match cell {
            Cell::Text(text) => text,
            Cell::Null => panic!("完整值不应是 NULL"),
        };
        assert_eq!(full.chars().count(), 600);
        grid.apply_full(&read, Some(full.clone()));

        // 查看、复制、编辑、导出和 WHERE 共用同一个原值，并且不再读第二次。
        grid.select(0, 1, false);
        grid.open_value(0, 1);
        assert_eq!(grid.viewer_state().map(|state| state.phase), Some(CellPhase::Full));
        assert_eq!(grid.viewer().map(|shown| shown.body), Some(full.clone()));
        assert!(!grid.viewer().unwrap().preview, "读到原值后不再标成预览");
        assert_eq!(grid.copy_cell(), full);
        assert_eq!(grid.edit_text(0, 1), full);
        assert!(grid.where_text(CopyScope::Cell).contains(&full), "WHERE 用的是原值");
        let (_, _, rows, _) = grid.exchange_sheet();
        assert_eq!(rows[0][1].as_deref(), Some(full.as_str()), "导出拿到的是原值");
        assert!(grid.begin_full(0, 1).is_none(), "已经读到原值后不再发起读取");
        assert!(grid.copy_row().contains(&full), "整行复制同样是原值");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn full_value_failure_keeps_the_preview_and_can_be_retried() {
        let body = "replace(hex(zeroblob(200)), '0', 'b')";
        let insert = format!("INSERT INTO notes (id, body) VALUES (1, {body})");
        let (dir, service) = sqlite_fixture(
            "full-fail",
            &["CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)", insert.as_str()],
        )
        .await;
        let mut grid = load_table(&service, "notes", 32).await;
        let read = grid.begin_full(0, 1).expect("需要读取完整值");
        let session = service.catalog_session("notes").unwrap();
        let schema = grid.schema().cloned().unwrap();
        // 定位到不存在的行：读取失败，预览仍然可见但不能当原值使用。
        let missing = vec![("id".to_owned(), Cell::Text("999".to_owned()))];
        let error = crate::grid::read_full_cell(&session, &schema, &missing, "body").await.unwrap_err();
        assert!(error.contains("找不到"), "{error}");
        grid.fail_full(&read, &error);
        grid.select(0, 1, false);
        assert_eq!(grid.copy_cell(), "", "失败时不复制预览");
        assert_eq!(grid.where_text(CopyScope::Cell), "", "失败时不生成 WHERE");
        let (_, _, rows, _) = grid.exchange_sheet();
        assert!(rows[0][1].as_deref().is_some_and(|value| value.starts_with('\u{1}')), "预览仍是预览");
        grid.open_value(0, 1);
        assert_eq!(grid.viewer_state().map(|state| state.phase), Some(CellPhase::Error));
        assert!(grid.viewer().unwrap().preview, "失败后显示的仍是预览");
        assert!(grid.viewer_state().is_some_and(|state| !state.message.is_empty()));
        // 重试：失败过的格子可以再发起一次，读到原值后各入口恢复。
        let retry = grid.begin_full(0, 1).expect("失败后可以重试");
        let cell = crate::grid::read_full_cell(&session, &schema, &locator_of(&retry), "body").await.unwrap();
        let full = match cell {
            Cell::Text(text) => text,
            Cell::Null => panic!("完整值不应是 NULL"),
        };
        grid.apply_full(&retry, Some(full.clone()));
        assert_eq!(grid.viewer_state().map(|state| state.phase), Some(CellPhase::Full));
        assert_eq!(grid.copy_cell(), full);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn binary_null_and_protocol_shaped_text_keep_their_own_values() {
        let (dir, service) = sqlite_fixture(
            "value-shapes",
            &[
                "CREATE TABLE marks (id INTEGER PRIMARY KEY, note TEXT, blob BLOB)",
                "INSERT INTO marks (id, note, blob) VALUES (1, char(1) || 't:3:abc', x'00ff'), (2, NULL, NULL)",
            ],
        )
        .await;
        let mut grid = load_table(&service, "marks", 1024).await;
        // 协议形状的普通短文本保持原值：不当作预览，也不需要读取。
        assert!(!grid.needs_full(0, 1));
        grid.select(0, 1, false);
        assert_eq!(grid.copy_cell(), "\u{1}t:3:abc");
        assert_eq!(grid.cell_label(0, 1), "\u{1}t:3:abc");
        assert!(grid.begin_full(0, 1).is_none(), "普通文本不发起完整值读取");
        // 二进制只有长度信封：需要按定位读取，读到的是十六进制原值。
        assert!(grid.needs_full(0, 2));
        assert_eq!(grid.show_cell(0, 2, false).kind, CellKind::Binary);
        let read = grid.begin_full(0, 2).expect("二进制需要读取原值");
        let session = service.catalog_session("notes").unwrap();
        let schema = grid.schema().cloned().unwrap();
        let cell = crate::grid::read_full_cell(&session, &schema, &locator_of(&read), "blob").await.unwrap();
        grid.apply_full(&read, Some(match cell {
            Cell::Text(text) => text,
            Cell::Null => panic!("二进制不应是 NULL"),
        }));
        grid.select(0, 2, false);
        assert_eq!(grid.copy_cell(), "00ff");
        // 数据库 NULL 与文本 NULL 仍然分开。
        grid.select(1, 1, false);
        assert_eq!(grid.copy_cell(), grid.null_copy().to_owned());
        assert_eq!(grid.sql_values(), "NULL");
        grid.select(1, 2, false);
        assert!(!grid.needs_full(1, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn real_schema_foreign_keys_navigate_single_and_composite_rows() {
        let (dir, service) = sqlite_fixture(
            "foreign-keys",
            &[
                "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT)",
                "CREATE TABLE books (id INTEGER PRIMARY KEY, author_id INTEGER, title TEXT, FOREIGN KEY (author_id) REFERENCES authors(id))",
                "CREATE TABLE regions (country TEXT NOT NULL, city TEXT NOT NULL, PRIMARY KEY (country, city))",
                "CREATE TABLE shops (id INTEGER PRIMARY KEY, country TEXT, city TEXT, FOREIGN KEY (country, city) REFERENCES regions(country, city))",
                "INSERT INTO authors VALUES (1, '甲')",
                "INSERT INTO books VALUES (1, 1, '书')",
                "INSERT INTO regions VALUES ('CN', '北京')",
                "INSERT INTO shops VALUES (1, 'CN', '北京'), (2, NULL, '北京')",
            ],
        )
        .await;
        let mut books = load_table(&service, "books", 1024).await;
        assert_eq!(books.foreign_keys().len(), 1, "外键来自真实结构");
        books.select(0, 1, false);
        assert!(books.has_reference(), "焦点在外键列上时适用导航");
        let jump = books.reference(0, 1).expect("单列外键可以跳转");
        assert_eq!(jump.connection_id, "notes");
        assert_eq!(jump.database, "main");
        assert_eq!(jump.table, "authors");
        assert_eq!(jump.columns, vec!["id".to_owned()]);
        assert_eq!(jump.values, vec![Some("1".to_owned())]);
        assert_eq!(jump.sql, "SELECT \"id\" FROM \"main\".\"authors\" WHERE \"id\" = 1");
        // 不是外键列时不给目标。
        assert!(books.reference(0, 2).is_err());

        let shops = load_table(&service, "shops", 1024).await;
        let composite = shops.reference(0, 1).expect("复合外键列齐时可以生成条件");
        assert_eq!(composite.columns, vec!["country".to_owned(), "city".to_owned()]);
        assert_eq!(composite.values, vec![Some("CN".to_owned()), Some("北京".to_owned())]);
        assert!(composite.sql.contains("\"country\" = 'CN'"), "{}", composite.sql);
        assert!(composite.sql.contains("\"city\" = '北京'"), "{}", composite.sql);
        // 复合外键里有一个 NULL：拒绝生成不完整的条件。
        assert_eq!(shops.reference(1, 1), Err("grid.fk.null"));
        // 缺一列同样拒绝。
        let mut partial = load_table(&service, "shops", 1024).await;
        partial.columns.retain(|column| column.name != "city");
        assert_eq!(partial.reference(0, 1), Err("grid.fk.missingColumn"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn foreign_key_previews_and_other_schema_targets_are_not_taken_as_values() {
        let code = "replace(hex(zeroblob(20)), '0', 'c')";
        let vendor = format!("INSERT INTO vendors VALUES ({code}, '供应商')");
        let bill = format!("INSERT INTO bills VALUES (1, {code})");
        let (dir, service) = sqlite_fixture(
            "fk-preview",
            &[
                "CREATE TABLE vendors (code TEXT PRIMARY KEY, label TEXT)",
                "CREATE TABLE bills (id INTEGER PRIMARY KEY, vendor TEXT, FOREIGN KEY (vendor) REFERENCES vendors(code))",
                vendor.as_str(),
                bill.as_str(),
            ],
        )
        .await;
        let mut grid = load_table(&service, "bills", 8).await;
        assert!(grid.needs_full(0, 1), "外键值也走同一套预览");
        // 预览值不能当外键条件。
        assert_eq!(grid.reference(0, 1), Err("grid.fk.preview"));
        let read = grid.begin_full(0, 1).expect("外键值需要读原值");
        let session = service.catalog_session("notes").unwrap();
        let schema = grid.schema().cloned().unwrap();
        let cell = crate::grid::read_full_cell(&session, &schema, &locator_of(&read), "vendor").await.unwrap();
        let value = match cell {
            Cell::Text(text) => text,
            Cell::Null => panic!("外键值不应是 NULL"),
        };
        grid.apply_full(&read, Some(value.clone()));
        let jump = grid.reference(0, 1).expect("读到原值后可以生成条件");
        assert!(jump.sql.contains(&value), "{}", jump.sql);
        assert!(jump.sql.contains("\"vendors\""), "{}", jump.sql);

        // 同名表在另一个模式里：引用的是外键声明的模式，不是当前表的模式。
        let mut other = load_table(&service, "bills", 1024).await;
        other.set_foreign_keys(vec![ForeignKeyInfo {
            name: "fk_customers".to_owned(),
            columns: vec!["vendor".to_owned()],
            ref_schema: "other".to_owned(),
            ref_table: "bills".to_owned(),
            ref_columns: vec!["code".to_owned()],
        }]);
        let target = other.reference(0, 1).expect("外键目标按声明的模式");
        assert_eq!(target.schema, "other");
        assert_eq!(target.table, "bills");
        assert_eq!(target.connection_id, "notes");
        assert_eq!(target.database, "main");
        assert!(target.sql.contains("\"other\".\"bills\""), "{}", target.sql);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn readonly_reasons_explain_views_no_key_joins_aggregates_and_multi_statements() {
        let (dir, service) = sqlite_fixture(
            "readonly",
            &[
                "CREATE TABLE plain (label TEXT)",
                "INSERT INTO plain VALUES ('原')",
                "CREATE VIEW recent AS SELECT label FROM plain",
                "CREATE TABLE keyed (id INTEGER PRIMARY KEY, name TEXT)",
                "INSERT INTO keyed VALUES (1, '甲')",
            ],
        )
        .await;
        let view = load_table(&service, "recent", 1024).await;
        assert!(!view.writable());
        assert_eq!(view.readonly_reason(), "grid.errors.viewReadonly");
        let loose = load_table(&service, "plain", 1024).await;
        assert!(!loose.writable());
        assert_eq!(loose.readonly_reason(), "grid.status.unlocated");
        let keyed = load_table(&service, "keyed", 1024).await;
        assert!(keyed.writable());
        // 每种只读形态给出各自的原因键，界面把它们原样显示出来。
        for (sql, reason) in [
            ("select keyed.name from keyed join plain on 1 = 1", "grid.readonly.join"),
            ("select count(*) from keyed", "grid.readonly.aggregate"),
            ("select name, count(*) from keyed group by name", "grid.readonly.aggregate"),
            ("select * from keyed; select * from plain", "grid.readonly.multiStatement"),
            ("select * from (select * from keyed) as inner_keyed", "grid.readonly.parentheses"),
            ("select upper(name) from keyed", "grid.readonly.parentheses"),
            ("select * from keyed, plain", "grid.readonly.multiTable"),
            ("select * from keyed union select * from keyed", "grid.readonly.setOperation"),
            ("delete from keyed where id = 1", "grid.readonly.notSelect"),
        ] {
            let mut grid = SharedGrid::new();
            grid.target.engine = "sqlite".into();
            grid.target.connection_id = "notes".into();
            grid.show_query_sheet(sql, vec!["name".into()], vec![vec![Some("甲".into())]], false, Some(1));
            assert!(!grid.writable(), "{sql} 应当只读");
            assert_eq!(grid.readonly_reason(), reason, "{sql}");
            // 原因不是键本身：两个语言都必须有这句说明。
            for locale in [Locale::ZhCn, Locale::En] {
                assert_ne!(i18n::text(locale, reason), reason, "{reason} 缺少文案");
            }
        }
        // 单表 SELECT 在确认唯一键前给出专门原因，而不是笼统的"只读"。
        let mut pending = SharedGrid::new();
        pending.target.engine = "sqlite".into();
        pending.target.connection_id = "notes".into();
        pending.show_query_sheet("select name from keyed", vec!["name".into()], vec![vec![Some("甲".into())]], false, Some(1));
        assert_eq!(pending.readonly_reason(), "grid.readonly.locatePending");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reference_and_cancelled_edit_keep_selection_scroll_staging_and_viewer() {
        let (dir, service) = sqlite_fixture(
            "state-keep",
            &[
                "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT)",
                "CREATE TABLE books (id INTEGER PRIMARY KEY, author_id INTEGER, title TEXT, FOREIGN KEY (author_id) REFERENCES authors(id))",
                "INSERT INTO authors VALUES (1, '甲'), (2, '乙')",
                "INSERT INTO books VALUES (1, 1, '书'), (2, 2, '册')",
            ],
        )
        .await;
        let mut grid = load_table(&service, "books", 1024).await;
        grid.select(1, 1, false);
        let selected = grid.selection_ids().map(|(anchor, focus)| (anchor.to_owned(), focus.to_owned()));
        assert!(selected.is_some());
        let scroll = grid.visible_bounds().0;
        assert_eq!(grid.stage_text(0, 2, Some("改名".into())), Gate::Writable);
        grid.open_value(1, 1);
        let viewer = grid.viewer_state().cloned();
        // 外键导航只读目标，不动选择、滚动、暂存和查看目标。
        let jump = grid.reference(1, 1).expect("单列外键");
        assert_eq!(jump.values, vec![Some("2".to_owned())]);
        assert_eq!(grid.selection_ids().map(|(anchor, focus)| (anchor.to_owned(), focus.to_owned())), selected);
        assert_eq!(grid.visible_bounds().0, scroll);
        assert!(grid.uncommitted(), "暂存仍然保留");
        assert_eq!(grid.viewer_state().cloned(), viewer);
        // 取消编辑（撤销暂存）同样保留选择与查看目标。
        assert!(grid.undo_last_stage());
        assert!(!grid.uncommitted());
        assert_eq!(grid.selection_ids().map(|(anchor, focus)| (anchor.to_owned(), focus.to_owned())), selected);
        assert_eq!(grid.viewer_state().cloned(), viewer);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 键盘移动：PageDown/PageUp 按可视行数走，Tab/Shift+Tab 逐格走，越界时 Tab 报告“没动”，
    /// 让调用方放行焦点离开网格；滚动始终跟在焦点后面。
    #[test]
    fn paging_and_tab_move_the_focus_and_keep_the_scroll_following_it() {
        let mut grid = SharedGrid::new();
        let columns = vec![text_column("id"), text_column("name"), text_column("note")];
        let rows = (0..30).map(|index| row(&index.to_string(), vec![Some("1"), Some("name"), Some("note")], true)).collect::<Vec<_>>();
        grid.load_local(columns, rows, Some(30), None);
        grid.set_viewport(5);
        grid.select(0, 0, false);
        assert_eq!(grid.viewport_rows(), 5);
        grid.move_key("pagedown", false);
        assert_eq!(grid.focus_cell(), Some((5, 0)), "PageDown 走一屏");
        assert_eq!(grid.visible_bounds().0, 1, "滚动跟着焦点走到窗口最下一行");
        grid.move_key("pagedown", false);
        assert_eq!(grid.focus_cell(), Some((10, 0)));
        assert_eq!(grid.visible_bounds().0, 6);
        grid.move_key("pageup", false);
        assert_eq!(grid.focus_cell(), Some((5, 0)), "PageUp 回退一屏");
        assert_eq!(grid.visible_bounds().0, 5, "向上翻页时焦点落在窗口第一行");
        grid.move_key("pageup", false);
        assert_eq!(grid.focus_cell(), Some((0, 0)));
        assert_eq!(grid.visible_bounds().0, 0, "焦点到顶时滚动也到顶");
        // Tab 在可见列之间逐格走，行尾换行；隐藏列不接住 Tab。
        assert!(grid.hide_column(2), "第三列可隐藏");
        assert!(grid.move_tab(true));
        assert_eq!(grid.focus_cell(), Some((0, 1)));
        assert!(grid.move_tab(true));
        assert_eq!(grid.focus_cell(), Some((1, 0)), "隐藏列被跳过，行尾换到下一行首格");
        assert!(grid.move_tab(false));
        assert_eq!(grid.focus_cell(), Some((0, 1)), "Shift+Tab 回到上一行末个可见列");
        grid.select(29, 1, false);
        assert!(!grid.move_tab(true), "末格处 Tab 不再拦，焦点可以离开网格");
        assert_eq!(grid.focus_cell(), Some((29, 1)));
        grid.select(0, 0, false);
        assert!(!grid.move_tab(false), "首格处 Shift+Tab 同样放行");
    }

    /// NULL、空字符串和真的存着 "NULL" 的文本是三种语义：界面按语义画，不按显示文本猜。
    #[test]
    fn cell_kind_separates_null_empty_and_the_text_null() {
        let mut grid = SharedGrid::new();
        grid.load_local(
            vec![text_column("id"), text_column("name")],
            vec![
                row("1", vec![Some("1"), None], true),
                row("2", vec![Some("2"), Some("")], true),
                row("3", vec![Some("3"), Some("NULL")], true),
            ],
            Some(3),
            None,
        );
        assert_eq!(grid.cell_kind_at(0, 1), CellKind::Null);
        assert_eq!(grid.cell_kind_at(1, 1), CellKind::Empty);
        assert_eq!(grid.cell_kind_at(2, 1), CellKind::Text, "存着 NULL 的字符串是普通文本");
        assert_eq!(grid.cell_label(2, 1), "NULL", "文本 NULL 原样显示");
        assert_ne!(grid.cell_label(1, 1), grid.cell_label(0, 1), "空字符串与 NULL 的显示不同");
    }

}

/// 多格式复制、列显示和批量写入（R2/R3/R4）。
#[cfg(test)]
mod batch_tests {
    use super::*;

    fn column(name: &str, type_name: &str, primary: bool, auto: bool) -> GridColumn {
        GridColumn { name: name.to_owned(), type_name: type_name.to_owned(), nullable: !primary, primary, unique: primary, auto, width: 120.0, editable: true }
    }

    fn people_schema(engine: &str, note_type: &str) -> TableSchema {
        schema_from_parts(
            engine,
            "",
            "people",
            "table",
            vec![column_of("id", "integer", true), column_of("name", "text", false), column_of("note", note_type, false)],
            vec!["id".to_owned()],
        )
        .unwrap()
    }

    fn people_row(id: &str, name: Option<&str>, note: Option<&str>) -> GridRow {
        GridRow {
            id: id.to_owned(),
            values: vec![Some(id.to_owned()), name.map(str::to_owned), note.map(str::to_owned)],
            located: true,
            locator: vec![("id".to_owned(), Some(id.to_owned()))],
            inserted: false,
        }
    }

    fn people_columns(note_type: &str) -> Vec<GridColumn> {
        vec![column("id", "integer", true, false), column("name", "text", false, false), column("note", note_type, false, false)]
    }

    fn people_target(engine: &str) -> GridTarget {
        GridTarget { connection_id: "notes".into(), database: "main".into(), table: "people".into(), engine: engine.into(), ..GridTarget::default() }
    }

    /// 可写的 people 表：id 是主键（非自增），name/note 是文本。
    fn people(engine: &str, rows: usize) -> SharedGrid {
        let mut grid = SharedGrid::new();
        grid.target = people_target(engine);
        let rows = (1..=rows).map(|index| people_row(&index.to_string(), Some(&format!("名{index}")), Some("备注"))).collect::<Vec<_>>();
        let count = rows.len() as i64;
        grid.load_local(people_columns("text"), rows, Some(count), Some(people_schema(engine, "text")));
        grid
    }

    /// 2 行 × 3 列，含一个 NULL 和一个带逗号与引号的文本。
    fn copy_sheet(engine: &str) -> SharedGrid {
        let mut grid = SharedGrid::new();
        grid.target = people_target(engine);
        grid.load_local(
            people_columns("text"),
            vec![people_row("1", Some("a,\"b\""), None), people_row("2", Some("乙"), Some("x"))],
            Some(2),
            Some(people_schema(engine, "text")),
        );
        grid.select(0, 0, false);
        grid.select(1, 2, true);
        grid
    }

    fn copied(grid: &SharedGrid, format: CopyFormat) -> String {
        grid.copy_as(format).expect("可以复制").text
    }

    #[test]
    fn copy_as_each_format_matches_the_expected_text_byte_for_byte() {
        let grid = copy_sheet("sqlite");
        assert_eq!(copied(&grid, CopyFormat::TsvHeader), "id\tname\tnote\n1\t\"a,\"\"b\"\"\"\tNULL\n2\t乙\tx");
        assert_eq!(copied(&grid, CopyFormat::Csv), "\"id\",\"name\",\"note\"\r\n\"1\",\"a,\"\"b\"\"\",NULL\r\n\"2\",\"乙\",\"x\"");
        assert_eq!(copied(&grid, CopyFormat::Json), "[{\"id\":\"1\",\"name\":\"a,\\\"b\\\"\",\"note\":null},{\"id\":\"2\",\"name\":\"乙\",\"note\":\"x\"}]");
        assert_eq!(copied(&grid, CopyFormat::Markdown), "| id | name | note |\r\n| --- | --- | --- |\r\n| 1 | a,\"b\" | NULL |\r\n| 2 | 乙 | x |");
        // NULL 复制标记来自设置：TSV/CSV 跟着变，JSON 仍是 null。
        let mut token = copy_sheet("sqlite");
        token.set_null_copy("(null)");
        assert_eq!(copied(&token, CopyFormat::TsvHeader).lines().nth(1), Some("1\t\"a,\"\"b\"\"\"\t(null)"));
        assert!(copied(&token, CopyFormat::Csv).contains(",(null)\r\n"));
        assert!(copied(&token, CopyFormat::Json).contains("\"note\":null"));

        let mysql = copy_sheet("mysql");
        let insert = mysql.copy_as(CopyFormat::Insert).unwrap();
        assert!(!insert.placeholder);
        assert_eq!(
            insert.text,
            "INSERT INTO `people` (`id`, `name`, `note`) VALUES (1, 'a,\"b\"', NULL);\r\nINSERT INTO `people` (`id`, `name`, `note`) VALUES (2, '乙', 'x');"
        );
        assert_eq!(insert.text.matches("INSERT INTO").count(), 2);
        let postgres = copy_sheet("postgres");
        assert!(copied(&postgres, CopyFormat::Insert).starts_with("INSERT INTO \"people\" (\"id\", \"name\", \"note\") VALUES (1, "));
    }

    #[test]
    fn copy_as_insert_uses_a_placeholder_for_unlocated_results_and_blocks_missing_values() {
        let mut query = SharedGrid::new();
        query.target.engine = "sqlite".into();
        query.show_query_sheet("select a.name from a join b on a.id = b.id", vec!["name".into()], vec![vec![Some("甲".into())]], false, Some(1));
        assert_eq!(query.copy_as(CopyFormat::Insert), Err(CopyBlock::Empty), "空选区不能复制");
        assert_eq!(query.copy_as(CopyFormat::Csv), Err(CopyBlock::Empty));
        query.select(0, 0, false);
        let insert = query.copy_as(CopyFormat::Insert).unwrap();
        assert!(insert.placeholder, "没有定位到表时要提示占位表名");
        assert_eq!(insert.text, format!("INSERT INTO \"{PLACEHOLDER_TABLE}\" (\"name\") VALUES ('甲');"));

        // 选区里有只读到预览、还没读到原值的格子：任何格式都停下，不拿预览信封充当原值。
        let envelope = format!("\u{1}t:3000:{}", "a".repeat(16));
        let mut grid = SharedGrid::new();
        grid.target = people_target("sqlite");
        grid.load_local(people_columns("text"), vec![people_row("1", Some(envelope.as_str()), None)], Some(1), Some(people_schema("sqlite", "text")));
        grid.select_all_loaded();
        for format in CopyFormat::ALL {
            assert_eq!(grid.copy_as(format), Err(CopyBlock::Missing), "{format:?}");
        }
    }

    #[test]
    fn hiding_and_freezing_change_only_the_display() {
        let mut grid = people("sqlite", 3);
        assert_eq!(grid.visible_columns(), vec![0, 1, 2]);
        grid.select(0, 1, false);
        assert!(grid.hide_column(1));
        assert_eq!(grid.visible_columns(), vec![0, 2]);
        assert_eq!(grid.display_to_logical(1), Some(2));
        assert_eq!(grid.logical_to_display(1), None);
        assert_eq!(grid.hidden_columns(), vec![(1, "name".to_owned())]);
        // 焦点不会停在被藏起来的列上。
        assert_eq!(grid.focus_cell(), Some((0, 2)));
        // 复制整行仍包含隐藏列；导出看到的列也不变。
        assert_eq!(grid.copy_row(), "1\t名1\t备注");
        assert_eq!(grid.exchange_sheet().1, vec!["id".to_owned(), "name".to_owned(), "note".to_owned()]);
        // 方向键跳过隐藏列。
        grid.select(0, 0, false);
        grid.move_key("right", false);
        assert_eq!(grid.focus_cell(), Some((0, 2)));
        grid.move_key("left", false);
        assert_eq!(grid.focus_cell(), Some((0, 0)));
        // 只剩一列可见时不能再藏。
        assert!(grid.hide_column(2));
        assert!(!grid.can_hide(0));
        assert!(!grid.hide_column(0));
        assert_eq!(grid.visible_columns(), vec![0]);
        assert!(grid.show_column(1));
        assert_eq!(grid.visible_columns(), vec![0, 1]);
        assert!(grid.show_all_columns());
        assert_eq!(grid.visible_columns(), vec![0, 1, 2]);
        // 冻结到第 2 列：前两个可见列固定在左侧；排序不改变它。
        assert!(grid.freeze_to(1));
        assert_eq!(grid.frozen_count(), 2);
        grid.toggle_sort("note");
        assert_eq!(grid.frozen_count(), 2);
        assert!(grid.unfreeze());
        assert_eq!(grid.frozen_count(), 0);
        // 换到另一张表时隐藏和冻结都不带过去。
        assert!(grid.hide_column(2));
        assert!(grid.freeze_to(1));
        let other = GridTarget { table: "other".into(), ..grid.target.clone() };
        grid.switch_target(other);
        assert!(grid.hidden_columns().is_empty());
        assert_eq!(grid.frozen_column(), None);
    }

    #[test]
    fn pasting_two_by_two_tsv_stages_exactly_four_changes() {
        let mut grid = people("sqlite", 3);
        grid.select(0, 1, false);
        assert_eq!(grid.paste_tsv(Some("甲\t一\r\n乙\t二\r\n")), Ok(4));
        assert_eq!(grid.staged_count(), 4);
        let sql = grid.open_preview().unwrap();
        assert_eq!(sql.len(), 4, "{sql:?}");
        assert_eq!(grid.cell_label(0, 1), "甲");
        assert_eq!(grid.cell_label(1, 2), "二");
        // 一次撤销回退整批。
        grid.cancel_preview();
        assert!(grid.undo_last_stage());
        assert_eq!(grid.staged_count(), 0);
        assert!(!grid.uncommitted());
        assert_eq!(grid.cell_label(0, 1), "名1");
    }

    #[test]
    fn rejected_pastes_leave_staging_and_overlays_untouched() {
        let mut grid = people("sqlite", 3);
        assert_eq!(grid.stage_text(2, 2, Some("已有".into())), Gate::Writable);
        let before = (grid.staged_count(), grid.cell_label(2, 2), grid.cell_label(0, 1));
        // 焦点在最后一列：第二列越界。
        grid.select(2, 2, false);
        assert_eq!(grid.paste_tsv(Some("a\tb")), Err(BatchReject::OutOfRange));
        // 超出已加载的行，不自动新增行。
        grid.select(2, 1, false);
        assert_eq!(grid.paste_tsv(Some("a\nb")), Err(BatchReject::OutOfRange));
        assert_eq!(grid.loaded_len(), 3);
        // 目标含主键列。
        grid.select(0, 0, false);
        assert_eq!(grid.paste_tsv(Some("9\tx")), Err(BatchReject::KeyColumn("id".into())));
        // 剪贴板为空或不是文本。
        grid.select(0, 1, false);
        assert_eq!(grid.paste_tsv(Some("")), Err(BatchReject::Empty));
        assert_eq!(grid.paste_tsv(Some("\r\n")), Err(BatchReject::Empty));
        assert_eq!(grid.paste_tsv(None), Err(BatchReject::Empty));
        // 隐藏列不算在可铺开的列里。
        assert!(grid.hide_column(2));
        grid.select(0, 1, false);
        assert_eq!(grid.paste_tsv(Some("a\tb")), Err(BatchReject::OutOfRange));
        assert_eq!((grid.staged_count(), grid.cell_label(2, 2), grid.cell_label(0, 1)), before);
        for reject in [BatchReject::Empty, BatchReject::OutOfRange, BatchReject::KeyColumn("id".into())] {
            assert!(!reject.message(Locale::En).is_empty());
            assert_ne!(reject.message(Locale::ZhCn), reject.message(Locale::En));
        }
    }

    #[test]
    fn readonly_results_reject_every_batch_with_the_existing_reason() {
        let mut query = SharedGrid::new();
        query.target.engine = "sqlite".into();
        query.show_query_sheet("select * from a join b on a.id = b.id", vec!["name".into()], vec![vec![Some("甲".into())]], false, Some(1));
        query.select(0, 0, false);
        let reason = query.gate_text(Gate::Readonly);
        assert_eq!(reason, i18n::text(Locale::ZhCn, "grid.readonly.join"));
        assert_eq!(query.paste_tsv(Some("x")), Err(BatchReject::Readonly(reason.clone())));
        assert_eq!(query.stage_selection(None), Err(BatchReject::Readonly(reason.clone())));
        assert_eq!(query.stage_clone(), Err(reason));
        assert!(!query.uncommitted());
    }

    #[test]
    fn tsv_paste_reads_quotes_and_the_null_copy_token() {
        assert_eq!(parse_tsv("a\tb\nc\td\n", "NULL"), vec![vec![Some("a".into()), Some("b".into())], vec![Some("c".into()), Some("d".into())]]);
        assert_eq!(parse_tsv("NULL\t\"NULL\"\t", "NULL"), vec![vec![None, Some("NULL".into()), Some(String::new())]]);
        assert_eq!(parse_tsv("\"x\ty\"\t\"多\n行\"\t\"a\"\"b\"", "NULL"), vec![vec![Some("x\ty".into()), Some("多\n行".into()), Some("a\"b".into())]]);
        // NULL 复制标记为空时，空字段是空字符串而不是 NULL。
        assert_eq!(parse_tsv("\t", ""), vec![vec![Some(String::new()), Some(String::new())]]);
        // 本网格复制出来的 TSV 粘贴回去得到同样的值。
        let grid = copy_sheet("sqlite");
        let tsv = grid.tsv();
        assert_eq!(
            parse_tsv(&tsv, grid.null_copy()),
            vec![vec![Some("1".into()), Some("a,\"b\"".into()), None], vec![Some("2".into()), Some("乙".into()), Some("x".into())]]
        );
    }

    #[test]
    fn undoing_a_batch_restores_replaced_changes_in_original_order() {
        let mut grid = people("sqlite", 3);
        // A = (0, name)，B = (0, note)，C = (1, name)。
        assert_eq!(grid.stage_text(0, 1, Some("改A".into())), Gate::Writable);
        assert_eq!(grid.stage_text(0, 2, Some("改B".into())), Gate::Writable);
        grid.select(0, 1, false);
        grid.select(1, 1, true);
        assert_eq!(grid.stage_selection(Some("批".into())), Ok(2));
        assert_eq!(grid.cell_label(0, 1), "批");
        assert_eq!(grid.cell_label(1, 1), "批");
        // 撤销整批：A 回到批量前的暂存值，C 回到原值，B 不动。
        assert!(grid.undo_last_stage());
        assert_eq!(grid.staged_count(), 2);
        assert_eq!(grid.cell_label(0, 1), "改A");
        assert_eq!(grid.cell_label(0, 2), "改B");
        assert_eq!(grid.cell_label(1, 1), "名2");
        // 下一次撤销按原暂存顺序先撤 B，A 保持。
        assert!(grid.undo_last_stage());
        assert_eq!(grid.staged_count(), 1);
        assert_eq!(grid.cell_label(0, 1), "改A");
        assert_eq!(grid.cell_label(0, 2), "备注");
        // 再撤销才轮到 A。
        assert!(grid.undo_last_stage());
        assert_eq!(grid.staged_count(), 0);
        assert_eq!(grid.cell_label(0, 1), "名1");
        assert!(!grid.undo_last_stage());
    }

    #[test]
    fn setting_selected_cells_to_null_is_one_batch_and_undoes_as_one() {
        let mut grid = people("sqlite", 3);
        // 先有一条单独的修改：整批撤销后它要回来。
        assert_eq!(grid.stage_text(0, 1, Some("先改".into())), Gate::Writable);
        grid.select(0, 1, false);
        grid.select(2, 1, true);
        assert_eq!(grid.stage_selection(None), Ok(3));
        assert_eq!(grid.staged_count(), 3, "同一格的旧修改被这一批替换");
        let sql = grid.open_preview().unwrap();
        assert_eq!(sql.len(), 3);
        assert!(sql.iter().all(|item| item.contains("= NULL")), "{sql:?}");
        grid.cancel_preview();
        assert!(grid.undo_last_stage());
        assert_eq!(grid.staged_count(), 1, "整批撤销，原来的单独修改放回来");
        assert_eq!(grid.cell_label(0, 1), "先改");
        assert_eq!(grid.cell_label(1, 1), "名2");
        // 选区含主键：整批拒绝，暂存不变。
        grid.select(0, 0, false);
        grid.select(1, 1, true);
        assert_eq!(grid.stage_selection(Some("x".into())), Err(BatchReject::KeyColumn("id".into())));
        assert_eq!(grid.staged_count(), 1);
        assert_eq!(grid.cell_label(1, 1), "名2");
        // 校验失败的格子也让整批不生效。
        let mut typed = SharedGrid::new();
        typed.target = people_target("sqlite");
        typed.load_local(
            people_columns("geometry"),
            vec![people_row("1", Some("甲"), None), people_row("2", Some("乙"), None)],
            Some(2),
            Some(people_schema("sqlite", "geometry")),
        );
        typed.select(0, 1, false);
        typed.select(1, 2, true);
        assert!(matches!(typed.stage_selection(Some("v".into())), Err(BatchReject::Blocked { .. })));
        assert!(!typed.uncommitted());
    }

    #[test]
    fn cloning_a_row_leaves_key_columns_unassigned_and_copies_the_rest() {
        let mut grid = people("sqlite", 2);
        grid.select(1, 2, false);
        assert_eq!(grid.stage_clone(), Ok(()));
        assert_eq!(grid.loaded_len(), 3);
        let sql = grid.open_preview().unwrap();
        assert_eq!(sql, vec!["INSERT INTO \"people\" (\"name\", \"note\") VALUES ('名2', '备注')".to_owned()]);
        let request = grid.confirm_request().unwrap();
        assert_eq!(request.changes.len(), 1);
        assert_eq!(request.changes[0].kind, "insert");
        assert!(request.changes[0].values.iter().all(|value| value.column != "id"));
        // 撤销一次就去掉这条新行。
        grid.cancel_preview();
        assert!(grid.undo_last_stage());
        assert_eq!(grid.loaded_len(), 2);
        assert!(!grid.uncommitted());
        // 新行里再粘贴：值跟着那条 INSERT 走，撤销这一批后回到复制来的值。
        grid.select(1, 1, false);
        assert_eq!(grid.stage_clone(), Ok(()));
        let inserted = grid.rows.iter().position(|row| row.inserted).unwrap();
        grid.select(inserted, 1, false);
        assert_eq!(grid.paste_tsv(Some("新名")), Ok(1));
        let sql = grid.open_preview().unwrap();
        assert_eq!(sql, vec!["INSERT INTO \"people\" (\"name\", \"note\") VALUES ('新名', '备注')".to_owned()]);
        grid.cancel_preview();
        assert!(grid.undo_last_stage());
        assert_eq!(grid.open_preview().unwrap(), vec!["INSERT INTO \"people\" (\"name\", \"note\") VALUES ('名2', '备注')".to_owned()]);
    }

    async fn item_rows(path: &std::path::Path) -> Vec<(i64, String, Option<i64>)> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(path).read_only(true))
            .await
            .unwrap();
        let rows: Vec<(i64, String, Option<i64>)> = sqlx::query_as("SELECT id, name, qty FROM items ORDER BY id").fetch_all(&pool).await.unwrap();
        pool.close().await;
        rows
    }

    /// 真实 SQLite：复制为新行后取消预览，库里不变；确认后多一行，主键由库分配，其余列与原行一致。
    #[tokio::test]
    async fn sqlite_clone_cancel_leaves_the_database_and_confirm_inserts_one_row() {
        let dir = std::env::temp_dir().join(format!("rillsql-grid-clone-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("notes.sqlite");
        let setup = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
            .await
            .unwrap();
        sqlx::query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, qty INTEGER)").execute(&setup).await.unwrap();
        sqlx::query("INSERT INTO items (id, name, qty) VALUES (1, '甲', 3), (2, '乙', NULL)").execute(&setup).await.unwrap();
        setup.close().await;
        let service = std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")));
        service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
        let table = TableRef { connection_id: "notes".into(), database: "main".into(), schema: String::new(), table: "items".into(), engine: "sqlite".into(), include_foreign_keys: false };
        let schema = schema_from_dto(&crate::application::grid::describe(&service, &table).await.unwrap()).unwrap();
        let page = crate::application::grid::page(
            &service,
            &PageRequest { table, filters: Vec::new(), sort: None, offset: 0, limit: 10, preview_bytes: 1024, search: None },
        )
        .await
        .unwrap();
        let mut grid = SharedGrid::new();
        grid.target = GridTarget { connection_id: "notes".into(), database: "main".into(), table: "items".into(), engine: "sqlite".into(), ..GridTarget::default() };
        let columns = columns_from_schema(&schema, true);
        grid.replace_fetched(columns.clone(), rows_from_page(&columns, &page), 0, Some(2), false, Some(schema));
        assert!(grid.writable());
        grid.select(0, 1, false);
        assert_eq!(grid.stage_clone(), Ok(()));
        let preview = grid.open_preview().unwrap();
        assert_eq!(preview.len(), 1);
        assert!(!preview[0].contains("\"id\""), "{preview:?}");
        grid.cancel_preview();
        grid.close_preview();
        let original = vec![(1, "甲".to_owned(), Some(3)), (2, "乙".to_owned(), None)];
        assert_eq!(item_rows(&database).await, original, "取消预览后库里不变");
        grid.open_preview().unwrap();
        let request = grid.confirm_request().unwrap();
        let report = crate::application::grid::commit(&service, &request, true).await.unwrap();
        assert!(report.failed.is_none(), "{:?}", report.failed.as_ref().map(|item| item.message.clone()));
        grid.apply_commit(&report);
        assert!(!grid.uncommitted());
        assert_eq!(item_rows(&database).await, vec![(1, "甲".to_owned(), Some(3)), (2, "乙".to_owned(), None), (3, "甲".to_owned(), Some(3))]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staged_counts_revert_one_cell_or_row_and_sql_stays_off_previews() {
        let mut grid = people("sqlite", 2);
        assert_eq!(grid.stage_text(0, 1, Some("改".into())), Gate::Writable);
        assert_eq!(grid.stage_insert(), Gate::Writable);
        assert_eq!(grid.stage_delete(1), Gate::Writable);
        let summary = grid.staged_summary();
        assert_eq!((summary.inserts, summary.updates, summary.deletes), (1, 1, 1));
        assert_eq!(grid.cell_dirty_state(0, 1), CellDirty::Updated);
        assert_eq!(grid.cell_dirty_state(1, 0), CellDirty::Deleted);
        let inserted = grid.rows.iter().position(|row| row.inserted).expect("新增行");
        assert_eq!(grid.cell_dirty_state(inserted, 1), CellDirty::Inserted);
        let id = SharedGrid::row_id(&grid, 0).unwrap().to_owned();
        assert!(grid.revert_cell(&id, 1));
        assert_eq!(grid.cell_label(0, 1), "名1");
        assert_eq!(grid.staged_summary().updates, 0);
        assert!(!grid.revert_cell(&id, 1));
        let next = grid.next_staged_cell().expect("还有暂存");
        grid.select(next.0, next.1, false);
        let row_id = SharedGrid::row_id(&grid, next.0).unwrap().to_owned();
        assert!(grid.revert_row(&row_id));
        grid.discard_all_staged();
        assert_eq!(grid.staged_count(), 0);
        assert!(grid.next_staged_cell().is_none());

        let mut grid = people("sqlite", 1);
        grid.select(0, 1, false);
        let select_sql = grid.select_statement();
        assert!(select_sql.starts_with("SELECT * FROM \"people\" WHERE "), "{select_sql}");
        assert!(select_sql.contains("\"id\" = 1"), "{select_sql}");
        let update_sql = grid.update_statement();
        assert!(update_sql.starts_with("UPDATE \"people\" SET \"name\" = "), "{update_sql}");
        assert!(update_sql.contains("WHERE "), "{update_sql}");
        assert!(!select_sql.contains('\u{1}') && !update_sql.contains('\u{1}'));
        assert_eq!(grid.stage_text(0, 1, Some("改".into())), Gate::Writable);
        let staged = grid.update_statement();
        assert!(staged.contains("\"name\" = '改'"), "{staged}");

        let envelope = format!("\u{1}t:5000:{}", "z".repeat(8));
        let mut wide = SharedGrid::new();
        wide.load_local(
            vec![column("id", "integer", true, false), column("body", "text", false, false)],
            vec![GridRow {
                id: "1".into(),
                values: vec![Some("1".into()), Some(envelope.clone())],
                located: true,
                locator: vec![("id".into(), Some("1".into()))],
                inserted: false,
            }],
            Some(1),
            Some(schema_from_parts("sqlite", "", "notes", "table", vec![column_of("id", "integer", true), column_of("body", "text", false)], vec!["id".to_owned()]).unwrap()),
        );
        wide.select(0, 1, false);
        assert!(wide.needs_full(0, 1));
        assert!(wide.select_statement().is_empty(), "预览不能写进 SELECT");
        assert!(wide.update_statement().is_empty(), "预览不能写进 UPDATE");

        let mut json_grid = SharedGrid::new();
        json_grid.load_local(
            vec![column("id", "integer", true, false), column("payload", "jsonb", false, false)],
            vec![GridRow {
                id: "1".into(),
                values: vec![Some("1".into()), Some("{\"ok\":true}".into())],
                located: true,
                locator: vec![("id".into(), Some("1".into()))],
                inserted: false,
            }],
            Some(1),
            Some(schema_from_parts("sqlite", "", "docs", "table", vec![column_of("id", "integer", true), column_of("payload", "jsonb", false)], vec!["id".to_owned()]).unwrap()),
        );
        assert_eq!(json_grid.stage_text(0, 1, Some("{".into())), Gate::Validation);
        assert_eq!(json_grid.overlay_message(0, 1), "JSON 不合法，不能写回");
        assert!(json_grid.open_preview().is_err(), "非法 JSON 不进预览");
        assert_eq!(type_abbr("jsonb"), "JSON");
        assert_eq!(type_abbr("timestamp"), "TS");
        assert_eq!(type_abbr("bytea"), "BLOB");
        json_grid.set_foreign_keys(vec![ForeignKeyInfo {
            name: "fk".into(),
            columns: vec!["payload".into()],
            ref_schema: String::new(),
            ref_table: "other".into(),
            ref_columns: vec!["id".into()],
        }]);
        let header = json_grid.column_header(1).unwrap();
        assert!(header.foreign && header.abbr == "JSON");
        assert!(json_grid.column_header(0).unwrap().primary);
    }
}

/// 千分位分组：12345 → "12,345"。
pub fn group_digits(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if value < 0 { format!("-{out}") } else { out }
}
