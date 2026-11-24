//! 可搜索选择器：顶部搜索框 + 虚拟列表，单选或多选。
//!
//! 输入时按子串实时过滤（大小写不敏感，匹配标题与说明），方向键移动高亮，
//! 回车选中（多选时切换勾选），Esc 关闭。弹层按窗口剩余空间限高，内部滚动，
//! 最后一项也能滚到；没有匹配项时显示空态。
//! 过滤与键盘逻辑在 [`PickerModel`] 里，是纯数据，单元测试直接驱动它。
#![allow(dead_code)] // 共享底座：本任务只建组件，各页面在后续任务里接入。

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    anchored, deferred, div, px, Anchor, AnyElement, App, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels, Point, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, UniformListScrollHandle, Window,
};

use super::kit::{self, ControlSize, IconSize, Metrics, Palette, Radius, Space, TypeScale};
use crate::i18n::{self, Locale};

// ---------------------------------------------------------------- 模型

#[derive(Clone, Debug)]
pub struct PickerItem {
    /// 稳定标识（列名、标签 id）：回传给页面。
    pub id: SharedString,
    pub label: SharedString,
    /// 右侧弱化说明（类型、连接、库）；也参与搜索。
    pub detail: Option<SharedString>,
    /// 标题前的类型图标。
    pub icon: Option<IconName>,
    /// 环境徽标：（环境代码，显示文字）；生产为红色。
    pub env: Option<(SharedString, SharedString)>,
    /// 脏 / 待提交标记的说明；有值时标题旁画一个圆点。
    pub dirty: Option<SharedString>,
    /// 行尾显示关闭按钮（点击发出 PickerEvent::Close）。
    pub closable: bool,
    /// 当前项（例如当前标签）：常亮强调，不依赖键盘高亮。
    pub current: bool,
}

impl PartialEq for PickerItem {
    fn eq(&self, other: &Self) -> bool {
        // 图标只影响外观，比较时不看。
        self.id == other.id
            && self.label == other.label
            && self.detail == other.detail
            && self.env == other.env
            && self.dirty == other.dirty
            && self.closable == other.closable
            && self.current == other.current
    }
}

impl Eq for PickerItem {}

impl PickerItem {
    pub fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self { id: id.into(), label: label.into(), detail: None, icon: None, env: None, dirty: None, closable: false, current: false }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn env(mut self, code: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        self.env = Some((code.into(), label.into()));
        self
    }

    pub fn dirty(mut self, tip: impl Into<SharedString>) -> Self {
        self.dirty = Some(tip.into());
        self
    }

    pub fn closable(mut self, closable: bool) -> Self {
        self.closable = closable;
        self
    }

    pub fn current(mut self, current: bool) -> Self {
        self.current = current;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerMode {
    Single,
    Multi,
}

/// 一次按键或点击的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickerOutcome {
    /// 单选：选中了这一项，弹层应关闭。
    Chosen(SharedString),
    /// 多选：勾选集合变了（按勾选先后排列），弹层保持打开。
    Toggled(Vec<SharedString>),
    /// Esc：关闭，不改选择。
    Dismissed,
}

#[derive(Clone, Debug)]
pub struct PickerModel {
    items: Vec<PickerItem>,
    mode: PickerMode,
    query: String,
    matches: Vec<usize>,
    highlight: usize,
    chosen: Vec<SharedString>,
}

impl PickerModel {
    pub fn new(items: Vec<PickerItem>, mode: PickerMode, chosen: Vec<SharedString>) -> Self {
        let mut model = Self { items, mode, query: String::new(), matches: Vec::new(), highlight: 0, chosen };
        model.refilter();
        // 打开时高亮停在当前选中项上，键盘可以从这里继续。
        if let Some(first) = model.chosen.first().cloned() {
            if let Some(at) = model.matches.iter().position(|ix| model.items[*ix].id == first) {
                model.highlight = at;
            }
        }
        model
    }

