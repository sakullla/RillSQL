//! 查询页右侧面板：语句大纲与光标处对象信息共用一列，用分段控件切换。
//! 宽窗口里它在编辑器旁边；窄窗口默认收起，打开时盖在编辑器上，不改变编辑器和结果区的布局。

use gpui_kit::assets::IconName;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, ClickEvent, Entity, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, div, px,
};

use crate::i18n::{self, Locale};
use crate::query::assist::{self, OutlineItem};
use crate::query::desk::ObjectInfo;
use crate::ui::kit::{self, Metrics, Palette, Radius, Tip as _, TypeScale};

use super::QueryPane;

/// 视口窄于它时右侧面板默认收起，打开时作为浮层。
pub const NARROW_WIDTH: f32 = 800.;
pub const PANEL_WIDTH: f32 = 240.;
pub const PANEL_MIN: f32 = 180.;
pub const PANEL_MAX: f32 = 400.;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SideMode {
    #[default]
    Outline,
    Object,
}

/// 每个查询标签自己的右侧面板状态。open 为 None 表示用户还没动过，按宽窄取默认值。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SideState {
    pub open: Option<bool>,
    pub mode: SideMode,
}

impl SideState {
    pub fn is_open(&self, narrow: bool) -> bool {
        self.open.unwrap_or(!narrow)
    }
}

pub enum SideBody<'a> {
    Outline(&'a [OutlineItem]),
    Object(Option<&'a ObjectInfo>),
}

pub struct SidePanel<'a> {
    pub mode: SideMode,
    pub body: SideBody<'a>,
    pub locale: Locale,
    pub metrics: Metrics,
    pub overlay: bool,
    pub width: f32,
    pub active_statement: Option<usize>,
    pub mono: SharedString,
    /// 当前目标的引擎，用来给点选插入的标识符加引号。
    pub engine: &'a str,
}

impl SidePanel<'_> {
    pub fn render(self, p: &Palette, entity: &Entity<QueryPane>) -> AnyElement {
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let mode_item = |id: &'static str, key: &str, mode: SideMode| {
            let entity = entity.clone();
            let command = match mode {
                SideMode::Outline => "side-mode:outline",
                SideMode::Object => "side-mode:object",
            };
            (
                SharedString::from(id),
                t(key),
                self.mode == mode,
                kit::click(move |_, _, cx| {
                    entity.update(cx, |pane, cx| pane.command(command, cx));
                }),
            )
        };
        let close_entity = entity.clone();
        let header = div()
            .id("query-side-head")
            .flex_none()
            .px(px(8.))
            .py(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .border_b_1()
            .border_color(p.line)
            .child(kit::segmented_sized(
                vec![
                    mode_item("query-side-mode-outline", "ui.query.outline", SideMode::Outline),
                    mode_item("query-side-mode-object", "query.side.object", SideMode::Object),
                ],
                p,
                self.metrics.control_h,
            ))
            .child(div().flex_1())
            // 收起：和页签关闭同一个纯图标动作，命中范围与四态不再由页面另拼。
            .child(kit::icon_button("query-side-close", IconName::X, t("query.side.close"), move |_, _, cx| {
                close_entity.update(cx, |pane, cx| pane.command("toggle-side", cx));
            })
            .xsmall());
        let body = match self.body {
            SideBody::Outline(items) => outline(items, self.active_statement, locale, self.metrics, &self.mono, p, entity),
            SideBody::Object(info) => object(info, locale, self.metrics, &self.mono, p, self.engine, entity),
        };
        let panel_w = if self.overlay { PANEL_WIDTH } else { self.width.clamp(PANEL_MIN, PANEL_MAX) };
        div()
            .id("query-side-panel")
            .test_support()
            .role(gpui_kit::Role::Complementary)
            .aria_label(t("query.side.toggle"))
            .flex_none()
            .w(px(panel_w))
            .h_full()
            .flex()
            .flex_col()
            .bg(p.chrome)
            .border_l_1()
            .border_color(p.line)
            .when(self.overlay, |this| this.absolute().top_0().right_0().shadow_lg())
            .child(header)
            .child(body)
            .into_any_element()
    }
}

