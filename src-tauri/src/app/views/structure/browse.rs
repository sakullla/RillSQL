//! 字段页（已有表）以浏览为主：`uniform_list` 只渲染可见行，每行默认是文本；
//! 点击某行进入编辑（换成 `column_row` 的输入控件），Esc 或点击其他行退出编辑并保留草稿。
//! 注释列始终可见：窄窗口里先把注释列收窄，再隐藏默认值列，“显示全部列”可强制显示默认值。
//! 另含新建索引表单里的多选列选择器（`ui::picker`）与已选列的有序列表。

use std::collections::HashMap;
use std::ops::Range;

use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, ScrollStrategy,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px,
};

use super::{ColumnChange, FieldWidths, MenuEntry, RowAction, Section, StructureMode, StructurePane, create, row_menu};
use crate::ddl::{ColumnDraft, IndexColumnDraft, IndexKind};
use crate::i18n;
use crate::ui::kit::{self, ControlSize, IconSize, Palette, Radius, ScrollAreaExt as _, Space, Tip as _, TypeScale};
use crate::ui::picker::{Picker, PickerEvent, PickerItem, PickerMode};

/// 拖动索引已选列时携带的源位置。
#[derive(Clone, Debug)]
pub(super) struct IndexColumnDrag {
    pub position: usize,
    pub label: SharedString,
}

/// 浏览态字段表的可见列与列宽。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BrowseColumns {
    pub widths: FieldWidths,
    pub default: bool,
    pub comment: bool,
}

impl BrowseColumns {
    pub const COMMENT_W: f32 = 160.;
    /// 注释列的最小宽度：再窄也不隐藏注释，只隐藏默认值列。
    pub const COMMENT_MIN: f32 = 84.;

    /// 按内容宽度决定显示哪些列。注释列优先级高于默认值列，始终显示：
    /// 放得下就全显示（注释 160）；不够时注释收窄到不小于 `COMMENT_MIN`；仍不够就隐藏默认值，
    /// 把让出的宽度给注释。`content_w <= 0` 表示还没量到宽度，按宽窗口处理。
    /// `show_all` 强制显示默认值（列宽取紧凑档，可能溢出）。
    pub fn of(content_w: f32, show_all: bool) -> Self {
        if content_w <= 0. {
            let mut widths = FieldWidths::wide();
            widths.comment = Self::COMMENT_W;
            return Self { widths, default: true, comment: true };
        }
        let with_comment = FieldWidths::of(content_w - Self::COMMENT_W - FieldWidths::GAP, false);
        if with_comment.min_total(false) + Self::COMMENT_W + FieldWidths::GAP <= content_w {
            let mut widths = with_comment;
            widths.comment = Self::COMMENT_W;
            return Self { widths, default: true, comment: true };
        }
        // 注释收窄：默认值列保持紧凑或标准宽度，剩余给注释。
        let mut widths = FieldWidths::of(content_w - Self::COMMENT_MIN - FieldWidths::GAP, false);
        let spare = content_w - widths.min_total(false) - FieldWidths::GAP;
        if spare >= Self::COMMENT_MIN || show_all {
            widths.comment = spare.clamp(Self::COMMENT_MIN, Self::COMMENT_W);
            return Self { widths, default: true, comment: true };
        }
        // 仍放不下：隐藏默认值列（连同它的间隙），注释拿到让出的宽度。
        let freed = widths.default + FieldWidths::GAP;
        widths.default = 0.;
        widths.comment = (spare + freed).clamp(Self::COMMENT_MIN, Self::COMMENT_W);
        Self { widths, default: false, comment: true }
    }

    /// 注释列是字段表里最长的文字：宽窗口里名称列只留到“最长字段名 + 标记”够用的宽度，
    /// 其余空余都给注释，不再在名称和类型之间留一大片空白、注释却被截断。
    pub fn widen_comment(&mut self, content_w: f32, name_w: f32) {
        if !self.comment || content_w <= 0. {
            return;
        }
        let mut fixed = self.widths.kind + self.widths.nullable + self.widths.drop + FieldWidths::PAD_LEFT + FieldWidths::PAD_RIGHT;
        let mut gaps = 4usize;
        if self.default {
            fixed += self.widths.default;
            gaps += 1;
        }
        let room = content_w - fixed - gaps as f32 * FieldWidths::GAP;
        let name = name_w.clamp(Self::NAME_MIN, Self::NAME_MAX).max(self.widths.name_min);
        let comment = (room - name).clamp(self.widths.comment, Self::COMMENT_MAX);
        self.widths.comment = comment;
    }

    pub const NAME_MIN: f32 = 180.;
    pub const NAME_MAX: f32 = 340.;
    pub const COMMENT_MAX: f32 = 720.;

