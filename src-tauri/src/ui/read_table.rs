//! 只读数据表：基于组件库 `DataTable` 的通用 delegate。
//!
//! 页面只给列定义与行数据（静态二维表或按需取值的闭包），这里负责：
//! 虚拟化渲染、单元格 / 行 / 列选中、Shift 扩展选区、⌘C 复制、右键复制菜单、
//! 双击或回车发出 [`ReadTableEvent::Activate`]。空表时复制全部禁用。
//! 复制文本的生成是纯函数（[`copy_text`]），可编辑网格也能共用同一套格式。
#![allow(dead_code)] // 共享底座：本任务只建组件，各页面在后续任务里接入。

use std::ops::RangeInclusive;
use std::rc::Rc;

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableEvent, TableState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    anchored, deferred, div, px, Anchor, App, AppContext as _, ClipboardItem, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement as _, Pixels, Point, Render,
    SharedString, Styled as _, Subscription, TestSupportExt as _, WeakEntity, Window,
};

use gpui_kit::prelude::FluentBuilder as _;

use super::kit::{ControlSize, Palette, Space, Tip as _, TypeScale};
use crate::i18n::{self, Locale};

// ---------------------------------------------------------------- 纯数据与复制文本

/// 一列：标题、宽度、对齐与是否等宽。
#[derive(Clone, Debug, PartialEq)]
pub struct ReadColumn {
    pub title: SharedString,
    pub width: f32,
    pub align_right: bool,
    pub mono: bool,
}

impl ReadColumn {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self { title: title.into(), width: 140., align_right: false, mono: false }
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// 数字、大小、计数等右对齐。
    pub fn right(mut self) -> Self {
        self.align_right = true;
        self
    }

    /// 键名、命令、哈希等用等宽字体。
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }
}

/// 行数据：小表直接给二维表；大表或已有存储的给行数与按需取值的闭包，不复制一份。
#[derive(Clone)]
pub enum ReadRows {
    Static(Vec<Vec<SharedString>>),
    Lazy { count: usize, cell: Rc<dyn Fn(usize, usize) -> SharedString> },
}

impl Default for ReadRows {
    fn default() -> Self {
        ReadRows::Static(Vec::new())
    }
}

impl ReadRows {
    pub fn len(&self) -> usize {
        match self {
            ReadRows::Static(rows) => rows.len(),
            ReadRows::Lazy { count, .. } => *count,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn cell(&self, row: usize, col: usize) -> SharedString {
        match self {
            ReadRows::Static(rows) => rows.get(row).and_then(|cells| cells.get(col)).cloned().unwrap_or_default(),
            ReadRows::Lazy { count, cell } => {
                if row < *count {
                    cell(row, col)
                } else {
                    SharedString::default()
                }
            }
        }
    }
}

/// 选区的形状：一块单元格、若干整行、一整列。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionMode {
    Cells,
    Rows,
    Column,
}

/// 选区：锚点 + 焦点。Shift 扩展只移动焦点，矩形由两者张成。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadSelection {
    pub anchor: (usize, usize),
    pub focus: (usize, usize),
    pub mode: SelectionMode,
}

impl ReadSelection {
    pub fn cell(row: usize, col: usize) -> Self {
        Self { anchor: (row, col), focus: (row, col), mode: SelectionMode::Cells }
    }

    /// 选区覆盖的行列（闭区间），按表的行列数收边；表为空或列为空时为 None。
    pub fn region(&self, rows: usize, cols: usize) -> Option<(RangeInclusive<usize>, RangeInclusive<usize>)> {
        if rows == 0 || cols == 0 {
            return None;
        }
        let clamp_row = |r: usize| r.min(rows - 1);
        let clamp_col = |c: usize| c.min(cols - 1);
        let (r0, r1) = (clamp_row(self.anchor.0.min(self.focus.0)), clamp_row(self.anchor.0.max(self.focus.0)));
        let (c0, c1) = (clamp_col(self.anchor.1.min(self.focus.1)), clamp_col(self.anchor.1.max(self.focus.1)));
        Some(match self.mode {
            SelectionMode::Cells => (r0..=r1, c0..=c1),
            SelectionMode::Rows => (r0..=r1, 0..=cols - 1),
            SelectionMode::Column => (0..=rows - 1, c0..=c1),
        })
    }

    pub fn contains(&self, row: usize, col: usize, rows: usize, cols: usize) -> bool {
        self.region(rows, cols).is_some_and(|(r, c)| r.contains(&row) && c.contains(&col))
    }
}

/// 右键菜单与快捷键可选的复制方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyKind {
    /// 焦点格的值本身。
    Value,
    /// 选区覆盖的整行，制表符分隔，不含表头。
    Row,
    /// 焦点列在选区行里的值，一行一个。
    Column,
    /// 选区，含表头的 TSV。
    Tsv,
    /// 选区，含表头的 CSV（全部加引号，CRLF 换行，与导出一致）。
    Csv,
    /// 选区，对象数组，键为列标题。
    Json,
}

impl CopyKind {
    pub const MENU: [CopyKind; 6] = [CopyKind::Value, CopyKind::Row, CopyKind::Column, CopyKind::Tsv, CopyKind::Csv, CopyKind::Json];

    pub fn id(self) -> &'static str {
        match self {
            CopyKind::Value => "read-table-copy-value",
            CopyKind::Row => "read-table-copy-row",
            CopyKind::Column => "read-table-copy-column",
            CopyKind::Tsv => "read-table-copy-tsv",
            CopyKind::Csv => "read-table-copy-csv",
            CopyKind::Json => "read-table-copy-json",
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            CopyKind::Value => "readTable.copyValue",
            CopyKind::Row => "readTable.copyRow",
            CopyKind::Column => "readTable.copyColumn",
            CopyKind::Tsv => "readTable.copyTsv",
            CopyKind::Csv => "readTable.copyCsv",
            CopyKind::Json => "readTable.copyJson",
        }
    }
}

