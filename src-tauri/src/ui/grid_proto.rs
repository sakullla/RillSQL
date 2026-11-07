//! Virtual DataTable prototype. A plain table is not used for these fixtures.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::BTreeMap;
use std::ops::Range;

use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Render, SharedString, Styled as _, Window, div, px,
};

pub const READONLY_ROWS: usize = 10_000;
pub const EDITABLE_ROWS: usize = 1_000;
const COLS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridFixture {
    ReadOnly,
    Editable,
}

pub struct GridPrototype {
    table: Entity<TableState<GridDelegate>>,
    fixture: GridFixture,
}

struct GridDelegate {
    fixture: GridFixture,
    rows: usize,
    edits: BTreeMap<(usize, usize), String>,
    draft: Option<Draft>,
    anchor: (usize, usize),
    focus: (usize, usize),
    widths: [Pixels; COLS],
    deleted: BTreeMap<usize, bool>,
}

struct Draft {
    row: usize,
    col: usize,
    original: String,
    text: String,
}

impl GridPrototype {
    pub fn new(fixture: GridFixture, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let delegate = GridDelegate::new(fixture);
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .cell_selectable(true)
                .row_selectable(true)
                .col_resizable(true)
                .row_header(true)
        });
        Self { table, fixture }
    }

    pub fn rows(&self, cx: &App) -> usize {
        self.table.read(cx).delegate().rows
    }

    pub fn select(&mut self, row: usize, col: usize, extend: bool, cx: &mut App) {
        self.table.update(cx, |state, cx| {
            state.delegate_mut().select(row, col, extend);
            state.set_selected_cell(row, col, cx);
        });
    }

    pub fn selected_cell(&self, cx: &App) -> Option<(usize, usize)> {
        self.table.read(cx).selected_cell()
    }

    pub fn scroll_to(&mut self, row: usize, cx: &mut App) {
        self.table.update(cx, |state, cx| state.scroll_to_row(row, cx));
    }

    pub fn visible_rows(&self, cx: &App) -> Range<usize> {
        self.table.read(cx).visible_range().rows().clone()
    }

    pub fn copy_tsv(&mut self, cx: &mut App) -> String {
        // Clicks and arrow keys update TableState's cell only. The copy rectangle
        // stays at the delegate anchor until that cell is adopted.
        self.table.update(cx, |state, _cx| {
            if let Some((row, col)) = state.selected_cell() {
                if state.delegate().focus != (row, col) {
                    state.delegate_mut().select(row, col, false);
                }
            }
        });
        let tsv = self.table.read(cx).delegate().tsv();
        cx.write_to_clipboard(ClipboardItem::new_string(tsv.clone()));
        tsv
    }

    pub fn begin_edit(&mut self, cx: &mut App) -> bool {
        self.table.update(cx, |state, cx| {
            let editable = state.delegate().fixture == GridFixture::Editable;
            if !editable {
                return false;
            }
            let focus = state.delegate().focus;
            state.delegate_mut().begin_edit(focus.0, focus.1);
            cx.notify();
            true
        })
    }

    pub fn set_draft(&mut self, text: &str, cx: &mut App) -> bool {
        self.table.update(cx, |state, cx| {
            let changed = state.delegate_mut().set_draft(text);
            if changed {
                cx.notify();
            }
            changed
        })
    }

    pub fn commit_edit(&mut self, cx: &mut App) -> bool {
        self.table.update(cx, |state, cx| {
            let committed = state.delegate_mut().commit();
            if committed {
                cx.notify();
            }
            committed
        })
    }

    pub fn cancel_edit(&mut self, cx: &mut App) -> bool {
        self.table.update(cx, |state, cx| {
            let cancelled = state.delegate_mut().cancel();
            if cancelled {
                cx.notify();
            }
            cancelled
        })
    }

    pub fn cell(&self, row: usize, col: usize, cx: &App) -> String {
        self.table.read(cx).delegate().cell_value(row, col)
    }

    pub fn fit_column(&mut self, col: usize, window: &mut Window, cx: &mut App) -> bool {
        self.table
            .update(cx, |state, cx| state.fit_col(col, window, cx))
    }

    pub fn set_column_width(&mut self, col: usize, width: Pixels, cx: &mut App) {
        self.table.update(cx, |state, cx| {
            if let Some(slot) = state.delegate_mut().widths.get_mut(col) {
                *slot = width;
            }
            state.refresh(cx);
        });
    }

    pub fn column_width(&self, col: usize, cx: &App) -> Option<Pixels> {
        self.table.read(cx).col_width(col)
    }

    pub fn mark_delete(&mut self, cx: &mut App) -> bool {
        self.table.update(cx, |state, cx| {
            if state.delegate().fixture != GridFixture::Editable {
                return false;
            }
            let row = state.delegate().focus.0;
            state.delegate_mut().deleted.insert(row, true);
            cx.notify();
            true
        })
    }
}