    /// 名称列所需宽度的估算：最长的字段名（全角字符按两倍宽）加上标记槽与徽标。
    pub fn name_width<'a>(names: impl Iterator<Item = &'a str>) -> f32 {
        let longest = names
            .map(|name| name.chars().map(|ch| if ch.is_ascii() { 7.4 } else { 13.5 }).sum::<f32>())
            .fold(0f32, f32::max);
        longest + NAME_INSET + 72.
    }

    /// 有列因为宽度不够被隐藏（工具条显示“显示全部列”开关）。
    pub fn hides_any(content_w: f32) -> bool {
        let all = Self::of(content_w, false);
        !(all.default && all.comment)
    }

    /// 所有可见列加上间隙与左右内边距所需的最小宽度（用于“显示全部列”时触发横向滚动，避免裁切右侧操作按钮）。
    pub fn min_total(&self) -> f32 {
        let mut total = self.widths.name_min + self.widths.kind + self.widths.nullable + self.widths.drop;
        let mut cols = 4usize;
        if self.default {
            total += self.widths.default;
            cols += 1;
        }
        if self.comment {
            total += self.widths.comment;
            cols += 1;
        }
        total + (cols.saturating_sub(1) as f32) * FieldWidths::GAP + FieldWidths::PAD_LEFT + FieldWidths::PAD_RIGHT
    }
}

/// 浏览态字段表的一列。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FieldCell {
    Name,
    Type,
    Nullable,
    Default,
    Comment,
    Actions,
}

/// 一列的宽度规则：名称列占剩余宽度（可收缩到 0，内容截断），其余列定宽。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum CellWidth {
    Fill,
    Fixed(f32),
}

/// 名称列里标记图标槽与名称之间的固定缩进（图标槽宽 + 间隙）；表头的名称标题按同一缩进对齐。
pub(super) const NAME_INSET: f32 = IconSize::INLINE + Space::XS;

impl BrowseColumns {
    /// 表头与每一行共用的列序列。行里的自增/生成/索引标记都放在名称列内部，不改变这份序列。
    pub fn cells(&self) -> Vec<(FieldCell, CellWidth)> {
        let mut cells = vec![
            (FieldCell::Name, CellWidth::Fill),
            (FieldCell::Type, CellWidth::Fixed(self.widths.kind)),
            (FieldCell::Nullable, CellWidth::Fixed(self.widths.nullable)),
        ];
        if self.default {
            cells.push((FieldCell::Default, CellWidth::Fixed(self.widths.default)));
        }
        if self.comment {
            cells.push((FieldCell::Comment, CellWidth::Fixed(self.widths.comment)));
        }
        cells.push((FieldCell::Actions, CellWidth::Fixed(self.widths.drop)));
        cells
    }
}

/// 名称列里的标记（主键/外键/问题图标、自增、生成、索引）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct NameMarks {
    pub icon: bool,
    pub auto: bool,
    pub generated: bool,
    pub indexed: bool,
}

/// 一行实际使用的列序列。标记只进名称列，所以与表头序列相同（测试守住）。
pub(super) fn row_cells(cols: &BrowseColumns, _marks: NameMarks) -> Vec<(FieldCell, CellWidth)> {
    cols.cells()
}

/// 按宽度规则生成单元格外框。名称列 `min_w_0 + overflow_hidden`：标记 pill 留在格内，名称先截断，不压到类型列上。
pub(super) fn cell_frame(width: CellWidth) -> gpui_kit::Div {
    match width {
        CellWidth::Fill => div().flex_1().min_w_0().overflow_hidden(),
        CellWidth::Fixed(w) => div().w(px(w)).flex_none().min_w_0(),
    }
}

impl StructurePane {
    /// 正在编辑的字段行（`kept_columns` 下标）。
    pub fn editing_row(&self) -> Option<usize> {
        self.editing_row
    }

    /// 已有表字段列表中当前键盘选中的行（`kept_columns` 下标）。
    pub fn focused_row(&self) -> Option<usize> {
        self.focused_row
    }

    /// 选中某一行用于键盘浏览（Up/Down/Enter/F2/Delete）。
    pub fn focus_row(&mut self, index: Option<usize>) {
        self.focused_row = index.filter(|i| *i < self.session.kept_columns.len());
    }

    /// 当前筛选条件下可见的字段行下标列表（与虚拟列表渲染顺序一致）。
    pub(super) fn visible_column_indices(&self) -> Vec<usize> {
        self.session
            .kept_columns
            .iter()
            .enumerate()
            .filter(|(_, column)| self.column_matches_filter(column, self.column_change(column)))
            .map(|(index, _)| index)
            .collect()
    }

    /// 点击或双击某一行：这一行换成输入控件，焦点落在字段名上。点其他行即切换，草稿都保留。
    pub fn begin_edit_row(&mut self, index: usize) {
        if index >= self.session.kept_columns.len() {
            return;
        }
        self.focused_row = Some(index);
        if self.editing_row == Some(index) {
            return;
        }
        self.editing_row = Some(index);
        self.pending_focus_draft = Some(10_000 + index as u32);
    }

    /// 退出编辑（Esc）：只换回文本显示，草稿不动。
    pub fn end_edit_row(&mut self) -> bool {
        if let Some(row) = self.editing_row.take() {
            self.focused_row = Some(row);
            return true;
        }
        false
    }

