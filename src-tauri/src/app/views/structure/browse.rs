//! 字段页（已有表）以浏览为主：`uniform_list` 只渲染可见行，每行默认是文本；
//! 点击某行进入编辑（换成 `column_row` 的输入控件），Esc 或点击其他行退出编辑并保留草稿。
//! 窄窗口里按优先级先隐藏注释列、再隐藏默认值列，“显示全部列”可强制显示。
//! 另含新建索引表单里的多选列选择器（`ui::picker`）与已选列的有序列表。

use std::collections::HashMap;
use std::ops::Range;

use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, ScrollStrategy,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px,
};

use super::{ColumnChange, FieldWidths, MenuEntry, RowAction, StructurePane, row_menu};
use crate::ddl::{ColumnDraft, IndexColumnDraft};
use crate::i18n;
use crate::ui::kit::{self, ControlSize, IconSize, Palette, Radius, Space, Tip as _, TypeScale};
use crate::ui::picker::{Picker, PickerEvent, PickerItem, PickerMode};

/// 浏览态字段表的可见列与列宽。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BrowseColumns {
    pub widths: FieldWidths,
    pub default: bool,
    pub comment: bool,
}

impl BrowseColumns {
    pub const COMMENT_W: f32 = 160.;

    /// 按内容宽度决定显示哪些列：放得下就全显示；不够时先隐藏注释，再隐藏默认值。
    /// `content_w <= 0` 表示还没量到宽度，按宽窗口处理。`show_all` 强制全部显示（列宽取紧凑档）。
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
        let mut widths = FieldWidths::of(content_w, false);
        if show_all {
            widths.comment = widths.default;
            return Self { widths, default: true, comment: true };
        }
        if widths.min_total(false) <= content_w {
            return Self { widths, default: true, comment: false };
        }
        widths.default = 0.;
        Self { widths, default: false, comment: false }
    }

    /// 有列因为宽度不够被隐藏（工具条显示“显示全部列”开关）。
    pub fn hides_any(content_w: f32) -> bool {
        let all = Self::of(content_w, false);
        !(all.default && all.comment)
    }
}

impl StructurePane {
    /// 正在编辑的字段行（`kept_columns` 下标）。
    pub fn editing_row(&self) -> Option<usize> {
        self.editing_row
    }

    /// 点击或双击某一行：这一行换成输入控件，焦点落在字段名上。点其他行即切换，草稿都保留。
    pub fn begin_edit_row(&mut self, index: usize) {
        if index >= self.session.kept_columns.len() || self.editing_row == Some(index) {
            return;
        }
        self.editing_row = Some(index);
        self.pending_focus_draft = Some(10_000 + index as u32);
    }

    /// 退出编辑（Esc）：只换回文本显示，草稿不动。
    pub fn end_edit_row(&mut self) -> bool {
        self.editing_row.take().is_some()
    }