fn outline(items: &[OutlineItem], active_statement: Option<usize>, locale: Locale, metrics: Metrics, mono: &SharedString, p: &Palette, entity: &Entity<QueryPane>) -> AnyElement {
    let mut list = div()
        .id("query-outline")
        .test_support()
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .p(px(8.))
        .flex()
        .flex_col()
        .gap(px(2.));
    if items.is_empty() {
        list = list.child(
            div()
                .id("query-outline-empty")
                .test_support()
                .px(px(6.))
                .py(px(4.))
                .text_size(px(TypeScale::SM))
                .text_color(p.text_muted)
                .child(SharedString::from(i18n::text(locale, "query.side.noStatements"))),
        );
    }
    for item in items {
        list = list.child(outline_row(item, active_statement == Some(item.index), locale, metrics, mono, p, entity));
    }
    list.into_any_element()
}

fn outline_row(item: &OutlineItem, current: bool, locale: Locale, metrics: Metrics, mono: &SharedString, p: &Palette, entity: &Entity<QueryPane>) -> AnyElement {
    let failed = item.status == "error" || item.status == "failed";
    let skipped = item.status == "skipped";
    let index = item.index;
    let jump = entity.clone();
    let run = entity.clone();
    kit::interactive_states(div().id(SharedString::from(format!("query-outline-{index}"))), p, false)
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(SharedString::from(format!("{} {}", i18n::text(locale, "query.outline.jump"), index)))
        .aria_selected(current)
        .on_click(move |event: &ClickEvent, _, cx| {
            let command = if event.click_count() >= 2 { format!("outline-run:{index}") } else { format!("outline:{index}") };
            jump.update(cx, |pane, cx| pane.command(&command, cx));
        })
        .flex_none()
        .min_h(px(metrics.tree_row_h))
        .px(px(6.))
        .rounded(px(Radius::CONTROL))
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(TypeScale::SM))
        .when(current, |this| this.bg(p.accent_soft))
        .when(!current, |this| this.hover(|style| style.bg(p.hover)))
        .child(
            div()
                .flex_none()
                .text_color(if current { p.accent_text } else { p.text_faint })
                .when(current, |this| this.font_weight(gpui_kit::FontWeight::SEMIBOLD))
                .child(SharedString::from(format!("{index}"))),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .font_family(mono.clone())
                .when(current && !failed && !skipped, |this| this.text_color(p.accent_text).font_weight(gpui_kit::FontWeight::MEDIUM))
                .when(failed, |this| this.text_color(p.danger))
                .when(skipped, |this| this.text_color(p.text_faint))
                .child(SharedString::from(item.preview.clone())),
        )
        .child(
            div()
                .id(SharedString::from(format!("query-outline-run-{index}")))
                .test_support()
                .role(gpui_kit::Role::Button)
                .aria_label(SharedString::from(i18n::text(locale, "query.outline.run")))
                .tooltip_text(SharedString::from(i18n::text(locale, "query.outline.run")))
                .flex_none()
                .size(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(Radius::CONTROL))
                .hover(|style| style.bg(p.active))
                .child(kit::icon(IconName::Play, 12., p.accent))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    run.update(cx, |pane, cx| pane.command(&format!("outline-run:{index}"), cx));
                }),
        )
        .into_any_element()
}