    /// 删除一条未落库的新增行后，编辑行与选中行下标跟着前移。
    pub(super) fn forget_row(&mut self, removed: usize) {
        self.editing_row = match self.editing_row {
            Some(row) if row == removed => None,
            Some(row) if row > removed => Some(row - 1),
            other => other,
        };
        let len = self.session.kept_columns.len();
        self.focused_row = match self.focused_row {
            Some(row) if row == removed => (len > 0).then(|| removed.min(len - 1)),
            Some(row) if row > removed => Some(row - 1),
            other => other.filter(|row| *row < len),
        };
    }

    /// 已有表字段列表的键盘流：Up/Down 选行、Enter/F2 进入编辑、Delete 标记/撤销删除。
    pub(super) fn handle_browse_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.session.mode != StructureMode::Existing
            || self.session.section != Section::Columns
            || self.session.object_kind == "view"
            || self.session.preview_open()
            || self.template_manager.is_some()
            || self.editing_row.is_some()
        {
            return false;
        }
        if self.fields.is_focused("column_filter", window, cx) {
            return false;
        }
        let mods = &event.keystroke.modifiers;
        if mods.control || mods.platform || mods.alt {
            return false;
        }
        let visible = self.visible_column_indices();
        if visible.is_empty() {
            return false;
        }
        match event.keystroke.key.as_str() {
            "up" => {
                let next = match self.focused_row.and_then(|row| visible.iter().position(|&idx| idx == row)) {
                    Some(pos) => visible[pos.saturating_sub(1)],
                    None => visible[0],
                };
                self.focused_row = Some(next);
                self.scroll_to_column = Some(next);
                true
            }
            "down" => {
                let next = match self.focused_row.and_then(|row| visible.iter().position(|&idx| idx == row)) {
                    Some(pos) => visible[(pos + 1).min(visible.len() - 1)],
                    None => visible[0],
                };
                self.focused_row = Some(next);
                self.scroll_to_column = Some(next);
                true
            }
            "enter" | "f2" => {
                let Some(row) = self.focused_row.filter(|row| visible.contains(row)) else {
                    return false;
                };
                self.begin_edit_row(row);
                self.scroll_to_column = Some(row);
                true
            }
            "delete" => {
                let Some(row) = self.focused_row.filter(|row| visible.contains(row)) else {
                    return false;
                };
                let generated = self.session.kept_columns.get(row).is_some_and(|col| self.original_flags(col, false).1);
                if !generated {
                    self.toggle_column(row, "drop");
                }
                true
            }
            _ => false,
        }
    }

    /// 字段行的右键菜单项：复制名称、还原、新增字段、删除（或撤销删除）。
    pub(super) fn column_menu_entries(&self, index: usize, column: &ColumnDraft, change: ColumnChange, create: bool) -> Vec<MenuEntry> {
        let locale = self.session.locale;
        let (orig_primary, orig_generated) = self.original_flags(column, create);
        let dropped = change == ColumnChange::Dropped;
        let is_unsaved_new = column.original.is_none();
        let drop_locked = orig_generated || (create && is_unsaved_new && self.session.kept_columns.len() <= 1);
        let drop_label = if dropped {
            "structure.field.undoDelete"
        } else if is_unsaved_new && !create {
            "structure.field.removeDraft"
        } else {
            "structure.field.delete"
        };
        let mut entries = vec![MenuEntry {
            id: format!("edit-copy-col-{index}"),
            label: i18n::text(locale, "structure.menu.copyName"),
            action: RowAction::Copy(column.name.clone()),
            disabled: column.name.trim().is_empty(),
            reason: None,
        }];
        if !create && change == ColumnChange::Modified {
            entries.push(MenuEntry {
                id: format!("edit-revert-col-{index}"),
                label: i18n::text(locale, "structure.field.revert"),
                action: RowAction::RevertColumn(index),
                disabled: false,
                reason: None,
            });
        }
        if !dropped && !orig_generated && column.generated.is_none() {
            entries.push(MenuEntry {
                id: format!("edit-pk-col-{index}"),
                label: i18n::text(locale, if column.primary { "structure.pk.remove" } else { "structure.pk.makePrimary" }),
                action: RowAction::TogglePrimary(index),
                disabled: false,
                reason: None,
            });
        }
        entries.push(MenuEntry {
            id: format!("edit-add-col-{index}"),
            label: i18n::text(locale, "structure.field.addColumn"),
            action: RowAction::AddColumn,
            disabled: false,
            reason: None,
        });
        entries.push(MenuEntry {
            id: format!("edit-drop-col-{index}"),
            label: i18n::text(locale, drop_label),
            action: RowAction::ToggleColumnDrop(index),
            disabled: drop_locked,
            reason: if orig_primary {
                Some(i18n::text(locale, "structure.field.pkLocked"))
            } else if orig_generated {
                Some(i18n::text(locale, "structure.field.genLocked"))
            } else {
                None
            },
        });
        entries
    }

    /// 快照里的原列是否为主键、生成列（新建表时都按 false）。
    pub(super) fn original_flags(&self, column: &ColumnDraft, create: bool) -> (bool, bool) {
        if create {
            return (false, false);
        }
        let before = column.original.as_deref().and_then(|orig| self.snapshot.as_ref().and_then(|snap| snap.columns.iter().find(|c| c.name == orig)));
        (before.is_some_and(|c| c.primary), before.is_some_and(|c| c.generated))
    }

    /// 已有表的字段列表：表头 + 虚拟列表 + 末尾“添加字段”。只有可见行进入元素树。
    pub(super) fn column_browser(
        &mut self,
        visible: Vec<usize>,
        cols: BrowseColumns,
        show_presets_in_footer: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _ = window;
        let p = Palette::of(cx);
        let metrics = kit::Metrics::of(cx);
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let entity = cx.entity();
        if let Some(target) = self.scroll_to_column.take() {
            if let Some(position) = visible.iter().position(|index| *index == target) {
                self.columns_scroll.scroll_to_item(position, ScrollStrategy::Nearest);
            }
        }
        let mut head = div()
            .id("structure-table-head")
            .test_support()
            .flex()
            .flex_none()
            .h(px(metrics.row_h))
            .items_center()
            .gap(px(FieldWidths::GAP))
            .pl(px(FieldWidths::PAD_LEFT))
            .pr(px(FieldWidths::PAD_RIGHT))
            .bg(p.chrome)
            .border_b_1()
            .border_color(p.line_soft)
            .text_size(px(TypeScale::XS))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(p.text_faint);
        for (cell, width) in cols.cells() {
            let frame = cell_frame(width);
            head = head.child(match cell {
                FieldCell::Name => frame.pl(px(NAME_INSET)).truncate().child(t("structure.field.column")),
                FieldCell::Type => frame.truncate().child(t("structure.field.type")),
                FieldCell::Nullable => frame.flex().justify_center().child(t("ui.structure.nullable")),
                FieldCell::Default => frame.truncate().child(t("ui.structure.default")),
                FieldCell::Comment => frame.child(div().id("structure-head-comment").test_support().truncate().child(t("structure.field.comment"))),
                FieldCell::Actions => frame,
            });
        }

        // 每帧只算一次：行问题、索引成员，交给虚拟列表的行回调按下标取。
        let issues: HashMap<usize, String> = self.draft_issues().into_iter().filter_map(|issue| issue.row.map(|row| (row, issue.text))).collect();
        let editing_issue = self.editing_row.and_then(|row| issues.get(&row).cloned());
        let count = visible.len();
        let list = gpui_kit::uniform_list(
            "structure-columns-list",
            count,
            cx.processor(move |this, range: Range<usize>, window, cx| {
                range
                    .filter_map(|position| visible.get(position).copied())
                    .map(|index| this.column_list_row(index, cols, issues.get(&index).map(String::as_str), window, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.columns_scroll)
        .size_full();
        let body = div()
            .id("structure-columns-body")
            .test_support()
            .relative()
            .flex_1()
            .min_h(px(metrics.row_h))
            .child(list)
            .child(kit::scrollbar_overlay("structure-columns-bar", &self.columns_scroll, kit::ScrollbarAxis::Vertical));
        let table_area: AnyElement = if self.show_all_columns {
            div()
                .id("structure-columns-hscroll")
                .test_support()
                .flex_1()
                .min_h_0()
                .w_full()
                .scroll_area_x()
                .child(
                    div()
                        .min_w(px(cols.min_total()))
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(head)
                        .child(body),
                )
                .into_any_element()
        } else {
            div().flex_1().min_h_0().flex().flex_col().child(head).child(body).into_any_element()
        };
        let add_entity = entity.clone();
        let footer_presets = show_presets_in_footer.then(|| self.templates_button(&entity));
        let add = div()
            .id("structure-add-column-row")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(Space::XS))
            .h(px(metrics.row_h + Space::XS))
            .px(px(Space::SM))
            .border_t_1()
            .border_color(p.line_soft)
            .bg(p.chrome)
            .child(kit::tool_button("structure-add-column", Some(IconName::Plus), t("structure.field.addColumn"), move |_, _, cx| {
                add_entity.update(cx, |pane, cx| {
                    pane.add_column();
                    cx.notify();
                });
            }))
            .children(footer_presets);
        kit::card(&p)
            .overflow_hidden()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(table_area)
            .when_some(editing_issue, |this, text| {
                this.child(
                    div()
                        .flex_none()
                        .border_t_1()
                        .border_color(p.line_soft)
                        .bg(p.danger_soft.opacity(0.35))
                        .child(create::issue_line("structure-editing-row-issue", &text, &p)),
                )
            })
            .child(add)
            .into_any_element()
    }

    fn column_list_row(&mut self, index: usize, cols: BrowseColumns, issue: Option<&str>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(column) = self.session.kept_columns.get(index).cloned() else {
            return div().into_any_element();
        };
        let change = self.column_change(&column);
        let p = Palette::of(cx);
        let entity = cx.entity();
        if self.editing_row == Some(index) {
            let row_h = kit::Metrics::of(cx).row_h;
            let engine = self.session.target.engine.clone();
            let mut widths = cols.widths;
            if !cols.comment {
                widths.comment = 0.;
            }
            return self.column_row(index, &column, change, false, &engine, row_h, widths, issue, &p, &entity, window, cx);
        }
        self.browse_row(index, &column, change, cols, issue, &p, &entity, cx)
    }

    /// 浏览态的一行：名称、类型、可空、默认值、注释、主键或索引标记，全是文本。
    #[allow(clippy::too_many_arguments)]
    fn browse_row(
        &self,
        index: usize,
        column: &ColumnDraft,
        change: ColumnChange,
        cols: BrowseColumns,
        issue: Option<&str>,
        p: &Palette,
        entity: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let metrics = kit::Metrics::of(cx);
        let mono = cx.theme().mono_font_family.clone();
        let snapshot = self.snapshot.as_ref();
        let before = column.original.as_deref().and_then(|orig| snapshot.and_then(|snap| snap.columns.iter().find(|c| c.name == orig)));
        let generated = before.is_some_and(|c| c.generated);
        let auto = before.is_some_and(|c| c.auto);
        let comment = column
            .original
            .as_deref()
            .and_then(|orig| snapshot.and_then(|snap| snap.column_comments.get(orig)).cloned())
            .unwrap_or_else(|| column.comment.clone());
        let fk_target = column.original.as_deref().and_then(|orig| {
            snapshot.and_then(|snap| {
                snap.foreign_keys
                    .iter()
                    .find(|fk| fk.columns.iter().any(|c| c == orig))
                    .map(|fk| format!("{}.{}", fk.ref_table, fk.ref_columns.join(",")))
            })
        });
        let indexes: Vec<String> = column
            .original
            .as_deref()
            .map(|orig| {
                snapshot
                    .map(|snap| {
                        snap.indexes.iter().filter(|ix| !ix.protected && ix.columns.iter().any(|c| c.name == orig)).map(|ix| ix.name.clone()).collect()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let dropped = change == ColumnChange::Dropped;
        let mark: Option<(IconName, gpui_kit::Hsla, String)> = if let Some(issue) = issue {
            Some((IconName::TriangleAlert, p.danger, issue.to_owned()))
        } else if column.primary {
            let mut tip = i18n::text(locale, "structure.field.primary");
            if auto {
                tip = format!("{tip} · {}", i18n::text(locale, "structure.field.auto"));
            }
            Some((IconName::Key, p.warn, tip))
        } else if let Some(target) = &fk_target {
            Some((IconName::Key, p.info, i18n::fill(&i18n::text(locale, "structure.field.badgeFk"), &[("target", target)])))
        } else if generated {
            Some((IconName::Lock, p.text_faint, i18n::text(locale, "structure.field.genLocked")))
        } else {
            None
        };
        let (bar, tint) = match change {
            ColumnChange::Added => (Some(p.ok), Some(p.ok_soft.opacity(0.45))),
            ColumnChange::Modified => (Some(p.warn), Some(p.warn_soft.opacity(0.45))),
            ColumnChange::Dropped => (Some(p.danger), Some(p.danger_soft.opacity(0.35))),
            ColumnChange::Clean => (None, None),
        };
        let default_text = column.default.label();
        let menu_entries = self.column_menu_entries(index, column, change, false);
        let menu_owner = entity.clone();
        let (orig_primary, orig_generated) = self.original_flags(column, false);
        let is_unsaved_new = column.original.is_none();
        let drop_label = if dropped {
            t("structure.field.undoDelete")
        } else if is_unsaved_new {
            t("structure.field.removeDraft")
        } else {
            t("structure.field.delete")
        };
        let drop_entity = entity.clone();
        let drop_btn = kit::icon_button(
            SharedString::from(format!("structure-drop-col-{index}")),
            if dropped {
                IconName::Undo2
            } else if is_unsaved_new {
                IconName::X
            } else {
                IconName::Trash
            },
            drop_label,
            move |_, _, cx| {
                cx.stop_propagation();
                drop_entity.update(cx, |pane, cx| {
                    pane.toggle_column(index, "drop");
                    cx.notify();
                });
            },
        )
        .xsmall()
        .disabled(orig_generated);
        let drop_btn = if orig_primary {
            drop_btn.tooltip(t("structure.field.pkLocked"))
        } else if orig_generated {
            drop_btn.tooltip(t("structure.field.genLocked"))
        } else {
            drop_btn
        };
        let revert_btn = (change == ColumnChange::Modified).then(|| {
            let owner = entity.clone();
            kit::icon_button(SharedString::from(format!("structure-revert-col-{index}")), IconName::Undo2, t("structure.field.revert"), move |_, _, cx| {
                cx.stop_propagation();
                owner.update(cx, |pane, cx| {
                    pane.revert_column(index);
                    cx.notify();
                });
            })
            .xsmall()
        });
        // 悬停名称：全名与标记含义（主键/外键、自增、生成、所在索引）。
        let mut name_tip = column.name.clone();
        if let Some((_, _, tip)) = &mark {
            name_tip.push_str(&format!("\n{tip}"));
        }
        if auto {
            name_tip.push_str(&format!("\n{}", i18n::text(locale, "structure.field.auto")));
        }
        if generated {
            name_tip.push_str(&format!("\n{}", i18n::text(locale, "structure.field.genLocked")));
        }
        if !indexes.is_empty() {
            name_tip.push_str(&format!("\n{}", i18n::fill(&i18n::text(locale, "structure.field.inIndexes"), &[("names", &indexes.join(", "))])));
        }
        let marks = NameMarks { icon: mark.is_some(), auto, generated, indexed: !indexes.is_empty() };
        let pk_badge = self.primary_key_position(index).filter(|_| self.primary_key_order().len() > 1);
        let mut mark = mark;
        let mut revert_btn = revert_btn;
        let mut drop_btn = Some(drop_btn);
        let row_group = SharedString::from(format!("structure-row-group-{index}"));
        let mut cells: Vec<AnyElement> = Vec::new();
        for (cell, width) in row_cells(&cols, marks) {
            let frame = cell_frame(width);
            let element = match cell {
                FieldCell::Name => frame
                    .flex()
                    .items_center()
                    .gap(px(Space::XS))
                    .child(
                        div().w(px(IconSize::INLINE)).flex_none().flex().items_center().justify_center().when_some(mark.take(), |this, (icon, color, tip)| {
                            this.child(
                                div()
                                    .id(SharedString::from(format!("structure-col-mark-{index}")))
                                    .tooltip_text(SharedString::from(tip))
                                    .child(kit::icon(icon, IconSize::INLINE, color)),
                            )
                        }),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("structure-col-name-{index}")))
                            .test_support()
                            // 不撑满：主键序号、自增、索引标记紧跟在名字后面，而不是被推到列的最右边。
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .tooltip_text(SharedString::from(name_tip.clone()))
                            .when(dropped, |this| this.opacity(0.5).line_through())
                            .child(SharedString::from(column.name.clone())),
                    )
                    .when_some(pk_badge, |this, position| {
                        this.child(
                            div()
                                .id(SharedString::from(format!("structure-col-pk-{index}")))
                                .test_support()
                                .flex_none()
                                .child(kit::pill(SharedString::from(format!("PK{position}")), p.warn, p.warn_soft)),
                        )
                    })
                    .when(marks.auto || marks.generated, |this| {
                        let key = if generated { "structure.field.badgeGen" } else { "structure.field.badgeAuto" };
                        this.child(kit::pill(t(key), p.text_muted, p.hover))
                    })
                    .when(marks.indexed, |this| {
                        this.child(
                            div()
                                .id(SharedString::from(format!("structure-col-index-{index}")))
                                .test_support()
                                .flex_none()
                                .tooltip_text(SharedString::from(indexes.join(", ")))
                                .child(kit::pill(t("structure.field.badgeIndex"), p.accent_text, p.accent_soft)),
                        )
                    })
                    .into_any_element(),
                FieldCell::Type => frame
                    .truncate()
                    .font_family(mono.clone())
                    .text_color(p.text_muted)
                    .when(dropped, |this| this.opacity(0.5))
                    .child(SharedString::from(column.type_name.clone()))
                    .into_any_element(),
                FieldCell::Nullable => frame
                    .flex()
                    .justify_center()
                    .when(column.nullable, |this| this.child(kit::icon(IconName::Check, IconSize::INLINE, p.text_muted)))
                    .into_any_element(),
                FieldCell::Default => frame
                    .child(
                        div()
                            .id(SharedString::from(format!("structure-col-default-{index}")))
                            .truncate()
                            .font_family(mono.clone())
                            .text_color(if default_text.is_empty() { p.text_faint } else { p.text_muted })
                            .when(!default_text.is_empty(), |this| this.tooltip_text(SharedString::from(default_text.clone())))
                            .child(SharedString::from(if default_text.is_empty() { "—".to_owned() } else { default_text.clone() })),
                    )
                    .into_any_element(),
                FieldCell::Comment => frame
                    .child(
                        div()
                            .id(SharedString::from(format!("structure-col-comment-{index}")))
                            .test_support()
                            .truncate()
                            .text_color(p.text_faint)
                            .when(!comment.is_empty(), |this| this.tooltip_text(SharedString::from(comment.clone())))
                            .child(SharedString::from(comment.clone())),
                    )
                    .into_any_element(),
                FieldCell::Actions => frame
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(Space::XXS))
                    // 干净的行只在悬停/选中时露出删除按钮：几十行的垃圾桶不再常驻成一条噪声。
                    // 改过、删过的行（还原/撤销按钮）始终可见。
                    .when(change == ColumnChange::Clean && self.focused_row != Some(index), |this| {
                        this.opacity(0.).group_hover(row_group.clone(), |style| style.opacity(1.))
                    })
                    .when_some(revert_btn.take(), |this, btn| this.child(btn))
                    .when_some(drop_btn.take(), |this, btn| this.child(btn))
                    .into_any_element(),
            };
            cells.push(element);
        }
        let is_focused = self.focused_row == Some(index);
        let edit_owner = entity.clone();
        div()
            .id(SharedString::from(format!("structure-row-columns-{index}")))
            .test_support()
            .group(row_group)
            .role(gpui_kit::Role::Row)
            .aria_label(SharedString::from(column.name.clone()))
            .aria_selected(is_focused)
            // 虚拟列表按内容宽度排版每一行：撑满列表宽度，名称列才和表头一样分到剩余宽度。
            .w_full()
            .flex()
            .items_center()
            .gap(px(FieldWidths::GAP))
            .relative()
            .h(px(metrics.row_h))
            .pl(px(FieldWidths::PAD_LEFT))
            .pr(px(FieldWidths::PAD_RIGHT))
            .border_b_1()
            .border_color(p.line_soft)
            .cursor_pointer()
            .text_size(px(TypeScale::SM))
            .when(index % 2 == 1 && tint.is_none() && !is_focused, |this| this.bg(p.zebra))
            .when_some(tint, |this, tint| this.bg(tint))
            .when(is_focused, |this| this.bg(p.selection))
            .hover(|style| style.bg(p.hover))
            .on_click(move |_, _, cx| {
                edit_owner.update(cx, |pane, cx| {
                    pane.begin_edit_row(index);
                    cx.notify();
                });
            })
            .context_menu(move |menu, _, _| row_menu(&menu_owner, &menu_entries, menu))
            .when_some(bar.or_else(|| is_focused.then_some(p.accent)), |this, color| {
                this.child(div().absolute().left_0().top_0().bottom_0().w(px(Space::XXS + 1.)).bg(color))
            })
            .children(cells)
            .into_any_element()
    }

    /// 新建索引：多选列选择器。列集合或已选集合在别处变了（移除、上下移）就重建，与草稿保持一致。
    pub(super) fn ensure_index_picker(&mut self, names: &[String], window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let types: HashMap<&str, &str> =
            self.snapshot.as_ref().map(|snap| snap.columns.iter().map(|c| (c.name.as_str(), c.type_name.as_str())).collect()).unwrap_or_default();
        let items: Vec<PickerItem> = names
            .iter()
            .map(|name| {
                let item = PickerItem::new(name.clone(), name.clone());
                match types.get(name.as_str()) {
                    Some(kind) => item.detail(kind.to_string()),
                    None => item,
                }
            })
            .collect();
        let chosen: Vec<SharedString> = self.session.index_columns.iter().map(|column| SharedString::from(column.name.clone())).collect();
        let stale = match &self.index_picker {
            None => true,
            Some((picker, _)) => {
                let model = picker.read(cx).model();
                model.items() != items.as_slice() || !same_members(model.chosen(), &chosen)
            }
        };
        if stale {
            let locale = self.session.locale;
            let picker = cx.new(|cx| Picker::new(items, PickerMode::Multi, chosen, locale, window, cx));
            let subscription = cx.subscribe_in(&picker, window, |this, _, event: &PickerEvent, _, cx| {
                match event {
                    PickerEvent::Toggled(ids) => this.set_index_selection(ids),
                    PickerEvent::Dismissed => this.composer_open = false,
                    PickerEvent::Chosen(_) => {}
                }
                cx.notify();
            });
            self.index_picker = Some((picker, subscription));
        }
        self.index_picker.as_ref().map(|(picker, _)| picker.clone().into_any_element()).unwrap_or_else(|| div().into_any_element())
    }

    /// 选择器里的勾选集合落到草稿：已选列保持原顺序，新勾的列接在末尾，取消勾选的移除。
    pub fn set_index_selection<S: AsRef<str>>(&mut self, ids: &[S]) {
        let wanted: Vec<&str> = ids.iter().map(AsRef::as_ref).collect();
        let key = |columns: &[IndexColumnDraft]| columns.iter().map(|column| (column.name.clone(), column.descending)).collect::<Vec<_>>();
        let before = key(&self.session.index_columns);
        self.session.index_columns.retain(|column| wanted.contains(&column.name.as_str()));
        for id in &wanted {
            if !self.session.index_columns.iter().any(|column| column.name == *id) {
                self.session.index_columns.push(IndexColumnDraft { name: (*id).to_owned(), descending: false, prefix: None });
            }
        }
        if key(&self.session.index_columns) == before {
            return;
        }
        self.session.index_column = self.session.index_columns.first().map(|column| column.name.clone()).unwrap_or_default();
        self.sync_auto_index_name();
        self.session.intent = super::EditIntent::Apply;
        self.session.cancel();
    }

    /// 已选列的有序列表：序号、列名、升降序切换、上移、下移、移除。生成的 SQL 按这里的顺序。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn index_order_list(
        &mut self,
        entity: &Entity<Self>,
        selected: &[IndexColumnDraft],
        kind: IndexKind,
        prefix_enabled: bool,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let locale = self.session.locale;
        let types: HashMap<String, String> =
            self.snapshot.as_ref().map(|snap| snap.columns.iter().map(|c| (c.name.clone(), c.type_name.clone())).collect()).unwrap_or_default();
        let prefix_placeholder = i18n::text(locale, "structure.index.prefixHint");
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let metrics = kit::Metrics::of(cx);
        let mono = cx.theme().mono_font_family.clone();
        let mut list = div()
            .id("structure-index-order")
            .test_support()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(Space::XS));
        if selected.is_empty() {
            return list.child(kit::muted(t("structure.index.pickHint"), p)).into_any_element();
        }
        let last = selected.len() - 1;
        for (position, column) in selected.iter().enumerate() {
            let name = column.name.clone();
            let order_entity = entity.clone();
            let up_entity = entity.clone();
            let down_entity = entity.clone();
            let remove_entity = entity.clone();
            let order_name = name.clone();
            let remove_name = name.clone();
            // 前缀长度只给字符串/二进制列，全文与空间索引不能带前缀；不支持的方言整项隐藏。
            let prefix_input = (prefix_enabled
                && !kind.plain_key_parts()
                && types.get(&name).is_some_and(|kind| crate::ddl::plan::prefixable_type(kind)))
            .then(|| {
                let value = column.prefix.map(|length| length.to_string()).unwrap_or_default();
                div()
                    .id(SharedString::from(format!("structure-index-prefix-{name}")))
                    .test_support()
                    .flex_none()
                    .w(px(ControlSize::PREFIX_W))
                    .tooltip_text(t("structure.index.prefix"))
                    .child(self.draft_input(120_001 + position as u32, &value, &prefix_placeholder, window, cx))
            });
            let accent = p.accent;
            list = list.child(
                div()
                    .id(SharedString::from(format!("structure-index-chip-{name}")))
                    .test_support()
                    .h(px(metrics.control_h))
                    .pl(px(Space::SM))
                    .border_1()
                    .border_color(p.accent_soft)
                    .on_drag(IndexColumnDrag { position, label: SharedString::from(name.clone()) }, |drag, _, _, cx| {
                        cx.new(|_| create::ColumnDragGhost { label: drag.label.clone() })
                    })
                    .drag_over::<IndexColumnDrag>(move |style, _, _, _| style.border_color(accent))
                    .on_drop(cx.listener(move |pane, drag: &IndexColumnDrag, _, cx| {
                        pane.move_index_column_to(drag.position, position);
                        cx.notify();
                    }))
                    .pr(px(Space::XXS))
                    .flex()
                    .items_center()
                    .gap(px(Space::SM))
                    .rounded(px(Radius::CONTROL))
                    .bg(p.accent_soft)
                    .text_color(p.accent_text)
                    .text_size(px(TypeScale::SM))
                    .child(
                        div()
                            .id(SharedString::from(format!("structure-index-grip-{name}")))
                            .test_support()
                            .flex_none()
                            .cursor_grab()
                            .tooltip_text(t("structure.index.dragHint"))
                            .child(kit::icon(IconName::GripVertical, IconSize::INLINE, p.text_faint)),
                    )
                    .child(div().flex_none().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(format!("{}.", position + 1))))
                    .child(div().flex_1().min_w_0().truncate().font_family(mono.clone()).child(SharedString::from(name.clone())))
                    .children(prefix_input)
                    .when(!kind.plain_key_parts(), |this| this.child(
                        div()
                            .id(SharedString::from(format!("structure-index-order-{name}")))
                            .test_support()
                            .flex_none()
                            .px(px(Space::XS))
                            .rounded(px(Radius::TAG))
                            .cursor_pointer()
                            .hover(|style| style.bg(p.surface))
                            .tooltip_text(t("structure.index.toggleOrder"))
                            .child(if column.descending { t("structure.index.desc") } else { t("structure.index.asc") })
                            .on_click(move |_, _, cx| {
                                let name = order_name.clone();
                                order_entity.update(cx, |pane, cx| {
                                    pane.toggle_index_order(&name);
                                    cx.notify();
                                });
                            }),
                    ))
                    .child(
                        kit::icon_button(SharedString::from(format!("structure-index-up-{name}")), IconName::ChevronUp, t("structure.index.moveLeft"), move |_, _, cx| {
                            up_entity.update(cx, |pane, cx| {
                                pane.move_index_column(position, -1);
                                cx.notify();
                            });
                        })
                        .xsmall()
                        .disabled(position == 0),
                    )
                    .child(
                        kit::icon_button(SharedString::from(format!("structure-index-down-{name}")), IconName::ChevronDown, t("structure.index.moveRight"), move |_, _, cx| {
                            down_entity.update(cx, |pane, cx| {
                                pane.move_index_column(position, 1);
                                cx.notify();
                            });
                        })
                        .xsmall()
                        .disabled(position == last),
                    )
                    .child(
                        kit::icon_button(SharedString::from(format!("structure-index-remove-{name}")), IconName::X, t("structure.index.removeColumn"), move |_, _, cx| {
                            let name = remove_name.clone();
                            remove_entity.update(cx, |pane, cx| {
                                pane.toggle_index_column(&name);
                                cx.notify();
                            });
                        })
                        .xsmall(),
                    ),
            );
        }
        list.into_any_element()
    }
}

fn same_members(left: &[SharedString], right: &[SharedString]) -> bool {
    left.len() == right.len() && left.iter().all(|item| right.contains(item))
}