    pub fn mode(&self) -> PickerMode {
        self.mode
    }

    pub fn items(&self) -> &[PickerItem] {
        &self.items
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_items(&mut self, items: Vec<PickerItem>) {
        let keep = self.highlighted().map(|item| item.id.clone());
        self.items = items;
        self.chosen.retain(|id| self.items.iter().any(|item| &item.id == id));
        self.refilter();
        if let Some(id) = keep {
            if let Some(at) = self.matches.iter().position(|ix| self.items[*ix].id == id) {
                self.highlight = at;
            }
        }
    }

    /// 改搜索词：重新过滤，高亮回到第一项。
    pub fn set_query(&mut self, query: &str) -> bool {
        if self.query == query {
            return false;
        }
        self.query = query.to_owned();
        self.refilter();
        self.highlight = 0;
        true
    }

    fn refilter(&mut self) {
        let needle = self.query.trim().to_lowercase();
        self.matches = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                needle.is_empty()
                    || item.label.to_lowercase().contains(&needle)
                    || item.detail.as_ref().is_some_and(|detail| detail.to_lowercase().contains(&needle))
            })
            .map(|(ix, _)| ix)
            .collect();
        self.highlight = self.highlight.min(self.matches.len().saturating_sub(1));
    }

    /// 当前匹配项（原始下标）。
    pub fn matches(&self) -> &[usize] {
        &self.matches
    }

    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    /// 高亮在匹配列表里的位置。
    pub fn highlight(&self) -> usize {
        self.highlight
    }

    pub fn highlighted(&self) -> Option<&PickerItem> {
        self.matches.get(self.highlight).map(|ix| &self.items[*ix])
    }

    pub fn chosen(&self) -> &[SharedString] {
        &self.chosen
    }

    pub fn is_chosen(&self, id: &str) -> bool {
        self.chosen.iter().any(|chosen| chosen.as_ref() == id)
    }

    /// 移动高亮，停在两端（不回绕：长列表里回绕容易迷路）。
    pub fn move_by(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() - 1;
        let next = (self.highlight as isize + delta).clamp(0, last as isize);
        self.highlight = next as usize;
    }

    pub fn move_to(&mut self, position: usize) {
        if !self.matches.is_empty() {
            self.highlight = position.min(self.matches.len() - 1);
        }
    }

    /// 回车或点击某一项。
    pub fn choose_at(&mut self, position: usize) -> Option<PickerOutcome> {
        let id = self.matches.get(position).map(|ix| self.items[*ix].id.clone())?;
        self.highlight = position;
        Some(match self.mode {
            PickerMode::Single => {
                self.chosen = vec![id.clone()];
                PickerOutcome::Chosen(id)
            }
            PickerMode::Multi => {
                if let Some(at) = self.chosen.iter().position(|chosen| *chosen == id) {
                    self.chosen.remove(at);
                } else {
                    self.chosen.push(id);
                }
                PickerOutcome::Toggled(self.chosen.clone())
            }
        })
    }

    /// 键盘：返回 None 表示这个键不归选择器管（交给输入框）。
    /// 返回 Some(None) 表示处理了但没有结果（移动高亮、空列表上回车）。
    pub fn key(&mut self, key: &str, page: usize) -> Option<Option<PickerOutcome>> {
        match key {
            "up" => self.move_by(-1),
            "down" => self.move_by(1),
            "pageup" => self.move_by(-(page.max(1) as isize)),
            "pagedown" => self.move_by(page.max(1) as isize),
            "enter" => return Some(self.choose_at(self.highlight)),
            "escape" => return Some(Some(PickerOutcome::Dismissed)),
            _ => return None,
        }
        Some(None)
    }
}

// ---------------------------------------------------------------- 弹层几何