/// 对象信息里一列的可读说明：名字、类型、主键、外键、可空和注释。
pub fn column_text(locale: Locale, column: &crate::query::assist::ColumnMeta) -> String {
    let mut parts = vec![column.name.clone()];
    if !column.type_name.is_empty() {
        parts.push(column.type_name.clone());
    }
    if column.primary {
        parts.push(i18n::text(locale, "query.side.primary"));
    }
    if column.foreign_key {
        parts.push(i18n::text(locale, "query.side.foreign"));
    }
    match column.nullable {
        Some(true) => parts.push(i18n::text(locale, "query.side.nullable")),
        Some(false) => parts.push(i18n::text(locale, "query.side.notNull")),
        None => {}
    }
    if !column.comment.is_empty() {
        parts.push(column.comment.clone());
    }
    parts.join(" · ")
}

#[allow(clippy::too_many_arguments)]
fn object_column(
    index: usize,
    column: &crate::query::assist::ColumnMeta,
    focused_name: Option<&str>,
    locale: Locale,
    metrics: Metrics,
    mono: &SharedString,
    p: &Palette,
    engine: &str,
    table: &str,
    entity: &Entity<QueryPane>,
) -> AnyElement {
    let focused = focused_name.is_some_and(|name| name.eq_ignore_ascii_case(&column.name));
    let nullable = match column.nullable {
        Some(true) => i18n::text(locale, "query.side.nullable"),
        Some(false) => i18n::text(locale, "query.side.notNull"),
        None => String::new(),
    };
    let quoted = assist::quote_ident(engine, &column.name);
    let insert = entity.clone();
    let menu_entity = entity.clone();
    let menu_name = column.name.clone();
    let menu_quoted = quoted.clone();
    let menu_table = table.to_owned();
    kit::row_states(div().id(SharedString::from(format!("query-side-column-{index}"))), p)
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(SharedString::from(column_text(locale, column)))
        .aria_selected(focused)
        .tooltip_text(SharedString::from(column_text(locale, column)))
        .flex_none()
        .min_h(px(metrics.row_h))
        .px(px(12.))
        .py(px(3.))
        .flex()
        .flex_col()
        .justify_center()
        .text_size(px(TypeScale::SM))
        .rounded(px(Radius::CONTROL))
        .when(focused, |this| this.bg(p.accent_soft))
        .hover(|style| style.bg(p.hover))
        .on_click(move |_, window, cx| {
            let sql = quoted.clone();
            insert.update(cx, |pane, cx| pane.insert_identifier(&sql, window, cx));
        })
        // 右键：写查询时最常用的几件事——插入、复制字段名/限定名、插入一个筛选条件。
        .context_menu(move |menu, _, _| {
            let (insert, copy, qualified, condition) = (menu_entity.clone(), menu_name.clone(), format!("{menu_table}.{menu_name}"), format!("{menu_quoted} = "));
            let insert_where = menu_entity.clone();
            let quoted = menu_quoted.clone();
            menu.item(PopupMenuItem::new(i18n::text(locale, "query.side.menuInsert")).icon(IconName::SquarePen).on_click(move |_, window, cx| {
                let sql = quoted.clone();
                insert.update(cx, |pane, cx| pane.insert_identifier(&sql, window, cx));
            }))
            .item(PopupMenuItem::new(i18n::text(locale, "query.side.menuWhere")).icon(IconName::Funnel).on_click(move |_, window, cx| {
                let sql = condition.clone();
                insert_where.update(cx, |pane, cx| pane.insert_identifier(&sql, window, cx));
            }))
            .separator()
            .item(PopupMenuItem::new(i18n::text(locale, "query.side.menuCopyName")).icon(IconName::Copy).on_click(move |_, _, cx| {
                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(copy.clone()));
            }))
            .item(PopupMenuItem::new(i18n::text(locale, "query.side.menuCopyQualified")).on_click(move |_, _, cx| {
                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(qualified.clone()));
            }))
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .when(column.primary, |this| this.child(kit::icon(IconName::Key, 12., p.warn)))
                .when(column.foreign_key, |this| this.child(kit::icon(IconName::Link, 12., p.accent)))
                .child(div().min_w(px(0.)).truncate().font_family(mono.clone()).text_color(p.text).child(SharedString::from(column.name.clone())))
                .child(div().flex_1())
                .child(div().flex_none().text_color(p.text_muted).child(SharedString::from(column.type_name.clone())))
                .when(!nullable.is_empty(), |this| this.child(div().flex_none().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(nullable)))),
        )
        .when(!column.comment.is_empty(), |this| {
            this.child(div().truncate().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(column.comment.clone())))
        })
        .into_any_element()
}

