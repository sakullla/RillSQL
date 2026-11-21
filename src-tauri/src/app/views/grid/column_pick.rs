//! 字段选择器（R4）：筛选条件的字段与工具栏“跳到列”共用一个 `ui::picker` 弹层。
//! 几百列的表上可以按名字或类型搜索；弹层按窗口剩余空间限高，内部滚动，不超出窗口。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    anchored, deferred, div, px, Anchor, AnyElement, App, AppContext as _, Bounds, Context, Entity, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Pixels, Point, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _,
    Window,
};

use super::GridPane;
use crate::i18n;
use crate::ui::kit::{self, ControlSize, Palette, Radius, ScrollAreaExt as _, Space, Tip as _, TypeScale};
use crate::ui::picker::{picker_popover, Picker, PickerEvent, PickerItem, PickerMode};

/// 选择器为谁打开。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ColumnPickTarget {
    /// 第 n 条筛选条件的字段。
    Filter(usize),
    /// 工具栏“跳到列”。
    Jump,
    /// 工具栏“列管理”（显隐与顺序）。
    Manage,
}

/// 集中式列管理弹层：搜索列、勾选显隐（至少保留一列）、调整顺序与重置。
pub(crate) struct ColumnManager {
    pub(crate) search: Entity<InputState>,
    pub(crate) query: String,
    _subscription: Subscription,
}

/// 触发按钮上一帧在窗口里的位置；弹层贴着它弹出。
pub(crate) type PickerAnchors = Rc<RefCell<HashMap<ColumnPickTarget, Bounds<Pixels>>>>;

pub(crate) struct ColumnPicker {
    pub(crate) target: ColumnPickTarget,
    pub(crate) picker: Entity<Picker>,
    _subscription: Subscription,
}

