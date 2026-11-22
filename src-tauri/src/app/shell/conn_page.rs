//! 连接页：左边是可搜索、可按标签/引擎筛选、按分组排列的连接列表，右边是所选连接的详情与操作。
//! 窄窗口只留列表，操作跟在选中行上。标签由用户自己维护（新建、改名、换色、删除）。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, AnyElement, AppContext as _, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
};

use crate::app::views::connections::LinkStatus;
use crate::connections::model::TlsMode;
use crate::connections::ConnectionRecord;
use crate::ui::kit::{self, Palette, Radius, Space, Tip as _, TypeScale};
use crate::ui::kit::ScrollAreaExt as _;

use super::chrome::{act, connection_detail, short_path, status_parts};
use super::model::DraftField;
use super::view::Workbench;

/// 宽度够时才在右侧放详情栏；更窄时操作跟着选中行走。
const DETAIL_MIN_WIDTH: f32 = 860.;
const DETAIL_W: f32 = 320.;
/// 标签可选的色调：与 `parse_tone` 一一对应。
pub(super) const TONES: [&str; 5] = ["dev", "test", "prod", "info", "local"];

impl Workbench {
    pub(super) fn connections_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(message) = self.model.bundle_unlock {
            return self.bundle_unlock_page(message, window, cx);
        }
        if !matches!(self.model.profile_import, super::conn_import::ImportState::Idle | super::conn_import::ImportState::Picking) {
            return self.import_profiles_page(window, cx);
        }
        let p = Palette::of(cx);
        let wide = f32::from(window.viewport_size().width) >= DETAIL_MIN_WIDTH;
        // 列表非空却没有选中：默认选第一条，详情栏不空着。
        if self.model.connections.selected.as_ref().is_none_or(|id| !self.model.connections.rows.iter().any(|row| &row.saved.id == id)) {
            self.model.connections.selected = self.model.connections.launcher_all().first().map(|row| row.saved.id.clone());
        }
        let detail = if wide && !self.model.connections.rows.is_empty() { Some(self.connection_detail_panel(&p, window, cx)) } else { None };
        div()
            .id("connections-page")
            .role(gpui_kit::Role::Group)
            .aria_label(self.t("connections.page.title"))
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(self.launcher_header(window, cx))
            .child(div().flex_1().min_h_0().flex().child(self.launcher_body(wide, window, cx)).children(detail))
            .into_any_element()
    }

    /// 头部两行：标题与在线概览 + 视图切换（看板/列表）与主操作；搜索、引擎筛选与环境标签条。
    pub(super) fn launcher_header(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let filter = self.input_for(DraftField::Filter, &self.model.text("ui.conn.search"), false, window, cx);
        let shown = self.model.connections.filtered().len();
        let total = self.model.connections.rows.len();
        let online = self
            .model
            .connections
            .rows
            .iter()
            .filter(|row| self.model.connections.row_status(&row.saved.id) == LinkStatus::Connected)
            .count();
        let count = if shown == total { total.to_string() } else { format!("{shown} / {total}") };
        let engine = self
            .model
            .connections
            .filter_engine
            .map(|engine| kit::engine_label(engine.as_str()).to_owned())
            .unwrap_or_else(|| self.model.text("connections.filter.engine"));
        let engine_open = self.model.conn_filter_menu == Some(super::model::ConnFilterMenu::Engine);
        let filtering = self.model.connections.filter_engine.is_some()
            || !self.model.connections.filter_env.is_empty()
            || !self.model.connections.filter_text.trim().is_empty();
        let board_mode = self.model.conn_board_cards;
        let wide_tools = self.model.viewport_width >= 980;
        let icon = |id: &'static str, icon: IconName, key: &str, command: &'static str, cx: &mut Context<Self>| {
            kit::icon_button(id, icon, self.model.text(key), act(command, cx)).small().into_any_element()
        };
        let layout_toggle = div()
            .flex_none()
            .p(px(2.))
            .flex()
            .items_center()
            .gap(px(2.))
            .rounded(px(Radius::CONTROL))
            .bg(p.sunken)
            .border_1()
            .border_color(p.line_soft)
            .child(
                kit::interactive_states(div().id("conn-layout-board"), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(self.t("conn.page.layoutBoard"))
                    .aria_selected(board_mode)
                    .h(px(m.control_h - 6.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .rounded(px(Radius::CONTROL))
                    .text_size(px(TypeScale::XS))
                    .when(board_mode, |this| {
                        this.bg(if p.dark { p.raised } else { p.surface })
                            .text_color(p.text)
                            .font_weight(FontWeight::MEDIUM)
                            .shadow_sm()
                    })
                    .when(!board_mode, |this| this.text_color(p.text_muted).hover(|s| s.text_color(p.text)))
                    .child(kit::icon(IconName::LayoutDashboard, 12., if board_mode { p.accent_text } else { p.text_muted }))
                    .child(self.t("conn.page.layoutBoard"))
                    .on_click(act("conn-layout-board", cx)),
            )
            .child(
                kit::interactive_states(div().id("conn-layout-list"), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(self.t("conn.page.layoutList"))
                    .aria_selected(!board_mode)
                    .h(px(m.control_h - 6.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .rounded(px(Radius::CONTROL))
                    .text_size(px(TypeScale::XS))
                    .when(!board_mode, |this| {
                        this.bg(if p.dark { p.raised } else { p.surface })
                            .text_color(p.text)
                            .font_weight(FontWeight::MEDIUM)
                            .shadow_sm()
                    })
                    .when(board_mode, |this| this.text_color(p.text_muted).hover(|s| s.text_color(p.text)))
                    .child(kit::icon(IconName::List, 12., if !board_mode { p.accent_text } else { p.text_muted }))
                    .child(self.t("conn.page.layoutList"))
                    .on_click(act("conn-layout-list", cx)),
            );

        let transfer_menu = self.transfer_dropdown("launcher-transfer-menu", wide_tools, true, &p, cx);
        let mut secondary: Vec<AnyElement> = if wide_tools {
            vec![
                kit::tool_button("launcher-group-new", Some(IconName::FolderPlus), self.t("ui.group.new"), act("group-new", cx)).small().into_any_element(),
                kit::tool_button("launcher-open-file", Some(IconName::FolderOpen), self.t("shell.command.openFile"), act("open-file", cx)).small().into_any_element(),
            ]
        } else {
            vec![
                icon("launcher-group-new", IconName::FolderPlus, "ui.group.new", "group-new", cx),
                icon("launcher-open-file", IconName::FolderOpen, "shell.command.openFile", "open-file", cx),
            ]
        };
        if total == 0 {
            secondary.push(icon("launcher-tags", IconName::Tag, "conn.page.manageTags", "tags-manage", cx));
        }
        secondary.push(transfer_menu);

        let focus = self.conn_launcher_focus.clone();
        let compact_search = self.model.viewport_width >= 820;
        div()
            .flex_none()
            .px(px(m.page_pad))
            .pt(px(m.page_pad_y))
            .pb(px(Space::MD))
            .flex()
            .flex_col()
            .gap(px(Space::MD))
            .bg(p.chrome)
            .border_b_1()
            .border_color(p.line_soft)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(Space::MD))
                    .when(!self.model.tabs.is_empty() && self.model.viewport_width >= 760, |this| {
                        this.child(kit::tool_button("conn-back", Some(IconName::ArrowLeft), self.t("ui.page.backWorkspace"), act("back-workspace", cx)).small())
                    })
                    .child(kit::title(self.t("connections.page.title"), TypeScale::LG, &p))
                    .child(
                        div()
                            .px(px(Space::SM))
                            .py(px(1.))
                            .rounded(px(Radius::TAG))
                            .bg(p.hover)
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text_muted)
                            .child(SharedString::from(count)),
                    )
                    .when(online > 0, |this| {
                        let online_text = crate::i18n::fill(&self.model.text("conn.page.onlineCount"), &[("count", &online.to_string())]);
                        this.child(
                            div()
                                .px(px(Space::SM))
                                .py(px(1.))
                                .rounded(px(Radius::TAG))
                                .bg(p.ok_soft)
                                .flex()
                                .items_center()
                                .gap(px(Space::XS))
                                .text_size(px(TypeScale::XS))
                                .text_color(p.ok)
                                .child(kit::status_dot(p.ok))
                                .child(SharedString::from(online_text)),
                        )
                    })
                    .child(div().flex_1())
                    .children(secondary)
                    .child(self.launcher_action("conn-new", IconName::Plus, self.t("common.newConnection"), Some(&focus), "conn-new-launcher", true, &p, cx)),
            )
            .when(total > 0, |this| {
                let search_box = if compact_search {
                    div().w(px(280.)).min_w(px(200.))
                } else {
                    div().flex_1().min_w(px(180.))
                }
                .child(filter.prefix(kit::input_prefix(IconName::Search, 13., p.text_faint)).cleanable(true));
                this.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(Space::MD))
                        .child(search_box)
                        .child(self.filter_button("conn-filter-engine", IconName::Database, SharedString::from(engine), engine_open, "conn-filter-engine", &p, cx))
                        .when(filtering, |this| {
                            this.child(kit::tool_button("conn-filter-clear", Some(IconName::X), self.t("connections.filter.clear"), act("conn-filter-clear", cx)).small())
                        })
                        .child(self.tag_chips(&p, cx))
                        .child(div().flex_1())
                        .when(self.model.viewport_width >= 680, |this| this.child(layout_toggle)),
                )
            })
            .into_any_element()
    }

    /// 标签条：全部 + 每个标签（带数量）+ 无标签。点一下按它筛选，再点回到全部。
    fn tag_chips(&mut self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let current = self.model.connections.filter_env.clone();
        let rows = &self.model.connections.rows;
        let count = |env: &str| rows.iter().filter(|row| super::chrome::env_key(&row.saved.env) == env).count();
        let mut chips: Vec<(String, SharedString, Option<String>, usize)> = vec![(String::new(), self.t("conn.page.allTags"), None, rows.len())];
        for tag in &self.model.env_tags {
            chips.push((tag.id.clone(), SharedString::from(tag.label.clone()), Some(tag.tone.clone()), count(&tag.id)));
        }
        let untagged = count("none");
        if untagged > 0 {
            chips.push(("none".into(), self.t("conn.page.noTag"), None, untagged));
        }
        let mut bar = div().id("conn-tag-bar").flex().flex_wrap().items_center().gap(px(Space::SM));
        for (id, label, tone, n) in chips {
            let on = current == id;
            let command = if on || id.is_empty() { "conn-filter-env:".to_owned() } else { format!("conn-filter-env:{id}") };
            let dot = tone.as_deref().map(|tone| kit::env_colors(tone, p).0);
            let aria = SharedString::from(format!("{label} {n}"));
            let chip_id = SharedString::from(format!("conn-tag-{}", if id.is_empty() { "all" } else { &id }));
            let muted_empty = n == 0 && !on && !id.is_empty();
            let chip = kit::filter_chip(chip_id, label, Some(aria), dot, Some(n), on, p, act(command, cx));
            bar = bar.child(div().when(muted_empty, |this| this.opacity(0.62)).child(chip));
        }
        bar.child(kit::tool_button("conn-tag-manage", Some(IconName::Settings2), self.t("conn.page.manageTags"), act("tags-manage", cx)).xsmall()).into_any_element()
    }

    /// 统一的“导入 / 导出”下拉入口：收拢 RillSQL JSON、DBeaver、Navicat 与导出，避免顶栏按钮杂乱堆叠。
    fn transfer_dropdown(&self, id: &'static str, show_label: bool, include_export: bool, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let import_rill = self.model.text("conn.page.importRill");
        let import_dbeaver = self.model.text("connections.profile.entry");
        let import_navicat = self.model.text("connections.profile.navicat");
        let export_rill = self.model.text("conn.page.exportRill");
        let label = self.t("conn.page.transfer");
        let entity = cx.entity().downgrade();
        Button::new(id)
            .ghost()
            .small()
            .tooltip(label.clone())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(Space::XS))
                    .child(kit::icon(IconName::Download, 13., p.text_muted))
                    .when(show_label, |this| this.child(label))
                    .child(kit::icon(IconName::ChevronDown, 12., p.text_faint)),
            )
            .dropdown_menu(move |mut menu, _, _| {
                const TRANSFER_MENU_MIN_W: f32 = 220.;
                menu = menu.min_w(px(TRANSFER_MENU_MIN_W));
                for (icon, text, cmd) in [
                    (IconName::Download, import_rill.clone(), "conn-import"),
                    (IconName::Database, import_dbeaver.clone(), "conn-import-dbeaver"),
                    (IconName::FileCode, import_navicat.clone(), "conn-import-navicat"),
                ] {
                    let ent = entity.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| {
                            kit::popup_menu_row(Some(icon), text.clone(), None, false, &Palette::of(cx))
                        })
                        .on_click(move |_, _, cx| {
                            if let Some(ent) = ent.upgrade() {
                                ent.update(cx, |this, cx| this.perform(cmd, cx));
                            }
                        }),
                    );
                }
                if include_export {
                    let ent = entity.clone();
                    let exp = export_rill.clone();
                    menu = menu.separator().item(
                        PopupMenuItem::element(move |_, cx| {
                            kit::popup_menu_row(Some(IconName::Upload), exp.clone(), None, false, &Palette::of(cx))
                        })
                        .on_click(move |_, _, cx| {
                            if let Some(ent) = ent.upgrade() {
                                ent.update(cx, |this, cx| this.perform("conn-export", cx));
                            }
                        }),
                    );
                }
                menu
            })
            .into_any_element()
    }

    /// 列表/看板：失败重试条 → 最近 → 分组 / 未分组。只有这一块滚动。
    pub(super) fn launcher_body(&mut self, wide: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let all: Vec<ConnectionRecord> = self.model.connections.launcher_all().into_iter().cloned().collect();
        let recent: Vec<ConnectionRecord> = self.model.connections.launcher_recent(5).into_iter().cloned().collect();
        let filtering = !self.model.connections.filter_text.trim().is_empty()
            || self.model.connections.filter_engine.is_some()
            || !self.model.connections.filter_env.is_empty();
        let cards = self.model.conn_board_cards && self.model.viewport_width >= 720;
        let mut body = div()
            .id("launcher-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .scroll_area_y()
            .scroll_handle(&self.launcher_scroll)
            .px(px(if cards { m.page_pad } else { m.page_pad - 6. }))
            .py(px(if cards { 12. } else { 8. }))
            .gap(px(if cards { 8. } else { 1. }));
        let mut child_count = 0usize;
        let mut selected_child: Option<usize> = None;
        if self.model.connections.rows.is_empty() {
            self.model.launcher_reveal = false;
            return body
                .child(kit::empty_state_with_action(
                    IconName::Database,
                    self.t("ui.explorer.empty"),
                    self.t("ui.explorer.emptyHint"),
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(Space::SM))
                        .child(kit::primary_button("launcher-empty-new", Some(IconName::Plus), self.t("common.newConnection"), act("conn-new-launcher", cx)))
                        .child(self.transfer_dropdown("launcher-empty-import", true, false, &p, cx)),
                    &p,
                ))
                .into_any_element();
        }
        if let Some(retry) = self.model.connect_retry.clone() {
            let name = self
                .model
                .connections
                .rows
                .iter()
                .find(|row| row.saved.id == retry.connection_id)
                .map(|row| row.saved.name.clone());
            let exists = name.is_some();
            let name = name.unwrap_or_else(|| self.model.text("connections.retry.missing"));
            body = body.child(
                div()
                    .id("launcher-connect-retry")
                    .test_support()
                    .mx(px(if cards { 0. } else { 6. }))
                    .mb(px(6.))
                    .px(px(10.))
                    .py(px(7.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(Radius::PANEL))
                    .bg(p.danger_soft)
                    .child(kit::icon(IconName::CircleAlert, 13., p.danger))
                    .child({
                        let reason = self.model.connections.failures.get(&retry.connection_id).cloned().unwrap_or_default();
                        let head = reason.lines().next().unwrap_or_default().to_owned();
                        let label = if head.is_empty() {
                            format!("{} · {name}", self.model.text("connections.status.error"))
                        } else {
                            format!("{} · {name}: {head}", self.model.text("connections.status.error"))
                        };
                        div()
                            .id("launcher-connect-retry-text")
                            .test_support()
                            .aria_label(SharedString::from(label.clone()))
                            .when(!reason.is_empty(), |this| this.tooltip_text(SharedString::from(reason.clone())))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(TypeScale::SM))
                            .text_color(p.danger)
                            .child(SharedString::from(label))
                    })
                    .when(exists, |this| this.child(kit::tool_button("launcher-connect-retry-action", Some(IconName::RefreshCw), self.t("common.retry"), act("conn-retry", cx))))
                    .child(kit::icon_button("launcher-connect-retry-dismiss", IconName::X, self.t("common.close"), act("conn-retry-dismiss", cx)).xsmall()),
            );
            child_count += 1;
        }
        if self.model.shell.page == crate::app::AppPage::Connections {
            if let Some(prompt) = self.group_prompt_panel(window, cx) {
                body = body.child(div().mb(px(6.)).child(prompt));
                child_count += 1;
            }
        }
        if all.is_empty() {
            self.model.launcher_reveal = false;
            return body
                .child(kit::no_match_state_with_action(
                    IconName::Search,
                    self.t("ui.explorer.noMatch"),
                    self.t("ui.conn.search"),
                    kit::secondary_button("launcher-clear-filter", Some(IconName::X), self.t("connections.filter.clear"), act("conn-filter-clear", cx)),
                    &p,
                ))
                .into_any_element();
        }
        if !recent.is_empty() && !filtering && all.len() > recent.len() + 2 {
            body = body.child(div().px(px(if cards { 0. } else { 6. })).child(super::chrome::section_label(self.t("connections.launcher.recent"), &p)));
            child_count += 1;
            if cards {
                body = body.child(self.launcher_card_grid(&recent, &p, cx, "recent"));
                child_count += 1;
            } else {
                for row in &recent {
                    body = body.child(self.launcher_row(row, wide, &p, cx, "recent"));
                    child_count += 1;
                }
            }
        }
        let layout = self.model.group_layout();
        let shown = |id: &str| all.iter().find(|row| row.saved.id == id).cloned();
        if layout.groups.is_empty() {
            if !recent.is_empty() && !filtering && all.len() > recent.len() + 2 {
                body = body.child(div().px(px(if cards { 0. } else { 6. })).child(super::chrome::section_label(self.t("connections.launcher.all"), &p)));
                child_count += 1;
            }
            if cards {
                if all.iter().any(|row| self.model.connections.selected.as_deref() == Some(row.saved.id.as_str())) {
                    selected_child = Some(child_count);
                }
                body = body.child(self.launcher_card_grid(&all, &p, cx, "all"));
            } else {
                for row in &all {
                    if self.model.connections.selected.as_deref() == Some(row.saved.id.as_str()) {
                        selected_child = Some(child_count);
                    }
                    body = body.child(self.launcher_row(row, wide, &p, cx, "all"));
                    child_count += 1;
                }
            }
            if std::mem::take(&mut self.model.launcher_reveal) {
                if let Some(child_ix) = selected_child {
                    self.launcher_scroll.scroll_to_item(child_ix);
                }
            }
            return body.into_any_element();
        }
        for (group, members) in &layout.groups {
            let rows: Vec<ConnectionRecord> = members.iter().filter_map(|id| shown(id)).collect();
            if filtering && rows.is_empty() {
                continue;
            }
            let expanded = filtering || !group.collapsed;
            body = body.child(self.launcher_group_row(&group.id, &group.name, rows.len(), expanded, &p, cx));
            child_count += 1;
            if !expanded {
                continue;
            }
            if rows.is_empty() {
                let empty_group_id = group.id.clone();
                let drop_group_id = group.id.clone();
                let drop_soft = p.accent_soft;
                let drop_border = p.accent;
                let entity = cx.entity().downgrade();
                body = body.child(
                    div()
                        .id(SharedString::from(format!("launcher-group-empty-{}", group.id)))
                        .test_support()
                        .ml(px(Space::XL))
                        .px(px(Space::LG))
                        .py(px(Space::SM))
                        .max_w(px(520.))
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(Space::MD))
                        .rounded(px(Radius::CONTROL))
                        .border_1()
                        .border_color(p.line_soft)
                        .bg(p.sunken.opacity(0.45))
                        .drag_over::<super::explorer::ConnectionDrag>(move |style, _, _, _| style.bg(drop_soft).border_color(drop_border))
                        .on_drop::<super::explorer::ConnectionDrag>(move |drag, _, cx| {
                            let connection = drag.connection.clone();
                            let gid = drop_group_id.clone();
                            if let Some(entity) = entity.upgrade() {
                                entity.update(cx, |this, cx| {
                                    this.model.assign_connection_group(&connection, Some(&gid));
                                    cx.notify();
                                });
                            }
                        })
                        .child(div().flex_1().min_w(px(180.)).text_size(px(TypeScale::XS)).text_color(p.text_muted).child(self.t("conn.page.emptyGroupDrop")))
                        .child(
                            kit::tool_button(
                                SharedString::from(format!("launcher-group-empty-add-{}", group.id)),
                                Some(IconName::Plus),
                                self.t("conn.page.newInGroup"),
                                act(format!("conn-new-in-group:{empty_group_id}"), cx),
                            )
                            .xsmall(),
                        ),
                );
                child_count += 1;
            } else if cards {
                if rows.iter().any(|row| self.model.connections.selected.as_deref() == Some(row.saved.id.as_str())) {
                    selected_child = Some(child_count);
                }
                body = body.child(div().pl(px(8.)).child(self.launcher_card_grid(&rows, &p, cx, "all")));
                child_count += 1;
            } else {
                for row in &rows {
                    if self.model.connections.selected.as_deref() == Some(row.saved.id.as_str()) {
                        selected_child = Some(child_count);
                    }
                    body = body.child(div().pl(px(16.)).child(self.launcher_row(row, wide, &p, cx, "all")));
                    child_count += 1;
                }
            }
        }
        let ungrouped: Vec<ConnectionRecord> = layout.ungrouped.iter().filter_map(|id| shown(id)).collect();
        if !ungrouped.is_empty() {
            let drop_soft = p.accent_soft;
            let entity = cx.entity().downgrade();
            body = body.child(
                div()
                    .id("launcher-ungrouped-drop")
                    .test_support()
                    .mt(px(4.))
                    .px(px(if cards { 4. } else { 6. }))
                    .py(px(2.))
                    .rounded(px(Radius::CONTROL))
                    .drag_over::<super::explorer::ConnectionDrag>(move |style, _, _, _| style.bg(drop_soft))
                    .on_drop::<super::explorer::ConnectionDrag>(move |drag, _, cx| {
                        let connection = drag.connection.clone();
                        if let Some(entity) = entity.upgrade() {
                            entity.update(cx, |this, cx| {
                                this.model.assign_connection_group(&connection, None);
                                cx.notify();
                            });
                        }
                    })
                    .child(super::chrome::section_label(self.t("connections.group.ungrouped"), &p)),
            );
            child_count += 1;
            if cards {
                if ungrouped.iter().any(|row| self.model.connections.selected.as_deref() == Some(row.saved.id.as_str())) {
                    selected_child = Some(child_count);
                }
                body = body.child(self.launcher_card_grid(&ungrouped, &p, cx, "all"));
            } else {
                for row in &ungrouped {
                    if self.model.connections.selected.as_deref() == Some(row.saved.id.as_str()) {
                        selected_child = Some(child_count);
                    }
                    body = body.child(self.launcher_row(row, wide, &p, cx, "all"));
                    child_count += 1;
                }
            }
        }
        if std::mem::take(&mut self.model.launcher_reveal) {
            if let Some(child_ix) = selected_child {
                self.launcher_scroll.scroll_to_item(child_ix);
            }
        }
        body.into_any_element()
    }

    /// 响应式连接卡片网格。
    fn launcher_card_grid(&mut self, rows: &[ConnectionRecord], p: &Palette, cx: &mut Context<Self>, prefix: &'static str) -> AnyElement {
        let mut grid = div().flex().flex_wrap().gap(px(10.));
        for row in rows {
            grid = grid.child(self.launcher_card(row, p, cx, prefix));
        }
        grid.into_any_element()
    }

    /// 连接看板卡片：引擎徽标、环境、在线状态、等宽地址、SSH/TLS 标识与一键直达操作。
    fn launcher_card(&mut self, row: &ConnectionRecord, p: &Palette, cx: &mut Context<Self>, prefix: &'static str) -> AnyElement {
        let id = row.saved.id.clone();
        let status = self.model.connections.row_status(&id);
        let (dot, state_key) = status_parts(status, p);
        let detail = connection_detail(row);
        let shown_detail = match row.saved.file_path.as_deref() {
            Some(path) if !path.is_empty() => short_path(path),
            _ => detail.clone(),
        };
        let selected = self.model.connections.selected.as_deref() == Some(id.as_str());
        let label = format!("{} {} {}", row.saved.name, row.saved.engine.as_str(), self.model.text(state_key));
        let aware = format!("{detail} · {}", self.model.text(state_key));
        let row_id = if prefix == "all" { format!("conn-row-{id}") } else { format!("launcher-{prefix}-{id}") };
        let group = SharedString::from(format!("conn-card-group-{prefix}-{id}"));
        let (select_id, menu_id, open_id, query_id, edit_id, more_id) =
            (id.clone(), id.clone(), id.clone(), id.clone(), id.clone(), id.clone());
        let connected = status == LinkStatus::Connected;
        let open_label = if connected { "conn.page.open" } else { "connections.page.connect" };
        let has_ssh = row.saved.ssh.is_some();
        let has_tls = row.saved.tls.as_ref().is_some_and(|tls| tls.mode != TlsMode::Off);
        let failed_reason = self.model.connections.failures.get(&id).and_then(|s| s.lines().next()).map(str::to_owned);

        let status_badge: AnyElement = if status == LinkStatus::Disconnected {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(px(TypeScale::XS))
                .text_color(p.text_faint)
                .child(kit::status_dot(dot))
                .child(self.model.text(state_key))
                .into_any_element()
        } else {
            div()
                .flex_none()
                .px(px(6.))
                .py(px(2.))
                .rounded(px(Radius::TAG))
                .bg(match status {
                    LinkStatus::Connected => p.ok_soft,
                    LinkStatus::Failed => p.danger_soft,
                    LinkStatus::Connecting => p.accent_soft,
                    LinkStatus::Disconnected => p.sunken,
                })
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(px(TypeScale::XS))
                .text_color(match status {
                    LinkStatus::Connected => p.ok,
                    LinkStatus::Failed => p.danger,
                    LinkStatus::Connecting => p.accent_text,
                    LinkStatus::Disconnected => p.text_muted,
                })
                .child(kit::status_dot(dot))
                .child(self.model.text(state_key))
                .into_any_element()
        };

        let is_mongo = crate::adapters::adapter(row.saved.engine).meta().workbench() == crate::adapters::Workbench::Mongo;
        let secondary_btn = if is_mongo {
            let browse_id = query_id.clone();
            kit::tool_button(
                SharedString::from(format!("launcher-query-{prefix}-{id}")),
                Some(IconName::Braces),
                self.model.text("conn.page.openBrowserMongo"),
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.perform(&format!("open-mongo:{browse_id}"), cx);
                }),
            )
            .xsmall()
        } else {
            kit::tool_button(
                SharedString::from(format!("launcher-query-{prefix}-{id}")),
                Some(IconName::SquareTerminal),
                self.model.text("conn.page.newQuery"),
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.perform(&format!("connection-query:{query_id}"), cx);
                }),
            )
            .xsmall()
        };

        let actions = div()
            .pt(px(Space::MD))
            .border_t_1()
            .border_color(p.line_soft)
            .flex()
            .items_center()
            .gap(px(Space::XS))
            .child(
                kit::tool_button(
                    SharedString::from(format!("launcher-open-{prefix}-{id}")),
                    Some(if connected { IconName::SquareArrowOutUpRight } else { IconName::Plug }),
                    self.model.text(open_label),
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.perform(&format!("connection:{open_id}"), cx);
                    }),
                )
                .xsmall(),
            )
            .child(secondary_btn)
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(Space::XXS))
                    .when(!selected, |this| this.opacity(0.).group_hover(group.clone(), |style| style.opacity(1.)))
                    .child(
                        kit::icon_button(
                            SharedString::from(format!("launcher-edit-{prefix}-{id}")),
                            IconName::Pencil,
                            self.model.text("conn.page.edit"),
                            cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.perform(&format!("connection-edit:{edit_id}"), cx);
                            }),
                        )
                        .xsmall(),
                    )
                    .child(
                        kit::icon_button(
                            SharedString::from(format!("launcher-more-{prefix}-{id}")),
                            IconName::Ellipsis,
                            self.model.text("conn.page.more"),
                            cx.listener(move |this, event: &ClickEvent, _, cx| {
                                this.menu_at = event.position();
                                this.model.connections.selected = Some(more_id.clone());
                                this.perform("menu-connection", cx);
                            }),
                        )
                        .xsmall(),
                    ),
            );

        kit::row_states(div().id(SharedString::from(row_id)), p)
            .test_support()
            .group(group)
            .role(gpui_kit::Role::ListItem)
            .aria_label(SharedString::from(label))
            .aria_selected(selected)
            .aria_description(SharedString::from(aware.clone()))
            .when(self.model.menu.is_none(), |this| this.tooltip_text(SharedString::from(aware)))
            .flex_1()
            .min_w(px(250.))
            .max_w(px(380.))
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(px(Radius::PANEL))
            .border_1()
            .when(selected, |this| this.border_color(p.accent.opacity(0.6)).bg(if p.dark { p.raised } else { p.surface }).shadow_sm())
            .when(!selected, |this| {
                this.border_color(p.line_soft)
                    .bg(if p.dark { p.raised } else { p.chrome })
                    .hover(|style| style.border_color(p.line).bg(p.hover))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .child(kit::engine_badge(row.saved.engine.as_str(), 30.))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_shrink(1.)
                                    .truncate()
                                    .text_size(px(TypeScale::MD))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(SharedString::from(row.saved.name.clone())),
                            )
                            .children(self.env_pill(&row.saved.env, p)),
                    )
                    .child(status_badge),
            )
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text_muted)
                            .child(SharedString::from(shown_detail)),
                    )
                    .when(has_ssh, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .px(px(5.))
                                .rounded(px(Radius::TAG))
                                .bg(p.sunken)
                                .text_size(px(TypeScale::XS))
                                .text_color(p.text_muted)
                                .child("SSH"),
                        )
                    })
                    .when(has_tls, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .px(px(5.))
                                .rounded(px(Radius::TAG))
                                .bg(p.sunken)
                                .text_size(px(TypeScale::XS))
                                .text_color(p.text_muted)
                                .child("TLS"),
                        )
                    }),
            )
            .when_some(failed_reason, |this, err| {
                this.child(
                    div()
                        .px(px(6.))
                        .py(px(3.))
                        .rounded(px(Radius::CONTROL))
                        .bg(p.danger_soft)
                        .truncate()
                        .text_size(px(TypeScale::XS))
                        .text_color(p.danger)
                        .child(SharedString::from(err)),
                )
            })
            .child(actions)
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.menu_at = event.position;
                    this.model.connections.selected = Some(menu_id.clone());
                    this.perform("menu-connection", cx);
                }),
            )
            .on_drag(
                super::explorer::ConnectionDrag { connection: id.clone() },
                |_, _, _, cx| cx.new(|_| super::chrome::ConnectionDragGhost),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.model.connections.selected = Some(select_id.clone());
                if event.click_count() >= 2 {
                    this.perform(&format!("connection:{select_id}"), cx);
                } else {
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    /// 一行：徽标 / 名称与标签 / 地址 / 状态；悬停或选中时露出“打开”和“更多”。双击打开。
    fn launcher_row(&mut self, row: &ConnectionRecord, wide: bool, p: &Palette, cx: &mut Context<Self>, prefix: &'static str) -> AnyElement {
        let m = self.metrics();
        let id = row.saved.id.clone();
        let status = self.model.connections.row_status(&id);
        let (dot, state_key) = status_parts(status, p);
        let detail = connection_detail(row);
        let shown_detail = match row.saved.file_path.as_deref() {
            Some(path) if !path.is_empty() => short_path(path),
            _ => detail.clone(),
        };
        let selected = self.model.connections.selected.as_deref() == Some(id.as_str());
        let label = format!("{} {} {}", row.saved.name, row.saved.engine.as_str(), self.model.text(state_key));
        let aware = format!("{detail} · {}", self.model.text(state_key));
        let row_id = if prefix == "all" { format!("conn-row-{id}") } else { format!("launcher-{prefix}-{id}") };
        let group = SharedString::from(format!("conn-row-group-{prefix}-{id}"));
        let (select_id, menu_id, open_id, more_id) = (id.clone(), id.clone(), id.clone(), id.clone());
        let connected = status == LinkStatus::Connected;
        let open_label = if connected { "conn.page.open" } else { "connections.page.connect" };
        let actions = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.))
            .when(!selected || wide, |this| this.opacity(0.).group_hover(group.clone(), |style| style.opacity(1.)))
            .child(
                kit::tool_button(SharedString::from(format!("launcher-open-{id}")), Some(if connected { IconName::SquareArrowOutUpRight } else { IconName::Plug }), self.model.text(open_label), cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.perform(&format!("connection:{open_id}"), cx);
                }))
                .xsmall(),
            )
            .child(
                kit::icon_button(SharedString::from(format!("launcher-more-{id}")), IconName::Ellipsis, self.model.text("conn.page.more"), cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.menu_at = event.position();
                    this.model.connections.selected = Some(more_id.clone());
                    this.perform("menu-connection", cx);
                }))
                .xsmall(),
            );
        kit::row_states(div().id(SharedString::from(row_id)), p)
            .test_support()
            .group(group)
            .role(gpui_kit::Role::ListItem)
            .aria_label(SharedString::from(label))
            .aria_selected(selected)
            .aria_description(SharedString::from(aware.clone()))
            .when(self.model.menu.is_none(), |this| this.tooltip_text(SharedString::from(aware)))
            // 两行：名称与环境在上，地址在下。名字长、地址长时各自截断，互不挤占。
            .h(px(m.row_h * 2. + 4.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded(px(Radius::CONTROL))
            .when(selected, |this| this.bg(p.selection))
            .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
            .child(kit::engine_badge(row.saved.engine.as_str(), 28.))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().min_w_0().flex_shrink(1.).truncate().font_weight(FontWeight::MEDIUM).child(SharedString::from(row.saved.name.clone())))
                            .children(self.env_pill(&row.saved.env, p)),
                    )
                    .child(div().min_w_0().truncate().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(shown_detail))),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .child(kit::status_dot(dot))
                    .when(status != LinkStatus::Disconnected, |this| this.child(div().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(self.model.text(state_key)))),
            )
            .child(actions)
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.menu_at = event.position;
                this.model.connections.selected = Some(menu_id.clone());
                this.perform("menu-connection", cx);
            }))
            .on_drag(
                super::explorer::ConnectionDrag { connection: id.clone() },
                |_, _, _, cx| cx.new(|_| super::chrome::ConnectionDragGhost),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.model.connections.selected = Some(select_id.clone());
                if event.click_count() >= 2 {
                    this.perform(&format!("connection:{select_id}"), cx);
                } else {
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    /// 详情栏：所选连接的身份、状态（含失败原因与一键重试）、常用操作、分组切换与配置摘要。
    fn connection_detail_panel(&mut self, p: &Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let detail_w = if self.model.viewport_width >= 1080 { 340. } else { DETAIL_W };
        let mut panel = div().id("conn-detail").test_support().flex_none().w(px(detail_w)).h_full().border_l_1().border_color(p.line_soft).bg(p.chrome).scroll_area_y();
        let Some(row) = self.model.connections.selected.as_ref().and_then(|id| self.model.connections.rows.iter().find(|row| &row.saved.id == id)).cloned() else {
            return panel.child(div().p(px(20.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child(self.t("conn.page.pickHint"))).into_any_element();
        };
        let id = row.saved.id.clone();
        let saved = &row.saved;
        let status = self.model.connections.row_status(&id);
        let (dot, state_key) = status_parts(status, p);
        let connected = status == LinkStatus::Connected;
        let failed = status == LinkStatus::Failed;
        let failure_text = failed.then(|| self.model.connections.failures.get(&id).cloned()).flatten().filter(|s| !s.trim().is_empty());
        let open_id = id.clone();
        let primary = if connected {
            kit::primary_button("conn-detail-open", Some(IconName::SquareArrowOutUpRight), self.t("conn.page.open"), cx.listener(move |this, _: &ClickEvent, _, cx| this.perform(&format!("connection:{open_id}"), cx)))
        } else {
            kit::primary_button("conn-detail-open", Some(IconName::Plug), self.t("connections.page.connect"), cx.listener(move |this, _: &ClickEvent, _, cx| this.perform(&format!("connection:{open_id}"), cx)))
        };
        let secondary_action = if crate::adapters::adapter(saved.engine).meta().workbench() == crate::adapters::Workbench::Mongo {
            let mongo_id = id.clone();
            kit::secondary_button(
                "conn-detail-query",
                Some(IconName::Braces),
                self.t("conn.page.openBrowserMongo"),
                cx.listener(move |this, _: &ClickEvent, _, cx| this.perform(&format!("open-mongo:{mongo_id}"), cx)),
            )
            .small()
        } else {
            kit::secondary_button("conn-detail-query", Some(IconName::SquareTerminal), self.t("conn.page.newQuery"), act("conn-new-query", cx)).small()
        };
        let mut actions = div()
            .flex()
            .flex_wrap()
            .gap(px(6.))
            .child(primary.small())
            .child(secondary_action);
        if connected {
            actions = actions.child(kit::secondary_button("conn-detail-disconnect", Some(IconName::Unplug), self.t("conn.page.disconnect"), act("conn-disconnect", cx)).small());
        }
        let tools = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(2.))
            .child(kit::tool_button("conn-detail-edit", Some(IconName::Pencil), self.t("conn.page.edit"), act("conn-edit", cx)).xsmall())
            .child(kit::tool_button("conn-detail-test", Some(IconName::Zap), self.t("conn.page.test"), act("conn-test", cx)).xsmall())
            .child(kit::tool_button("conn-detail-duplicate", Some(IconName::Copy), self.t("conn.page.duplicate"), act("conn-duplicate", cx)).xsmall())
            .child(div().flex_1())
            .child(kit::icon_button("conn-detail-delete", IconName::Trash, self.t("conn.page.delete"), act("conn-delete", cx)).xsmall());

        let failure_callout = failure_text.map(|reason| {
            let retry_id = id.clone();
            div()
                .id("conn-detail-failure")
                .test_support()
                .p(px(9.))
                .rounded(px(Radius::CONTROL))
                .bg(p.danger_soft)
                .border_1()
                .border_color(p.danger.opacity(0.28))
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(6.))
                        .child(div().pt(px(1.)).flex_none().child(kit::icon(IconName::CircleAlert, 13., p.danger)))
                        .child(
                            div()
                                .id("conn-detail-failure-scroll")
                                .flex_1()
                                .min_w_0()
                                .max_h(px(88.))
                                .scroll_area_y()
                                .text_size(px(TypeScale::XS))
                                .text_color(p.danger)
                                .child(SharedString::from(reason)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(4.))
                        .child(
                            kit::tool_button(
                                "conn-detail-retry",
                                Some(IconName::RefreshCw),
                                self.t("common.retry"),
                                cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.perform(&format!("connection:{retry_id}"), cx);
                                }),
                            )
                            .xsmall(),
                        )
                        .child(kit::tool_button("conn-detail-failure-edit", Some(IconName::Pencil), self.t("conn.page.edit"), act("conn-edit", cx)).xsmall()),
                )
        });

        let none = self.model.text("conn.page.none");
        let mut facts: Vec<(&str, String)> = vec![("conn.page.engine", kit::engine_label(saved.engine.as_str()).to_owned())];
        match saved.file_path.as_deref() {
            Some(path) if !path.is_empty() => facts.push(("conn.page.file", path.to_owned())),
            _ => {
                let host = saved.host.clone().unwrap_or_default();
                let address = match saved.port {
                    Some(port) if !host.is_empty() => format!("{host}:{port}"),
                    _ => host,
                };
                facts.push(("conn.page.address", if address.is_empty() { none.clone() } else { address }));
                facts.push(("conn.page.database", saved.database.clone().filter(|value| !value.is_empty()).unwrap_or_else(|| none.clone())));
                facts.push(("conn.page.user", saved.username.clone().filter(|value| !value.is_empty()).unwrap_or_else(|| none.clone())));
                // 密码存在哪里：没存进钥匙串的连接每次运行第一次连接时会弹框要密码，这里先说清楚。
                facts.push(("conn.page.password", self.model.text(if saved.password_saved { "conn.page.passwordSaved" } else { "conn.page.passwordAsk" })));
                let tls = match saved.tls.as_ref().map(|tls| tls.mode) {
                    None | Some(TlsMode::Off) => "conn.page.tlsOff",
                    Some(TlsMode::Require) => "conn.page.tlsRequire",
                    Some(TlsMode::Verify) => "conn.page.tlsVerify",
                };
                facts.push(("conn.page.tls", self.model.text(tls)));
                facts.push(("conn.page.ssh", saved.ssh.as_ref().map(|ssh| format!("{}@{}:{}", ssh.username, ssh.host, ssh.port)).unwrap_or_else(|| none.clone())));
            }
        }
        let mut table = div().flex().flex_col().gap(px(8.));
        for (key, value) in facts {
            let has_value = value != none;
            // 地址、文件、库名、用户名、SSH 都是会被贴到别处的值：点一下就复制，技术值使用等宽字体。
            let copyable = has_value && matches!(key, "conn.page.database" | "conn.page.user" | "conn.page.ssh");
            let address = has_value && matches!(key, "conn.page.address" | "conn.page.file");
            let mono_fact = has_value && matches!(key, "conn.page.address" | "conn.page.file" | "conn.page.database" | "conn.page.user" | "conn.page.ssh");
            let value_cell: AnyElement = if copyable {
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .when(mono_fact, |this| this.font_family(cx.theme().mono_font_family.clone()).text_size(px(TypeScale::XS)))
                    .text_color(p.text)
                    .child(kit::copyable(
                        SharedString::from(format!("conn-fact-{key}")),
                        value.clone(),
                        SharedString::from(value.clone()),
                        crate::i18n::fill(&self.model.text("ui.copy.path"), &[("name", &value)]),
                        p,
                        window,
                        cx,
                    ))
                    .into_any_element()
            } else {
                div()
                    .id(SharedString::from(format!("conn-fact-{key}")))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(mono_fact, |this| this.font_family(cx.theme().mono_font_family.clone()).text_size(px(TypeScale::XS)))
                    .text_color(if has_value { p.text } else { p.text_faint })
                    .when(has_value, |this| this.tooltip_text(SharedString::from(value.clone())))
                    .child(SharedString::from(value))
                    .into_any_element()
            };
            table = table.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .text_size(px(TypeScale::SM))
                    .child(div().flex_none().w(px(72.)).text_color(p.text_muted).child(self.t(key)))
                    .child(value_cell)
                    .when(address, |this| {
                        this.child(
                            kit::icon_button(
                                "conn-detail-copy-addr",
                                IconName::Copy,
                                self.t("conn.page.copyAddress"),
                                act("conn-copy-address", cx),
                            )
                            .xsmall(),
                        )
                    }),
            );
        }

        let current_group_id = self.model.group_of(&id).map(str::to_owned);
        let group_name = current_group_id
            .as_deref()
            .and_then(|gid| self.model.groups.iter().find(|item| item.id == gid))
            .map(|g| g.name.clone())
            .unwrap_or_else(|| none.clone());
        let has_group = current_group_id.is_some();
        let groups_for_menu = self.model.groups.clone();
        let conn_for_group = id.clone();
        let move_out_label = self.model.text("ui.group.moveOut");
        let new_group_label = self.model.text("ui.group.new");
        let workbench_entity = cx.entity().downgrade();
        let group_action: AnyElement = if groups_for_menu.is_empty() {
            kit::tool_button("conn-detail-group", Some(IconName::FolderPlus), self.t("ui.group.new"), act("group-new", cx))
                .xsmall()
                .into_any_element()
        } else {
            let current_for_menu = current_group_id.clone();
            Button::new("conn-detail-group")
                .ghost()
                .xsmall()
                .label(self.t("conn.page.changeGroup"))
                .dropdown_menu(move |mut menu, _, _| {
                    menu = menu.min_w(px(180.));
                    for grp in &groups_for_menu {
                        let gid = grp.id.clone();
                        let gname = grp.name.clone();
                        let selected = current_for_menu.as_deref() == Some(gid.as_str());
                        let cid = conn_for_group.clone();
                        let ent = workbench_entity.clone();
                        menu = menu.item(
                            PopupMenuItem::element(move |_, cx| {
                                kit::popup_menu_row(
                                    Some(if selected { IconName::Check } else { IconName::Folder }),
                                    gname.clone(),
                                    None,
                                    false,
                                    &Palette::of(cx),
                                )
                            })
                            .on_click(move |_, _, cx| {
                                if let Some(ent) = ent.upgrade() {
                                    ent.update(cx, |this, cx| {
                                        this.perform(&format!("group-assign:{cid}\u{1}{gid}"), cx);
                                    });
                                }
                            }),
                        );
                    }
                    if current_for_menu.is_some() {
                        let cid = conn_for_group.clone();
                        let out_text = move_out_label.clone();
                        let ent = workbench_entity.clone();
                        menu = menu.separator().item(
                            PopupMenuItem::element(move |_, cx| {
                                kit::popup_menu_row(Some(IconName::X), out_text.clone(), None, false, &Palette::of(cx))
                            })
                            .on_click(move |_, _, cx| {
                                if let Some(ent) = ent.upgrade() {
                                    ent.update(cx, |this, cx| {
                                        this.perform(&format!("group-assign:{cid}\u{1}"), cx);
                                    });
                                }
                            }),
                        );
                    }
                    let new_text = new_group_label.clone();
                    let ent = workbench_entity.clone();
                    menu = menu.separator().item(
                        PopupMenuItem::element(move |_, cx| {
                            kit::popup_menu_row(Some(IconName::FolderPlus), new_text.clone(), None, false, &Palette::of(cx))
                        })
                        .on_click(move |_, _, cx| {
                            if let Some(ent) = ent.upgrade() {
                                ent.update(cx, |this, cx| {
                                    this.perform("group-new", cx);
                                });
                            }
                        }),
                    );
                    menu
                })
                .into_any_element()
        };
        table = table.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .text_size(px(TypeScale::SM))
                .child(div().flex_none().w(px(72.)).text_color(p.text_muted).child(self.t("conn.page.group")))
                .child(
                    div()
                        .id("conn-fact-conn.page.group")
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(if has_group { p.text } else { p.text_faint })
                        .child(SharedString::from(group_name)),
                )
                .child(group_action),
        );

        let tag = self.env_pill(&saved.env, p);
        table = table.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .text_size(px(TypeScale::SM))
                .child(div().flex_none().w(px(72.)).text_color(p.text_muted).child(self.t("conn.page.tag")))
                .child(match tag {
                    Some(tag) => tag,
                    None => div().text_color(p.text_faint).child(none.clone()).into_any_element(),
                })
                .child(div().flex_1())
                .child(kit::tool_button("conn-detail-tags", None, self.t("conn.page.manageTags"), act("tags-manage", cx)).xsmall()),
        );

        panel = panel.child(
            div()
                .p(px(16.))
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(
                    kit::card(p)
                        .p(px(14.))
                        .flex()
                        .flex_col()
                        .gap(px(11.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(12.))
                                .child(kit::engine_badge(saved.engine.as_str(), 36.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap(px(4.))
                                        .child(div().flex().font_weight(FontWeight::SEMIBOLD).text_size(px(TypeScale::LG)).child(kit::copyable(
                                            "conn-detail-name",
                                            saved.name.clone(),
                                            SharedString::from(saved.name.clone()),
                                            self.model.text("ui.copy.name"),
                                            p,
                                            window,
                                            cx,
                                        )))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(6.))
                                                .text_size(px(TypeScale::XS))
                                                .text_color(if failed { p.danger } else { p.text_muted })
                                                .child(kit::status_dot(dot))
                                                .child(self.model.text(state_key)),
                                        ),
                                ),
                        )
                        .children(failure_callout)
                        .child(actions)
                        .child(div().pt(px(4.)).border_t_1().border_color(p.line_soft).child(tools)),
                )
                .child(kit::card(p).p(px(14.)).child(table))
                .when(!connected, |this| this.child(div().px(px(4.)).text_size(px(TypeScale::XS)).text_color(p.text_faint).child(self.t("conn.page.openHint")))),
        );
        panel.into_any_element()
    }

    /// 标签管理对话框：每个标签一行（色块预览、使用数、改、删）；新建/编辑就地展开。
    pub(super) fn tag_manager_layer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(manager) = self.model.tag_manager.clone() else {
            self.tag_label_focused = false;
            return None;
        };
        let p = Palette::of(cx);
        let editing = manager.editing.clone();
        let editor = editing.as_ref().map(|draft| {
            let (input, state) = self.field_input(DraftField::TagLabel, &self.model.text("tags.namePlaceholder"), false, window, cx);
            if !self.tag_label_focused {
                self.tag_label_focused = true;
                let handle = gpui_kit::Focusable::focus_handle(state.read(cx), cx);
                window.focus(&handle, cx);
            }
            let mut tones = div().flex().items_center().gap(px(6.)).child(div().text_size(px(TypeScale::XS)).text_color(p.text_muted).mr(px(4.)).child(self.t("tags.tone")));
            for tone in TONES {
                let (fg, _) = kit::env_colors(tone, &p);
                let on = draft.tone == tone;
                tones = tones.child(
                    kit::interactive_states(div().id(SharedString::from(format!("tag-tone-{tone}"))), &p, false)
                        .test_support()
                        .role(gpui_kit::Role::Button)
                        .aria_label(self.t(&format!("tags.tone.{tone}")))
                        .aria_selected(on)
                        .tooltip_text(self.t(&format!("tags.tone.{tone}")))
                        .size(px(22.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .border_2()
                        .border_color(if on { fg } else { gpui_kit::transparent_black() })
                        .hover(|style| style.bg(p.hover))
                        .child(div().size(px(14.)).rounded_full().bg(fg))
                        .on_click(act(format!("tag-tone:{tone}"), cx)),
                );
            }
            let preview_label = if draft.label.trim().is_empty() { self.model.text("tags.namePlaceholder") } else { draft.label.trim().to_owned() };
            div()
                .id("tag-editor")
                .p(px(10.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .rounded(px(Radius::PANEL))
                .border_1()
                .border_color(p.accent)
                .bg(p.surface)
                .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| match event.keystroke.key.as_str() {
                    "enter" => {
                        cx.stop_propagation();
                        this.perform("tag-save", cx);
                    }
                    "escape" => {
                        cx.stop_propagation();
                        this.perform("tag-cancel", cx);
                    }
                    _ => {}
                }))
                .child(div().flex().items_center().gap(px(8.)).child(div().flex_1().child(input)).child(kit::env_tag(&draft.tone, preview_label, &p)))
                .child(tones)
                .when_some(manager.error.clone(), |this, error| this.child(div().text_size(px(TypeScale::XS)).text_color(p.danger).child(SharedString::from(error))))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(6.))
                        .child(kit::secondary_button("tag-cancel", None, self.t("common.cancel"), act("tag-cancel", cx)).small())
                        .child(kit::primary_button("tag-save", None, self.t("common.save"), act("tag-save", cx)).small().loading(manager.busy)),
                )
                .into_any_element()
        });
        if editing.is_none() {
            self.tag_label_focused = false;
        }
        let editing_id = editing.as_ref().and_then(|draft| draft.id.clone());
        let mut editor = editor;
        let mut list = div().id("tag-list").flex().flex_col().gap(px(4.)).max_h(px(360.)).scroll_area_y();
        for tag in self.model.env_tags.clone() {
            if editing_id.as_deref() == Some(tag.id.as_str()) {
                if let Some(editor) = editor.take() {
                    list = list.child(editor);
                }
                continue;
            }
            let usage = self.model.tag_usage(&tag.id);
            let confirming = manager.confirm_delete.as_deref() == Some(tag.id.as_str());
            let expanded = usage > 0 && manager.expanded.as_deref() == Some(tag.id.as_str());
            let usage_text = if usage == 0 { self.model.text("tags.unused") } else { crate::i18n::fill(&self.model.text("tags.usage"), &[("count", &usage.to_string())]) };
            // 使用数可点：展开关联连接，逐条移出这个标签。未使用时只是文字。
            let usage_cell = if usage == 0 {
                div().flex_1().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(usage_text)).into_any_element()
            } else {
                div()
                    .flex_1()
                    .flex()
                    .child(
                        kit::interactive_states(div().id(SharedString::from(format!("tag-usage-{}", tag.id))), &p, false)
                            .test_support()
                            .role(gpui_kit::Role::Button)
                            .aria_label(SharedString::from(format!("{} {}", self.model.text("tags.showConnections"), usage_text)))
                            .aria_expanded(expanded)
                            .tooltip_text(self.t("tags.showConnections"))
                            .px(px(4.))
                            .rounded(px(Radius::CONTROL))
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .text_size(px(TypeScale::XS))
                            .text_color(p.accent_text)
                            .hover(|style| style.bg(p.hover))
                            .child(SharedString::from(usage_text))
                            .child(kit::icon(if expanded { IconName::ChevronDown } else { IconName::ChevronRight }, 12., p.text_faint))
                            .on_click(act(format!("tag-expand:{}", tag.id), cx)),
                    )
                    .into_any_element()
            };
            let mut row = kit::row_states(div().id(SharedString::from(format!("tag-row-{}", tag.id))), &p)
                .px(px(8.))
                .py(px(6.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .rounded(px(Radius::CONTROL))
                .hover(|style| style.bg(p.hover))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(kit::env_tag(&tag.tone, SharedString::from(tag.label.clone()), &p))
                        .child(usage_cell)
                        .child(kit::icon_button(SharedString::from(format!("tag-edit-{}", tag.id)), IconName::Pencil, self.model.text("tags.edit"), act(format!("tag-edit:{}", tag.id), cx)).xsmall())
                        .child(kit::icon_button(SharedString::from(format!("tag-delete-{}", tag.id)), IconName::Trash, self.model.text("tags.delete"), act(format!("tag-delete:{}", tag.id), cx)).xsmall()),
                );
            if expanded {
                let mut members = div().id(SharedString::from(format!("tag-members-{}", tag.id))).flex().flex_col().gap(px(2.)).pl(px(12.));
                for (connection, name) in self.model.tag_connections(&tag.id) {
                    members = members.child(
                        div()
                            .id(SharedString::from(format!("tag-member-{connection}")))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(div().flex_1().min_w_0().truncate().text_size(px(TypeScale::SM)).text_color(p.text).child(SharedString::from(name)))
                            .child(
                                kit::icon_button(SharedString::from(format!("tag-untag-{connection}")), IconName::X, self.model.text("tags.untag"), act(format!("tag-untag:{connection}"), cx))
                                    .xsmall(),
                            ),
                    );
                }
                row = row.child(members);
            }
            if confirming {
                let message = if usage == 0 { self.model.text("tags.deleteUnused") } else { crate::i18n::fill(&self.model.text("tags.deleteConfirm"), &[("count", &usage.to_string())]) };
                row = row.bg(p.danger_soft).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(div().flex_1().text_size(px(TypeScale::XS)).text_color(p.danger).child(SharedString::from(message)))
                        .child(kit::secondary_button("tag-delete-cancel", None, self.t("common.cancel"), act("tag-delete-cancel", cx)).xsmall())
                        .child(kit::danger_button(SharedString::from(format!("tag-delete-confirm-{}", tag.id)), None, self.t("tags.delete"), act(format!("tag-delete-confirm:{}", tag.id), cx)).xsmall()),
                );
            }
            list = list.child(row);
        }
        if self.model.env_tags.is_empty() && editor.is_none() {
            list = list.child(div().py(px(8.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child(self.t("tags.empty")));
        }
        let card = div()
            .id("tag-manager")
            .test_support()
            .role(gpui_kit::Role::Dialog)
            .aria_label(self.t("tags.title"))
            .occlude()
            .w(px(440.))
            .max_w_full()
            .p(px(18.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .bg(p.raised)
            .border_1()
            .border_color(p.line)
            .rounded(px(Radius::PANEL))
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(kit::title(self.t("tags.title"), TypeScale::LG, &p))
                    .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(self.t("tags.body"))),
            )
            .child(list)
            .children(editor)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .when(editing.is_none(), |this| this.child(kit::secondary_button("tag-new", Some(IconName::Plus), self.t("tags.new"), act("tag-new", cx)).small()))
                    .child(div().flex_1())
                    .child(kit::primary_button("tags-close", None, self.t("tags.done"), act("tags-close", cx)).small()),
            );
        Some(
            super::chrome::backdrop("tags-backdrop", 0.32, cx)
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.))
                .child(card)
                .into_any_element(),
        )
    }
}