/// TSV 字段：含制表符、换行或引号时整体加引号，内部引号加倍（与可编辑网格一致）。
pub fn quote_tsv(value: &str) -> String {
    if value.contains('\t') || value.contains('\n') || value.contains('\r') || value.contains('"') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn quote_csv(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

/// 按复制方式生成文本。表为空或没有选区时为 None（调用方据此保持剪贴板不变）。
pub fn copy_text(columns: &[ReadColumn], rows: &ReadRows, selection: Option<ReadSelection>, kind: CopyKind) -> Option<String> {
    let selection = selection?;
    let (row_range, col_range) = selection.region(rows.len(), columns.len())?;
    let focus = (selection.focus.0.min(rows.len() - 1), selection.focus.1.min(columns.len() - 1));
    let line = |row: usize, cols: RangeInclusive<usize>, quote: fn(&str) -> String, sep: &str| {
        cols.map(|col| quote(&rows.cell(row, col))).collect::<Vec<_>>().join(sep)
    };
    let header = |cols: RangeInclusive<usize>, quote: fn(&str) -> String, sep: &str| {
        cols.map(|col| quote(&columns[col].title)).collect::<Vec<_>>().join(sep)
    };
    Some(match kind {
        CopyKind::Value => rows.cell(focus.0, focus.1).to_string(),
        CopyKind::Row => row_range.map(|row| line(row, 0..=columns.len() - 1, quote_tsv, "\t")).collect::<Vec<_>>().join("\n"),
        CopyKind::Column => {
            let col = if selection.mode == SelectionMode::Column { *col_range.start() } else { focus.1 };
            row_range.map(|row| quote_tsv(&rows.cell(row, col))).collect::<Vec<_>>().join("\n")
        }
        CopyKind::Tsv => {
            let mut lines = vec![header(col_range.clone(), quote_tsv, "\t")];
            lines.extend(row_range.map(|row| line(row, col_range.clone(), quote_tsv, "\t")));
            lines.join("\n")
        }
        CopyKind::Csv => {
            let mut lines = vec![header(col_range.clone(), quote_csv, ",")];
            lines.extend(row_range.map(|row| line(row, col_range.clone(), quote_csv, ",")));
            lines.join("\r\n")
        }
        CopyKind::Json => {
            let objects = row_range.map(|row| {
                let fields = col_range
                    .clone()
                    .map(|col| format!("{}:{}", json_string(&columns[col].title), json_string(&rows.cell(row, col))))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("{{{fields}}}")
            });
            format!("[{}]", objects.collect::<Vec<_>>().join(","))
        }
    })
}

/// ⌘C 的文本：单格复制值本身，多格复制含表头的 TSV（与可编辑网格的 copy-region 一致）。
pub fn clipboard_text(columns: &[ReadColumn], rows: &ReadRows, selection: Option<ReadSelection>) -> Option<String> {
    let (r, c) = selection?.region(rows.len(), columns.len())?;
    let single = r.start() == r.end() && c.start() == c.end();
    copy_text(columns, rows, selection, if single { CopyKind::Value } else { CopyKind::Tsv })
}

/// 右键菜单条目：(复制方式, 是否禁用)。空表或没有选区时全部禁用。
pub fn menu_entries(rows: usize, cols: usize, selection: Option<ReadSelection>) -> Vec<(CopyKind, bool)> {
    let enabled = selection.is_some_and(|s| s.region(rows, cols).is_some());
    CopyKind::MENU.iter().map(|kind| (*kind, !enabled)).collect()
}

// ---------------------------------------------------------------- 行列表的多选

/// 点选一行时的修饰：普通单击替换选区，Shift 扩展到这一行，⌘/Ctrl 切换这一行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowPick {
    Replace,
    Extend,
    Toggle,
}

impl RowPick {
    pub fn from_modifiers(shift: bool, secondary: bool) -> Self {
        if shift {
            RowPick::Extend
        } else if secondary {
            RowPick::Toggle
        } else {
            RowPick::Replace
        }
    }
}

/// 以行为单位的只读列表（库概览、历史等）的多选，与 `ReadTable` 的 Shift 扩展、复制规则一致。
///
/// 列表自己的“焦点行”（概览的 `selected`、历史的 `selected_run`）仍是唯一的当前行，
/// 这里只记锚点与额外选中的行。焦点不在集合里时（键盘移动、外部直接改了焦点）只算焦点一行，
/// 所以旧的单选代码路径不需要知道多选的存在。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowMarks {
    anchor: Option<String>,
    ids: Vec<String>,
}

impl RowMarks {
    pub fn clear(&mut self) {
        self.anchor = None;
        self.ids.clear();
    }

    /// 选中行在 `visible` 中的下标，升序。
    pub fn selected(&self, visible: &[String], focus: Option<&str>) -> Vec<usize> {
        let Some(focus) = focus else { return Vec::new() };
        if self.ids.iter().any(|id| id == focus) {
            visible.iter().enumerate().filter(|(_, id)| self.ids.contains(id)).map(|(index, _)| index).collect()
        } else {
            visible.iter().position(|id| id == focus).into_iter().collect()
        }
    }

    pub fn is_selected(&self, visible: &[String], focus: Option<&str>, id: &str) -> bool {
        match focus {
            Some(focus) if self.ids.iter().any(|item| item == focus) => self.ids.iter().any(|item| item == id) && visible.iter().any(|item| item == id),
            Some(focus) => focus == id,
            None => false,
        }
    }

    /// 点选 `target`，返回新的焦点行。目标不在可见行里时按普通单击处理。
    pub fn pick(&mut self, visible: &[String], focus: Option<&str>, target: &str, how: RowPick) -> Option<String> {
        let Some(target_ix) = visible.iter().position(|id| id == target) else {
            self.anchor = Some(target.to_owned());
            self.ids = vec![target.to_owned()];
            return Some(target.to_owned());
        };
        match how {
            RowPick::Replace => {
                self.anchor = Some(target.to_owned());
                self.ids = vec![target.to_owned()];
                Some(target.to_owned())
            }
            RowPick::Extend => {
                let anchor_ix = self
                    .anchor
                    .as_deref()
                    .filter(|anchor| self.ids.iter().any(|id| Some(id.as_str()) == focus) || focus == Some(*anchor))
                    .or(focus)
                    .and_then(|anchor| visible.iter().position(|id| id == anchor))
                    .unwrap_or(target_ix);
                self.anchor = Some(visible[anchor_ix].clone());
                let (from, to) = (anchor_ix.min(target_ix), anchor_ix.max(target_ix));
                self.ids = visible[from..=to].to_vec();
                Some(target.to_owned())
            }
            RowPick::Toggle => {
                let mut current: Vec<String> = self.selected(visible, focus).into_iter().map(|index| visible[index].clone()).collect();
                self.anchor = Some(target.to_owned());
                if let Some(position) = current.iter().position(|id| id == target) {
                    if current.len() == 1 {
                        // 最后一行不取消：列表总要有一个当前行。
                        self.ids = current;
                        return Some(target.to_owned());
                    }
                    current.remove(position);
                    let next = current[position.min(current.len() - 1)].clone();
                    self.ids = current;
                    Some(next)
                } else {
                    current.push(target.to_owned());
                    self.ids = current;
                    Some(target.to_owned())
                }
            }
        }
    }