/// 记下触发按钮的位置：放在按钮里、铺满按钮，不参与布局。
pub(crate) fn anchor_probe(anchors: &PickerAnchors, target: ColumnPickTarget) -> AnyElement {
    let anchors = anchors.clone();
    gpui_kit::canvas(move |area, _, _| {
        anchors.borrow_mut().insert(target, area);
    }, |_, _, _, _| {})
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

impl GridPane {
    /// 正在打开（或等绘制阶段打开）的选择器。
    pub(crate) fn column_picker_target(&self) -> Option<ColumnPickTarget> {
        self.column_picker.as_ref().map(|open| open.target).or(self.picker_request)
    }

    /// 选择器里的列：筛选列出全部列（隐藏列也能筛），跳到列只列出界面上看得见的列。
    /// 说明栏写类型，搜索同样能按类型找。
    pub(crate) fn column_picker_items(&self, target: ColumnPickTarget) -> (Vec<PickerItem>, Vec<SharedString>) {
        let names = self.session.column_names();
        let item = |logical: usize| {
            let name = names[logical].clone();
            let item = PickerItem::new(name.clone(), name);
            match self.session.column_type(logical).filter(|kind| !kind.is_empty()) {
                Some(kind) => item.detail(kind.to_owned()),
                None => item,
            }
        };
        match target {
            ColumnPickTarget::Filter(index) => {
                let chosen = self.session.filters().get(index).map(|draft| SharedString::from(draft.column.clone())).into_iter().collect();
                ((0..names.len()).map(item).collect(), chosen)
            }
            ColumnPickTarget::Jump | ColumnPickTarget::Manage => {
                let chosen = self.session.focus_cell().and_then(|(_, column)| names.get(column)).map(|name| SharedString::from(name.clone())).into_iter().collect();
                (self.session.visible_columns().into_iter().filter(|logical| *logical < names.len()).map(item).collect(), chosen)
            }
        }
    }

    /// 打开或关闭（再点一次同一个按钮）选择器；搜索框拿到键盘焦点。
    pub(crate) fn toggle_column_picker(&mut self, target: ColumnPickTarget, window: &mut Window, cx: &mut Context<Self>) {
        if self.column_picker.as_ref().is_some_and(|open| open.target == target) {
            self.close_column_picker(window, cx);
            return;
        }
        self.open_column_picker(target, window, cx);
    }

    pub(crate) fn open_column_picker(&mut self, target: ColumnPickTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.picker_request = None;
        let (items, chosen) = self.column_picker_items(target);
        if items.is_empty() {
            self.column_picker = None;
            return;
        }
        let locale = self.locale;
        let picker = cx.new(|cx| Picker::new(items, PickerMode::Single, chosen, locale, window, cx));
        let subscription = cx.subscribe_in(&picker, window, move |this, _, event: &PickerEvent, window, cx| match event {
            PickerEvent::Chosen(id) => {
                this.close_column_picker(window, cx);
                this.column_picked(target, id, cx);
            }
            PickerEvent::Dismissed => this.close_column_picker(window, cx),
            PickerEvent::Toggled(_) => {}
        });
        picker.update(cx, |picker, cx| picker.focus_search(window, cx));
        self.column_picker = Some(ColumnPicker { target, picker, _subscription: subscription });
        cx.notify();
    }

    pub(crate) fn close_column_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.column_picker.take().is_some() {
            // 键盘回到网格，Esc 之后方向键仍然可用。
            window.focus(&self.grid_focus, cx);
            cx.notify();
        }
    }

    fn column_picked(&mut self, target: ColumnPickTarget, column: &str, cx: &mut Context<Self>) {
        match target {
            ColumnPickTarget::Filter(index) => self.pick_filter_column(index, column, cx),
            ColumnPickTarget::Jump | ColumnPickTarget::Manage => {
                self.jump_to_column(column, cx);
            }
        }
    }

    /// 跳到列：选中整列（与点列头相同的矩形），并把它横向滚进视野。冻结列本来就在视野里，不滚动。
    pub fn jump_to_column(&mut self, column: &str, cx: &mut Context<Self>) -> bool {
        let Some(logical) = self.session.column_names().iter().position(|name| name == column) else {
            return false;
        };
        let Some(display) = self.session.logical_to_display(logical) else {
            return false;
        };
        let last = self.session.loaded_len().saturating_sub(1);
        if self.session.selection_bounds() != Some((0, logical, last, logical)) {
            self.record_row_changed();
            self.session.select(0, logical, false);
            self.session.select(last, logical, true);
        }
        self.clear_notice();
        self.sync_table(cx);
        if display >= self.session.frozen_count() {
            // 表格列号含最左边的行号列。
            self.table.update(cx, |state, cx| state.scroll_to_col(display + 1, cx));
        }
        self.refocus = true;
        cx.notify();
        true
    }

    /// 绘制阶段：落地等着打开的选择器，并把弹层（连同点击即关闭的遮罩）挂在触发按钮旁。
    pub(crate) fn render_column_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some(target) = self.picker_request.take() {
            // 新条件这一帧才第一次画出来：旧位置作废，用回退锚点先弹出，下一帧贴回按钮。
            self.picker_anchors.borrow_mut().remove(&target);
            self.open_column_picker(target, window, cx);
        }
        let open = self.column_picker.as_ref()?;
        let target = open.target;
        let picker = open.picker.clone();
        let anchors = self.picker_anchors.borrow();
        let anchor = match anchors.get(&target) {
            Some(area) => *area,
            None => {
                window.request_animation_frame();
                anchors
                    .get(&ColumnPickTarget::Jump)
                    .copied()
                    .unwrap_or_default()
            }
        };
        drop(anchors);
        let viewport = window.viewport_size();
        let entity = cx.entity();
        // 遮罩铺满窗口、画在弹层下面：点弹层以外的任何地方都只是关闭它，不会顺手点到网格。
        let backdrop = deferred(
            anchored().position(Point::default()).child(
                div()
                    .id("grid-column-picker-backdrop")
                    .w(viewport.width)
                    .h(viewport.height)
                    .occlude()
                    .on_mouse_down(gpui_kit::MouseButton::Left, move |_, window, cx: &mut App| {
                        entity.update(cx, |pane, cx| pane.close_column_picker(window, cx));
                    }),
            ),
        )
        .with_priority(2);
        let popover = picker_popover(&picker, anchor, window, cx);
        Some(div().child(backdrop).child(popover).into_any_element())
    }

    pub(crate) fn toggle_column_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.column_manager.is_some() {
            self.close_column_manager(window, cx);
        } else {
            self.close_column_picker(window, cx);
            self.open_column_manager(window, cx);
        }
    }

    pub(crate) fn open_column_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = SharedString::from(i18n::text(self.locale, "grid.column.search"));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscription = cx.subscribe_in(&search, window, |this, state, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                if let Some(manager) = this.column_manager.as_mut() {
                    manager.query = state.read(cx).value().to_string();
                    cx.notify();
                }
            }
        });
        search.update(cx, |input, cx| input.focus(window, cx));
        self.column_manager = Some(ColumnManager { search, query: String::new(), _subscription: subscription });
        cx.notify();
    }

    pub(crate) fn close_column_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.column_manager.take().is_some() {
            window.focus(&self.grid_focus, cx);
            cx.notify();
        }
    }

    pub(crate) fn render_column_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (search, query) = {
            let manager = self.column_manager.as_ref()?;
            (manager.search.clone(), manager.query.clone())
        };
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let p = Palette::of(cx);
        let entity = cx.entity();
        let viewport = window.viewport_size();
        let anchors = self.picker_anchors.borrow();
        let anchor = anchors.get(&ColumnPickTarget::Manage).copied().unwrap_or_default();
        drop(anchors);
        let pos = Point::new(anchor.origin.x, anchor.bottom() + px(4.));

        let ordered = self.session.ordered_columns();
        let names = self.session.column_names();
        let needle = query.trim().to_lowercase();
        let hidden_count = self.session.hidden_columns().len();
        let reordered = self.session.has_custom_column_order();

        let mut list = div()
            .id("grid-column-manager-list")
            .debug_selector(|| "grid-column-manager-list".to_owned())
            .test_support()
            .max_h(px(260.))
            .scroll_area_y()
            .flex()
            .flex_col()
            .gap(px(2.));
        let mut matched = 0usize;
        let total = ordered.len();
        for (pos_ix, logical) in ordered.into_iter().enumerate() {
            let Some(name) = names.get(logical) else { continue };
            let type_name = self.session.column_type(logical).unwrap_or("");
            if !needle.is_empty() && !name.to_lowercase().contains(&needle) && !type_name.to_lowercase().contains(&needle) {
                continue;
            }
            matched += 1;
            let hidden = self.session.column_hidden(logical);
            let can_hide = self.session.can_hide(logical);
            let toggle_disabled = !hidden && !can_hide;
            let toggle_owner = entity.clone();
            let toggle_cmd = if hidden { "column-show" } else { "column-hide" };
            let up_owner = entity.clone();
            let down_owner = entity.clone();
            let can_up = pos_ix > 0;
            let can_down = pos_ix + 1 < total;
            let toggle_tip = if toggle_disabled {
                t("grid.column.lastVisible")
            } else if hidden {
                SharedString::from(i18n::fill(&i18n::text(locale, "grid.column.show"), &[("name", name)]))
            } else {
                t("grid.column.hide")
            };
            list = list.child(
                div()
                    .id(("grid-col-mgr-row", logical))
                    .debug_selector(move || format!("grid-col-mgr-row-{logical}"))
                    .test_support()
                    .h(px(26.))
                    .px(px(6.))
                    .rounded(px(Radius::CONTROL))
                    .hover(|style| style.bg(p.hover))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .id(("grid-col-mgr-toggle", logical))
                            .debug_selector(move || format!("grid-col-mgr-toggle-{logical}"))
                            .test_support()
                            .flex_none()
                            .size(px(16.))
                            .rounded(px(Radius::TAG))
                            .border_1()
                            .border_color(if hidden { p.line } else { p.accent })
                            .bg(if hidden { p.surface } else { p.accent })
                            .flex()
                            .items_center()
                            .justify_center()
                            .tooltip_text(toggle_tip)
                            .when(toggle_disabled, |this| this.opacity(0.45))
                            .when(!toggle_disabled, |this| {
                                this.cursor_pointer().on_click(move |_, _, cx| {
                                    toggle_owner.update(cx, |pane, cx| pane.column_command(logical, toggle_cmd, cx));
                                })
                            })
                            .when(!hidden, |this| this.child(kit::icon(IconName::Check, 11., p.on_accent))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(TypeScale::SM))
                            .text_color(if hidden { p.text_faint } else { p.text })
                            .child(SharedString::from(name.clone())),
                    )
                    .when(!type_name.is_empty(), |this| {
                        this.child(
                            div()
                                .flex_none()
                                .max_w(px(72.))
                                .truncate()
                                .text_size(px(TypeScale::XS))
                                .text_color(p.text_faint)
                                .child(SharedString::from(type_name.to_owned())),
                        )
                    })
                    .child(
                        kit::icon_button(("grid-col-mgr-up", logical), IconName::ChevronUp, t("grid.column.moveLeft"), move |_, _, cx| {
                            up_owner.update(cx, |pane, cx| pane.column_command(logical, "column-move-up", cx));
                        })
                        .xsmall()
                        .disabled(!can_up),
                    )
                    .child(
                        kit::icon_button(("grid-col-mgr-down", logical), IconName::ChevronDown, t("grid.column.moveRight"), move |_, _, cx| {
                            down_owner.update(cx, |pane, cx| pane.column_command(logical, "column-move-down", cx));
                        })
                        .xsmall()
                        .disabled(!can_down),
                    ),
            );
        }
        if matched == 0 {
            list = list.child(div().p(px(10.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child(t("grid.column.emptySearch")));
        }

        let show_all_owner = entity.clone();
        let reset_owner = entity.clone();
        let key_owner = entity.clone();
        let backdrop_owner = entity.clone();

        let backdrop = deferred(
            anchored().position(Point::default()).child(
                div()
                    .id("grid-column-manager-backdrop")
                    .test_support()
                    .w(viewport.width)
                    .h(viewport.height)
                    .occlude()
                    .on_mouse_down(gpui_kit::MouseButton::Left, move |_, window, cx: &mut App| {
                        backdrop_owner.update(cx, |pane, cx| pane.close_column_manager(window, cx));
                    }),
            ),
        )
        .with_priority(2);

        let popover = deferred(
            anchored()
                .position(pos)
                .anchor(Anchor::TopLeft)
                .snap_to_window_with_margin(px(ControlSize::POPOVER_MARGIN))
                .child(
                    div()
                        .id("grid-column-manager")
                        .debug_selector(|| "grid-column-manager".to_owned())
                        .test_support()
                        .occlude()
                        .w(px(292.))
                        .p(px(Space::SM))
                        .flex()
                        .flex_col()
                        .gap(px(Space::SM))
                        .bg(p.raised)
                        .border_1()
                        .border_color(p.line)
                        .rounded(px(Radius::PANEL))
                        .shadow_lg()
                        .on_key_down(move |event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key.as_str() == "escape" {
                                cx.stop_propagation();
                                key_owner.update(cx, |pane, cx| pane.close_column_manager(window, cx));
                            }
                        })
                        .child(
                            kit::text_input("grid-column-manager-search", &search, t("grid.column.search"))
                                .small()
                                .prefix(kit::input_prefix(IconName::Search, 13., p.text_faint))
                                .cleanable(true),
                        )
                        .child(list)
                        .child(
                            div()
                                .pt(px(4.))
                                .border_t_1()
                                .border_color(p.line_soft)
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(4.))
                                .child(
                                    kit::tool_button("grid-col-mgr-show-all", None, t("grid.column.showAll"), move |_, _, cx| {
                                        show_all_owner.update(cx, |pane, cx| pane.column_command(0, "column-show-all", cx));
                                    })
                                    .xsmall()
                                    .disabled(hidden_count == 0),
                                )
                                .child(
                                    kit::tool_button("grid-col-mgr-reset-order", None, t("grid.column.resetOrder"), move |_, _, cx| {
                                        reset_owner.update(cx, |pane, cx| pane.column_command(0, "column-reset-order", cx));
                                    })
                                    .xsmall()
                                    .disabled(!reordered),
                                ),
                        ),
                ),
        )
        .with_priority(3);

        Some(div().child(backdrop).child(popover).into_any_element())
    }
}
