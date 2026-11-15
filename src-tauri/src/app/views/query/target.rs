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
#[derive(Default)]
pub struct TargetPicker {
    /// 弹层的锚点（打开它的那次点击位置）；None 表示关闭。
    pub anchor: Option<Point<Pixels>>,
    /// 左栏当前选中的连接（右栏列它的库）。
    pub focus: Option<String>,
    pub search: Option<Entity<InputState>>,
    /// 等着连接成功后再绑定的连接。
    pub pending: Option<String>,
}

fn matches(text: &str, filter: &str) -> bool {
    filter.is_empty() || text.to_lowercase().contains(filter)
}

/// 目标按钮：引擎徽标 + 连接名 › 库名 + 下拉箭头。没有目标时提示选择。
#[allow(clippy::too_many_arguments)]
pub fn button(open: bool, choices: &[TargetChoice], connection_id: &str, connection_name: &str, database: &str, engine: &str, locale: Locale, metrics: Metrics, p: &Palette, entity: &Entity<QueryPane>) -> AnyElement {
    let t = |key: &str| SharedString::from(i18n::text(locale, key));
    let has_target = !connection_id.is_empty();
    let connected = choices.iter().any(|choice| choice.id == connection_id && choice.connected);
    let owner = entity.clone();
    kit::interactive_states(div().id("query-connection"), p, false)
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(t("ui.query.chooseTarget"))
        .tooltip_text_opt((has_target && !open).then(|| SharedString::from(format!("{connection_name} › {database}"))))
        .h(px(metrics.control_h))
        .max_w(px(380.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(Radius::CONTROL))
        .border_1()
        .border_color(if open { p.accent } else { p.line })
        .when(open, |this| this.bg(p.hover))
        .hover(|style| style.border_color(p.line_strong).bg(p.hover))
        .text_size(px(TypeScale::SM))
        .when(has_target, |this| {
            this.child(kit::engine_badge(engine, 16.))
                .child(div().min_w(px(0.)).truncate().font_weight(FontWeight::MEDIUM).child(SharedString::from(if connection_name.is_empty() { connection_id.to_owned() } else { connection_name.to_owned() })))
                .child(kit::status_dot(if connected { p.ok } else { p.line_strong }))
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
    locale: Locale,
    metrics: Metrics,
    p: &Palette,
    entity: &Entity<QueryPane>,
) -> Option<AnyElement> {
    let anchor = picker.anchor?;
    let t = |key: &str| SharedString::from(i18n::text(locale, key));
    let (current_connection, current_database) = current;
    let focus = picker.focus.clone().or_else(|| (!current_connection.is_empty()).then(|| current_connection.to_owned())).or_else(|| choices.first().map(|choice| choice.id.clone()));
    // 搜索命中：连接名命中或它的某个库命中都留下这条连接。
    let shown: Vec<&TargetChoice> = choices
        .iter()
        .filter(|choice| matches(&choice.name, filter) || matches(&choice.id, filter) || choice.databases.iter().any(|database| matches(database, filter)))
        .collect();
    let focused = focus.as_deref().and_then(|id| choices.iter().find(|choice| choice.id == id));
    let row_h = metrics.row_h + 2.;

    let mut left = div().id("query-target-connections").w(px(230.)).flex_none().h_full().overflow_y_scroll().border_r_1().border_color(p.line).py(px(4.));
    left = left.child(div().px(px(10.)).py(px(4.)).text_size(px(TypeScale::XS)).text_color(p.text_faint).child(t("ui.query.targetConnections")));
    if shown.is_empty() {
        left = left.child(div().px(px(10.)).py(px(6.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child(t("ui.query.targetNoMatch")));
    }
    for choice in &shown {
        let selected = focused.is_some_and(|focused| focused.id == choice.id);
        let current = choice.id == current_connection;
        let owner = entity.clone();
        let id = choice.id.clone();
        left = left.child(
            kit::row_states(div().id(SharedString::from(format!("query-target-conn-{}", choice.id))), p)
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

    let mut right = div().id("query-target-databases").flex_1().min_w(px(0.)).h_full().overflow_y_scroll().py(px(4.));
    match focused {
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
            let databases: Vec<&String> = choice.databases.iter().filter(|database| matches(database, filter) || matches(&choice.name, filter)).collect();
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
            for database in databases {
                let current = choice.id == current_connection && database == current_database;
                let owner = entity.clone();
                let (id, name) = (choice.id.clone(), database.clone());
                right = right.child(
                    kit::row_states(div().id(SharedString::from(format!("query-target-db-{}-{database}", choice.id))), p)
                        .mx(px(4.))
                        .h(px(row_h))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .rounded(px(Radius::CONTROL))
                        .when(current, |this| this.bg(p.accent_soft))
                        .hover(|style| style.bg(p.hover))
                        .child(kit::icon(IconName::Database, 13., if current { p.accent_text } else { p.text_muted }))
                        .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::SM)).when(current, |this| this.text_color(p.accent_text).font_weight(FontWeight::MEDIUM)).child(SharedString::from(database.clone())))
                        .when(current, |this| this.child(kit::icon(IconName::Check, 13., p.accent_text)))
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
    let close = entity.clone();
    let keys = entity.clone();
    let panel = div()
        .id("query-target-popover")
        .occlude()
        .w(px(560.))
        .h(px(360.))
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
                "enter" => {
                    cx.stop_propagation();
                    keys.update(cx, |pane, cx| pane.pick_first_target(cx));
                }
                _ => {}
            }
        })
        .children(search.map(|input| div().flex_none().p(px(8.)).border_b_1().border_color(p.line).child(input)))
        .child(div().flex_1().min_h(px(0.)).flex().child(left).child(right));
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