    /// Shift+方向键：从焦点向上或向下扩展一行，返回新的焦点。
    pub fn extend_step(&mut self, visible: &[String], focus: Option<&str>, step: isize) -> Option<String> {
        if visible.is_empty() {
            return None;
        }
        let current = focus.and_then(|focus| visible.iter().position(|id| id == focus));
        let next = match current {
            Some(index) => (index as isize + step).clamp(0, visible.len() as isize - 1) as usize,
            None if step < 0 => visible.len() - 1,
            None => 0,
        };
        let focus = focus.filter(|_| current.is_some());
        self.pick(visible, focus, &visible[next].clone(), if focus.is_some() { RowPick::Extend } else { RowPick::Replace })
    }
}

/// 行列表的复制：`rows` 只含选中的行（按列表顺序），`focus_row` 是焦点行在其中的位置，
/// `column` 是右键所在的列。值 = 焦点行这一列；行 = 选中行，无表头；列 = 这一列在选中行里的值；
/// TSV / CSV / JSON = 选中行全部列，含表头。没有选中行时为 None。
pub fn row_list_copy_text(columns: &[ReadColumn], rows: &ReadRows, focus_row: usize, column: usize, kind: CopyKind) -> Option<String> {
    if rows.is_empty() || columns.is_empty() {
        return None;
    }
    let column = column.min(columns.len() - 1);
    if kind == CopyKind::Value {
        return Some(rows.cell(focus_row.min(rows.len() - 1), column).to_string());
    }
    let selection = ReadSelection { anchor: (0, column), focus: (rows.len() - 1, column), mode: SelectionMode::Rows };
    copy_text(columns, rows, Some(selection), kind)
}

/// 行列表的 ⌘C：一行复制这一行（制表符分隔，无表头），多行复制含表头的 TSV。
pub fn row_list_clipboard_text(columns: &[ReadColumn], rows: &ReadRows) -> Option<String> {
    let kind = if rows.len() == 1 { CopyKind::Row } else { CopyKind::Tsv };
    row_list_copy_text(columns, rows, 0, 0, kind)
}

// ---------------------------------------------------------------- delegate

pub struct ReadDelegate {
    columns: Vec<ReadColumn>,
    rows: ReadRows,
    selection: Option<ReadSelection>,
    locale: Locale,
    empty: Option<SharedString>,
}

impl ReadDelegate {
    fn cols(&self) -> usize {
        self.columns.len()
    }
}

fn read_cell_may_clip(text: &str, col_width: f32) -> bool {
    if text.is_empty() {
        return false;
    }
    if text.contains('\n') || text.chars().count() > 80 {
        return true;
    }
    let width: f32 = text.chars().map(|ch| if ch.is_ascii() { 7.8 } else { 14.0 }).sum();
    width * 1.1 + Space::MD * 2.0 + 4.0 > col_width
}

impl TableDelegate for ReadDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let spec = &self.columns[col_ix];
        let column = Column::new(SharedString::from(format!("c{col_ix}")), spec.title.clone()).width(px(spec.width)).p_0();
        if spec.align_right {
            column.text_right()
        } else {
            column
        }
    }

    fn render_td(&mut self, row_ix: usize, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let p = Palette::of(cx);
        let spec = &self.columns[col_ix];
        let selected = self.selection.is_some_and(|s| s.contains(row_ix, col_ix, self.rows.len(), self.cols()));
        let mono = spec.mono.then(|| cx.theme().mono_font_family.clone());
        let text = self.rows.cell(row_ix, col_ix);
        let clipped = read_cell_may_clip(&text, spec.width);
        div()
            .id(("read-table-td", row_ix * self.cols().max(1) + col_ix))
            .size_full()
            .px(px(Space::MD))
            .flex()
            .items_center()
            .when(spec.align_right, |this| this.justify_end())
            .when(selected, |this| this.bg(p.selection))
            .when_some(mono, |this, family| this.font_family(family))
            .text_size(px(TypeScale::SM))
            .text_color(p.text)
            .overflow_hidden()
            .when(clipped, |this| this.tooltip_text(text.clone()))
            .child(div().truncate().child(text))
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.rows.cell(row_ix, col_ix).to_string()
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        Some(self.rows.cell(row_ix, col_ix))
    }

    fn render_empty(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let p = Palette::of(cx);
        let text = self.empty.clone().unwrap_or_else(|| i18n::text(self.locale, "readTable.empty").into());
        div()
            .id("read-table-empty")
            .test_support()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(TypeScale::SM))
            .text_color(p.text_faint)
            .child(text)
    }
}

// ---------------------------------------------------------------- 视图

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadTableEvent {
    /// 双击或回车：页面决定主操作（打开、插入、跳转）。
    Activate(usize),
    /// 通过 ⌘C 或右键菜单写入了剪贴板。
    Copied(CopyKind),
}

struct OpenMenu {
    at: Point<Pixels>,
    menu: Entity<PopupMenu>,
    _dismiss: Subscription,
}

