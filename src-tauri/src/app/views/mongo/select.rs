//! 文档表的选区与复制（R13）：沿用 `ui::read_table` 的选区模型与复制文本。
//!
//! 文档表本身保留行尾的查看 / 复制 / 编辑 / 删除图标，所以不整体换成 `ReadTable`，
//! 只接入同一套选择与复制：单击选中一格，Shift+单击或 Shift+方向键扩展选区，
//! ⌘C 复制（单格为值，多格为含表头的 TSV），右键弹出「复制值 / 行 / 列 / TSV / CSV / JSON」。
//! 复制只写剪贴板，不触发任何写库操作。

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{anchored, deferred, Anchor, DismissEvent, Focusable as _, Pixels, Point, WeakEntity};

use super::*;
use crate::ui::kit::ControlSize;
use crate::ui::read_table::{clipboard_text, copy_text, menu_entries, CopyKind, ReadColumn, ReadRows, ReadSelection, SelectionMode};

pub(super) struct CopyMenu {
    at: Point<Pixels>,
    menu: Entity<PopupMenu>,
    _dismiss: Subscription,
}

impl MongoPane {
    /// 表格视图当前显示的二维数据（列 = 显示的字段，行 = 本页文档），与渲染用同一份取值。
    fn doc_grid(&self) -> (Vec<ReadColumn>, ReadRows) {
        let shown = self.shown_columns();
        let rows = self
            .documents
            .iter()
            .take(PAGE_LIMIT as usize)
            .map(|doc| {
                let parsed = serde_json::from_str::<serde_json::Value>(&doc.json).ok();
                shown.iter().map(|name| SharedString::from(cell_text_value(parsed.as_ref().and_then(|value| value.get(name))))).collect()
            })
            .collect();
        (shown.into_iter().map(ReadColumn::new).collect(), ReadRows::Static(rows))
    }

    pub fn doc_selection(&self) -> Option<ReadSelection> {
        self.doc_selection
    }

    /// 选中一格；`extend` 时保留锚点、只移动焦点。
    pub(super) fn select_doc_cell(&mut self, row: usize, column: usize, extend: bool) {
        self.doc_selection = Some(match self.doc_selection {
            Some(previous) if extend && previous.mode == SelectionMode::Cells => ReadSelection { anchor: previous.anchor, focus: (row, column), mode: SelectionMode::Cells },
            _ => ReadSelection::cell(row, column),
        });
    }

    pub(super) fn doc_cell_selected(&self, row: usize, column: usize) -> bool {
        let rows = self.documents.len().min(PAGE_LIMIT as usize);
        let columns = self.shown_columns().len();
        self.doc_selection.is_some_and(|selection| selection.contains(row, column, rows, columns))
    }

    /// 写剪贴板：`None` 按 ⌘C 的规则（单格复制值，多格复制含表头的 TSV）。没有选区或表为空时不动剪贴板。
    pub fn copy_docs(&mut self, kind: Option<CopyKind>, cx: &mut Context<Self>) -> Option<String> {
        let (columns, rows) = self.doc_grid();
        let text = match kind {
            Some(kind) => copy_text(&columns, &rows, self.doc_selection, kind),
            None => clipboard_text(&columns, &rows, self.doc_selection),
        }?;
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.copied = text.clone();
        self.note = self.text("mongo.docs.copiedCells");
        self.error.clear();
        Some(text)
    }

    /// 右键菜单的条目：(复制方式, 是否禁用)，按当前选区算。
    pub fn doc_menu_entries(&self) -> Vec<(CopyKind, bool)> {
        menu_entries(self.documents.len().min(PAGE_LIMIT as usize), self.shown_columns().len(), self.doc_selection)
    }

    pub fn copy_menu_open(&self) -> bool {
        self.copy_menu.is_some()
    }

    /// 右键：点在选区外先选中这一格，再在指针处打开复制菜单。
    pub(super) fn open_copy_menu(&mut self, row: usize, column: usize, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.doc_cell_selected(row, column) {
            self.select_doc_cell(row, column, false);
        }
        self.focused_cell = Some((row, column));
        let entries = self.doc_menu_entries();
        let locale = self.locale;
        let owner: WeakEntity<Self> = cx.entity().downgrade();
        let menu = PopupMenu::build(window, cx, move |mut menu, _, _| {
            for (kind, disabled) in entries {
                let owner = owner.clone();
                menu = menu.item(PopupMenuItem::new(i18n::text(locale, kind.label_key())).disabled(disabled).on_click(move |_, _, cx| {
                    owner
                        .update(cx, |this, cx| {
                            this.copy_docs(Some(kind), cx);
                            this.close_copy_menu(cx);
                        })
                        .ok();
                }));
            }
            menu.min_w(px(ControlSize::PICKER_MIN_W - 60.))
        });
        let dismiss = cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| this.close_copy_menu(cx));
        menu.read(cx).focus_handle(cx).focus(window, cx);
        self.copy_menu = Some(CopyMenu { at, menu, _dismiss: dismiss });
        cx.notify();
    }

    pub(super) fn close_copy_menu(&mut self, cx: &mut Context<Self>) {
        if self.copy_menu.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn copy_menu_element(&self) -> Option<AnyElement> {
        let open = self.copy_menu.as_ref()?;
        Some(
            deferred(
                anchored()
                    .position(open.at)
                    .anchor(Anchor::TopLeft)
                    .snap_to_window_with_margin(px(ControlSize::POPOVER_MARGIN))
                    .child(div().id("mongo-copy-menu").test_support().child(open.menu.clone())),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }
}