/// 弹层高度与朝向：优先放在锚点下方；下方放不下而上方更宽时翻到上方。
/// 高度不超过所在一侧的剩余空间（扣掉边距），内容更多时内部滚动。
pub fn popover_height(desired: f32, anchor_top: f32, anchor_bottom: f32, window_h: f32, margin: f32) -> (f32, bool) {
    let below = (window_h - anchor_bottom - margin).max(0.);
    let above = (anchor_top - margin).max(0.);
    if desired <= below || below >= above {
        (desired.min(below), false)
    } else {
        (desired.min(above), true)
    }
}

/// 列表区高度：行数 × 行高，不超过总高扣掉搜索框与边距后的空间。
pub fn list_height(rows: usize, row_h: f32, total_max: f32, chrome_h: f32) -> f32 {
    (rows as f32 * row_h).min((total_max - chrome_h).max(row_h))
}

// ---------------------------------------------------------------- 视图

/// 发给页面的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickerEvent {
    Chosen(SharedString),
    Toggled(Vec<SharedString>),
    Dismissed,
}

/// 行尾关闭按钮的处理：页面关闭这一项（例如关闭标签），选择器保持打开。
pub type CloseHandler = std::rc::Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// 默认列表最多显示的行数（再多就滚动）。
const VISIBLE_ROWS: usize = 12;

