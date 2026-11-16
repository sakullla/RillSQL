//! 连接页：左边是可搜索、可按标签/引擎筛选、按分组排列的连接列表，右边是所选连接的详情与操作。
//! 窄窗口只留列表，操作跟在选中行上。标签由用户自己维护（新建、改名、换色、删除）。

use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, AnyElement, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
};

use crate::app::views::connections::LinkStatus;
use crate::connections::model::TlsMode;
use crate::connections::ConnectionRecord;
use crate::ui::kit::{self, Palette, Radius, Tip as _, TypeScale};

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
        let p = Palette::of(cx);
        let wide = f32::from(window.viewport_size().width) >= DETAIL_MIN_WIDTH;
        // 列表非空却没有选中：默认选第一条，详情栏不空着。
        if self.model.connections.selected.as_ref().is_none_or(|id| !self.model.connections.rows.iter().any(|row| &row.saved.id == id)) {
            self.model.connections.selected = self.model.connections.launcher_all().first().map(|row| row.saved.id.clone());
        }
        let detail = if wide && !self.model.connections.rows.is_empty() { Some(self.connection_detail_panel(&p, cx)) } else { None };
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
            .child(div().flex_1().min_h(px(0.)).flex().child(self.launcher_body(wide, window, cx)).children(detail))
            .into_any_element()
    }

    /// 头部两行：标题 + 主操作（新建连接）与次要入口（图标）；搜索、引擎筛选与标签条。
    pub(super) fn launcher_header(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let filter = self.input_for(DraftField::Filter, &self.model.text("ui.conn.search"), false, window, cx);
        let shown = self.model.connections.filtered().len();
        let total = self.model.connections.rows.len();
        let count = if shown == total { total.to_string() } else { format!("{shown} / {total}") };
        let engine = self.model.connections.filter_engine.map(|engine| kit::engine_label(engine.as_str()).to_owned()).unwrap_or_else(|| self.model.text("connections.filter.engine"));
        let engine_open = self.model.conn_filter_menu == Some(super::model::ConnFilterMenu::Engine);
        let filtering = self.model.connections.filter_engine.is_some() || !self.model.connections.filter_env.is_empty() || !self.model.connections.filter_text.trim().is_empty();
        let icon = |id: &'static str, icon: IconName, key: &str, command: &'static str, cx: &mut Context<Self>| {
            kit::icon_button(id, icon, self.model.text(key), act(command, cx)).small().into_any_element()
        };
        let secondary = [
            icon("launcher-group-new", IconName::FolderPlus, "ui.group.new", "group-new", cx),
            icon("launcher-tags", IconName::Tag, "conn.page.manageTags", "tags-manage", cx),
            icon("launcher-sqlite", IconName::FolderOpen, "common.openSqlite", "open-sqlite", cx),
            icon("launcher-import", IconName::Download, "ui.conn.import", "conn-import", cx),
            icon("launcher-export", IconName::Upload, "ui.conn.export", "conn-export", cx),
        ];
        let focus = self.conn_launcher_focus.clone();
        div()
            .flex_none()
            .px(px(m.page_pad))
            .pt(px(m.page_pad_y))
            .pb(px(10.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .border_b_1()
            .border_color(p.line)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(kit::title(self.t("connections.page.title"), TypeScale::LG, &p))
                    .child(div().px(px(6.)).rounded(px(Radius::TAG)).bg(p.hover).text_size(px(TypeScale::XS)).text_color(p.text_muted).child(SharedString::from(count)))
                    .child(div().flex_1())
                    .children(secondary)
                    .child(kit::vline(&p))
                    .child(self.launcher_action("conn-new", IconName::Plus, self.t("common.newConnection"), Some(&focus), "conn-new-launcher", true, &p, cx)),
            )
            .when(total > 0, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .child(div().flex_1().min_w(px(180.)).child(filter.prefix(kit::icon(IconName::Search, 13., p.text_faint)).cleanable(true)))
                        .child(self.filter_button("conn-filter-engine", IconName::Database, SharedString::from(engine), engine_open, "conn-filter-engine", &p, cx))
                        .when(filtering, |this| this.child(kit::tool_button("conn-filter-clear", Some(IconName::X), self.t("connections.filter.clear"), act("conn-filter-clear", cx)))),
                )
                .child(self.tag_chips(&p, cx))
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
        let mut bar = div().id("conn-tag-bar").flex().flex_wrap().items_center().gap(px(6.));
        for (id, label, tone, n) in chips {
            let on = current == id;
            let command = if on || id.is_empty() { "conn-filter-env:".to_owned() } else { format!("conn-filter-env:{id}") };
            let dot = tone.as_deref().map(|tone| kit::env_colors(tone, p).0);
            let aria = SharedString::from(format!("{label} {n}"));
            let chip_id = SharedString::from(format!("conn-tag-{}", if id.is_empty() { "all" } else { &id }));
            bar = bar.child(kit::filter_chip(chip_id, label, Some(aria), dot, Some(n), on, p, act(command, cx)));
        }
        bar.child(kit::tool_button("conn-tag-manage", Some(IconName::Settings2), self.t("conn.page.manageTags"), act("tags-manage", cx)).xsmall()).into_any_element()
    }

    /// 列表：失败重试条 → 最近 → 分组 / 未分组。只有这一块滚动。
    pub(super) fn launcher_body(&mut self, wide: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let all: Vec<ConnectionRecord> = self.model.connections.launcher_all().into_iter().cloned().collect();
        let recent: Vec<ConnectionRecord> = self.model.connections.launcher_recent(5).into_iter().cloned().collect();
        let filtering = !self.model.connections.filter_text.trim().is_empty() || self.model.connections.filter_engine.is_some() || !self.model.connections.filter_env.is_empty();
        let mut body = div()
            .id("launcher-scroll")
            .track_scroll(&self.launcher_scroll)
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .overflow_y_scroll()
            .px(px(m.page_pad - 6.))
            .py(px(8.))
            .gap(px(1.));
        let mut child_count = 0usize;
        let mut selected_child: Option<usize> = None;
        if self.model.connections.rows.is_empty() {
            self.model.launcher_reveal = false;
            return body
                .child(kit::empty_state_with_action(
                    IconName::Database,
                    self.t("ui.explorer.empty"),
                    self.t("ui.explorer.emptyHint"),
                    // 第一次打开时三条起步路径都给文字按钮：新建、打开本机 SQLite、导入已有的连接清单。
                    // 页头那排只有图标，第一次用的人认不出“导入”。
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(6.))
                        .child(kit::primary_button("launcher-empty-new", Some(IconName::Plus), self.t("common.newConnection"), act("conn-new-launcher", cx)))
                        .child(kit::secondary_button("launcher-empty-sqlite", Some(IconName::FolderOpen), self.t("common.openSqlite"), act("open-sqlite", cx)))
                        .child(kit::secondary_button("launcher-empty-import", Some(IconName::Download), self.t("ui.conn.importList"), act("conn-import", cx))),
                    &p,
                ))
                .into_any_element();
        }
        if let Some(retry) = self.model.connect_retry.clone() {
            let name = self.model.connections.rows.iter().find(|row| row.saved.id == retry.connection_id).map(|row| row.saved.name.clone()).unwrap_or_else(|| retry.connection_id.clone());
            body = body.child(
                div()
                    .id("launcher-connect-retry")
                    .test_support()
                    .mx(px(6.))
                    .mb(px(6.))
                    .px(px(8.))
                    .py(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(Radius::PANEL))
                    .bg(p.danger_soft)
                    .child(kit::icon(IconName::CircleAlert, 13., p.danger))
                    .child({
                        // 横幅写出这条连接最近一次失败的原因（首行），完整原因和处理建议在悬停提示里。
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
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(TypeScale::SM))
                            .text_color(p.danger)
                            .child(SharedString::from(label))
                    })
                    .child(kit::tool_button("launcher-connect-retry-action", Some(IconName::RefreshCw), self.t("common.retry"), act("conn-retry", cx))),
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
            body = body.child(div().px(px(6.)).child(super::chrome::section_label(self.t("connections.launcher.recent"), &p)));
            child_count += 1;
            for row in &recent {
                body = body.child(self.launcher_row(row, wide, &p, cx, "recent"));
                child_count += 1;
            }
        }
        let layout = self.model.group_layout();
        let shown = |id: &str| all.iter().find(|row| row.saved.id == id).cloned();
        if layout.groups.is_empty() {
            if !recent.is_empty() && !filtering && all.len() > recent.len() + 2 {
                body = body.child(div().px(px(6.)).child(super::chrome::section_label(self.t("connections.launcher.all"), &p)));
                child_count += 1;
            }
            for row in &all {
                if self.model.connections.selected.as_deref() == Some(row.saved.id.as_str()) {
                    selected_child = Some(child_count);
                }
                body = body.child(self.launcher_row(row, wide, &p, cx, "all"));
                child_count += 1;
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
                body = body.child(div().pl(px(32.)).py(px(4.)).text_size(px(TypeScale::XS)).text_color(p.text_faint).child(self.t("connections.group.emptyHint")));
                child_count += 1;
            }
            for row in &rows {
                if self.model.connections.selected.as_deref() == Some(row.saved.id.as_str()) {
                    selected_child = Some(child_count);
                }
                body = body.child(div().pl(px(16.)).child(self.launcher_row(row, wide, &p, cx, "all")));
                child_count += 1;
            }
        }
        let ungrouped: Vec<ConnectionRecord> = layout.ungrouped.iter().filter_map(|id| shown(id)).collect();
        if !ungrouped.is_empty() {
            body = body.child(div().px(px(6.)).child(super::chrome::section_label(self.t("connections.group.ungrouped"), &p)));
            child_count += 1;
            for row in &ungrouped {
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
        body.into_any_element()
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
            .h(px(m.row_h + 6.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(Radius::CONTROL))
            .when(selected, |this| this.bg(p.selection))
            .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
            .child(kit::engine_badge(row.saved.engine.as_str(), 20.))
            .child(
                div()
                    .min_w(px(0.))
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().min_w(px(0.)).flex_shrink(1.).truncate().font_weight(FontWeight::MEDIUM).child(SharedString::from(row.saved.name.clone())))
                    .children(self.env_pill(&row.saved.env, p))
                    .child(div().min_w(px(0.)).flex_1().truncate().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(shown_detail))),
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

    /// 详情栏：所选连接的身份、状态、常用操作与配置摘要。
    fn connection_detail_panel(&mut self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = div().id("conn-detail").test_support().flex_none().w(px(DETAIL_W)).h_full().border_l_1().border_color(p.line).bg(p.chrome).overflow_y_scroll();
        let Some(row) = self.model.connections.selected.as_ref().and_then(|id| self.model.connections.rows.iter().find(|row| &row.saved.id == id)).cloned() else {
            return panel.child(div().p(px(20.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child(self.t("conn.page.pickHint"))).into_any_element();
        };
        let id = row.saved.id.clone();
        let saved = &row.saved;
        let status = self.model.connections.row_status(&id);
        let (dot, state_key) = status_parts(status, p);
        let connected = status == LinkStatus::Connected;
        let open_id = id.clone();
        let primary = if connected {
            kit::primary_button("conn-detail-open", Some(IconName::SquareArrowOutUpRight), self.t("conn.page.open"), cx.listener(move |this, _: &ClickEvent, _, cx| this.perform(&format!("connection:{open_id}"), cx)))
        } else {
            kit::primary_button("conn-detail-open", Some(IconName::Plug), self.t("connections.page.connect"), cx.listener(move |this, _: &ClickEvent, _, cx| this.perform(&format!("connection:{open_id}"), cx)))
        };
        let mut actions = div().flex().flex_wrap().gap(px(6.)).child(primary.small());
        if connected {
            actions = actions
                .child(kit::secondary_button("conn-detail-query", Some(IconName::SquareTerminal), self.t("conn.page.newQuery"), act("conn-new-query", cx)).small())
                .child(kit::secondary_button("conn-detail-disconnect", Some(IconName::Unplug), self.t("conn.page.disconnect"), act("conn-disconnect", cx)).small());
        }
        let tools = div()
            .flex()
            .flex_wrap()
            .gap(px(2.))
            .child(kit::tool_button("conn-detail-edit", Some(IconName::Pencil), self.t("conn.page.edit"), act("conn-edit", cx)).xsmall())
            .child(kit::tool_button("conn-detail-test", Some(IconName::Zap), self.t("conn.page.test"), act("conn-test", cx)).xsmall())
            .child(kit::tool_button("conn-detail-duplicate", Some(IconName::Copy), self.t("conn.page.duplicate"), act("conn-duplicate", cx)).xsmall())
            .child(div().flex_1())
            .child(kit::icon_button("conn-detail-delete", IconName::Trash, self.t("conn.page.delete"), act("conn-delete", cx)).xsmall());

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
                let tls = match saved.tls.as_ref().map(|tls| tls.mode) {
                    None | Some(TlsMode::Off) => "conn.page.tlsOff",
                    Some(TlsMode::Require) => "conn.page.tlsRequire",
                    Some(TlsMode::Verify) => "conn.page.tlsVerify",
                };
                facts.push(("conn.page.tls", self.model.text(tls)));
                facts.push(("conn.page.ssh", saved.ssh.as_ref().map(|ssh| format!("{}@{}:{}", ssh.username, ssh.host, ssh.port)).unwrap_or_else(|| none.clone())));
            }
        }
        let group = self.model.group_of(&id).and_then(|group| self.model.groups.iter().find(|item| item.id == group)).map(|group| group.name.clone()).unwrap_or_else(|| none.clone());
        facts.push(("conn.page.group", group));
        let mut table = div().flex().flex_col().gap(px(8.));
        for (key, value) in facts {
            let has_value = value != none;
            let copyable = has_value && matches!(key, "conn.page.address" | "conn.page.file");
            table = table.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .text_size(px(TypeScale::SM))
                    .child(div().flex_none().w(px(72.)).text_color(p.text_muted).child(self.t(key)))
                    .child(
                        div()
                            .id(SharedString::from(format!("conn-fact-{key}")))
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_color(if has_value { p.text } else { p.text_faint })
                            .when(has_value, |this| this.tooltip_text(SharedString::from(value.clone())))
                            .child(SharedString::from(value)),
                    )
                    .when(copyable, |this| {
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
                .p(px(18.))
                .flex()
                .flex_col()
                .gap(px(14.))
                .child(
                    kit::card(p)
                        .p(px(14.))
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(12.))
                                .child(kit::engine_badge(saved.engine.as_str(), 36.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .flex()
                                        .flex_col()
                                        .gap(px(4.))
                                        .child(div().truncate().font_weight(FontWeight::SEMIBOLD).text_size(px(TypeScale::LG)).child(SharedString::from(saved.name.clone())))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(6.))
                                                .text_size(px(TypeScale::XS))
                                                .text_color(p.text_muted)
                                                .child(kit::status_dot(dot))
                                                .child(self.model.text(state_key)),
                                        ),
                                ),
                        )
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
        let mut list = div().id("tag-list").flex().flex_col().gap(px(4.)).max_h(px(360.)).overflow_y_scroll();
        for tag in self.model.env_tags.clone() {
            if editing_id.as_deref() == Some(tag.id.as_str()) {
                if let Some(editor) = editor.take() {
                    list = list.child(editor);
                }
                continue;
            }
            let usage = self.model.tag_usage(&tag.id);
            let confirming = manager.confirm_delete.as_deref() == Some(tag.id.as_str());
            let usage_text = if usage == 0 { self.model.text("tags.unused") } else { crate::i18n::fill(&self.model.text("tags.usage"), &[("count", &usage.to_string())]) };
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
                        .child(div().flex_1().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(usage_text)))
                        .child(kit::icon_button(SharedString::from(format!("tag-edit-{}", tag.id)), IconName::Pencil, self.model.text("tags.edit"), act(format!("tag-edit:{}", tag.id), cx)).xsmall())
                        .child(kit::icon_button(SharedString::from(format!("tag-delete-{}", tag.id)), IconName::Trash, self.model.text("tags.delete"), act(format!("tag-delete:{}", tag.id), cx)).xsmall()),
                );
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