fn object(info: Option<&ObjectInfo>, locale: Locale, metrics: Metrics, mono: &SharedString, p: &Palette, engine: &str, entity: &Entity<QueryPane>) -> AnyElement {
    let Some(info) = info else {
        return div()
            .id("query-side-object")
            .test_support()
            .flex_1()
            .p(px(12.))
            .child(
                div()
                    .id("query-side-object-none")
                    .test_support()
                    .text_size(px(TypeScale::SM))
                    .text_color(p.text_muted)
                    .child(SharedString::from(i18n::text(locale, "query.side.noObject"))),
            )
            .into_any_element();
    };
    let object_sql = assist::qualified_name(engine, &info.schema, &info.name);
    let qualified = if info.schema.is_empty() { info.name.clone() } else { format!("{}.{}", info.schema, info.name) };
    let insert_object = entity.clone();
    let kind_key = format!("query.complete.kind.{}", info.kind);
    let kind = i18n::text(locale, &kind_key);
    let kind = if kind == kind_key { info.kind.clone() } else { kind };
    let mut body = div()
        .id("query-side-object")
        .test_support()
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_none()
                .px(px(12.))
                .py(px(8.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    kit::interactive_states(div().id("query-side-object-name"), p, false)
                        .test_support()
                        .role(gpui_kit::Role::Button)
                        .aria_label(SharedString::from(format!("{} {}", i18n::text(locale, "catalog.menu.insertEditor"), qualified)))
                        .tooltip_text(SharedString::from(format!("{} {}", i18n::text(locale, "catalog.menu.insertEditor"), qualified)))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .rounded(px(Radius::CONTROL))
                        .hover(|style| style.bg(p.hover))
                        .on_click({
                            let object_sql = object_sql.clone();
                            let insert_object = insert_object.clone();
                            move |_, window, cx| {
                                let sql = object_sql.clone();
                                insert_object.update(cx, |pane, cx| pane.insert_identifier(&sql, window, cx));
                            }
                        })
                        .context_menu({
                            let name = qualified.clone();
                            move |menu, _, _| {
                                let (insert, sql, copy) = (insert_object.clone(), object_sql.clone(), name.clone());
                                menu.item(PopupMenuItem::new(i18n::text(locale, "query.side.menuInsert")).icon(IconName::SquarePen).on_click(move |_, window, cx| {
                                    let sql = sql.clone();
                                    insert.update(cx, |pane, cx| pane.insert_identifier(&sql, window, cx));
                                }))
                                .item(PopupMenuItem::new(i18n::text(locale, "query.side.menuCopyObject")).icon(IconName::Copy).on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(copy.clone()));
                                }))
                            }
                        })
                        .child(div().min_w(px(0.)).truncate().font_family(mono.clone()).text_size(px(TypeScale::MD)).text_color(p.text).child(SharedString::from(qualified)))
                        .child(div().flex_none().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(kind))),
                )
                .when(!info.comment.is_empty(), |this| {
                    this.child(div().id("query-side-object-comment").test_support().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(info.comment.clone())))
                }),
        );
    if info.columns.is_empty() {
        body = body.child(div().px(px(12.)).text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(i18n::text(locale, "query.side.noColumns"))));
    }
    for (index, column) in info.columns.iter().enumerate() {
        body = body.child(object_column(index, column, info.column.as_deref(), locale, metrics, mono, p, engine, &info.name, entity));
    }
    body.into_any_element()
}