    /// 删除一条未落库的新增行后，编辑行下标跟着前移。
    pub(super) fn forget_row(&mut self, removed: usize) {
        self.editing_row = match self.editing_row {
            Some(row) if row == removed => None,
            Some(row) if row > removed => Some(row - 1),
            other => other,
        };
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
    pub(super) fn column_browser(&mut self, visible: Vec<usize>, cols: BrowseColumns, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let p = Palette::of(cx);
        let metrics = kit::Metrics::of(cx);
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let entity = cx.entity();
        let widths = cols.widths;
        if let Some(target) = self.scroll_to_column.take() {
            if let Some(position) = visible.iter().position(|index| *index == target) {
                self.columns_scroll.scroll_to_item(position, ScrollStrategy::Nearest);
            }
        }
        let head = div()
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
            .border_color(p.line)
            .text_size(px(TypeScale::XS))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(p.text_faint)
            .child(div().flex_1().min_w(px(widths.name_min)).pl(px(Space::XL)).child(t("structure.field.column")))
            .child(div().w(px(widths.kind)).flex_none().child(t("structure.field.type")))
            .child(div().w(px(widths.nullable)).flex_none().flex().justify_center().child(t("ui.structure.nullable")))
            .when(cols.default, |this| this.child(div().w(px(widths.default)).flex_none().child(t("ui.structure.default"))))
            .when(cols.comment, |this| this.child(div().id("structure-head-comment").test_support().w(px(widths.comment)).flex_none().child(t("structure.field.comment"))))
            .child(div().w(px(widths.drop)).flex_none());

        // 每帧只算一次：行问题、索引成员，交给虚拟列表的行回调按下标取。
        let issues: HashMap<usize, String> = self.draft_issues().into_iter().filter_map(|issue| issue.row.map(|row| (row, issue.text))).collect();
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
        let add_entity = entity.clone();
        let add = div()
            .id("structure-add-column-row")
            .flex()
            .flex_none()
            .items_center()
            .h(px(metrics.row_h + Space::XS))
            .px(px(Space::SM))
            .border_t_1()
            .border_color(p.line_soft)
            .bg(p.surface)
            .child(kit::tool_button("structure-add-column", Some(IconName::Plus), t("structure.field.addColumn"), move |_, _, cx| {
                add_entity.update(cx, |pane, cx| {
                    pane.add_column();
                    cx.notify();
                });
            }));
        kit::card(&p).overflow_hidden().flex().flex_col().flex_1().min_h_0().child(head).child(body).child(add).into_any_element()
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
            return self.column_row(index, &column, change, false, &engine, row_h, widths, &p, &entity, window, cx);
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
        let widths = cols.widths;
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
        let edit_owner = entity.clone();
        div()
            .id(SharedString::from(format!("structure-row-columns-{index}")))
            .test_support()
            .role(gpui_kit::Role::Row)
            .aria_label(SharedString::from(column.name.clone()))
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
            .when_some(tint, |this, tint| this.bg(tint))
            .hover(|style| style.bg(p.hover))
            .on_click(move |_, _, cx| {
                edit_owner.update(cx, |pane, cx| {
                    pane.begin_edit_row(index);
                    cx.notify();
                });
            })
            .context_menu(move |menu, _, _| row_menu(&menu_owner, &menu_entries, menu))
            .when_some(bar, |this, color| this.child(div().absolute().left_0().top_0().bottom_0().w(px(Space::XXS + 1.)).bg(color)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(widths.name_min))
                    .flex()
                    .items_center()
                    .gap(px(Space::XS))
                    .child(
                        div().w(px(IconSize::INLINE)).flex_none().flex().items_center().justify_center().when_some(mark, |this, (icon, color, tip)| {
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
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .when(dropped, |this| this.opacity(0.5).line_through())
                            .child(SharedString::from(column.name.clone())),
                    )
                    .when(auto || generated, |this| {
                        let key = if generated { "structure.field.badgeGen" } else { "structure.field.badgeAuto" };
                        this.child(kit::pill(t(key), p.text_muted, p.hover))
                    })
                    .when(!indexes.is_empty(), |this| {
                        this.child(
                            div()
                                .id(SharedString::from(format!("structure-col-index-{index}")))
                                .test_support()
                                .flex_none()
                                .tooltip_text(SharedString::from(indexes.join(", ")))
                                .child(kit::pill(t("structure.field.badgeIndex"), p.accent_text, p.accent_soft)),
                        )
                    }),
            )
            .child(
                div()
                    .w(px(widths.kind))
                    .flex_none()
                    .min_w_0()
                    .truncate()
                    .font_family(mono.clone())
                    .text_color(p.text_muted)
                    .when(dropped, |this| this.opacity(0.5))
                    .child(SharedString::from(column.type_name.clone())),
            )
            .child(
                div()
                    .w(px(widths.nullable))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .when(column.nullable, |this| this.child(kit::icon(IconName::Check, IconSize::INLINE, p.text_muted))),
            )
            .when(cols.default, |this| {
                this.child(
                    div()
                        .id(SharedString::from(format!("structure-col-default-{index}")))
                        .w(px(widths.default))
                        .flex_none()
                        .min_w_0()
                        .truncate()
                        .font_family(mono.clone())
                        .text_color(if default_text.is_empty() { p.text_faint } else { p.text_muted })
                        .when(!default_text.is_empty(), |this| this.tooltip_text(SharedString::from(default_text.clone())))
                        .child(SharedString::from(if default_text.is_empty() { "—".to_owned() } else { default_text.clone() })),
                )
            })
            .when(cols.comment, |this| {
                this.child(
                    div()
                        .id(SharedString::from(format!("structure-col-comment-{index}")))
                        .test_support()
                        .w(px(widths.comment))
                        .flex_none()
                        .min_w_0()
                        .truncate()
                        .text_color(p.text_faint)
                        .when(!comment.is_empty(), |this| this.tooltip_text(SharedString::from(comment.clone())))
                        .child(SharedString::from(comment.clone())),
                )
            })
            .child(
                div()
                    .w(px(widths.drop))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(Space::XXS))
                    .when_some(revert_btn, |this, btn| this.child(btn))
                    .child(drop_btn),
            )
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
                self.session.index_columns.push(IndexColumnDraft { name: (*id).to_owned(), descending: false });
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
    pub(super) fn index_order_list(&self, entity: &Entity<Self>, selected: &[IndexColumnDraft], p: &Palette, cx: &App) -> AnyElement {
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let metrics = kit::Metrics::of(cx);
        let mono = cx.theme().mono_font_family.clone();
        let mut list = div()
            .id("structure-index-order")
            .test_support()
            .flex_1()
            .min_w(px(ControlSize::PICKER_MIN_W))
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
            list = list.child(
                div()
                    .id(SharedString::from(format!("structure-index-chip-{name}")))
                    .test_support()
                    .h(px(metrics.control_h))
                    .pl(px(Space::MD))
                    .pr(px(Space::XXS))
                    .flex()
                    .items_center()
                    .gap(px(Space::SM))
                    .rounded(px(Radius::CONTROL))
                    .bg(p.accent_soft)
                    .text_color(p.accent_text)
                    .text_size(px(TypeScale::SM))
                    .child(div().flex_none().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(format!("{}.", position + 1))))
                    .child(div().flex_1().min_w_0().truncate().font_family(mono.clone()).child(SharedString::from(name.clone())))
                    .child(
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
                    )
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