pub struct Picker {
    model: PickerModel,
    input: Entity<InputState>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    locale: Locale,
    /// 弹层可用的最大总高度；由 [`picker_popover`] 按窗口剩余空间设定。
    max_h: Option<f32>,
    /// 上一次滚到的高亮位置：高亮变了才滚动，鼠标滚动不被拉回。
    scrolled_to: Option<usize>,
    close_handler: Option<CloseHandler>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PickerEvent> for Picker {}

impl Picker {
    pub fn new(items: Vec<PickerItem>, mode: PickerMode, chosen: Vec<SharedString>, locale: Locale, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(SharedString::from(i18n::text(locale, "picker.search"))));
        let subscription = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                let query = input.read(cx).value().to_string();
                this.set_query(&query, cx);
            }
        });
        Self {
            model: PickerModel::new(items, mode, chosen),
            input,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            locale,
            max_h: None,
            scrolled_to: None,
            close_handler: None,
            _subscriptions: vec![subscription],
        }
    }

    pub fn model(&self) -> &PickerModel {
        &self.model
    }

    pub fn set_items(&mut self, items: Vec<PickerItem>, cx: &mut Context<Self>) {
        self.model.set_items(items);
        self.scrolled_to = None;
        cx.notify();
    }

    pub fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        if self.model.set_query(query) {
            self.scrolled_to = None;
            cx.notify();
        }
    }

    /// 带关闭按钮（PickerItem::closable）的行点击关闭时调用。
    pub fn set_close_handler(&mut self, handler: CloseHandler) {
        self.close_handler = Some(handler);
    }

    pub fn set_max_height(&mut self, max_h: Option<f32>) {
        self.max_h = max_h;
    }

    /// 打开后把键盘焦点放进搜索框。
    pub fn focus_search(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn page(&self, cx: &App) -> usize {
        let row_h = Metrics::of(cx).menu_row_h;
        let list_h = self.max_h.map(|h| list_height(VISIBLE_ROWS, row_h, h, Metrics::of(cx).input_h() + Space::LG)).unwrap_or(VISIBLE_ROWS as f32 * row_h);
        ((list_h / row_h).floor() as usize).max(1)
    }

    /// 处理一个键；返回是否由选择器消费。
    pub fn press(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let page = self.page(cx);
        let Some(outcome) = self.model.key(key, page) else { return false };
        if let Some(outcome) = outcome {
            self.emit(outcome, cx);
        }
        cx.notify();
        true
    }

    pub fn choose_at(&mut self, position: usize, cx: &mut Context<Self>) {
        if let Some(outcome) = self.model.choose_at(position) {
            self.emit(outcome, cx);
        }
        cx.notify();
    }

    fn emit(&mut self, outcome: PickerOutcome, cx: &mut Context<Self>) {
        cx.emit(match outcome {
            PickerOutcome::Chosen(id) => PickerEvent::Chosen(id),
            PickerOutcome::Toggled(ids) => PickerEvent::Toggled(ids),
            PickerOutcome::Dismissed => PickerEvent::Dismissed,
        });
    }

    fn row(&self, position: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = Metrics::of(cx);
        let item = &self.model.items[self.model.matches[position]];
        let highlighted = position == self.model.highlight;
        let checked = self.model.is_chosen(&item.id);
        let multi = self.model.mode == PickerMode::Multi;
        let current = item.current;
        let close_id = item.id.clone();
        let close_handler = self.close_handler.clone();
        let close_label = SharedString::from(i18n::text(self.locale, "common.close"));
        div()
            .id(("picker-row", position))
            .test_support()
            .role(gpui_kit::Role::ListItem)
            .aria_label(item.label.clone())
            .aria_selected(checked)
            .h(px(m.menu_row_h))
            .px(px(Space::MD))
            .flex()
            .items_center()
            .gap(px(Space::MD))
            .rounded(px(Radius::CONTROL))
            .cursor_pointer()
            .text_size(px(TypeScale::SM))
            .text_color(p.text)
            .when(highlighted, |this| this.bg(p.selection))
            .when(!highlighted && current, |this| this.bg(p.accent_soft))
            .when(!highlighted && !current, |this| this.hover(|s| s.bg(p.hover)))
            // 当前项：左侧强调条常亮。
            .when(current, |this| this.border_l_2().border_color(p.accent))
            .when(!current, |this| this.border_l_2().border_color(gpui_kit::transparent_black()))
            .when_some(item.icon, |this, icon| {
                this.child(div().flex_none().child(kit::icon(icon, IconSize::INLINE, if current { p.accent_text } else { p.text_muted })))
            })
            .when(multi, |this| {
                this.child(
                    div()
                        .flex_none()
                        .size(px(IconSize::CONTROL))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(checked, |this| this.child(kit::icon(IconName::Check, IconSize::INLINE, p.accent_text))),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .when((checked && !multi) || current, |this| this.font_weight(FontWeight::MEDIUM).text_color(p.accent_text))
                    .child(item.label.clone()),
            )
            .when_some(item.dirty.clone(), |this, tip| {
                this.child(
                    div()
                        .id(("picker-row-dirty", position))
                        .test_support()
                        .flex_none()
                        .aria_label(tip)
                        .child(kit::status_dot(p.warn)),
                )
            })
            .when_some(item.env.clone(), |this, (code, label)| this.child(div().flex_none().child(kit::env_tag(&code, label, &p))))
            .when_some(item.detail.clone(), |this, detail| {
                this.child(div().flex_none().max_w(px(ControlSize::PICKER_MIN_W / 2.)).truncate().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(detail))
            })
            .when(item.closable && self.close_handler.is_some(), |this| {
                this.child(
                    div()
                        .id(("picker-row-close", position))
                        .test_support()
                        .role(gpui_kit::Role::Button)
                        .aria_label(close_label)
                        .flex_none()
                        .size(px(IconSize::CONTROL))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(Radius::CONTROL))
                        .hover(|s| s.bg(p.hover))
                        .cursor_pointer()
                        .child(kit::icon(IconName::X, IconSize::INLINE, p.text_muted))
                        .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            if let Some(handler) = close_handler.as_ref() {
                                handler(&close_id, window, cx);
                            }
                        }),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| this.choose_at(position, cx)))
            .into_any_element()
    }
}