pub struct ReadTable {
    table: Entity<TableState<ReadDelegate>>,
    /// 下一次表格选中事件是否按 Shift 扩展（由鼠标按下或 Shift+方向键记下）。
    extend_next: bool,
    /// `select` / `select_row` 已直接应用的选中；表格随后发回的同一事件跳过，不把扩展选区重置。
    echo: Option<(SelectionMode, usize, usize)>,
    /// 鼠标按下时的横向滚动位置。表格选中格/列时会把它滚到可见，宽列（比视口还宽的值列）会因此左右跳；
    /// 鼠标点到的格本来就在眼前，所以点击引起的选中恢复这个位置，只有键盘移动才自动滚。
    click_scroll_x: Option<Pixels>,
    menu: Option<OpenMenu>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ReadTableEvent> for ReadTable {}

impl ReadTable {
    pub fn new(columns: Vec<ReadColumn>, rows: ReadRows, locale: Locale, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let delegate = ReadDelegate { columns, rows, selection: None, locale, empty: None };
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx).cell_selectable(true).row_selectable(true).col_selectable(true).col_resizable(true).row_header(true)
        });
        let subscription = cx.subscribe_in(&table, window, |this, _, event: &TableEvent, window, cx| this.on_table_event(event, window, cx));
        Self { table, extend_next: false, echo: None, click_scroll_x: None, menu: None, _subscriptions: vec![subscription] }
    }

    /// 换数据：选区超出新数据时清掉，其余保留（刷新诊断页时选中行不丢）。
    pub fn set_data(&mut self, columns: Vec<ReadColumn>, rows: ReadRows, cx: &mut Context<Self>) {
        self.table.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            let column_shape_changed = delegate.columns.len() != columns.len() || delegate.columns.iter().zip(&columns).any(|(a, b)| a.title != b.title);
            delegate.columns = columns;
            delegate.rows = rows;
            let (rows, cols) = (delegate.rows.len(), delegate.cols());
            if delegate.selection.is_some_and(|s| s.anchor.0.max(s.focus.0) >= rows || s.anchor.1.max(s.focus.1) >= cols) {
                delegate.selection = None;
            }
            if column_shape_changed {
                state.refresh(cx);
            }
            cx.notify();
        });
        cx.notify();
    }

    /// 空表时显示的文字；None 用默认的“没有数据”。
    pub fn set_empty_text(&mut self, text: Option<SharedString>, cx: &mut Context<Self>) {
        self.table.update(cx, |state, cx| {
            state.delegate_mut().empty = text;
            cx.notify();
        });
    }

    pub fn set_locale(&mut self, locale: Locale, cx: &mut Context<Self>) {
        self.table.update(cx, |state, _| state.delegate_mut().locale = locale);
        cx.notify();
    }

    pub fn table(&self) -> &Entity<TableState<ReadDelegate>> {
        &self.table
    }

    pub fn rows_len(&self, cx: &App) -> usize {
        self.table.read(cx).delegate().rows.len()
    }

    pub fn selection(&self, cx: &App) -> Option<ReadSelection> {
        self.table.read(cx).delegate().selection
    }

    pub fn menu_open(&self) -> bool {
        self.menu.is_some()
    }

    /// 选中一格；`extend` 时保留锚点、只移动焦点（Shift 扩展）。
    pub fn select(&mut self, row: usize, col: usize, extend: bool, cx: &mut Context<Self>) {
        self.extend_next = false;
        self.apply_selection(row, col, SelectionMode::Cells, extend, cx);
        self.echo = Some((SelectionMode::Cells, row, col));
        self.table.update(cx, |state, cx| state.set_selected_cell(row, col, cx));
    }

    /// 选中整行；`extend` 时从锚点行扩到这一行。
    pub fn select_row(&mut self, row: usize, extend: bool, cx: &mut Context<Self>) {
        self.extend_next = false;
        self.apply_selection(row, 0, SelectionMode::Rows, extend, cx);
        self.echo = Some((SelectionMode::Rows, row, 0));
        self.table.update(cx, |state, cx| state.set_selected_row(row, cx));
    }

    fn on_table_event(&mut self, event: &TableEvent, window: &mut Window, cx: &mut Context<Self>) {
        let extend = std::mem::take(&mut self.extend_next);
        let echo = self.echo.take();
        if matches!(event, TableEvent::SelectCell(..) | TableEvent::SelectColumn(_)) {
            if let Some(x) = self.click_scroll_x.take() {
                self.table.update(cx, |state, cx| {
                    let mut offset = state.horizontal_scroll_handle.offset();
                    if offset.x != x {
                        offset.x = x;
                        state.horizontal_scroll_handle.set_offset(offset);
                        cx.notify();
                    }
                });
            }
        }
        match event {
            TableEvent::SelectCell(row, col) if echo == Some((SelectionMode::Cells, *row, *col)) => {}
            TableEvent::SelectRow(row) if echo == Some((SelectionMode::Rows, *row, 0)) => {}
            TableEvent::SelectCell(row, col) => self.apply_selection(*row, *col, SelectionMode::Cells, extend, cx),
            TableEvent::SelectRow(row) => self.apply_selection(*row, 0, SelectionMode::Rows, extend, cx),
            TableEvent::SelectColumn(col) => self.apply_selection(0, *col, SelectionMode::Column, extend, cx),
            TableEvent::ClearSelection => {
                self.table.update(cx, |state, _| state.delegate_mut().selection = None);
                cx.notify();
            }
            TableEvent::DoubleClickedCell(row, _) | TableEvent::DoubleClickedRow(row) => cx.emit(ReadTableEvent::Activate(*row)),
            TableEvent::RightClickedCell(row, col) => {
                let inside = self.selection(cx).is_some_and(|s| {
                    let state = self.table.read(cx);
                    s.contains(*row, *col, state.delegate().rows.len(), state.delegate().cols())
                });
                if !inside {
                    self.apply_selection(*row, *col, SelectionMode::Cells, false, cx);
                }
                self.open_menu(window.mouse_position(), window, cx);
            }
            TableEvent::RightClickedRow(Some(row)) => {
                let inside = self.selection(cx).is_some_and(|s| {
                    let state = self.table.read(cx);
                    s.region(state.delegate().rows.len(), state.delegate().cols()).is_some_and(|(r, _)| r.contains(row))
                });
                if !inside {
                    self.apply_selection(*row, 0, SelectionMode::Rows, false, cx);
                }
                self.open_menu(window.mouse_position(), window, cx);
            }
            _ => {}
        }
    }

    fn apply_selection(&mut self, row: usize, col: usize, mode: SelectionMode, extend: bool, cx: &mut Context<Self>) {
        self.table.update(cx, |state, _| {
            let delegate = state.delegate_mut();
            delegate.selection = Some(match delegate.selection {
                Some(previous) if extend && previous.mode == mode => ReadSelection { anchor: previous.anchor, focus: (row, col), mode },
                _ => ReadSelection { anchor: (row, col), focus: (row, col), mode },
            });
        });
        cx.notify();
    }

    /// 按复制方式写剪贴板；空表或没有选区时剪贴板不动，返回 None。
    pub fn copy(&mut self, kind: CopyKind, cx: &mut Context<Self>) -> Option<String> {
        let text = {
            let delegate = self.table.read(cx).delegate();
            copy_text(&delegate.columns, &delegate.rows, delegate.selection, kind)
        }?;
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        cx.emit(ReadTableEvent::Copied(kind));
        Some(text)
    }

    /// ⌘C：单格复制值，多格复制含表头的 TSV。
    pub fn copy_selection(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let (kind, text) = {
            let delegate = self.table.read(cx).delegate();
            let (r, c) = delegate.selection?.region(delegate.rows.len(), delegate.cols())?;
            let kind = if r.start() == r.end() && c.start() == c.end() { CopyKind::Value } else { CopyKind::Tsv };
            let text = copy_text(&delegate.columns, &delegate.rows, delegate.selection, kind)?;
            (kind, text)
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        cx.emit(ReadTableEvent::Copied(kind));
        Some(text)
    }

    pub fn menu_entries(&self, cx: &App) -> Vec<(CopyKind, bool)> {
        let delegate = self.table.read(cx).delegate();
        menu_entries(delegate.rows.len(), delegate.cols(), delegate.selection)
    }

    /// 在指针位置打开复制菜单。条目在打开时按当前选区算好禁用状态。
    pub fn open_menu(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.menu_entries(cx);
        let locale = self.table.read(cx).delegate().locale;
        let owner: WeakEntity<Self> = cx.entity().downgrade();
        let menu = PopupMenu::build(window, cx, move |mut menu, _, _| {
            for (kind, disabled) in entries {
                let owner = owner.clone();
                menu = menu.item(PopupMenuItem::new(i18n::text(locale, kind.label_key())).disabled(disabled).on_click(move |_, _, cx| {
                    owner
                        .update(cx, |this, cx| {
                            this.copy(kind, cx);
                            this.close_menu(cx);
                        })
                        .ok();
                }));
            }
            menu.min_w(px(ControlSize::PICKER_MIN_W - 60.))
        });
        let dismiss = cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| this.close_menu(cx));
        menu.read(cx).focus_handle(cx).focus(window, cx);
        self.menu = Some(OpenMenu { at, menu, _dismiss: dismiss });
        cx.notify();
    }

    pub fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // 只有 Shift+方向键扩展；其余按键（含表格自己的方向键）都按新选区处理。
        self.extend_next = false;
        self.click_scroll_x = None;
        let keystroke = &event.keystroke;
        let key = keystroke.key.as_str();
        let mods = keystroke.modifiers;
        if key == "c" && mods.secondary(cx) && !mods.shift && !mods.alt {
            if self.copy_selection(cx).is_some() {
                cx.stop_propagation();
            }
            return;
        }
        if key == "enter" && !mods.modified() {
            if let Some(selection) = self.selection(cx) {
                cx.stop_propagation();
                cx.emit(ReadTableEvent::Activate(selection.focus.0));
            }
            return;
        }
        if mods.shift && !mods.secondary(cx) && matches!(key, "up" | "down" | "left" | "right") {
            let Some(selection) = self.selection(cx) else { return };
            let (rows, cols) = {
                let delegate = self.table.read(cx).delegate();
                (delegate.rows.len(), delegate.cols())
            };
            if rows == 0 || cols == 0 {
                return;
            }
            let (row, col) = selection.focus;
            let next = match key {
                "up" => (row.saturating_sub(1), col),
                "down" => ((row + 1).min(rows - 1), col),
                "left" => (row, col.saturating_sub(1)),
                _ => (row, (col + 1).min(cols - 1)),
            };
            cx.stop_propagation();
            if selection.mode == SelectionMode::Rows {
                self.select_row(next.0, true, cx);
            } else if selection.mode == SelectionMode::Column {
                self.apply_selection(0, next.1, SelectionMode::Column, true, cx);
            } else {
                self.select(next.0, next.1, true, cx);
            }
        }
    }
}

