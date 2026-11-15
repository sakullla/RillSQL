//! 查询目标选择器：一个按钮显示“实例 › 库”，点开是双栏弹层——左边全部连接（含未打开的），
//! 右边所选连接的库。顶部一个搜索框同时过滤连接与库；点库即绑定连接与库，未打开的连接先连接再列库。

use gpui_kit::assets::IconName;
use gpui_kit::component::input::InputState;
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    anchored, deferred, div, px, Anchor, AnyElement, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Point, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
};

use crate::i18n::{self, Locale};
use crate::ui::kit::{self, Metrics, Palette, Radius, TypeScale};

use super::QueryPane;

/// 选择器里的一条连接。来自对象树的连接快照：已保存的连接都在，未打开的也在。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetChoice {
    pub id: String,
    pub name: String,
    pub engine: String,
    pub connected: bool,
    /// 已打开时这条连接下的库（默认库排在最前）；未打开时为空。
    pub databases: Vec<String>,
}

/// 选择器弹层的界面状态。
pub struct TargetPicker {
    /// 弹层的锚点（打开它的那次点击位置）；None 表示关闭。
    pub anchor: Option<Point<Pixels>>,
    /// 左栏当前选中的连接（右栏列它的库）。
    pub focus: Option<String>,
    /// 右栏当前键盘高亮的库下标。
    pub db_index: usize,
    pub search: Option<Entity<InputState>>,
    /// 等着连接成功后再绑定的连接。
    pub pending: Option<String>,
    pub conn_scroll: gpui_kit::ScrollHandle,
    pub db_scroll: gpui_kit::ScrollHandle,
}

impl Default for TargetPicker {
    fn default() -> Self {
        Self {
            anchor: None,
            focus: None,
            db_index: 0,
            search: None,
            pending: None,
            conn_scroll: gpui_kit::ScrollHandle::new(),
            db_scroll: gpui_kit::ScrollHandle::new(),
        }
    }
}

pub fn matches(text: &str, filter: &str) -> bool {
    filter.is_empty() || text.to_lowercase().contains(filter)
}

pub fn filtered_choices<'a>(choices: &'a [TargetChoice], filter: &str) -> Vec<&'a TargetChoice> {
    choices
        .iter()
        .filter(|choice| {
            matches(&choice.name, filter)
                || matches(&choice.id, filter)
                || choice.databases.iter().any(|database| matches(database, filter))
        })
        .collect()
}

pub fn filtered_databases<'a>(choice: &'a TargetChoice, filter: &str) -> Vec<&'a String> {
    choice
        .databases
        .iter()
        .filter(|database| matches(database, filter) || matches(&choice.name, filter) || matches(&choice.id, filter))
        .collect()
}

pub fn resolved_focus(
    picker: &TargetPicker,
    choices: &[TargetChoice],
    current_connection: &str,
    filter: &str,
) -> Option<String> {
    let shown = filtered_choices(choices, filter);
    if shown.is_empty() {
        return picker
            .focus
            .clone()
            .or_else(|| (!current_connection.is_empty()).then(|| current_connection.to_owned()))
            .or_else(|| choices.first().map(|choice| choice.id.clone()));
    }
    if let Some(id) = picker.focus.as_deref() {
        if shown.iter().any(|choice| choice.id == id) {
            return Some(id.to_owned());
        }
    }
    if !current_connection.is_empty() && shown.iter().any(|choice| choice.id == current_connection) {
        return Some(current_connection.to_owned());
    }
    shown.first().map(|choice| choice.id.clone())
}

/// 目标按钮上一帧的位置（窗口坐标）。没有点击坐标时（键盘、状态栏、执行被拦下）选择器贴着它弹出。
pub type ButtonBounds = std::rc::Rc<std::cell::Cell<Option<gpui_kit::Bounds<Pixels>>>>;

