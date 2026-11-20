//! 字段选择器（R4）：筛选条件的字段与工具栏“跳到列”共用一个 `ui::picker` 弹层。
//! 几百列的表上可以按名字或类型搜索；弹层按窗口剩余空间限高，内部滚动，不超出窗口。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::{
    anchored, deferred, div, AnyElement, App, AppContext as _, Bounds, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, Point, SharedString, Styled as _, Subscription, Window,
};

use super::GridPane;
use crate::ui::picker::{picker_popover, Picker, PickerEvent, PickerItem, PickerMode};

/// 选择器为谁打开。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ColumnPickTarget {
    /// 第 n 条筛选条件的字段。
    Filter(usize),
    /// 工具栏“跳到列”。
    Jump,
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
            ColumnPickTarget::Jump => {
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
            ColumnPickTarget::Jump => {
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
        self.notice.clear();
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
}