impl Focusable for ReadTable {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.table.read(cx).focus_handle(cx)
    }
}

impl Render for ReadTable {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let menu = self.menu.as_ref().map(|open| {
            deferred(
                anchored()
                    .position(open.at)
                    .anchor(Anchor::TopLeft)
                    .snap_to_window_with_margin(px(ControlSize::POPOVER_MARGIN))
                    .child(div().id("read-table-menu").test_support().child(open.menu.clone())),
            )
            .with_priority(3)
        });
        div()
            .id("read-table")
            .test_support()
            .size_full()
            .relative()
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                // 选中事件在松开时才到；按下时先记住是否按着 Shift，决定那次选中是新建还是扩展。
                this.extend_next = event.button == MouseButton::Left && event.modifiers.shift;
                this.click_scroll_x = (event.button == MouseButton::Left).then(|| this.table.read(cx).horizontal_scroll_handle.offset().x);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.on_key(event, window, cx)))
            .child(DataTable::new(&self.table).stripe(false).bordered(false))
            .children(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{size, Bounds, Modifiers, TestAppContext, WindowBounds, WindowOptions};
    use std::cell::RefCell;

    fn columns() -> Vec<ReadColumn> {
        vec![ReadColumn::new("id").width(60.).right(), ReadColumn::new("name"), ReadColumn::new("note").mono()]
    }

    fn rows() -> ReadRows {
        let text = |cells: [&str; 3]| cells.iter().map(|c| SharedString::from(c.to_string())).collect::<Vec<_>>();
        ReadRows::Static(vec![text(["1", "甲", "a\tb"]), text(["2", "乙", "say \"hi\""]), text(["3", "丙", ""])])
    }

    fn block(anchor: (usize, usize), focus: (usize, usize)) -> Option<ReadSelection> {
        Some(ReadSelection { anchor, focus, mode: SelectionMode::Cells })
    }

    #[test]
    fn copy_text_builds_each_format_for_the_selected_block() {
        let (cols, rows) = (columns(), rows());
        // 从右下往左上 Shift 扩展：矩形由锚点和焦点张成，与方向无关。
        let sel = block((1, 2), (0, 1));
        assert_eq!(copy_text(&cols, &rows, sel, CopyKind::Value).as_deref(), Some("甲"), "值取焦点格");
        assert_eq!(copy_text(&cols, &rows, sel, CopyKind::Row).as_deref(), Some("1\t甲\t\"a\tb\"\n2\t乙\t\"say \"\"hi\"\"\""));
        assert_eq!(copy_text(&cols, &rows, sel, CopyKind::Column).as_deref(), Some("甲\n乙"));
        assert_eq!(copy_text(&cols, &rows, sel, CopyKind::Tsv).as_deref(), Some("name\tnote\n甲\t\"a\tb\"\n乙\t\"say \"\"hi\"\"\""));
        assert_eq!(copy_text(&cols, &rows, sel, CopyKind::Csv).as_deref(), Some("\"name\",\"note\"\r\n\"甲\",\"a\tb\"\r\n\"乙\",\"say \"\"hi\"\"\""));
        assert_eq!(
            copy_text(&cols, &rows, sel, CopyKind::Json).as_deref(),
            Some("[{\"name\":\"甲\",\"note\":\"a\\tb\"},{\"name\":\"乙\",\"note\":\"say \\\"hi\\\"\"}]")
        );
    }

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn row_marks_extend_toggle_and_fall_back_to_the_focus_row() {
        let visible = ids(&["a", "b", "c", "d"]);
        let mut marks = RowMarks::default();
        let focus = marks.pick(&visible, None, "b", RowPick::Replace);
        assert_eq!(marks.selected(&visible, focus.as_deref()), vec![1]);
        // Shift 扩展：锚点留在 b，向下到 d；再向上到 a 时仍以 b 为锚点。
        let focus = marks.pick(&visible, focus.as_deref(), "d", RowPick::Extend);
        assert_eq!(marks.selected(&visible, focus.as_deref()), vec![1, 2, 3]);
        let focus = marks.pick(&visible, focus.as_deref(), "a", RowPick::Extend);
        assert_eq!(marks.selected(&visible, focus.as_deref()), vec![0, 1]);
        // ⌘ 切换：加上 d，再去掉 a，焦点落到剩下的行。
        let focus = marks.pick(&visible, focus.as_deref(), "d", RowPick::Toggle);
        assert_eq!(marks.selected(&visible, focus.as_deref()), vec![0, 1, 3]);
        let focus = marks.pick(&visible, focus.as_deref(), "a", RowPick::Toggle);
        assert_eq!(marks.selected(&visible, focus.as_deref()), vec![1, 3]);
        assert!(focus.as_deref() == Some("b") || focus.as_deref() == Some("d"));
        assert!(marks.is_selected(&visible, focus.as_deref(), "d"));
        assert!(!marks.is_selected(&visible, focus.as_deref(), "c"));
        // 焦点被外部移到集合外（键盘上下）：只算焦点一行。
        assert_eq!(marks.selected(&visible, Some("c")), vec![2]);
        assert!(!marks.is_selected(&visible, Some("c"), "d"));
        // Shift+方向键从外部焦点开始扩展。
        let focus = marks.extend_step(&visible, Some("c"), 1);
        assert_eq!(marks.selected(&visible, focus.as_deref()), vec![2, 3]);
        // 最后一行不能被 ⌘ 取消。
        let mut single = RowMarks::default();
        let focus = single.pick(&visible, None, "a", RowPick::Toggle);
        let focus = single.pick(&visible, focus.as_deref(), "a", RowPick::Toggle);
        assert_eq!(single.selected(&visible, focus.as_deref()), vec![0]);
        assert_eq!(single.selected(&visible, None), Vec::<usize>::new());
    }

    #[test]
    fn row_list_copy_covers_value_row_column_and_formats() {
        let cols = columns();
        let text = |cells: [&str; 3]| cells.iter().map(|c| SharedString::from(c.to_string())).collect::<Vec<_>>();
        let picked = ReadRows::Static(vec![text(["1", "甲", "x"]), text(["3", "丙", ""])]);
        assert_eq!(row_list_copy_text(&cols, &picked, 1, 1, CopyKind::Value).as_deref(), Some("丙"));
        assert_eq!(row_list_copy_text(&cols, &picked, 1, 1, CopyKind::Row).as_deref(), Some("1\t甲\tx\n3\t丙\t"));
        assert_eq!(row_list_copy_text(&cols, &picked, 1, 1, CopyKind::Column).as_deref(), Some("甲\n丙"));
        assert_eq!(row_list_copy_text(&cols, &picked, 0, 0, CopyKind::Tsv).as_deref(), Some("id\tname\tnote\n1\t甲\tx\n3\t丙\t"));
        assert_eq!(row_list_copy_text(&cols, &picked, 0, 0, CopyKind::Json).as_deref(), Some("[{\"id\":\"1\",\"name\":\"甲\",\"note\":\"x\"},{\"id\":\"3\",\"name\":\"丙\",\"note\":\"\"}]"));
        assert_eq!(row_list_clipboard_text(&cols, &picked).as_deref(), Some("id\tname\tnote\n1\t甲\tx\n3\t丙\t"));
        let one = ReadRows::Static(vec![text(["1", "甲", "x"])]);
        assert_eq!(row_list_clipboard_text(&cols, &one).as_deref(), Some("1\t甲\tx"), "一行不带表头");
        assert_eq!(row_list_copy_text(&cols, &ReadRows::default(), 0, 0, CopyKind::Tsv), None);
    }

    #[test]
    fn clipboard_copies_a_value_for_one_cell_and_tsv_with_headers_for_many() {
        let (cols, rows) = (columns(), rows());
        assert_eq!(clipboard_text(&cols, &rows, block((2, 0), (2, 0))).as_deref(), Some("3"));
        assert_eq!(clipboard_text(&cols, &rows, block((0, 0), (1, 1))).as_deref(), Some("id\tname\n1\t甲\n2\t乙"));
        let whole_rows = Some(ReadSelection { anchor: (0, 0), focus: (1, 0), mode: SelectionMode::Rows });
        assert_eq!(copy_text(&cols, &rows, whole_rows, CopyKind::Tsv).as_deref(), Some("id\tname\tnote\n1\t甲\t\"a\tb\"\n2\t乙\t\"say \"\"hi\"\"\""));
        let column = Some(ReadSelection { anchor: (0, 1), focus: (0, 1), mode: SelectionMode::Column });
        assert_eq!(clipboard_text(&cols, &rows, column).as_deref(), Some("name\n甲\n乙\n丙"), "整列含表头");
        assert_eq!(copy_text(&cols, &rows, column, CopyKind::Column).as_deref(), Some("甲\n乙\n丙"));
        // 选区超出数据（刷新后行变少）按表收边，不越界。
        assert_eq!(copy_text(&cols, &rows, block((9, 9), (9, 9)), CopyKind::Value).as_deref(), Some(""));
    }

    #[test]
    fn empty_table_or_no_selection_disables_every_copy_item() {
        let cols = columns();
        let empty = ReadRows::default();
        assert_eq!(copy_text(&cols, &empty, block((0, 0), (0, 0)), CopyKind::Tsv), None);
        assert_eq!(clipboard_text(&cols, &empty, block((0, 0), (0, 0))), None);
        let entries = menu_entries(0, cols.len(), block((0, 0), (0, 0)));
        assert_eq!(entries.len(), CopyKind::MENU.len());
        assert!(entries.iter().all(|(_, disabled)| *disabled), "空表时复制项全部禁用");
        assert!(menu_entries(3, 3, None).iter().all(|(_, disabled)| *disabled), "没有选区时禁用");
        assert!(menu_entries(3, 3, block((0, 0), (0, 0))).iter().all(|(_, disabled)| !*disabled));
        let lazy = ReadRows::Lazy { count: 2, cell: Rc::new(|row, col| format!("r{row}c{col}").into()) };
        assert_eq!(copy_text(&cols, &lazy, block((0, 0), (1, 1)), CopyKind::Row).as_deref(), Some("r0c0\tr0c1\tr0c2\nr1c0\tr1c1\tr1c2"));
        assert_eq!(lazy.cell(5, 0), SharedString::default());
    }

    fn open(cx: &mut TestAppContext, rows: ReadRows) -> (gpui_kit::AnyWindowHandle, Entity<ReadTable>, Rc<RefCell<Vec<ReadTableEvent>>>) {
        cx.update(gpui_kit::init);
        let events = Rc::new(RefCell::new(Vec::new()));
        let (window, table) = cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(800.), px(400.)) };
            gpui_kit::open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, cx, |window, cx| {
                cx.new(|cx| ReadTable::new(columns(), rows, Locale::ZhCn, window, cx))
            })
            .expect("read table window")
        });
        let sink = events.clone();
        cx.update(|cx| cx.subscribe(&table, move |_, event: &ReadTableEvent, _| sink.borrow_mut().push(event.clone())).detach());
        (window, table, events)
    }

    /// 每一步单独提交：表格事件在一次 update 结束时才派发给订阅者。
    fn step<R>(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        let result = cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            f(window, cx)
        });
        cx.run_until_parked();
        result.unwrap()
    }

    fn clipboard(cx: &mut TestAppContext) -> Option<String> {
        cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
    }

    #[gpui_kit::test]
    fn click_shift_click_and_shortcut_copy_the_block(cx: &mut TestAppContext) {
        let (window, table, events) = open(cx, rows());
        step(cx, window, |window, cx| window.click("table-cell-0:0", cx));
        assert_eq!(cx.update(|cx| table.read(cx).selection(cx)), block((0, 0), (0, 0)));
        step(cx, window, |window, cx| window.click_with_modifiers("table-cell-1:1", Modifiers::shift(), cx));
        assert_eq!(cx.update(|cx| table.read(cx).selection(cx)), block((0, 0), (1, 1)), "Shift 点击扩展选区");
        step(cx, window, |window, cx| {
            let focus = table.read(cx).focus_handle(cx);
            focus.focus(window, cx);
        });
        let copy = if cfg!(target_os = "macos") { "cmd-c" } else { "ctrl-c" };
        step(cx, window, |window, cx| window.press(copy, cx));
        assert_eq!(clipboard(cx).as_deref(), Some("id\tname\n1\t甲\n2\t乙"));
        step(cx, window, |window, cx| window.press("shift-down", cx));
        assert_eq!(cx.update(|cx| table.read(cx).selection(cx)), block((0, 0), (2, 1)), "Shift+方向键扩展");
        step(cx, window, |window, cx| window.click("table-cell-2:1", cx));
        assert_eq!(cx.update(|cx| table.read(cx).selection(cx)), block((2, 1), (2, 1)), "普通点击重新开始选区");
        step(cx, window, |window, cx| window.press(copy, cx));
        assert_eq!(clipboard(cx).as_deref(), Some("丙"), "单格复制值本身");
        step(cx, window, |window, cx| window.press("left", cx));
        assert_eq!(cx.update(|cx| table.read(cx).selection(cx)), block((2, 0), (2, 0)), "方向键移动并重新开始选区");
        assert!(events.borrow().contains(&ReadTableEvent::Copied(CopyKind::Tsv)));
        assert!(events.borrow().contains(&ReadTableEvent::Copied(CopyKind::Value)), "单格复制发出 CopyKind::Value");
    }

    #[gpui_kit::test]
    fn shift_select_column_extends_columns_and_clip_detects_long_cells(cx: &mut TestAppContext) {
        assert!(!read_cell_may_clip("", 60.));
        assert!(!read_cell_may_clip("id", 60.));
        assert!(read_cell_may_clip("a very long description that overflows column width", 60.));
        assert!(read_cell_may_clip("line1\nline2", 200.));

        let (window, table, events) = open(cx, rows());
        step(cx, window, |window, cx| {
            table.update(cx, |this, cx| {
                this.on_table_event(&TableEvent::SelectColumn(0), window, cx);
                this.extend_next = true;
                this.on_table_event(&TableEvent::SelectColumn(2), window, cx);
            });
        });
        assert_eq!(
            cx.update(|cx| table.read(cx).selection(cx)),
            Some(ReadSelection { anchor: (0, 0), focus: (0, 2), mode: SelectionMode::Column }),
            "Shift 选列应扩展多列选区"
        );
        step(cx, window, |_, cx| {
            let copied = table.update(cx, |this, cx| this.copy_selection(cx));
            assert_eq!(copied.as_deref(), Some("id\tname\tnote\n1\t甲\t\"a\tb\"\n2\t乙\t\"say \"\"hi\"\"\"\n3\t丙\t"));
        });
        assert_eq!(events.borrow().last(), Some(&ReadTableEvent::Copied(CopyKind::Tsv)));
        // 单格复制只发 Value
        step(cx, window, |_, cx| {
            table.update(cx, |this, cx| {
                this.select(0, 1, false, cx);
                let copied = this.copy_selection(cx);
                assert_eq!(copied.as_deref(), Some("甲"));
            });
        });
        assert_eq!(events.borrow().last(), Some(&ReadTableEvent::Copied(CopyKind::Value)));
    }

    #[gpui_kit::test]
    fn clicking_a_column_wider_than_the_view_does_not_scroll_sideways(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        // 值列比 800 宽的窗口还宽：点它不该把视图横向跳到列尾，点回别的列也不该跳回来。
        let wide = vec![ReadColumn::new("id").width(60.), ReadColumn::new("value").width(1200.), ReadColumn::new("note").width(200.)];
        let (window, table) = cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(800.), px(400.)) };
            gpui_kit::open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, cx, |window, cx| {
                cx.new(|cx| ReadTable::new(wide, rows(), Locale::ZhCn, window, cx))
            })
            .expect("read table window")
        });
        let scroll_x = |cx: &mut TestAppContext| cx.update(|cx| table.read(cx).table().read(cx).horizontal_scroll_handle.offset().x);
        step(cx, window, |_, _| {});
        let start = scroll_x(cx);
        step(cx, window, |window, cx| window.click("table-cell-0:1", cx));
        step(cx, window, |_, _| {});
        assert_eq!(cx.update(|cx| table.read(cx).selection(cx)), block((0, 1), (0, 1)));
        assert_eq!(scroll_x(cx), start, "点击宽列不横向滚动");
        step(cx, window, |window, cx| window.click("table-cell-1:0", cx));
        step(cx, window, |_, _| {});
        assert_eq!(scroll_x(cx), start, "点回左边的列也不滚动");
        // 键盘移到屏外的列仍然自动滚到可见。
        step(cx, window, |window, cx| {
            let focus = table.read(cx).focus_handle(cx);
            focus.focus(window, cx);
        });
        step(cx, window, |window, cx| window.press("right", cx));
        step(cx, window, |window, cx| window.press("right", cx));
        step(cx, window, |_, _| {});
        assert_eq!(cx.update(|cx| table.read(cx).selection(cx)), block((1, 2), (1, 2)));
        assert!(scroll_x(cx) < start, "键盘移到屏外的列时仍然滚到可见");
    }

    #[gpui_kit::test]
    fn double_click_and_enter_activate_the_row(cx: &mut TestAppContext) {
        let (window, table, events) = open(cx, rows());
        step(cx, window, |window, cx| window.double_click("table-cell-1:0", cx));
        step(cx, window, |window, cx| window.click("table-cell-2:1", cx));
        step(cx, window, |window, cx| {
            let focus = table.read(cx).focus_handle(cx);
            focus.focus(window, cx);
        });
        step(cx, window, |window, cx| window.press("enter", cx));
        let activated: Vec<_> = events.borrow().iter().filter_map(|e| if let ReadTableEvent::Activate(row) = e { Some(*row) } else { None }).collect();
        assert_eq!(activated, vec![1, 2]);
    }

    #[gpui_kit::test]
    fn right_click_opens_the_copy_menu_and_copies(cx: &mut TestAppContext) {
        let (window, table, _) = open(cx, rows());
        step(cx, window, |window, cx| window.right_click("table-cell-2:1", cx));
        step(cx, window, |window, cx| {
            assert!(table.read(cx).menu_open(), "右键打开复制菜单");
            assert!(window.try_find("read-table-menu").is_some(), "菜单画在指针处");
            assert_eq!(table.read(cx).selection(cx), block((2, 1), (2, 1)), "右键选区外的格子先选中它");
            assert!(table.read(cx).menu_entries(cx).iter().all(|(_, disabled)| !*disabled));
            let copied = table.update(cx, |table, cx| table.copy(CopyKind::Row, cx));
            assert_eq!(copied.as_deref(), Some("3\t丙\t"));
            table.update(cx, |table, cx| table.close_menu(cx));
            assert!(!table.read(cx).menu_open());
        });
    }

    #[gpui_kit::test]
    fn empty_table_keeps_the_clipboard_and_disables_copy(cx: &mut TestAppContext) {
        let (window, table, events) = open(cx, ReadRows::default());
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("read-table-empty").is_some(), "空表显示空态");
            cx.write_to_clipboard(ClipboardItem::new_string("原内容".to_owned()));
            assert_eq!(table.update(cx, |table, cx| table.copy(CopyKind::Tsv, cx)), None);
            assert_eq!(table.update(cx, |table, cx| table.copy_selection(cx)), None);
            assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some("原内容"));
            assert!(table.read(cx).menu_entries(cx).iter().all(|(_, disabled)| *disabled));
            // 换成有数据后可复制；再换回空表，旧选区被清掉。
            table.update(cx, |table, cx| {
                table.set_data(columns(), rows(), cx);
                table.select(1, 1, false, cx);
            });
            assert_eq!(table.update(cx, |table, cx| table.copy_selection(cx)).as_deref(), Some("乙"));
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| table.read(cx).selection(cx)), block((1, 1), (1, 1)), "表格回发的同一选中事件不改选区");
        step(cx, window, |_, cx| table.update(cx, |table, cx| table.set_data(columns(), ReadRows::default(), cx)));
        assert_eq!(cx.update(|cx| table.read(cx).selection(cx)), None);
        assert!(!events.borrow().iter().any(|e| matches!(e, ReadTableEvent::Activate(_))));
    }
}