/// 选择器的锚点：按钮右下角向内一点，和鼠标点按钮时弹出的位置一致。
pub fn anchor_for(bounds: Option<gpui_kit::Bounds<Pixels>>) -> Point<Pixels> {
    match bounds {
        Some(area) => gpui_kit::point(area.origin.x + area.size.width - px(4.), area.origin.y + area.size.height / 2.),
        None => gpui_kit::point(px(0.), px(0.)),
    }
}

/// 标签绑着的连接已经不在保存的连接里（被删除或导入覆盖）。连接清单还没读到时不下结论，
/// 免得启动瞬间把每个标签都标成“已不存在”。
pub fn target_missing(known: bool, choices: &[TargetChoice], connection_id: &str) -> bool {
    known && !connection_id.is_empty() && !choices.iter().any(|choice| choice.id == connection_id)
}

/// 目标按钮：引擎徽标 + 连接名 › 库名 + 下拉箭头。没有目标时提示选择。
#[allow(clippy::too_many_arguments)]
pub fn button(open: bool, choices: &[TargetChoice], known: bool, connection_id: &str, connection_name: &str, database: &str, engine: &str, bounds: &ButtonBounds, locale: Locale, metrics: Metrics, p: &Palette, entity: &Entity<QueryPane>) -> AnyElement {
    let t = |key: &str| SharedString::from(i18n::text(locale, key));
    let has_target = !connection_id.is_empty();
    let connected = choices.iter().any(|choice| choice.id == connection_id && choice.connected);
    let missing = target_missing(known, choices, connection_id);
    let owner = entity.clone();
    let tooltip = if missing {
        Some(t("ui.query.targetMissing"))
    } else {
        (has_target && !open).then(|| SharedString::from(format!("{connection_name} › {database}")))
    };
    kit::interactive_states(div().id("query-connection"), p, false)
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(if missing { t("ui.query.targetMissing") } else { t("ui.query.chooseTarget") })
        .tooltip_text_opt(tooltip.filter(|_| !open))
        .h(px(metrics.control_h))
        .max_w(px(380.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(Radius::CONTROL))
        .border_1()
        .border_color(if open { p.accent } else if missing { p.warn } else { p.line })
        .when(open, |this| this.bg(p.hover))
        .hover(|style| style.border_color(p.line_strong).bg(p.hover))
        .text_size(px(TypeScale::SM))
        .when(has_target, |this| {
            this.child(kit::engine_badge(engine, 16.))
                .child(
                    div()
                        .min_w(px(0.))
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .when(missing, |this| this.text_color(p.warn).line_through())
                        .child(SharedString::from(if connection_name.is_empty() { connection_id.to_owned() } else { connection_name.to_owned() })),
                )
                .child(if missing {
                    kit::icon(IconName::TriangleAlert, 12., p.warn).into_any_element()
                } else {
                    kit::status_dot(if connected { p.ok } else { p.line_strong }).into_any_element()
                })
                .child(kit::icon(IconName::ChevronRight, 12., p.text_faint))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(kit::icon(IconName::Database, 12., p.text_muted))
                        .child(div().max_w(px(140.)).truncate().text_color(if database.is_empty() { p.text_faint } else { p.text }).child(if database.is_empty() { t("ui.query.chooseDatabase") } else { SharedString::from(database.to_owned()) })),
                )
        })
        .when(!has_target, |this| {
            this.child(kit::icon(IconName::Database, 14., p.text_muted)).child(div().text_color(p.text_muted).child(t("ui.query.chooseTarget")))
        })
        .child(kit::icon(IconName::ChevronDown, 12., p.text_faint))
        // 记下按钮在窗口里的位置：状态栏点目标、或执行时发现目标已不存在，都从这里弹出同一个选择器。
        .child({
            let bounds = bounds.clone();
            gpui_kit::canvas(move |area, _, _| bounds.set(Some(area)), |_, _, _, _| {}).absolute().top_0().left_0().size_full()
        })
        .relative()
        .on_click(move |event: &gpui_kit::ClickEvent, _, cx| {
            let at = event.position();
            owner.update(cx, |pane, cx| pane.toggle_target_picker(at, cx));
        })
        .into_any_element()
}