impl Focusable for Picker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Picker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let m = Metrics::of(cx);
        let row_h = m.menu_row_h;
        let chrome_h = m.input_h() + Space::LG + Space::MD;
        let total_max = self.max_h.unwrap_or(VISIBLE_ROWS as f32 * row_h + chrome_h);
        let count = self.model.matches.len();
        if self.scrolled_to != Some(self.model.highlight) && count > 0 {
            self.scroll.scroll_to_item(self.model.highlight, ScrollStrategy::Nearest);
            self.scrolled_to = Some(self.model.highlight);
        }
        let body = if count == 0 {
            let (heading, detail) = if self.model.items.is_empty() {
                ("picker.noItems", None)
            } else {
                ("picker.empty", Some("picker.emptyHint"))
            };
            div()
                .id("picker-empty")
                .test_support()
                .py(px(Space::LG))
                .px(px(Space::MD))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(Space::XS))
                .text_size(px(TypeScale::SM))
                .text_color(p.text_muted)
                .child(i18n::text(self.locale, heading))
                .when_some(detail, |this, key| this.child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(i18n::text(self.locale, key))))
                .into_any_element()
        } else {
            let list_h = list_height(count, row_h, total_max, chrome_h);
            let list = gpui_kit::uniform_list("picker-list", count, cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                range.map(|position| this.row(position, cx)).collect::<Vec<_>>()
            }))
            .track_scroll(&self.scroll)
            .h(px(list_h));
            div()
                .relative()
                .h(px(list_h))
                .child(list)
                .child(kit::scrollbar_overlay("picker-list-bar", &self.scroll, kit::ScrollbarAxis::Vertical))
                .into_any_element()
        };
        div()
            .id("picker")
            .test_support()
            .track_focus(&self.focus)
            .role(gpui_kit::Role::List)
            .min_w(px(ControlSize::PICKER_MIN_W))
            .max_w(px(ControlSize::PICKER_MAX_W))
            .max_h(px(total_max))
            .flex()
            .flex_col()
            .gap(px(Space::XS))
            .p(px(Space::XS))
            .bg(p.raised)
            .border_1()
            .border_color(p.line)
            .rounded(px(Radius::PANEL))
            .shadow_lg()
            .overflow_hidden()
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let key = event.keystroke.key.as_str();
                if event.keystroke.modifiers.modified() && key != "enter" {
                    return;
                }
                if this.press(key, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(kit::text_input("picker-search", &self.input, i18n::text(self.locale, "picker.search")).prefix(kit::input_prefix(IconName::Search, IconSize::INLINE, p.text_faint)))
            .child(body)
    }
}