impl Render for GridPrototype {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("data-grid")
            .h(px(280.))
            .w_full()
            .child(
                DataTable::new(&self.table).accessibility_label(match self.fixture {
                    GridFixture::ReadOnly => "只读结果",
                    GridFixture::Editable => "可编辑表格",
                }),
            )
    }
}

impl GridDelegate {
    fn new(fixture: GridFixture) -> Self {
        let rows = match fixture {
            GridFixture::ReadOnly => READONLY_ROWS,
            GridFixture::Editable => EDITABLE_ROWS,
        };
        Self {
            fixture,
            rows,
            edits: BTreeMap::new(),
            draft: None,
            anchor: (0, 0),
            focus: (0, 0),
            widths: [px(96.), px(120.), px(88.), px(140.)],
            deleted: BTreeMap::new(),
        }
    }

    fn select(&mut self, row: usize, col: usize, extend: bool) {
        let row = row.min(self.rows.saturating_sub(1));
        let col = col.min(COLS - 1);
        if !extend {
            self.anchor = (row, col);
        }
        self.focus = (row, col);
    }

    fn rect(&self) -> (Range<usize>, Range<usize>) {
        let row_start = self.anchor.0.min(self.focus.0);
        let row_end = self.anchor.0.max(self.focus.0) + 1;
        let col_start = self.anchor.1.min(self.focus.1);
        let col_end = self.anchor.1.max(self.focus.1) + 1;
        (row_start..row_end, col_start..col_end)
    }

    fn cell_value(&self, row: usize, col: usize) -> String {
        if let Some(draft) = &self.draft {
            if draft.row == row && draft.col == col {
                return draft.text.clone();
            }
        }
        self.edits
            .get(&(row, col))
            .cloned()
            .unwrap_or_else(|| format!("r{row}c{col}"))
    }

    fn begin_edit(&mut self, row: usize, col: usize) {
        let original = self.cell_value(row, col);
        self.draft = Some(Draft {
            row,
            col,
            text: original.clone(),
            original,
        });
    }

    fn set_draft(&mut self, text: &str) -> bool {
        let Some(draft) = &mut self.draft else {
            return false;
        };
        draft.text = text.to_string();
        true
    }

    fn commit(&mut self) -> bool {
        let Some(draft) = self.draft.take() else {
            return false;
        };
        self.edits.insert((draft.row, draft.col), draft.text);
        true
    }

    fn cancel(&mut self) -> bool {
        let Some(draft) = self.draft.take() else {
            return false;
        };
        let _ = draft.original;
        true
    }

    fn tsv(&self) -> String {
        let (rows, cols) = self.rect();
        let mut lines = Vec::new();
        for row in rows {
            let mut cells = Vec::new();
            for col in cols.clone() {
                cells.push(self.cell_value(row, col));
            }
            lines.push(cells.join("\t"));
        }
        lines.join("\n")
    }
}

impl TableDelegate for GridDelegate {
    fn columns_count(&self, _: &App) -> usize {
        COLS
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        Column::new(format!("c{col_ix}"), format!("列{col_ix}"))
            .width(self.widths[col_ix])
            .resizable(true)
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div().child(SharedString::from(self.cell_value(row_ix, col_ix)))
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.cell_value(row_ix, col_ix)
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        Some(SharedString::from(self.cell_value(row_ix, col_ix)))
    }