/// 双栏弹层。filter 已转小写。
#[allow(clippy::too_many_arguments)]
pub fn popover(
    picker: &TargetPicker,
    choices: &[TargetChoice],
    current: (&str, &str),
    filter: &str,
    viewport_width: f32,
    locale: Locale,
    metrics: Metrics,
    p: &Palette,
    entity: &Entity<QueryPane>,
) -> Option<AnyElement> {
    let anchor = picker.anchor?;
    let t = |key: &str| SharedString::from(i18n::text(locale, key));
    let (current_connection, current_database) = current;
    let shown = filtered_choices(choices, filter);
    let focus = resolved_focus(picker, choices, current_connection, filter);
    let focused = focus.as_deref().and_then(|id| choices.iter().find(|choice| choice.id == id));
    let row_h = metrics.row_h + 2.;
    let panel_w = (if viewport_width > 0. { viewport_width - 24. } else { 560. }).clamp(300., 560.);
    let left_w = if panel_w < 460. { 156. } else { 220. };

    let mut left = div()
        .id("query-target-connections")
        .track_scroll(&picker.conn_scroll)
        .w(px(left_w))
        .flex_none()
        .h_full()
        .overflow_y_scroll()
        .border_r_1()
        .border_color(p.line)
        .py(px(4.));
    left = left.child(div().px(px(10.)).py(px(4.)).text_size(px(TypeScale::XS)).text_color(p.text_faint).child(t("ui.query.targetConnections")));
    // 一条连接都没有和“筛选后没有匹配”是两回事：前者要给出新建连接的出口，而不是让人对着空列表找。
    let no_connections = choices.is_empty();
    if no_connections {
        left = left.child(div().px(px(10.)).py(px(6.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child(t("ui.query.targetNoConnections")));
    } else if shown.is_empty() {
        left = left.child(div().px(px(10.)).py(px(6.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child(t("ui.query.targetNoMatch")));
    }
    for choice in &shown {
        let selected = focused.is_some_and(|focused| focused.id == choice.id);
        let current = choice.id == current_connection;
        let owner = entity.clone();
        let id = choice.id.clone();
        left = left.child(
            kit::row_states(div().id(SharedString::from(format!("query-target-conn-{}", choice.id))), p)
                .test_support()
                .role(gpui_kit::Role::ListItem)
                .aria_label(SharedString::from(choice.name.clone()))
                .aria_selected(selected)
                .mx(px(4.))
                .h(px(row_h))
                .px(px(6.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(Radius::CONTROL))
                .when(selected, |this| this.bg(p.selection))
                .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
                .child(kit::engine_badge(&choice.engine, 16.))
                .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::SM)).when(current, |this| this.font_weight(FontWeight::SEMIBOLD)).child(SharedString::from(choice.name.clone())))
                .child(kit::status_dot(if choice.connected { p.ok } else { p.line_strong }))
                .child(kit::icon(IconName::ChevronRight, 12., if selected { p.text_muted } else { p.text_faint }))
                .on_click(move |_, _, cx| {
                    let id = id.clone();
                    owner.update(cx, |pane, cx| pane.focus_target_connection(&id, cx));
                }),
        );
    }

    let mut right = div()
        .id("query-target-databases")
        .track_scroll(&picker.db_scroll)
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .overflow_y_scroll()
        .py(px(4.));
    match focused {
        None if no_connections => {
            let owner = entity.clone();
            right = right.child(
                div()
                    .id("query-target-empty")
                    .test_support()
                    .p(px(14.))
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(8.))
                    .child(div().text_size(px(TypeScale::SM)).font_weight(FontWeight::MEDIUM).child(t("ui.query.targetNoConnections")))
                    .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(t("ui.query.targetNoConnectionsHint")))
                    .child(kit::primary_button("query-target-new-connection", Some(IconName::Plus), t("shell.command.newConnection"), move |_, _, cx| {
                        owner.update(cx, |pane, cx| pane.new_connection_from_target(cx));
                    })),
            );
        }
        None => {}
        Some(choice) if !choice.connected => {
            let pending = picker.pending.as_deref() == Some(choice.id.as_str());
            let owner = entity.clone();
            let id = choice.id.clone();
            right = right.child(
                div()
                    .p(px(14.))
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(8.))
                    .child(div().text_size(px(TypeScale::SM)).font_weight(FontWeight::MEDIUM).child(SharedString::from(choice.name.clone())))
                    .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(t("ui.query.targetNotOpen")))
                    .child(if pending {
                        kit::inline_loading(t("ui.conn.connecting"), &metrics, p).into_any_element()
                    } else {
                        kit::primary_button("query-target-connect", Some(IconName::Plug), t("connections.page.connect"), move |_, _, cx| {
                            let id = id.clone();
                            owner.update(cx, |pane, cx| pane.connect_target(&id, cx));
                        })
                        .into_any_element()
                    }),
            );
        }
        Some(choice) => {
            let databases = filtered_databases(choice, filter);
            let active_db = picker.db_index.min(databases.len().saturating_sub(1));
            right = right.child(
                div()
                    .px(px(10.))
                    .py(px(4.))
                    .text_size(px(TypeScale::XS))
                    .text_color(p.text_faint)
                    .child(SharedString::from(i18n::fill(&i18n::text(locale, "ui.query.targetDatabases"), &[("name", &choice.name)]))),
            );
            if databases.is_empty() {
                right = right.child(div().px(px(10.)).py(px(6.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child(t("ui.query.targetNoDatabase")));
            }
            for (ix, database) in databases.into_iter().enumerate() {
                let current = choice.id == current_connection && database == current_database;
                let highlighted = ix == active_db;
                let owner = entity.clone();
                let (id, name) = (choice.id.clone(), database.clone());
                right = right.child(
                    kit::row_states(div().id(SharedString::from(format!("query-target-db-{}-{database}", choice.id))), p)
                        .test_support()
                        .role(gpui_kit::Role::ListItem)
                        .aria_label(SharedString::from(database.clone()))
                        .aria_selected(highlighted)
                        .mx(px(4.))
                        .h(px(row_h))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .rounded(px(Radius::CONTROL))
                        .when(current && !highlighted, |this| this.bg(p.accent_soft))
                        .when(highlighted && !current, |this| this.bg(p.selection))
                        .when(highlighted && current, |this| this.bg(p.accent_soft).border_1().border_color(p.accent))
                        .hover(|style| style.bg(p.hover))
                        .child(kit::icon(IconName::Database, 13., if current || highlighted { p.accent_text } else { p.text_muted }))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .text_size(px(TypeScale::SM))
                                .when(current, |this| this.text_color(p.accent_text).font_weight(FontWeight::MEDIUM))
                                .when(highlighted && !current, |this| this.font_weight(FontWeight::MEDIUM))
                                .child(SharedString::from(database.clone())),
                        )
                        .when(current, |this| this.child(kit::icon(IconName::Check, 13., p.accent_text)))
                        .when(highlighted && !current, |this| this.child(kit::icon(IconName::CornerDownLeft, 12., p.text_muted)))
                        .on_click(move |_, _, cx| {
                            let (id, name) = (id.clone(), name.clone());
                            owner.update(cx, |pane, cx| pane.pick_target(&id, &name, cx));
                        }),
                );
            }
        }
    }

    let search = picker.search.clone().map(|state| {
        kit::text_input("query-target-search", &state, t("ui.query.targetSearch")).small().prefix(kit::icon(IconName::Search, 13., p.text_faint)).cleanable(true)
    });
    let hint = |keys: &'static str, label: SharedString| div().flex().items_center().gap(px(4.)).child(kit::kbd(keys, p)).child(label);
    let footer = div()
        .id("query-target-footer")
        .flex_none()
        .px(px(10.))
        .py(px(5.))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(10.))
        .border_t_1()
        .border_color(p.line)
        .bg(p.chrome)
        .text_size(px(TypeScale::XS))
        .text_color(p.text_faint)
        .child(hint("↑↓", t("ui.query.targetHintMove")))
        .child(hint("Tab", t("ui.query.targetHintConn")))
        .child(hint("Enter", t("shell.palette.hintRun")))
        .child(hint("Esc", t("shell.palette.hintClose")));
    // 高度贴合内容，最高 360：只有一两条连接时不留一大块空白。右栏按所有已打开连接里最长的库清单估算，
    // 用 Tab 在连接间切换时面板不会跟着忽高忽低。
    let left_rows = shown.len().max(1);
    let right_rows = choices
        .iter()
        .filter(|choice| choice.connected)
        .map(|choice| filtered_databases(choice, filter).len())
        .max()
        .unwrap_or(0)
        .max(if no_connections || choices.iter().any(|choice| !choice.connected) { 4 } else { 1 });
    let list_h = left_rows.max(right_rows) as f32 * row_h + 34.;
    let panel_h = (list_h + 46. + 32.).clamp(220., 360.);
    let close = entity.clone();
    let keys = entity.clone();
    let panel = div()
        .id("query-target-popover")
        .test_support()
        .occlude()
        .w(px(panel_w))
        .h(px(panel_h))
        .flex()
        .flex_col()
        .bg(p.raised)
        .border_1()
        .border_color(p.line)
        .rounded(px(Radius::PANEL))
        .shadow_lg()
        .overflow_hidden()
        .on_mouse_down_out(move |_, _, cx| close.update(cx, |pane, cx| pane.close_target_picker(cx)))
        .on_key_down(move |event: &gpui_kit::KeyDownEvent, _, cx| {
            match event.keystroke.key.as_str() {
                "escape" => {
                    cx.stop_propagation();
                    keys.update(cx, |pane, cx| pane.close_target_picker(cx));
                }
                "up" => {
                    cx.stop_propagation();
                    keys.update(cx, |pane, cx| pane.step_target_picker(-1, cx));
                }
                "down" => {
                    cx.stop_propagation();
                    keys.update(cx, |pane, cx| pane.step_target_picker(1, cx));
                }
                "tab" => {
                    cx.stop_propagation();
                    let backward = event.keystroke.modifiers.shift;
                    keys.update(cx, |pane, cx| pane.cycle_target_connection(backward, cx));
                }
                "enter" => {
                    cx.stop_propagation();
                    keys.update(cx, |pane, cx| pane.pick_highlighted_target(cx));
                }
                _ => {}
            }
        })
        .children(search.map(|input| div().flex_none().p(px(8.)).border_b_1().border_color(p.line).child(input)))
        .child(div().flex_1().min_h(px(0.)).flex().child(left).child(right))
        .child(footer);
    Some(deferred(anchored().position(anchor).anchor(Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(div().pt(px(14.)).child(panel))).with_priority(2).into_any_element())
}

trait TooltipOpt: Sized {
    fn tooltip_text_opt(self, text: Option<SharedString>) -> Self;
}

impl<E: gpui_kit::StatefulInteractiveElement + kit::Tip> TooltipOpt for E {
    fn tooltip_text_opt(self, text: Option<SharedString>) -> Self {
        match text {
            Some(text) => self.tooltip_text(text),
            None => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(id: &str) -> TargetChoice {
        TargetChoice { id: id.to_owned(), name: id.to_owned(), engine: "postgres".to_owned(), connected: false, databases: Vec::new() }
    }

    #[test]
    fn a_target_is_missing_only_after_the_connection_list_is_known() {
        let choices = vec![choice("pg")];
        assert!(!target_missing(false, &[], "gone"), "清单还没读到时不下结论");
        assert!(target_missing(true, &choices, "gone"), "绑定的连接已不在清单里");
        assert!(target_missing(true, &[], "gone"), "删掉最后一条连接也算不存在");
        assert!(!target_missing(true, &choices, "pg"));
        assert!(!target_missing(true, &choices, ""), "没绑定目标不是“已不存在”");
    }
}