/// 把选择器挂在锚点（触发按钮的窗口坐标）下方；放不下时翻到上方，高度按窗口剩余空间限制。
/// 页面在自己的 render 里调用，并在 `PickerEvent::Dismissed` 或点击遮罩时不再渲染它。
pub fn picker_popover(picker: &Entity<Picker>, anchor: Bounds<Pixels>, window: &Window, cx: &mut App) -> AnyElement {
    let m = Metrics::of(cx);
    let window_h = f32::from(window.viewport_size().height);
    let desired = VISIBLE_ROWS as f32 * m.menu_row_h + m.input_h() + Space::LG + Space::MD;
    let margin = ControlSize::POPOVER_MARGIN;
    let (height, above) = popover_height(desired, f32::from(anchor.top()), f32::from(anchor.bottom()), window_h, margin);
    picker.update(cx, |picker, _| picker.set_max_height(Some(height)));
    let (position, corner): (Point<Pixels>, Anchor) = if above { (anchor.origin, Anchor::BottomLeft) } else { (anchor.bottom_left(), Anchor::TopLeft) };
    deferred(anchored().position(position).anchor(corner).snap_to_window_with_margin(px(margin)).child(picker.clone())).with_priority(3).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{size, TestAppContext, WindowBounds, WindowOptions};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn columns(n: usize) -> Vec<PickerItem> {
        (0..n).map(|ix| PickerItem::new(format!("col_{ix}"), format!("col_{ix}")).detail(if ix % 2 == 0 { "int" } else { "VARCHAR(64)" })).collect()
    }

    fn ids(model: &PickerModel) -> Vec<&str> {
        model.matches().iter().map(|ix| model.items()[*ix].id.as_ref()).collect()
    }

    #[test]
    fn typing_a_substring_filters_case_insensitively_over_label_and_detail() {
        let mut model = PickerModel::new(columns(300), PickerMode::Single, Vec::new());
        assert_eq!(model.matches().len(), 300);
        assert!(model.set_query("COL_29"));
        assert_eq!(ids(&model), ["col_29", "col_290", "col_291", "col_292", "col_293", "col_294", "col_295", "col_296", "col_297", "col_298", "col_299"]);
        assert!(!model.set_query("COL_29"), "同一搜索词不重算");
        model.set_query("varchar");
        assert_eq!(model.matches().len(), 150, "说明也参与搜索");
        model.set_query("  col_7  ");
        assert_eq!(ids(&model)[0], "col_7", "首尾空白不影响");
    }

    #[test]
    fn keyboard_moves_the_highlight_and_enter_chooses() {
        let mut model = PickerModel::new(columns(300), PickerMode::Single, Vec::new());
        assert_eq!(model.key("up", 10), Some(None));
        assert_eq!(model.highlight(), 0, "停在顶端不回绕");
        model.key("down", 10);
        model.key("down", 10);
        assert_eq!(model.highlighted().map(|item| item.id.as_ref()), Some("col_2"));
        model.key("pagedown", 10);
        assert_eq!(model.highlight(), 12);
        for _ in 0..40 {
            model.key("pagedown", 10);
        }
        assert_eq!(model.highlight(), 299, "停在最后一项");
        assert_eq!(model.key("enter", 10), Some(Some(PickerOutcome::Chosen("col_299".into()))));
        assert_eq!(model.chosen(), ["col_299"]);
        assert_eq!(model.key("escape", 10), Some(Some(PickerOutcome::Dismissed)));
        assert_eq!(model.key("a", 10), None, "其他键交给输入框");
        // 搜索后高亮回到第一项；再打开时停在已选项上。
        model.set_query("col_1");
        assert_eq!(model.highlight(), 0);
        let reopened = PickerModel::new(columns(300), PickerMode::Single, vec!["col_42".into()]);
        assert_eq!(reopened.highlighted().map(|item| item.id.as_ref()), Some("col_42"));
    }

    #[test]
    fn multi_mode_toggles_in_the_order_chosen() {
        let mut model = PickerModel::new(columns(10), PickerMode::Multi, Vec::new());
        model.move_to(3);
        assert_eq!(model.key("enter", 5), Some(Some(PickerOutcome::Toggled(vec!["col_3".into()]))));
        assert_eq!(model.choose_at(1), Some(PickerOutcome::Toggled(vec!["col_3".into(), "col_1".into()])));
        assert_eq!(model.choose_at(3), Some(PickerOutcome::Toggled(vec!["col_1".into()])), "再次回车取消勾选");
        assert!(model.is_chosen("col_1") && !model.is_chosen("col_3"));
        // 换数据时去掉已经不存在的勾选。
        model.set_items(columns(1));
        assert!(model.chosen().is_empty());
    }

    #[test]
    fn empty_state_when_nothing_matches_or_there_are_no_items() {
        let mut model = PickerModel::new(columns(5), PickerMode::Single, Vec::new());
        model.set_query("zzz");
        assert!(model.is_empty());
        assert_eq!(model.highlighted(), None);
        assert_eq!(model.key("enter", 5), Some(None), "空列表上回车什么也不选");
        model.key("down", 5);
        assert_eq!(model.highlight(), 0);
        let none = PickerModel::new(Vec::new(), PickerMode::Multi, Vec::new());
        assert!(none.is_empty() && none.items().is_empty());
    }

    #[test]
    fn popover_stays_inside_the_window() {
        // 下方空间够：放下方，用期望高度。
        assert_eq!(popover_height(300., 100., 120., 800., 8.), (300., false));
        // 下方不够、上方更宽：翻到上方，高度不超过上方空间。
        assert_eq!(popover_height(500., 600., 620., 800., 8.), (500., true));
        assert_eq!(popover_height(700., 600., 620., 800., 8.), (592., true));
        // 下方更宽但不够：放下方并收矮，内部滚动。
        assert_eq!(popover_height(700., 200., 220., 800., 8.), (572., false));
        // 列表区扣掉搜索框后不超过总高，至少一行。
        assert_eq!(list_height(300, 24., 300., 44.), 256.);
        assert_eq!(list_height(3, 24., 300., 44.), 72.);
        assert_eq!(list_height(300, 24., 30., 44.), 24.);
    }

    fn open(cx: &mut TestAppContext, height: f32) -> (gpui_kit::AnyWindowHandle, Entity<Picker>, Rc<RefCell<Vec<PickerEvent>>>) {
        cx.update(gpui_kit::init);
        let (window, picker) = cx.update(|cx| {
            let bounds = Bounds { origin: Point::default(), size: size(px(480.), px(height)) };
            gpui_kit::open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, cx, |window, cx| {
                cx.new(|cx| Picker::new(columns(300), PickerMode::Single, Vec::new(), Locale::ZhCn, window, cx))
            })
            .expect("picker window")
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        cx.update(|cx| cx.subscribe(&picker, move |_, event: &PickerEvent, _| sink.borrow_mut().push(event.clone())).detach());
        (window, picker, events)
    }

    fn step<R>(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        let result = cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            f(window, cx)
        });
        cx.run_until_parked();
        result.unwrap()
    }

    #[gpui_kit::test]
    fn typing_arrows_and_enter_choose_from_three_hundred_columns(cx: &mut TestAppContext) {
        let (window, picker, events) = open(cx, 420.);
        step(cx, window, |window, cx| picker.update(cx, |picker, cx| picker.focus_search(window, cx)));
        step(cx, window, |window, cx| window.input("col_29", cx));
        assert_eq!(cx.update(|cx| picker.read(cx).model().matches().len()), 11, "输入即过滤");
        step(cx, window, |window, cx| window.press("down", cx));
        step(cx, window, |window, cx| window.press("enter", cx));
        assert_eq!(events.borrow().as_slice(), [PickerEvent::Chosen("col_290".into())]);
        step(cx, window, |window, cx| window.input("zz", cx));
        step(cx, window, |window, _| assert!(window.try_find("picker-empty").is_some(), "没有匹配项时显示空态"));
        step(cx, window, |window, cx| window.press("escape", cx));
        assert_eq!(events.borrow().last(), Some(&PickerEvent::Dismissed));
    }

    #[gpui_kit::test]
    fn last_item_scrolls_into_view_inside_a_short_window(cx: &mut TestAppContext) {
        let (window, picker, events) = open(cx, 260.);
        // 限高按窗口剩余空间：260 高的窗口里弹层不超过窗口。
        step(cx, window, |_, cx| picker.update(cx, |picker, _| picker.set_max_height(Some(popover_height(400., 10., 30., 260., 8.).0))));
        step(cx, window, |window, cx| {
            assert!(window.try_find(("picker-row", 299usize)).is_none(), "虚拟列表只渲染可见行");
            picker.update(cx, |picker, cx| picker.focus_search(window, cx));
        });
        for _ in 0..60 {
            step(cx, window, |window, cx| window.press("pagedown", cx));
        }
        step(cx, window, |window, cx| {
            assert_eq!(picker.read(cx).model().highlight(), 299);
            assert!(window.try_find(("picker-row", 299usize)).is_some(), "最后一项滚到了可见区");
        });
        step(cx, window, |window, cx| window.click(("picker-row", 298usize), cx));
        assert_eq!(events.borrow().as_slice(), [PickerEvent::Chosen("col_298".into())]);
    }
}