    fn fit_column(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> Option<Pixels> {
        Some(self.widths.get(col_ix).copied().unwrap_or(px(96.)) + px(48.))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{Point, TestAppContext, WindowBounds, WindowOptions, px, size};

    fn open(
        cx: &mut TestAppContext,
        fixture: GridFixture,
    ) -> (gpui_kit::AnyWindowHandle, Entity<GridPrototype>) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            let bounds = gpui_kit::Bounds {
                origin: Point::default(),
                size: size(px(900.), px(420.)),
            };
            let (window, grid) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| GridPrototype::new(fixture, window, cx)),
            )
            .expect("grid window");
            (window, grid)
        })
    }

    #[gpui_kit::test]
    fn readonly_virtual_grid_scrolls_selects_copies_and_resizes(cx: &mut TestAppContext) {
        let (window, grid) = open(cx, GridFixture::ReadOnly);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(grid.read(cx).rows(cx), READONLY_ROWS);
            grid.update(cx, |grid, cx| grid.scroll_to(READONLY_ROWS - 1, cx));
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let visible = grid.read(cx).visible_rows(cx);
            assert!(
                visible.end <= READONLY_ROWS && visible.len() < 500,
                "virtual window should not materialize 10000 rows: {visible:?}"
            );
            assert!(
                visible.start > 100 || visible.end == READONLY_ROWS,
                "scroll_to_row should move the window: {visible:?}"
            );
            grid.update(cx, |grid, cx| {
                grid.select(10, 0, false, cx);
                grid.select(12, 2, true, cx);
            });
            assert_eq!(grid.read(cx).selected_cell(cx), Some((12, 2)));
            let tsv = grid.update(cx, |grid, cx| grid.copy_tsv(cx));
            assert_eq!(
                tsv,
                "r10c0\tr10c1\tr10c2\nr11c0\tr11c1\tr11c2\nr12c0\tr12c1\tr12c2"
            );
            assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some(tsv.as_str()));
            assert!(!grid.update(cx, |grid, cx| grid.begin_edit(cx)));
            let before = grid.read(cx).column_width(0, cx);
            assert!(grid.update(cx, |grid, cx| grid.fit_column(0, window, cx)));
            let after = grid.read(cx).column_width(0, cx);
            assert_ne!(before, after);
            grid.update(cx, |grid, cx| grid.set_column_width(1, px(180.), cx));
            assert_eq!(grid.read(cx).column_width(1, cx), Some(px(180.)));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn editable_grid_commits_and_cancels_without_writing_on_cancel(cx: &mut TestAppContext) {
        let (window, grid) = open(cx, GridFixture::Editable);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(grid.read(cx).rows(cx), EDITABLE_ROWS);
            grid.update(cx, |grid, cx| grid.select(4, 1, false, cx));
            assert!(grid.update(cx, |grid, cx| grid.begin_edit(cx)));
            assert!(grid.update(cx, |grid, cx| grid.set_draft("中文单元格", cx)));
            assert!(grid.update(cx, |grid, cx| grid.cancel_edit(cx)));
            assert_eq!(grid.read(cx).cell(4, 1, cx), "r4c1");
            assert!(grid.update(cx, |grid, cx| grid.begin_edit(cx)));
            assert!(grid.update(cx, |grid, cx| grid.set_draft("已改", cx)));
            assert!(grid.update(cx, |grid, cx| grid.commit_edit(cx)));
            assert_eq!(grid.read(cx).cell(4, 1, cx), "已改");
            assert!(grid.update(cx, |grid, cx| grid.mark_delete(cx)));
            grid.update(cx, |grid, cx| grid.scroll_to(EDITABLE_ROWS - 1, cx));
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let visible = grid.read(cx).visible_rows(cx);
            assert!(visible.len() < EDITABLE_ROWS, "{visible:?}");
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn copy_follows_a_cell_click_or_arrow_without_manual_select(cx: &mut TestAppContext) {
        use gpui_kit::Focusable as _;

        let (window, grid) = open(cx, GridFixture::ReadOnly);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("table-cell-0:1", cx);
            assert_eq!(grid.read(cx).selected_cell(cx), Some((0, 1)));
            let tsv = grid.update(cx, |grid, cx| grid.copy_tsv(cx));
            assert_eq!(tsv, "r0c1", "click must not copy the default (0, 0) cell");
            grid.update(cx, |grid, cx| {
                let handle = grid.table.read(cx).focus_handle(cx);
                handle.focus(window, cx);
            });
            window.press("right", cx);
            assert_eq!(grid.read(cx).selected_cell(cx), Some((0, 2)));
            let tsv = grid.update(cx, |grid, cx| grid.copy_tsv(cx));
            assert_eq!(tsv, "r0c2");
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
                Some("r0c2")
            );
        })
        .unwrap();
    }
}
