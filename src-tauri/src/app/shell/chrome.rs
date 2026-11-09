//! 外壳的画法：顶栏、活动栏、资源管理器、标签条、状态栏、页面与浮层。
//! 状态与命令都在 `WorkbenchModel`；这里只把它画出来，并把点击交回 `perform`。

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, TitleBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px,
};

use crate::app::views::connections::{engines, LinkStatus};
use crate::app::views::history::HistorySection;
use crate::app::{AppPage, ThemePreference};
use crate::connections::model::{CaMode, Engine, SshAuthKind, TlsMode};
use crate::i18n;
use crate::ui::kit::{self, FieldSpec, Palette, Tip as _};

use super::model::{DraftField, WorkItemKind, WorkTool};
use super::view::Workbench;

const RAIL_W: f32 = 52.;

/// 点击后执行一条外壳命令。
fn act<S: Into<String>>(id: S, cx: &mut Context<Workbench>) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static + use<S> {
    let id: String = id.into();
    let entity = cx.entity().downgrade();
    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        if let Some(entity) = entity.upgrade() {
            entity.update(cx, |this, cx| this.perform(&id, cx));
        }
    }
}

fn field_changed(this: &mut Workbench, field: DraftField, value: String, _cx: &mut Context<Workbench>) {
    this.model.set_field(field, value);
}

impl Workbench {
    fn t(&self, key: &str) -> SharedString {
        SharedString::from(self.model.text(key))
    }

    /// 当前密度的尺寸指标；绘制只读这一处。
    fn metrics(&self) -> kit::Metrics {
        self.model.shell.density.metrics()
    }

    /// 绑定一个真实文本框到模型字段。
    pub(super) fn input_for(&mut self, field: DraftField, placeholder: &str, masked: bool, window: &mut Window, cx: &mut Context<Self>) -> Input {
        let value = self.model.field_value(field);
        let state = self.fields.bind(field, &value, FieldSpec { placeholder, masked }, window, cx, field_changed);
        let label = if placeholder.is_empty() { format!("{field:?}") } else { placeholder.to_owned() };
        kit::text_input(SharedString::from(format!("field-{field:?}")), &state, label)
    }

    // ------------------------------------------------------------ 根

    pub(super) fn render_root(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let entity = cx.entity();
        let page = self.model.shell.page;
        let frame = self.model.painted();
        let palette_open = self.model.shell.palette_open;
        let help_open = self.model.shell.help_open;
        if !palette_open {
            self.palette_focused = false;
        }
        let sidebar_visible = self.model.shell.sidebar_open && matches!(page, AppPage::Workspace | AppPage::Structure | AppPage::Compare | AppPage::Diagram);
        let overlay_sidebar = sidebar_visible && frame.sidebar_overlay;
        let docked_sidebar = sidebar_visible && !frame.sidebar_overlay;

        let content: AnyElement = match page {
            AppPage::Connections => self.connections_page(window, cx),
            AppPage::History => self.history_page(window, cx),
            AppPage::Settings => self.settings_page(cx),
            _ => self.workspace(window, cx),
        };

        let sidebar = if sidebar_visible { Some(self.explorer(window, cx)) } else { None };

        let mut root = div()
            .id("workbench")
            .test_support()
            .key_context("Workbench")
            .size_full()
            .flex()
            .flex_col()
            .bg(p.chrome)
            .text_color(p.text)
            .text_size(px(13.))
            .font_family(cx.theme().font_family.clone())
            .on_drop({
                let entity = entity.clone();
                move |paths: &gpui_kit::ExternalPaths, _, cx| {
                    entity.update(cx, |this, cx| this.accept_exchange_drop(paths, cx));
                }
            })
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| this.overlay_key(event, cx)))
            .child(self.topbar(cx))
            .child(
                div()
                    .id("workbench-body")
                    .relative()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(self.rail(cx))
                    .when_some(if docked_sidebar { sidebar } else { None }, |this, sidebar| this.child(sidebar))
                    .child(
                        div()
                            .id("workbench-content")
                            .test_support()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .bg(p.surface)
                            .border_l_1()
                            .border_color(p.line)
                            .overflow_hidden()
                            .child(content),
                    )
                    .when(overlay_sidebar, |this| {
                        let sidebar = self.explorer(window, cx);
                        this.child(
                            div()
                                .id("sidebar-overlay")
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(px(RAIL_W))
                                .shadow_lg()
                                .child(sidebar),
                        )
                    }),
            )
            .child(self.status_bar(cx));

        if self.model.exchange_open {
            root = root.child(self.exchange_layer(cx));
        }
        if palette_open || help_open {
            root = root.child(self.palette_layer(window, cx));
        }
        if self.model.menu.is_some() {
            root = root.child(self.context_layer(cx));
        }
        if self.model.close_prompt.is_some() {
            root = root.child(self.close_layer(cx));
        }
        if self.model.connections.confirm_delete {
            root = root.child(self.delete_layer(cx));
        }
        if self.model.history.confirm_clear {
            root = root.child(self.clear_history_layer(cx));
        }
        if self.model.history.confirm_delete {
            root = root.child(self.delete_saved_layer(cx));
        }
        root.into_any_element()
    }

    /// 浮层打开时，方向键、回车与 Esc 归浮层。回车经由 `perform` 执行，保证效果完整。
    fn overlay_key(&mut self, event: &gpui_kit::KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if key == "escape" && self.model.connections.confirm_delete && self.model.menu.is_none() && self.model.close_prompt.is_none() {
            self.perform("conn-delete-cancel", cx);
            return;
        }
        if key == "escape" && self.model.history.confirm_clear && self.model.menu.is_none() && self.model.close_prompt.is_none() {
            self.perform("history-clear-cancel", cx);
            return;
        }
        if key == "escape" && self.model.history.confirm_delete && self.model.menu.is_none() && self.model.close_prompt.is_none() {
            self.perform("saved-delete-cancel", cx);
            return;
        }
        if key == "escape" && self.model.exchange_open && self.model.menu.is_none() && !self.model.shell.palette_open {
            self.model.exchange_open = false;
            cx.notify();
            return;
        }
        let Some(nav) = super::view::nav_key(key) else { return };
        if self.model.menu.is_none() && self.model.close_prompt.is_none() && !self.model.shell.palette_open {
            return;
        }
        if matches!(nav, super::menus::NavKey::Enter) {
            if let Some(menu) = &self.model.menu {
                let id = menu.current().map(|item| item.id.to_owned());
                self.model.menu = None;
                self.model.shell.focus = self.model.menu_return;
                if let Some(id) = id {
                    self.perform(&id, cx);
                }
                cx.notify();
                return;
            }
            if self.model.shell.palette_open {
                let entries = self.model.palette_entries();
                let id = entries.get(self.model.palette_index).map(|entry| entry.id.clone());
                self.model.shell.close_overlay();
                if let Some(id) = id {
                    self.perform(&id, cx);
                }
                cx.notify();
                return;
            }
        }
        let _ = self.model.key(nav);
        cx.notify();
    }

    // ------------------------------------------------------------ 顶栏

    fn topbar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let dark = matches!(self.model.shell.resolved_theme(), crate::app::ResolvedTheme::Dark);
        let theme_next = if dark { "theme-light" } else { "theme-dark" };
        let narrow = self.model.viewport_width <= 760;
        let entity = cx.entity();
        // 顶栏就是窗口标题栏：空白处拖动窗口、双击最大化，Windows/Linux 在右侧画最小化/最大化/关闭。
        // 按钮包在 occlude 里，命中测试才不会把点击当成拖动标题栏。
        let controls = |child: AnyElement| div().flex().items_center().gap(px(8.)).occlude().child(child);
        let row = div()
            .id("toolbar")
            .test_support()
            .role(gpui_kit::Role::Toolbar)
            .aria_label(self.t("shell.command.newQuery"))
            .h_full()
            .flex_1()
            .min_w(px(0.))
            .pr(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(controls(
                kit::icon_button("top-sidebar", IconName::PanelLeft, self.t("shell.command.toggleSidebar"), act("sidebar", cx)).into_any_element(),
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(brand_mark(22., &p))
                    .when(!narrow, |this| this.child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child("RillSQL"))),
            )
            .child(div().flex_1())
            .child(controls(
                div()
                    .id("top-search")
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(self.t("shell.palette.title"))
                    .w(px(if narrow { 180. } else { 360. }))
                    .h(px(30.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(p.line)
                    .bg(p.surface)
                    .cursor_pointer()
                    .hover(|style| style.border_color(p.line_strong))
                    .child(kit::icon(IconName::Search, 14., p.text_faint))
                    .child(div().flex_1().truncate().text_size(px(12.5)).text_color(p.text_faint).child(self.t("ui.search.placeholder")))
                    .child(kit::kbd(kit::shortcut("Ctrl+K"), &p))
                    .on_click(act("command-palette", cx))
                    .into_any_element(),
            ))
            .child(div().flex_1())
            .child(
                controls(kit::icon_button("top-sqlite", IconName::FolderOpen, self.t("shell.command.openSqlite"), act("open-sqlite", cx)).into_any_element())
                    .child(kit::icon_button(
                        "top-theme",
                        if dark { IconName::Sun } else { IconName::Moon },
                        self.t(if dark { "shell.command.themeLight" } else { "shell.command.themeDark" }),
                        act(theme_next, cx),
                    ))
                    .child(if narrow {
                        kit::icon_button("top-new-query", IconName::Plus, self.t("shell.command.newQuery"), act("new-query", cx)).primary().into_any_element()
                    } else {
                        kit::primary_button("top-new-query", Some(IconName::Plus), self.t("shell.command.newQuery"), act("new-query", cx)).into_any_element()
                    }),
            );
        TitleBar::new()
            .h(px(m.topbar_h))
            .when(!cfg!(target_os = "macos"), |this| this.pl(px(10.)))
            .bg(p.chrome)
            .border_color(p.line)
            // Linux 自绘的关闭按钮走与系统关闭相同的未保存守卫。
            .on_close_window(move |_, _, cx| {
                entity.update(cx, |this, cx| this.perform("quit-window", cx));
            })
            .child(row)
            .into_any_element()
    }

    // ------------------------------------------------------------ 活动栏

    fn rail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let page = self.model.shell.page;
        // 固定活动栏只留工作台、连接、历史与底部设置；对比、导入导出和帮助
        // 走命令面板与对象/结果上下文菜单，不再占固定位置。
        let mut top = div().flex().flex_col().items_center().gap(px(4.));
        for id in crate::app::model::RAIL_TOP {
            if let Some((icon, key, active)) = rail_entry(id, page) {
                top = top.child(rail_item(id, icon, self.t(key), active, &p, cx));
            }
        }
        let mut bottom = div().flex().flex_col().items_center().gap(px(4.));
        for id in crate::app::model::RAIL_BOTTOM {
            if let Some((icon, key, active)) = rail_entry(id, page) {
                bottom = bottom.child(rail_item(id, icon, self.t(key), active, &p, cx));
            }
        }
        div()
            .id("activity")
            .role(gpui_kit::Role::Navigation)
            .aria_label(self.t("shell.nav.main"))
            .flex_none()
            .w(px(RAIL_W))
            .py(px(8.))
            .flex()
            .flex_col()
            .justify_between()
            .bg(p.rail)
            .child(top)
            .child(bottom)
            .into_any_element()
    }

    // ------------------------------------------------------------ 资源管理器

    fn explorer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let width = self.model.layout().sidebar_width.max(220) as f32;
        let filter = self.input_for(DraftField::ObjectFilter, &self.model.text("ui.explorer.filterObjects"), false, window, cx);
        // 对象索引来自模型里的同一份快照；连接列表保持完整，连接筛选只属于连接页。
        let rows: Vec<_> = self.model.connections.rows.clone();
        let mut list = div().id("explorer-list").flex().flex_col().flex_1().min_h(px(0.)).overflow_y_scroll().px(px(6.)).pb(px(8.));
        if rows.is_empty() {
            list = list.child(
                kit::empty_state(IconName::Database, self.t("ui.explorer.empty"), self.t("ui.explorer.emptyHint"), &p)
                    .child(kit::primary_button("explorer-new", Some(IconName::Plus), self.t("common.newConnection"), act("new-connection", cx)))
                    .child(kit::tool_button("explorer-sqlite", Some(IconName::FolderOpen), self.t("common.openSqlite"), act("open-sqlite", cx))),
            );
        }
        // 读取失败只留一条带原因的提示和重试入口，不在每个连接下重复。
        if !self.model.catalog_error.is_empty() {
            let error = self.model.catalog_error.clone();
            let full_error = format!("{}: {}", self.model.text("ui.explorer.error"), error);
            list = list.child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .mb(px(4.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .id("explorer-error")
                            .test_support()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .tooltip_text(SharedString::from(full_error.clone()))
                            .text_size(px(12.))
                            .text_color(p.danger)
                            .child(SharedString::from(full_error)),
                    )
                    .child(kit::tool_button(
                        "explorer-tree-retry",
                        Some(IconName::RefreshCw),
                        self.t("common.retry"),
                        act("refresh-tree", cx),
                    )),
            );
        }
        for row in rows {
            let id = row.saved.id.clone();
            let status = self.model.connections.status.get(&id).copied().unwrap_or(LinkStatus::Disconnected);
            let expanded = self.expanded.contains(&id) && status == LinkStatus::Connected;
            let selected = self.model.connections.selected.as_deref() == Some(id.as_str());
            list = list.child(self.connection_row(&row, status, expanded, selected, &p, cx));
            if !expanded {
                continue;
            }
            match row.saved.engine {
                Engine::Redis | Engine::Mongodb => {}
                _ => {
                    let groups = self.model.object_groups(&id);
                    if groups.is_empty() {
                        // 空目录与筛选无命中各有文案；读取失败由上方提示条负责。
                        if let Some(key) = self.model.object_notice(&id) {
                            if key != "ui.explorer.error" {
                                list = list.child(div().pl(px(42.)).py(px(4.)).text_size(px(12.)).text_color(p.text_faint).child(self.t(key)));
                            }
                        }
                    }
                    let environment = row.saved.env.clone();
                    // 同一库下的多个模式只打印一次库名。
                    let mut databases = std::collections::BTreeSet::new();
                    for group in groups {
                        // 数据库 / 模式按已读取的元数据分层；空段不制造标题。
                        if !group.database.is_empty() && databases.insert(group.database.clone()) {
                            list = list.child(
                                div()
                                    .pl(px(30.))
                                    .pt(px(6.))
                                    .pb(px(2.))
                                    .text_size(px(11.))
                                    .text_color(p.text_muted)
                                    .child(SharedString::from(group.database.clone())),
                            );
                        }
                        if !group.schema.is_empty() {
                            list = list.child(
                                div()
                                    .pl(px(38.))
                                    .pb(px(2.))
                                    .text_size(px(11.))
                                    .text_color(p.text_faint)
                                    .child(SharedString::from(group.schema.clone())),
                            );
                        }
                        for object in &group.objects {
                            list = list.child(self.object_row(object, &environment, &p, cx));
                        }
                    }
                }
            }
        }
        div()
            .id("sidebar")
            .test_support()
            .role(gpui_kit::Role::Navigation)
            .aria_label(self.t("shell.sidebar.explorer"))
            .aria_expanded(self.model.shell.sidebar_open)
            .flex_none()
            .w(px(width))
            .h_full()
            .flex()
            .flex_col()
            .bg(p.chrome)
            .border_l_1()
            .border_color(p.line)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(m.tabs_h))
                    .pl(px(14.))
                    .pr(px(6.))
                    .child(kit::eyebrow(self.t("shell.sidebar.explorer"), &p))
                    .child(
                        div()
                            .flex()
                            .gap(px(2.))
                            .child(kit::icon_button("explorer-refresh", IconName::RefreshCw, self.t("query.tree.refresh"), act("refresh-tree", cx)).xsmall())
                            .child(kit::icon_button("explorer-add", IconName::Plus, self.t("common.newConnection"), act("new-connection", cx)).xsmall()),
                    ),
            )
            .child(div().px(px(10.)).pb(px(8.)).child(filter.prefix(kit::icon(IconName::Search, 13., p.text_faint)).cleanable(true)))
            .child(list)
            .into_any_element()
    }

    fn connection_row(&self, row: &crate::connections::ConnectionRecord, status: LinkStatus, expanded: bool, selected: bool, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let m = self.metrics();
        let id = row.saved.id.clone();
        let name = SharedString::from(row.saved.name.clone());
        let detail = connection_detail(row);
        let full_name = if detail.is_empty() {
            name.clone()
        } else {
            SharedString::from(format!("{} · {}", row.saved.name, detail))
        };
        let (dot, state_key) = match status {
            LinkStatus::Connected => (p.ok, "connections.status.connected"),
            LinkStatus::Connecting => (p.warn, "ui.conn.connecting"),
            LinkStatus::Failed => (p.danger, "connections.status.error"),
            LinkStatus::Disconnected => (p.line_strong, "ui.conn.notConnected"),
        };
        let chevron = if expanded { IconName::ChevronDown } else { IconName::ChevronRight };
        let click_id = id.clone();
        let menu_id = id.clone();
        div()
            .id(SharedString::from(format!("side-{id}")))
            .role(gpui_kit::Role::TreeItem)
            .aria_label(name.clone())
            .aria_selected(selected)
            .aria_expanded(expanded)
            .tooltip_text(full_name)
            .h(px(m.row_h))
            .px(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(6.))
            .cursor_pointer()
            .when(selected, |this| this.bg(p.active))
            .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
            .child(kit::icon(chevron, 13., p.text_faint))
            .child(kit::engine_badge(row.saved.engine.as_str(), 18.))
            .child(div().flex_1().min_w(px(0.)).truncate().font_weight(FontWeight::MEDIUM).child(name))
            .when(status == LinkStatus::Connecting, |this| this.child(Spinner::new().xsmall().color(p.text_muted)))
            .when_some(browser_command(row), |this, (command, key, element_id)| {
                let label = format!("{} {}", self.model.text(key), row.saved.name);
                let icon = if row.saved.engine == Engine::Redis { IconName::Key } else { IconName::Braces };
                this.child(kit::icon_button(SharedString::from(element_id), icon, SharedString::from(label), act(command, cx)).xsmall())
            })
            .child(
                div()
                    .id(SharedString::from(format!("side-state-{id}")))
                    .tooltip_text(self.t(state_key))
                    .child(kit::status_dot(dot)),
            )
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.menu_at = event.position;
                this.model.connections.selected = Some(menu_id.clone());
                this.perform("menu-connection", cx);
            }))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.click_connection(&click_id, event.click_count() >= 2, cx);
            }))
            .into_any_element()
    }

    fn click_connection(&mut self, id: &str, double: bool, cx: &mut Context<Self>) {
        self.model.connections.selected = Some(id.to_owned());
        let status = self.model.connections.status.get(id).copied().unwrap_or(LinkStatus::Disconnected);
        match status {
            LinkStatus::Connected => {
                if let Some((command, _, _)) = self.model.connections.rows.iter().find(|row| row.saved.id == id).and_then(browser_command) {
                    self.perform(&command, cx);
                    return;
                }
                if double || !self.expanded.remove(id) {
                    self.expanded.insert(id.to_owned());
                }
                cx.notify();
            }
            LinkStatus::Connecting => {}
            LinkStatus::Disconnected | LinkStatus::Failed => {
                if self.model.connections.rows.iter().any(|row| row.saved.id == id && matches!(row.saved.engine, Engine::Redis | Engine::Mongodb)) {
                    self.pending_browser = Some(id.to_owned());
                }
                self.expanded.insert(id.to_owned());
                self.model.connections.status.insert(id.to_owned(), LinkStatus::Connecting);
                self.perform("conn-connect", cx);
            }
        }
    }

    fn object_row(&self, object: &crate::query::assist::TreeObject, environment: &str, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let m = self.metrics();
        let spec = crate::app::views::query::object_spec(object, environment);
        // 行身份也用完整目标：同名对象在不同连接或不同库里必须是不同的节点。
        let key = spec.clone();
        let selected = self.model.object_spec.as_deref() == Some(spec.as_str());
        let view = object.kind.contains("view");
        let icon = if view { IconName::Eye } else { IconName::Table };
        let name = if object.schema.is_empty() || object.schema == "dbo" || object.schema == "public" {
            object.name.clone()
        } else {
            format!("{}.{}", object.schema, object.name)
        };
        let label = SharedString::from(name);
        let click_spec = spec.clone();
        let menu_spec = spec.clone();
        // “在这里新建查询”带着完整目标，查询页不再靠 连接:对象名 反查。
        let query_id = format!("object:{spec}");
        let structure_id = format!("open-structure-for:{spec}");
        div()
            .id(SharedString::from(format!("obj-{key}")))
            .group("obj-row")
            .role(gpui_kit::Role::TreeItem)
            .aria_label(label.clone())
            .aria_selected(selected)
            .tooltip_text(label.clone())
            .h(px(m.tree_row_h))
            .pl(px(30.))
            .pr(px(4.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(6.))
            .cursor_pointer()
            .when(selected, |this| this.bg(p.selection))
            .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
            .child(kit::icon(icon, 13., if view { p.info } else { p.text_muted }))
            .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.5)).child(label))
            .child(
                div()
                    .flex()
                    .gap(px(1.))
                    .invisible()
                    .group_hover("obj-row", |style| style.visible())
                    .child(kit::icon_button(SharedString::from(format!("obj-q-{key}")), IconName::SquareTerminal, self.t("catalog.tree.newQueryHere"), act(query_id, cx)).xsmall())
                    .child(kit::icon_button(SharedString::from(format!("obj-s-{key}")), IconName::Columns3, self.t("catalog.tree.viewStructure"), act(structure_id, cx)).xsmall()),
            )
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.menu_at = event.position;
                this.model.object_spec = Some(menu_spec.clone());
                this.perform("menu-object-item", cx);
            }))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.model.object_spec = Some(click_spec.clone());
                if event.click_count() >= 2 {
                    this.perform(&format!("open-table:{click_spec}"), cx);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    // ------------------------------------------------------------ 工作区

    fn workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let page = self.model.shell.page;
        let tool = self.model.tool;
        let table_view = page == AppPage::Workspace && tool == WorkTool::Table;
        let redis_view = page == AppPage::Workspace && tool == WorkTool::Redis;
        let mongo_view = page == AppPage::Workspace && tool == WorkTool::Mongo;
        let tool_view = table_view || redis_view || mongo_view || matches!(page, AppPage::Structure | AppPage::Compare | AppPage::Diagram);
        let welcome = self.model.show_welcome() && !tool_view;
        let body: AnyElement = if welcome {
            self.welcome_page(cx)
        } else if page == AppPage::Structure && tool == WorkTool::Admin {
            self.admin.clone().into_any_element()
        } else if page == AppPage::Structure {
            self.structure.clone().into_any_element()
        } else if page == AppPage::Compare {
            self.compare.clone().into_any_element()
        } else if page == AppPage::Diagram {
            self.diagram.clone().into_any_element()
        } else if redis_view {
            self.redis.clone().into_any_element()
        } else if mongo_view {
            self.mongo.clone().into_any_element()
        } else if table_view {
            let entity = cx.entity();
            div()
                .id("table-host")
                .flex()
                .flex_col()
                .size_full()
                .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.menu_at = event.position;
                        this.perform("menu-grid", cx);
                    });
                })
                .child(self.table.clone())
                .into_any_element()
        } else {
            let entity = cx.entity();
            div()
                .id("editor-host")
                .flex()
                .flex_col()
                .size_full()
                .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.menu_at = event.position;
                        let grid = this.query.update(cx, |pane, _| pane.take_grid_menu_request());
                        this.perform(if grid { "menu-grid" } else { "menu-editor" }, cx);
                    });
                })
                .child(self.query.clone())
                .into_any_element()
        };
        let _ = window;
        div()
            .flex()
            .flex_col()
            .size_full()
            .when(!welcome || !self.model.tabs.is_empty(), |this| this.child(self.tab_strip(cx)))
            .child(div().flex_1().min_h(px(0.)).flex().flex_col().child(body))
            .into_any_element()
    }

    fn tab_strip(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        // 标签列表独立滚动，新建入口固定在右侧；当前标签可通过横向滚动或 Ctrl+Tab 到达。
        let mut list = div()
            .id("tab-list")
            .flex()
            .items_end()
            .flex_1()
            .min_w(px(0.))
            .overflow_x_scroll();
        let work_page = matches!(self.model.shell.page, AppPage::Workspace | AppPage::Structure);
        for tab in self.model.tabs.clone() {
            // 只有工作页才把标签显示为选中；设置、对比、关系图页不伪装成某个工作项。
            let active = work_page && self.model.active_tab.as_deref() == Some(tab.id.as_str());
            let dirty = tab.uncommitted_cells;
            let title = SharedString::from(tab.title.clone());
            let id = tab.id.clone();
            let close_id = tab.id.clone();
            let menu_id = tab.id.clone();
            let middle_id = tab.id.clone();
            let chip = tab_chip(
                SharedString::from(format!("tab-{id}")),
                item_icon(tab.kind),
                title,
                active,
                dirty,
                tab.running,
                &p,
            )
            .child(
                kit::icon_button(SharedString::from(format!("tab-close-{id}")), IconName::X, self.t("common.close"), cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.model.request_close_tab(&close_id);
                    this.persist_if_needed();
                    cx.notify();
                }))
                .xsmall(),
            )
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                // 右键先真正激活该工作项，菜单动作与随后的视图目标才指向同一项。
                if this.model.tab_menu_for(&menu_id) {
                    this.menu_at = event.position;
                    this.persist_if_needed();
                }
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Middle, cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                this.model.request_close_tab(&middle_id);
                this.persist_if_needed();
                cx.notify();
            }))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                let _ = this.model.activate_item(&id);
                if this.model.tool == WorkTool::Query {
                    this.query.update(cx, |pane, _| pane.request_focus());
                }
                this.persist_if_needed();
                cx.notify();
            }));
            list = list.child(chip);
        }
        div()
            .id("tabs")
            .test_support()
            .role(gpui_kit::Role::TabList)
            .aria_label(self.t("shell.tabs.tablist"))
            .flex_none()
            .h(px(m.tabs_h))
            .flex()
            .items_end()
            .bg(p.chrome)
            .border_b_1()
            .border_color(p.line)
            .child(list)
            .child(
                div()
                    .h_full()
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(px(6.))
                    .child(kit::icon_button("tab-new", IconName::Plus, self.t("shell.command.newQuery"), act("new-query", cx)).xsmall()),
            )
            .into_any_element()
    }

    // ------------------------------------------------------------ 状态栏

    fn status_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        // 目标名可能很长；只压缩这一段，状态提示与语言开关保持在位。
        let target_max = (self.model.viewport_width as f32 * 0.42).max(120.);
        let target: AnyElement = match self.model.active_tab() {
            Some(tab) if tab.connection_id.is_some() => {
                let engine = tab.engine.clone().unwrap_or_default();
                let env = self
                    .model
                    .connections
                    .rows
                    .iter()
                    .find(|row| Some(&row.saved.id) == tab.connection_id.as_ref())
                    .map(|row| row.saved.env.clone())
                    .unwrap_or_default();
                let name = tab.connection_name.clone().unwrap_or_default();
                let database = tab.database.clone().unwrap_or_default();
                div()
                    .id("status-target")
                    .test_support()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w(px(0.))
                    .max_w(px(target_max))
                    .child(kit::engine_badge(&engine, 14.))
                    .child(div().min_w(px(0.)).truncate().child(SharedString::from(if database.is_empty() { name } else { format!("{name} / {database}") })))
                    .when(!env.is_empty(), |this| this.child(kit::env_tag(&env, self.t(&format!("ui.conn.env.{env}")), &p)))
                    .into_any_element()
            }
            _ => div().flex().items_center().gap(px(6.)).child(kit::status_dot(p.line_strong)).child(self.t("ui.status.noTarget")).into_any_element(),
        };
        let notice = match self.model.notice_key {
            Some(key) if !self.model.notice_detail.is_empty() => format!("{} {}", self.model.text(key), self.model.notice_detail),
            Some(key) => self.model.text(key),
            None => self.model.text("ui.status.ready"),
        };
        let running = self.model.running_jobs > 0 || self.model.exchange_running || self.model.compare_running || self.model.tabs.iter().any(|tab| tab.running);
        let locale_next = if self.model.shell.locale == crate::app::Locale::En { "locale-zh" } else { "locale-en" };
        let locale_label = if self.model.shell.locale == crate::app::Locale::En { "EN" } else { "中" };
        div()
            .id("status")
            .test_support()
            .role(gpui_kit::Role::Status)
            .aria_label(SharedString::from(notice.clone()))
            .flex_none()
            .h(px(m.status_h))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(14.))
            .bg(p.chrome)
            .border_t_1()
            .border_color(p.line)
            .text_size(px(11.5))
            .text_color(p.text_muted)
            .child(target)
            .child(div().flex_1().min_w(px(0.)).truncate().child(SharedString::from(notice)))
            .when(running, |this| {
                this.child(div().flex().items_center().gap(px(6.)).child(Spinner::new().xsmall().color(p.accent)).child(self.t("query.status.running")))
            })
            .child(
                div()
                    .id("status-locale")
                    .px(px(6.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|style| style.bg(p.hover).text_color(p.text))
                    .child(locale_label)
                    .on_click(act(locale_next, cx)),
            )
            .into_any_element()
    }

    // ------------------------------------------------------------ 欢迎页

    fn welcome_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let actions = div()
            .flex()
            .flex_wrap()
            .gap(px(12.))
            .child(action_card("welcome-connections", IconName::Plug, self.t("common.newConnection"), self.t("ui.welcome.connectionHint"), &p, act("new-connection", cx)))
            .child(action_card("welcome-sqlite", IconName::FolderOpen, self.t("common.openSqlite"), self.t("ui.welcome.sqliteHint"), &p, act("open-sqlite", cx)))
            .child(action_card("welcome-query", IconName::SquareTerminal, self.t("shell.command.newQuery"), self.t("ui.welcome.queryHint"), &p, act("new-query", cx)));

        let mut connections = div().flex().flex_col().gap(px(2.));
        let rows: Vec<_> = self.model.connections.rows.iter().take(6).cloned().collect();
        for row in &rows {
            let id = row.saved.id.clone();
            let detail = connection_detail(row);
            connections = connections.child(
                list_row(SharedString::from(format!("welcome-conn-{id}")), &p, m.row_pad_y)
                    .child(kit::engine_badge(row.saved.engine.as_str(), 22.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(div().truncate().font_weight(FontWeight::MEDIUM).child(SharedString::from(row.saved.name.clone())))
                            .child(div().truncate().text_size(px(11.5)).text_color(p.text_faint).child(SharedString::from(detail))),
                    )
                    .child(kit::env_tag(&row.saved.env, self.t(&format!("ui.conn.env.{}", row.saved.env)), &p))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.model.shell.sidebar_open = true;
                        this.click_connection(&id, false, cx);
                    })),
            );
        }
        if rows.is_empty() {
            connections = connections.child(div().py(px(8.)).text_size(px(12.)).text_color(p.text_faint).child(self.t("ui.explorer.emptyHint")));
        }

        let mut recent = div().flex().flex_col().gap(px(2.));
        let runs: Vec<_> = self.model.welcome.recent(&self.model.history.runs).to_vec();
        for entry in runs.iter().take(6) {
            let id = entry.id.clone();
            recent = recent.child(
                list_row(SharedString::from(format!("welcome-run-{id}")), &p, m.row_pad_y)
                    .child(kit::icon(IconName::Clock, 14., p.text_faint))
                    .child(div().flex_1().min_w(px(0.)).truncate().font_family(cx.theme().mono_font_family.clone()).text_size(px(12.)).child(SharedString::from(one_line(&entry.sql))))
                    .child(
                        div()
                            .id(SharedString::from(format!("welcome-run-client-{id}")))
                            .flex_none()
                            .max_w(px(160.))
                            .truncate()
                            .tooltip_text(SharedString::from(entry.connection_name.clone()))
                            .text_size(px(11.))
                            .text_color(p.text_faint)
                            .child(SharedString::from(entry.connection_name.clone())),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.model.history.selected_run = Some(id.clone());
                        this.model.history.section = HistorySection::Runs;
                        this.perform("history-load", cx);
                    })),
            );
        }
        let saved: Vec<_> = self.model.welcome.saved(&self.model.history.saved).to_vec();
        for item in saved.iter().take(4) {
            let saved = item.clone();
            recent = recent.child(
                list_row(SharedString::from(format!("welcome-saved-{}", item.id)), &p, m.row_pad_y)
                    .child(kit::icon(IconName::Bookmark, 14., p.accent))
                    .child(div().flex_1().min_w(px(0.)).truncate().child(SharedString::from(item.name.clone())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let known = this.model.known_ids();
                        let plan = this.model.welcome.open_saved(&saved, &known);
                        this.model.history.section = HistorySection::Saved;
                        this.model.history.selected_saved = Some(saved.id.clone());
                        this.model.apply_load(plan);
                        cx.notify();
                    })),
            );
        }
        if runs.is_empty() && saved.is_empty() {
            recent = recent.child(div().py(px(8.)).text_size(px(12.)).text_color(p.text_faint).child(self.t("ui.welcome.noRecent")));
        }

        let two_col = self.model.viewport_width > 980;
        div()
            .id("welcome-page")
            .role(gpui_kit::Role::Group)
            .aria_label(self.t("shell.welcome.title"))
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .mx_auto()
                    .max_w(px(880.))
                    .px(px(m.page_pad + 4.))
                    .py(px(m.page_pad_y + 20.))
                    .flex()
                    .flex_col()
                    .gap(px(m.page_pad))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(14.))
                            .child(brand_mark(44., &p))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(kit::title("RillSQL", 22., &p))
                                    .child(kit::muted(self.t("ui.welcome.subtitle"), &p)),
                            ),
                    )
                    .child(div().flex().flex_col().gap(px(10.)).child(kit::eyebrow(self.t("ui.welcome.start"), &p)).child(actions))
                    .child(
                        div()
                            .flex()
                            .when(two_col, |this| this.flex_row())
                            .when(!two_col, |this| this.flex_col())
                            .gap(px(20.))
                            .child(
                                kit::card(&p)
                                    .flex_1()
                                    .flex_basis(px(0.))
                                    .min_w(px(0.))
                                    .p(px(m.page_pad / 2.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(8.))
                                    .child(section_head(self.t("ui.welcome.connections"), Some(("welcome-manage", self.t("ui.welcome.manage"), "connections")), &p, cx))
                                    .child(connections),
                            )
                            .child(
                                kit::card(&p)
                                    .flex_1()
                                    .flex_basis(px(0.))
                                    .min_w(px(0.))
                                    .p(px(m.page_pad / 2.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(8.))
                                    .child(section_head(self.t("shell.welcome.resumeTitle"), Some(("welcome-history", self.t("ui.welcome.allHistory"), "history")), &p, cx))
                                    .child(recent),
                            ),
                    )
                    .child(div().text_size(px(11.5)).text_color(p.text_faint).child(self.t("shell.welcome.factEngines"))),
            )
            .into_any_element()
    }

    // ------------------------------------------------------------ 连接管理

    fn connections_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let filter = self.input_for(DraftField::Filter, &self.model.text("ui.conn.search"), false, window, cx);
        let narrow = self.model.viewport_width <= 900;
        let rows: Vec<_> = self.model.connections.filtered().into_iter().cloned().collect();
        let mut list = div().id("conn-list").flex().flex_col().gap(px(2.)).flex_1().min_h(px(0.)).overflow_y_scroll().px(px(8.)).pb(px(8.));
        for row in &rows {
            let id = row.saved.id.clone();
            let selected = self.model.connections.selected.as_deref() == Some(id.as_str());
            let status = self.model.connections.status.get(&id).copied().unwrap_or(LinkStatus::Disconnected);
            let dot = match status {
                LinkStatus::Connected => p.ok,
                LinkStatus::Connecting => p.warn,
                LinkStatus::Failed => p.danger,
                LinkStatus::Disconnected => p.line_strong,
            };
            let select = format!("connection:{id}");
            let menu = select.clone();
            let label = format!("{} {}", row.saved.name, row.saved.engine.as_str());
            list = list.child(
                div()
                    .id(SharedString::from(format!("conn-row-{id}")))
                    .test_support()
                    .role(gpui_kit::Role::ListItem)
                    .aria_label(SharedString::from(label))
                    .aria_selected(selected)
                    .px(px(8.))
                    .py(px(m.row_pad_y))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .when(selected, |this| this.bg(p.active))
                    .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
                    .child(kit::engine_badge(row.saved.engine.as_str(), 26.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .child(div().truncate().font_weight(FontWeight::MEDIUM).child(SharedString::from(row.saved.name.clone())))
                            .child(div().truncate().text_size(px(11.5)).text_color(p.text_faint).child(SharedString::from(connection_detail(row)))),
                    )
                    .child(div().flex().flex_col().items_end().gap(px(4.)).child(kit::env_tag(&row.saved.env, self.t(&format!("ui.conn.env.{}", row.saved.env)), &p)).child(kit::status_dot(dot)))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.menu_at = event.position;
                        let _ = this.model.dispatch(&menu);
                        this.perform("menu-connection", cx);
                    }))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        this.perform(&select, cx);
                        this.model.connections.edit_selected();
                        if event.click_count() >= 2 {
                            this.perform("conn-connect", cx);
                        }
                        cx.notify();
                    })),
            );
        }
        if rows.is_empty() {
            // 空列表给下一步，而不是只留一句“没有连接”。
            list = list.child(
                div()
                    .p(px(16.))
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(10.))
                    .child(div().text_size(px(12.)).text_color(p.text_faint).child(self.t("ui.explorer.empty")))
                    .child(kit::tool_button("conn-empty-new", Some(IconName::Plus), self.t("common.newConnection"), act("new-connection", cx))),
            );
        }

        let new_button = Button::new("conn-new")
            .primary()
            .small()
            .icon(IconName::Plus)
            .label(self.t("ui.conn.new"))
            .accessibility_label(self.t("common.newConnection"))
            .on_click(act("new-connection", cx));

        let left = div()
            .flex_none()
            .w(px(if narrow { 260. } else { 320. }))
            .h_full()
            .flex()
            .flex_col()
            .bg(p.chrome)
            .border_r_1()
            .border_color(p.line)
            .child(
                div()
                    .px(px(14.))
                    .pt(px(14.))
                    .pb(px(10.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(div().flex().items_center().justify_between().child(kit::title(self.t("connections.page.title"), 15., &p)).child(new_button))
                    .child(filter.prefix(kit::icon(IconName::Search, 13., p.text_faint)).cleanable(true)),
            )
            .child(list)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(10.))
                    .py(px(8.))
                    .border_t_1()
                    .border_color(p.line)
                    .child(kit::tool_button("conn-import", Some(IconName::Download), self.t("ui.conn.import"), act("conn-import", cx)))
                    .child(kit::tool_button("conn-export", Some(IconName::Upload), self.t("ui.conn.export"), act("conn-export", cx))),
            );

        let right: AnyElement = if self.model.connections.editor.is_some() {
            self.connection_form(window, cx)
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    kit::empty_state(IconName::Plug, self.t("ui.conn.emptyTitle"), self.t("connections.page.note"), &p)
                        .child(kit::primary_button("conn-empty-new", Some(IconName::Plus), self.t("common.newConnection"), act("new-connection", cx)))
                        .child(kit::tool_button("conn-empty-sqlite", Some(IconName::FolderOpen), self.t("common.openSqlite"), act("open-sqlite", cx))),
                )
                .into_any_element()
        };

        div()
            .id("connections-page")
            .role(gpui_kit::Role::Group)
            .aria_label(self.t("connections.page.title"))
            .size_full()
            .flex()
            .child(left)
            .child(div().flex_1().min_w(px(0.)).h_full().child(right))
            .into_any_element()
    }

    fn connection_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let narrow = self.model.viewport_width <= 900;
        let Some(draft) = self.model.connections.editor.clone() else {
            return div().into_any_element();
        };
        let existing = draft.id.is_some();
        let engine = draft.engine;
        let file_based = engine == Engine::Sqlite;
        let t = |this: &Self, key: &str| this.model.text(key);

        // 引擎选择：一排徽标。
        let mut engine_row = div().flex().flex_wrap().gap(px(6.));
        for candidate in engines() {
            let selected = candidate == engine;
            let name = candidate.as_str();
            engine_row = engine_row.child(
                div()
                    .id(SharedString::from(format!("conn-engine-{name}")))
                    .role(gpui_kit::Role::Button)
                    .aria_label(SharedString::from(kit::engine_label(name)))
                    .aria_selected(selected)
                    .h(px(30.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(7.))
                    .border_1()
                    .cursor_pointer()
                    .when(selected, |this| this.border_color(p.accent).bg(p.accent_soft).text_color(p.accent_text))
                    .when(!selected, |this| this.border_color(p.line).hover(|style| style.border_color(p.line_strong)))
                    .child(kit::engine_badge(name, 16.))
                    .child(div().text_size(px(12.)).child(kit::engine_label(name)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let Some(editor) = this.model.connections.editor.as_mut() {
                            if editor.engine != candidate {
                                let keep_name = editor.name.clone();
                                let id = editor.id.clone();
                                let mut fresh = crate::app::views::connections::blank_draft(candidate);
                                fresh.name = keep_name;
                                fresh.id = id;
                                *editor = fresh;
                            }
                        }
                        cx.notify();
                    })),
            );
        }

        let mut env_items = Vec::new();
        for env in ["dev", "test", "prod", "local"] {
            let selected = draft.env == env;
            env_items.push((
                SharedString::from(format!("conn-env-{env}")),
                self.t(&format!("ui.conn.env.{env}")),
                selected,
                kit::click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(editor) = this.model.connections.editor.as_mut() {
                        editor.env = env.to_owned();
                    }
                    cx.notify();
                })),
            ));
        }

        let name = self.input_for(DraftField::Name, &t(self, "ui.conn.namePlaceholder"), false, window, cx);
        let url = self.input_for(DraftField::Url, &t(self, "ui.conn.urlHint"), false, window, cx);
        let mut general = div().flex().flex_col().gap(px(14.));
        general = general.child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.))
                .items_end()
                .child(div().flex_1().min_w(px(180.)).child(kit::form_row(self.t("ui.conn.url"), url, &p)))
                .child(kit::secondary_button("conn-apply-url", Some(IconName::WandSparkles), self.t("ui.conn.parse"), act("conn-apply-url", cx))),
        );
        general = general.child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(12.))
                .child(div().flex_1().min_w(px(180.)).child(kit::form_row(self.t("ui.conn.name"), name, &p)))
                .child(kit::form_row(self.t("ui.conn.env"), kit::segmented_sized(env_items, &p, m.control_h), &p)),
        );
        if file_based {
            let file = self.input_for(DraftField::File, &t(self, "connections.validate.chooseFile"), false, window, cx);
            general = general.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .items_end()
                    .child(div().flex_1().min_w(px(180.)).child(kit::form_row(self.t("ui.conn.file"), file, &p)))
                    .child(kit::secondary_button("conn-browse", Some(IconName::FolderOpen), self.t("ui.conn.browse"), cx.listener(|this, _: &ClickEvent, _, cx| this.prompt_draft_file(cx)))),
            );
        } else {
            let host = self.input_for(DraftField::Host, "localhost", false, window, cx);
            let port = self.input_for(DraftField::Port, "", false, window, cx);
            let database = self.input_for(DraftField::Database, &t(self, if engine == Engine::Redis { "ui.conn.redisDb" } else { "ui.conn.optional" }), false, window, cx);
            let user = self.input_for(DraftField::Username, "", false, window, cx);
            let password = self.input_for(DraftField::Password, &t(self, if existing { "ui.conn.passwordKeep" } else { "" }), true, window, cx);
            general = general
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(12.))
                        .child(div().flex_1().min_w(px(180.)).child(kit::form_row(self.t("ui.conn.host"), host, &p)))
                        .child(div().w(px(110.)).child(kit::form_row(self.t("ui.conn.port"), port, &p))),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(12.))
                        .child(div().flex_1().min_w(px(160.)).child(kit::form_row(self.t("ui.conn.user"), user, &p)))
                        .child(div().flex_1().min_w(px(160.)).child(kit::form_row(self.t("ui.conn.password"), password.mask_toggle(), &p))),
                )
                .child(kit::form_row(self.t("ui.conn.database"), database, &p));
            if engine == Engine::Mongodb {
                let replica = self.input_for(DraftField::Replica, "rs0", false, window, cx);
                general = general.child(kit::form_row(self.t("connections.editor.replicaSet"), replica, &p));
            }
        }

        // 安全：TLS 与 SSH。
        let mut security = div().flex().flex_col().gap(px(14.));
        if !file_based {
            let mut tls_items = Vec::new();
            for (mode, key) in [(TlsMode::Off, "connections.page.tlsOff"), (TlsMode::Require, "ui.conn.tlsRequire"), (TlsMode::Verify, "ui.conn.tlsVerify")] {
                tls_items.push((
                    SharedString::from(format!("conn-tls-{key}")),
                    self.t(key),
                    draft.tls_mode == mode,
                    kit::click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let Some(editor) = this.model.connections.editor.as_mut() {
                            editor.tls_mode = mode;
                        }
                        cx.notify();
                    })),
                ));
            }
            security = security.child(kit::form_row(self.t("connections.editor.tls.title"), kit::segmented_sized(tls_items, &p, self.metrics().control_h), &p));
            if draft.tls_mode == TlsMode::Verify {
                let mut ca_items = Vec::new();
                for (mode, key) in [(CaMode::System, "connections.editor.tls.caSystem"), (CaMode::File, "connections.editor.tls.caFile")] {
                    ca_items.push((
                        SharedString::from(format!("conn-ca-{key}")),
                        self.t(key),
                        draft.ca_mode == mode,
                        kit::click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if let Some(editor) = this.model.connections.editor.as_mut() {
                                editor.ca_mode = mode;
                            }
                            cx.notify();
                        })),
                    ));
                }
                security = security.child(kit::form_row(self.t("connections.editor.tls.caLabel"), kit::segmented_sized(ca_items, &p, self.metrics().control_h), &p));
                if draft.ca_mode == CaMode::File {
                    let ca = self.input_for(DraftField::CaFile, "ca.pem", false, window, cx);
                    security = security.child(kit::form_row(self.t("connections.editor.tls.caFile"), ca, &p));
                }
                let cert = self.input_for(DraftField::ClientCert, "", false, window, cx);
                let key = self.input_for(DraftField::ClientKey, "", false, window, cx);
                security = security.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(12.))
                        .child(div().flex_1().min_w(px(160.)).child(kit::form_row(self.t("connections.editor.tls.clientCert"), cert, &p)))
                        .child(div().flex_1().min_w(px(160.)).child(kit::form_row(self.t("connections.editor.tls.clientKey"), key, &p))),
                );
            }
            let ssh_on = draft.ssh_on;
            security = security.child(
                div()
                    .id("conn-ssh-toggle")
                    .role(gpui_kit::Role::Switch)
                    .aria_label(self.t("connections.editor.ssh.title"))
                    .aria_selected(ssh_on)
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .cursor_pointer()
                    .child(switch_visual(ssh_on, &p))
                    .child(div().flex().flex_col().child(div().font_weight(FontWeight::MEDIUM).child(self.t("connections.editor.ssh.title"))).child(kit::muted(self.t("ui.conn.sshHint"), &p)))
                    .on_click(act("conn-ssh-toggle", cx)),
            );
            if ssh_on {
                let ssh_host = self.input_for(DraftField::SshHost, "bastion.example.com", false, window, cx);
                let ssh_port = self.input_for(DraftField::SshPort, "22", false, window, cx);
                let ssh_user = self.input_for(DraftField::SshUser, "", false, window, cx);
                let mut auth_items = Vec::new();
                for (kind, key) in [(SshAuthKind::Key, "connections.editor.ssh.authKey"), (SshAuthKind::Password, "connections.editor.ssh.authPassword")] {
                    auth_items.push((
                        SharedString::from(format!("conn-ssh-auth-{key}")),
                        self.t(key),
                        draft.ssh_auth == kind,
                        kit::click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if let Some(editor) = this.model.connections.editor.as_mut() {
                                editor.ssh_auth = kind;
                            }
                            cx.notify();
                        })),
                    ));
                }
                let mut ssh = div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .when(!narrow, |this| this.pl(px(46.)))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(12.))
                            .child(div().flex_1().min_w(px(160.)).child(kit::form_row(self.t("ui.conn.sshHost"), ssh_host, &p)))
                            .child(div().w(px(90.)).child(kit::form_row(self.t("connections.editor.ssh.port"), ssh_port, &p))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(12.))
                            .items_end()
                            .child(div().flex_1().min_w(px(160.)).child(kit::form_row(self.t("ui.conn.sshUser"), ssh_user, &p)))
                            .child(kit::form_row(self.t("connections.editor.ssh.authLabel"), kit::segmented_sized(auth_items, &p, m.control_h), &p)),
                    );
                if draft.ssh_auth == SshAuthKind::Key {
                    let ssh_key = self.input_for(DraftField::SshKey, "~/.ssh/id_ed25519", false, window, cx);
                    let passphrase = self.input_for(DraftField::Passphrase, &t(self, "ui.conn.optional"), true, window, cx);
                    ssh = ssh.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(12.))
                            .child(div().flex_1().min_w(px(160.)).child(kit::form_row(self.t("connections.editor.ssh.keyFile"), ssh_key, &p)))
                            .child(div().flex_1().min_w(px(160.)).child(kit::form_row(self.t("ui.conn.passphrase"), passphrase, &p))),
                    );
                } else {
                    let ssh_password = self.input_for(DraftField::SshPassword, "", true, window, cx);
                    ssh = ssh.child(kit::form_row(self.t("ui.conn.sshPassword"), ssh_password.mask_toggle(), &p));
                }
                security = security.child(ssh);
            }
        }

        // 校验与连接结果。
        let mut notes = div().flex().flex_col().gap(px(6.));
        for issue in &self.model.connections.issues {
            notes = notes.child(banner(IconName::CircleAlert, self.t(issue.key), p.danger, p.danger_soft));
        }
        if let Some(key) = self.model.connections.message_key {
            let ok = key == "connections.status.connected" || key == "connections.editor.test.ok";
            let text = if self.model.connections.message_detail.is_empty() {
                self.model.text(key)
            } else {
                format!("{} · {}", self.model.text(key), self.model.connections.message_detail)
            };
            notes = notes.child(if ok {
                banner(IconName::CircleCheck, SharedString::from(text), p.ok, p.ok_soft)
            } else {
                banner(IconName::CircleAlert, SharedString::from(text), p.danger, p.danger_soft)
            });
        }

        let status = draft.id.as_ref().and_then(|id| self.model.connections.status.get(id)).copied().unwrap_or(LinkStatus::Disconnected);
        // 窄窗口里操作换行而不是被右侧裁掉；主要动作仍在末尾、优先可达。
        let mut footer = div().flex().flex_wrap().items_center().gap(px(8.)).pt(px(4.));
        if existing {
            footer = footer
                .child(kit::tool_button("conn-delete", Some(IconName::Trash), self.t("connections.page.menuDelete"), act("conn-delete", cx)))
                .child(kit::tool_button("conn-duplicate", Some(IconName::Copy), self.t("common.copy"), act("conn-duplicate", cx)));
        }
        footer = footer
            .child(div().flex_1())
            .child(kit::secondary_button("conn-test", Some(IconName::Zap), self.t("connections.editor.test.button"), act("conn-test", cx)))
            .child(kit::secondary_button("conn-save", Some(IconName::Save), self.t("common.save"), act("conn-save", cx)));
        if existing && status == LinkStatus::Connected {
            footer = footer
                .child(kit::secondary_button("conn-disconnect", Some(IconName::Unplug), self.t("connections.page.menuDisconnect"), act("conn-disconnect", cx)))
                .child(kit::primary_button("conn-open-query", Some(IconName::SquareTerminal), self.t("shell.command.newQuery"), cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.perform("activity-workspace", cx);
                    this.perform("new-query", cx);
                })));
        } else if existing {
            footer = footer.child(kit::primary_button("conn-connect", Some(IconName::Plug), self.t("connections.page.connect"), act("conn-connect", cx)));
        }

        let heading = if existing { SharedString::from(draft.name.clone()) } else { self.t("connections.editor.newTitle") };
        let form_pad = if narrow { 16. } else { m.page_pad };
        // 表单可滚动，但测试 / 保存 / 连接固定在底部，高度不足时也不用先滚到底。
        div()
            .id("conn-form")
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("conn-form-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .child(
                        div()
                            .max_w(px(720.))
                            .min_w(px(0.))
                            .mx_auto()
                            .px(px(form_pad))
                            .py(px(m.page_pad_y))
                            .flex()
                            .flex_col()
                            .gap(px(18.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(12.))
                                    .child(kit::engine_badge(engine.as_str(), 36.))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .min_w(px(0.))
                                            .child(div().id("conn-form-title").truncate().tooltip_text(heading.clone()).text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(heading))
                                            .child(kit::muted(SharedString::from(kit::engine_label(engine.as_str())), &p)),
                                    ),
                            )
                            .child(form_section(self.t("ui.conn.pickEngine"), engine_row, &p))
                            .child(form_section(self.t("ui.conn.general"), general, &p))
                            .when(!file_based, |this| this.child(form_section(self.t("ui.conn.security"), security, &p)))
                            .child(notes),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(form_pad))
                    .py(px(8.))
                    .border_t_1()
                    .border_color(p.line)
                    .bg(p.surface)
                    .child(div().max_w(px(720.)).min_w(px(0.)).mx_auto().child(footer)),
            )
            .into_any_element()
    }

    fn prompt_draft_file(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(self.t("connections.validate.chooseFile")),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(path) = paths.into_iter().next() {
                    cx.update(|cx| {
                        let _ = this.update(cx, |view, cx| {
                            if let Ok(path) = crate::app::accept_open_path(&path) {
                                if let Some(editor) = view.model.connections.editor.as_mut() {
                                    if editor.name.trim().is_empty() {
                                        editor.name = path.file_stem().and_then(|name| name.to_str()).unwrap_or("sqlite").to_owned();
                                    }
                                }
                                view.model.set_field(DraftField::File, path.display().to_string());
                            }
                            cx.notify();
                        });
                    });
                }
            }
        })
        .detach();
    }

    // ------------------------------------------------------------ 历史

    fn history_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let section = self.model.history.section;
        let search = self.input_for(DraftField::HistorySearch, &self.model.text("ui.history.search"), false, window, cx);
        let save_name = self.input_for(DraftField::SaveName, &self.model.text("ui.history.saveAs"), false, window, cx);
        let rename_input = self.input_for(DraftField::RenameName, &self.model.text("history.page.rename"), false, window, cx);
        let tabs = kit::segmented_sized(
            vec![
                (
                    SharedString::from("history-runs"),
                    self.t("history.page.runs"),
                    section == HistorySection::Runs,
                    kit::click(act("history", cx)),
                ),
                (
                    SharedString::from("history-saved"),
                    self.t("history.page.saved"),
                    section == HistorySection::Saved,
                    kit::click(act("saved-queries", cx)),
                ),
            ],
            &p,
            self.metrics().control_h,
        );
        let filter_status = self.model.history.filter_status.clone();
        let status_seg = kit::segmented_sized(
            vec![
                (
                    SharedString::from("history-status-all"),
                    self.t("history.filter.all"),
                    filter_status.is_empty(),
                    kit::click(act("history-status-all", cx)),
                ),
                (
                    SharedString::from("history-status-ok"),
                    self.t("history.status.ok"),
                    filter_status == "ok",
                    kit::click(act("history-status-ok", cx)),
                ),
                (
                    SharedString::from("history-status-failed"),
                    self.t("history.status.failed"),
                    filter_status == "failed",
                    kit::click(act("history-status-failed", cx)),
                ),
            ],
            &p,
            self.metrics().control_h,
        );
        let filter_connection = self.model.history.filter_connection.clone();
        let connection_label = if filter_connection.is_empty() {
            self.t("history.page.filterConnection")
        } else {
            self.model
                .connections
                .rows
                .iter()
                .find(|row| row.saved.id == filter_connection)
                .map(|row| SharedString::from(row.saved.name.clone()))
                .unwrap_or_else(|| SharedString::from(filter_connection.clone()))
        };
        let has_filters = self.model.history.has_filters();
        let mono = cx.theme().mono_font_family.clone();
        let mut list = div()
            .id("history-list")
            .flex()
            .flex_col()
            .gap(px(6.))
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(m.page_pad))
            .pb(px(m.page_pad_y));
        if let Some(key) = self.model.history.failure_key {
            let detail = self.model.history.failure_detail.clone();
            list = list.child(
                div()
                    .id("history-failure")
                    .role(gpui_kit::Role::Status)
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(p.danger)
                    .bg(p.danger_soft)
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(kit::icon(IconName::CircleAlert, 14., p.danger))
                            .child(div().font_weight(FontWeight::MEDIUM).text_color(p.danger).child(self.t(key))),
                    )
                    .when(!detail.is_empty(), |this| this.child(div().text_size(px(12.)).text_color(p.text_muted).child(SharedString::from(detail)))),
            );
        }
        if section == HistorySection::Runs {
            let runs: Vec<_> = self.model.history.filtered_runs().into_iter().take(200).cloned().collect();
            if runs.is_empty() {
                if self.model.history.runs.is_empty() {
                    // 第一次为空：直接给出能产生历史记录的动作。
                    list = list.child(
                        kit::empty_state(IconName::Clock, self.t("ui.history.empty"), self.t("history.page.loadOnlySql"), &p)
                            .child(kit::primary_button("history-empty-query", Some(IconName::SquareTerminal), self.t("shell.command.newQuery"), act("new-query", cx))),
                    );
                } else {
                    list = list.child(
                        kit::empty_state(IconName::Search, self.t("history.page.noMatch"), self.t("history.filter.reset"), &p)
                            .child(kit::secondary_button("history-empty-reset", Some(IconName::X), self.t("history.filter.reset"), act("history-filter-reset", cx))),
                    );
                }
            }
            for entry in runs {
                let id = entry.id.clone();
                let selected = self.model.history.selected_run.as_deref() == Some(id.as_str());
                let ok = entry.status == "ok" || entry.status == "success" || entry.status == "done";
                let elapsed = entry.elapsed_ms.map(|ms| if ms >= 1000 { format!("{:.1} s", ms as f64 / 1000.) } else { format!("{ms} ms") }).unwrap_or_default();
                let when = entry.started_at.replace('T', " ").chars().take(19).collect::<String>();
                let select = id.clone();
                let menu = id.clone();
                let load = id.clone();
                let delete = id.clone();
                list = list.child(
                    div()
                        .id(SharedString::from(format!("run-{id}")))
                        .group("run-row")
                        .role(gpui_kit::Role::ListItem)
                        .aria_label(SharedString::from(entry.sql.clone()))
                        .aria_selected(selected)
                        .p(px(m.row_pad_y + 5.))
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .rounded(px(10.))
                        .border_1()
                        .cursor_pointer()
                        .when(selected, |this| this.border_color(p.accent).bg(p.accent_soft))
                        .when(!selected, |this| this.border_color(p.line).hover(|style| style.border_color(p.line_strong)))
                        .child(
                            div()
                                .min_w(px(0.))
                                .font_family(mono.clone())
                                .text_size(px(12.5))
                                .text_color(p.text)
                                .when(!selected, |this| this.max_h(px(40.)).truncate())
                                .when(selected, |this| this.max_h(px(120.)).overflow_hidden())
                                .child(SharedString::from(if selected { entry.sql.clone() } else { clip(&entry.sql, 240) })),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .text_size(px(11.5))
                                .text_color(p.text_faint)
                                .child(kit::status_dot(if ok { p.ok } else { p.danger }))
                                .child(kit::engine_badge(&entry.engine, 14.))
                                .child(
                                    div()
                                        .id(SharedString::from(format!("run-target-{id}")))
                                        .max_w(px(240.))
                                        .truncate()
                                        .tooltip_text(SharedString::from(if entry.database_name.is_empty() { entry.connection_name.clone() } else { format!("{} / {}", entry.connection_name, entry.database_name) }))
                                        .child(SharedString::from(if entry.database_name.is_empty() { entry.connection_name.clone() } else { format!("{} / {}", entry.connection_name, entry.database_name) })),
                                )
                                .child(SharedString::from(when))
                                .when(!elapsed.is_empty(), |this| this.child(SharedString::from(elapsed)))
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .flex()
                                        .gap(px(4.))
                                        .when(!selected, |this| this.invisible().group_hover("run-row", |style| style.visible()))
                                        .child(kit::tool_button(SharedString::from(format!("run-copy-{id}")), Some(IconName::Copy), self.t("common.copy"), cx.listener({
                                            let id = id.clone();
                                            move |this, _: &ClickEvent, _, cx| {
                                                this.model.history.selected_run = Some(id.clone());
                                                this.perform("history-copy", cx);
                                            }
                                        })).xsmall())
                                        .child(kit::tool_button(SharedString::from(format!("run-del-{id}")), Some(IconName::Trash), self.t("ui.history.delete"), cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            this.model.history.selected_run = Some(delete.clone());
                                            this.perform("history-delete", cx);
                                        })).xsmall())
                                        .child(kit::secondary_button(SharedString::from(format!("run-load-{id}")), Some(IconName::SquarePen), self.t("ui.history.load"), cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            this.model.history.selected_run = Some(load.clone());
                                            this.perform("history-load", cx);
                                        })).xsmall()),
                                ),
                        )
                        .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.menu_at = event.position;
                            this.model.history.selected_run = Some(menu.clone());
                            this.perform("menu-history", cx);
                        }))
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            this.model.history.selected_run = Some(select.clone());
                            this.model.history.preview_open = true;
                            if event.click_count() >= 2 {
                                this.perform("history-load", cx);
                            }
                            cx.notify();
                        })),
                );
            }
        } else {
            let saved: Vec<_> = self.model.history.filtered_saved().into_iter().take(200).cloned().collect();
            if saved.is_empty() {
                if self.model.history.saved.is_empty() {
                    list = list.child(
                        kit::empty_state(IconName::Bookmark, self.t("ui.history.emptySaved"), self.t("history.savedPanel.openNote"), &p)
                            .child(kit::primary_button("history-empty-saved-query", Some(IconName::SquareTerminal), self.t("shell.command.newQuery"), act("new-query", cx))),
                    );
                } else {
                    list = list.child(
                        kit::empty_state(IconName::Search, self.t("history.page.noMatchSaved"), self.t("history.filter.reset"), &p)
                            .child(kit::secondary_button("history-empty-saved-reset", Some(IconName::X), self.t("history.filter.reset"), act("history-filter-reset", cx))),
                    );
                }
            }
            for item in saved {
                let id = item.id.clone();
                let selected = self.model.history.selected_saved.as_deref() == Some(id.as_str());
                let select = id.clone();
                let copy = id.clone();
                let load = id.clone();
                let rename = id.clone();
                let delete = id.clone();
                list = list.child(
                    div()
                        .id(SharedString::from(format!("saved-{id}")))
                        .group("saved-row")
                        .role(gpui_kit::Role::ListItem)
                        .aria_label(SharedString::from(item.name.clone()))
                        .aria_selected(selected)
                        .p(px(m.row_pad_y + 5.))
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .rounded(px(10.))
                        .border_1()
                        .cursor_pointer()
                        .when(selected, |this| this.border_color(p.accent).bg(p.accent_soft))
                        .when(!selected, |this| this.border_color(p.line).hover(|style| style.border_color(p.line_strong)))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(kit::icon(IconName::Bookmark, 14., p.accent))
                                .child(div().id(SharedString::from(format!("saved-name-{id}"))).flex_1().min_w(px(0.)).truncate().tooltip_text(SharedString::from(item.name.clone())).font_weight(FontWeight::MEDIUM).child(SharedString::from(item.name.clone())))
                                .child(kit::engine_badge(&item.engine, 14.))
                                .child(
                                    div()
                                        .id(SharedString::from(format!("saved-client-{id}")))
                                        .max_w(px(180.))
                                        .truncate()
                                        .tooltip_text(SharedString::from(item.connection_name.clone()))
                                        .text_size(px(11.5))
                                        .text_color(p.text_faint)
                                        .child(SharedString::from(item.connection_name.clone())),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap(px(4.))
                                        .when(!selected, |this| this.invisible().group_hover("saved-row", |style| style.visible()))
                                        .child(kit::tool_button(SharedString::from(format!("saved-copy-{id}")), Some(IconName::Copy), self.t("common.copy"), cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            this.model.history.section = HistorySection::Saved;
                                            this.model.history.selected_saved = Some(copy.clone());
                                            this.perform("history-copy", cx);
                                        })).xsmall())
                                        .child(kit::tool_button(SharedString::from(format!("saved-load-{id}")), Some(IconName::SquarePen), self.t("ui.history.load"), cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            this.model.history.section = HistorySection::Saved;
                                            this.model.history.selected_saved = Some(load.clone());
                                            this.perform("history-load", cx);
                                        })).xsmall())
                                        .child(kit::tool_button(SharedString::from(format!("saved-rename-{id}")), Some(IconName::Pencil), self.t("history.page.rename"), cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            this.model.history.section = HistorySection::Saved;
                                            this.model.history.selected_saved = Some(rename.clone());
                                            this.perform("history-rename", cx);
                                        })).xsmall())
                                        .child(kit::icon_button(SharedString::from(format!("saved-del-{id}")), IconName::Trash, self.t("ui.history.delete"), cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            this.model.history.section = HistorySection::Saved;
                                            this.model.history.selected_saved = Some(delete.clone());
                                            this.perform("saved-delete", cx);
                                        })).xsmall()),
                                ),
                        )
                        .child(div().min_w(px(0.)).font_family(mono.clone()).text_size(px(12.)).text_color(p.text_muted).max_h(px(36.)).truncate().child(SharedString::from(clip(&item.sql, 200))))
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            this.model.history.section = HistorySection::Saved;
                            this.model.history.selected_saved = Some(select.clone());
                            this.model.history.preview_open = true;
                            if event.click_count() >= 2 {
                                this.perform("history-load", cx);
                            }
                            cx.notify();
                        })),
                );
            }
        }

        let can_save = self.model.active_tab().is_some_and(|tab| tab.kind.is_query());
        let selected_any = if section == HistorySection::Saved {
            self.model.history.selected_saved.is_some()
        } else {
            self.model.history.selected_run.is_some()
        };
        let preview_open = self.model.history.preview_open;
        let toolbar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(10.))
            .child(tabs)
            .child(div().w(px(260.)).child(search.prefix(kit::icon(IconName::Search, 13., p.text_faint)).cleanable(true)))
            .when(section == HistorySection::Runs, |this| this.child(status_seg))
            .child(kit::tool_button("history-filter-connection", Some(IconName::Funnel), connection_label, act("history-filter-connection", cx)))
            .when(has_filters, |this| this.child(kit::tool_button("history-filter-reset", Some(IconName::X), self.t("history.filter.reset"), act("history-filter-reset", cx))))
            .when(selected_any && !preview_open, |this| this.child(kit::tool_button("history-preview-open", Some(IconName::Eye), self.t("history.page.sqlPreview"), act("history-detail", cx))))
            .child(div().flex_1())
            .when(section == HistorySection::Runs, |this| {
                this.child(kit::tool_button("history-clear", Some(IconName::Trash), self.t("ui.history.clear"), act("history-clear", cx)))
                    .child(kit::tool_button("history-refresh", Some(IconName::RefreshCw), self.t("redis.tab.refresh"), act("history-refresh", cx)))
            })
            .when(section == HistorySection::Saved && can_save, |this| {
                this.child(div().w(px(220.)).child(save_name)).child(kit::primary_button("history-save-named", Some(IconName::Save), self.t("history.savedPanel.save"), act("history-save-named", cx)))
            });
        let rename_bar = if section == HistorySection::Saved && self.model.history.rename_open {
            div()
                .px(px(m.page_pad))
                .pb(px(10.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().w(px(260.)).child(rename_input))
                .child(kit::primary_button("history-rename-confirm", Some(IconName::Check), self.t("history.page.renameSave"), act("history-rename-confirm", cx)))
                .child(kit::secondary_button("history-rename-cancel", Some(IconName::X), self.t("common.cancel"), act("history-rename-cancel", cx)))
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let known = self.model.known_ids();
        // 窄窗口把预览放到列表上方；并排会挤压卡片操作，长错误/元数据会互相覆盖。
        let narrow = history_stacks_preview(self.model.layout().content_width as f32);
        let has_preview = preview_open && self.model.history.preview_plan(&known).is_some();
        let preview = if has_preview {
            self.history_preview(&known, &p, narrow, cx)
        } else {
            div().into_any_element()
        };
        let body = if narrow {
            let mut column = div().flex().flex_1().min_w(px(0.)).min_h(px(0.)).flex_col();
            if has_preview {
                // 与列表分享剩余高度，最高 300；空预览不占位，矮窗口列表不会被压没。
                column = column.child(
                    div()
                        .flex_1()
                        .max_h(px(300.))
                        .min_h(px(0.))
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .child(preview),
                );
            }
            column.child(list)
        } else {
            div().flex().flex_1().min_w(px(0.)).min_h(px(0.)).child(list).child(preview)
        };
        div()
            .id("history-page")
            .role(gpui_kit::Role::Group)
            .aria_label(self.t("history.page.titleRuns"))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(m.page_pad))
                    .pt(px(20.))
                    .pb(px(14.))
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .child(div().flex().flex_col().gap(px(2.)).child(kit::title(self.t("history.page.titleRuns"), 17., &p)).child(kit::muted(self.t("history.page.loadOnlySql"), &p)))
                    .child(toolbar),
            )
            .child(rename_bar)
            .child(body)
            .into_any_element()
    }

    fn history_preview(&self, known: &std::collections::BTreeSet<String>, p: &Palette, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(plan) = self.model.history.preview_plan(known) else {
            return div().into_any_element();
        };
        let mono = cx.theme().mono_font_family.clone();
        let status = if self.model.history.section == HistorySection::Runs {
            self.model.history.selected_run_entry().map(|entry| entry.status.clone())
        } else {
            None
        };
        let target = if plan.needs_target {
            self.model.text("history.plan.deletedNotice")
        } else if plan.database.as_deref().is_some_and(|database| !database.is_empty()) {
            format!("{} / {}", plan.connection_name, plan.database.unwrap_or_default())
        } else {
            plan.connection_name.clone()
        };
        // 预览给列表留出至少一列卡片的空间，窄窗口不再把 SQL 挤出去。
        let content = self.model.layout().content_width as f32;
        let preview_width = (content - 220.).clamp(260., 380.);
        let status_label = status.map(|status| {
            let ok = status == "ok" || status == "success" || status == "done";
            (if ok { self.t("history.status.ok") } else { self.t("history.status.failed") }, ok)
        });
        div()
            .id("history-preview")
            .role(gpui_kit::Role::Group)
            .flex_none()
            .min_w(px(0.))
            .min_h(px(0.))
            .when(narrow, |this| this.h_full().w_full().border_t_1())
            .when(!narrow, |this| this.w(px(preview_width)).border_l_1())
            .border_color(p.line)
            .p(px(if narrow { 12. } else { 18. }))
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(kit::title(self.t("history.page.sqlPreview"), 13., p))
                    .child(div().flex_1())
                    .child(kit::icon_button("history-preview-close", IconName::X, self.t("history.page.previewClose"), act("history-preview-close", cx))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(kit::engine_badge(&plan.engine, 14.))
                    .child(div().text_size(px(11.5)).text_color(if plan.needs_target { p.warn } else { p.text_muted }).child(SharedString::from(target))),
            )
            .when_some(status_label, |this, (label, ok)| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(11.5))
                        .text_color(if ok { p.ok } else { p.danger })
                        .child(kit::status_dot(if ok { p.ok } else { p.danger }))
                        .child(label),
                )
            })
            .child(
                div()
                    .id("history-preview-sql")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .rounded(px(8.))
                    .bg(p.chrome)
                    .p(px(12.))
                    .font_family(mono)
                    .text_size(px(12.5))
                    .text_color(p.text)
                    .child(SharedString::from(plan.sql)),
            )
            .child(div().text_size(px(11.5)).text_color(p.text_faint).child(self.t("history.page.loadOnlySql")))
            .into_any_element()
    }

    // ------------------------------------------------------------ 设置

    fn settings_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let s = self.model.settings.clone();
        let theme = self.model.shell.theme;
        let seg = |this: &mut Self, items: &[(&str, &str, bool)], cx: &mut Context<Self>| {
            let built = items
                .iter()
                .map(|(id, key, selected)| (SharedString::from(id.to_string()), this.t(key), *selected, kit::click(act(id.to_string(), cx))))
                .collect::<Vec<_>>();
            kit::segmented_sized(built, &Palette::of(cx), m.control_h)
        };
        let theme_seg = seg(
            self,
            &[
                ("theme-light", "shell.theme.light", theme == ThemePreference::Light),
                ("theme-dark", "shell.theme.dark", theme == ThemePreference::Dark),
                ("theme-system", "shell.theme.system", theme == ThemePreference::System),
            ],
            cx,
        );
        let density_seg = seg(
            self,
            &[
                ("density-compact", "shell.settings.densityCompact", s.density == "compact"),
                ("density-standard", "shell.settings.densityStandard", s.density == "standard"),
                ("density-relaxed", "shell.settings.densityRelaxed", s.density == "relaxed"),
            ],
            cx,
        );
        let locale_seg = seg(
            self,
            &[("locale-zh", "common.localeZh", self.model.shell.locale == crate::app::Locale::ZhCn), ("locale-en", "common.localeEn", self.model.shell.locale == crate::app::Locale::En)],
            cx,
        );
        let timeout_seg = seg(
            self,
            &[
                ("timeout-0", "ui.settings.timeoutOff", s.query_timeout_sec == 0),
                ("timeout-15", "ui.settings.timeout15", s.query_timeout_sec == 15),
                ("timeout-60", "ui.settings.timeout60", s.query_timeout_sec == 60),
            ],
            cx,
        );
        let null_seg = seg(
            self,
            &[("null-NULL", "ui.settings.nullWord", s.null_copy == "NULL"), ("null-empty", "ui.settings.nullEmpty", s.null_copy == "empty"), ("null-N", "ui.settings.nullN", s.null_copy == "N")],
            cx,
        );
        let font = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(kit::icon_button("font-down", IconName::Minus, self.t("ui.settings.smaller"), act("font-down", cx)).outline())
            .child(div().w(px(44.)).text_center().font_weight(FontWeight::MEDIUM).child(SharedString::from(format!("{} px", s.editor_font))))
            .child(kit::icon_button("font-up", IconName::Plus, self.t("ui.settings.larger"), act("font-up", cx)).outline());
        let editor_theme = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(div().font_weight(FontWeight::MEDIUM).child(self.t(crate::app::views::settings::editor_theme_key(&s.editor_theme))))
            .child(kit::icon_button("editor-theme-prev", IconName::ChevronLeft, self.t("ui.settings.change"), act("editor-theme-prev", cx)).outline())
            .child(kit::icon_button("editor-theme-next", IconName::ChevronRight, self.t("ui.settings.change"), act("editor-theme-next", cx)).outline());
        let stepper = |this: &Self, id: &'static str, value: String, cx: &mut Context<Self>| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(kit::icon_button(SharedString::from(format!("{id}-down")), IconName::Minus, this.t("ui.settings.smaller"), act(format!("{id}-down"), cx)).outline())
                .child(div().min_w(px(56.)).text_center().font_weight(FontWeight::MEDIUM).child(SharedString::from(value)))
                .child(kit::icon_button(SharedString::from(format!("{id}-up")), IconName::Plus, this.t("ui.settings.increase"), act(format!("{id}-up"), cx)).outline())
        };
        let page_size = stepper(self, "page-size", s.page_size.to_string(), cx);
        let preview = stepper(self, "preview", format!("{} KB", s.preview_kb), cx);
        let object_page = stepper(self, "object-page", s.object_page.to_string(), cx);
        let updates = div()
            .id("toggle-updates")
            .role(gpui_kit::Role::Switch)
            .aria_label(self.t("shell.update.autoCheck"))
            .aria_selected(s.auto_check_updates)
            .cursor_pointer()
            .child(switch_visual(s.auto_check_updates, &p))
            .on_click(act("toggle-updates", cx));

        let group = |title: SharedString, rows: Vec<(SharedString, Option<SharedString>, AnyElement)>, p: &Palette| {
            let mut card = kit::card(p).flex().flex_col();
            let count = rows.len();
            for (index, (label, hint, control)) in rows.into_iter().enumerate() {
                card = card.child(
                    div()
                        .px(px(16.))
                        .py(px(m.row_pad_y + 5.))
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(16.))
                        .when(index + 1 < count, |this| this.border_b_1().border_color(p.line_soft))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(180.))
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(div().font_weight(FontWeight::MEDIUM).child(label))
                                .when_some(hint, |this, hint| this.child(div().text_size(px(12.)).text_color(p.text_faint).child(hint))),
                        )
                        .child(control),
                );
            }
            div().flex().flex_col().gap(px(8.)).child(kit::eyebrow(title, p)).child(card)
        };

        div()
            .id("settings-page")
            .test_support()
            .role(gpui_kit::Role::Group)
            .aria_label(self.t("shell.settings.title"))
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .max_w(px(760.))
                    .min_w(px(0.))
                    .mx_auto()
                    .px(px(m.page_pad))
                    .py(px(m.page_pad_y))
                    .flex()
                    .flex_col()
                    .gap(px(m.page_pad - 4.))
                    .child(kit::title(self.t("shell.settings.title"), 20., &p))
                    .child(group(
                        self.t("ui.settings.appearance"),
                        vec![
                            (self.t("shell.settings.theme"), None, theme_seg),
                            (self.t("shell.settings.density"), None, density_seg),
                            (self.t("shell.settings.language"), None, locale_seg),
                        ],
                        &p,
                    ))
                    .child(group(
                        self.t("ui.settings.editor"),
                        vec![
                            (self.t("shell.settings.editorFont"), None, font.into_any_element()),
                            (self.t("shell.settings.editorTheme"), Some(self.t("shell.settings.editorThemeAuto")), editor_theme.into_any_element()),
                        ],
                        &p,
                    ))
                    .child(group(
                        self.t("ui.settings.data"),
                        vec![
                            (self.t("shell.settings.rowsPerPage"), None, page_size.into_any_element()),
                            (self.t("shell.settings.previewLimit"), None, preview.into_any_element()),
                            (self.t("shell.settings.objectPage"), None, object_page.into_any_element()),
                            (self.t("shell.settings.nullCopy"), None, null_seg),
                            (self.t("shell.settings.timeout"), None, timeout_seg),
                        ],
                        &p,
                    ))
                    .child(group(self.t("ui.settings.general"), vec![(self.t("shell.update.autoCheck"), None, updates.into_any_element())], &p))
                    .child(div().text_size(px(11.5)).text_color(p.text_faint).child(SharedString::from(format!("RillSQL {}", env!("CARGO_PKG_VERSION"))))),
            )
            .into_any_element()
    }

    // ------------------------------------------------------------ 浮层

    fn palette_layer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let help = self.model.shell.help_open && !self.model.shell.palette_open;
        let mut panel = div()
            .id("palette")
            .test_support()
            .role(gpui_kit::Role::Dialog)
            .aria_label(self.t(if help { "shell.command.help" } else { "shell.palette.title" }))
            .aria_expanded(self.model.shell.palette_open)
            .key_context("Palette")
            .occlude()
            .w_full()
            .max_w(px(560.))
            .max_h(px(460.))
            .flex()
            .flex_col()
            .bg(p.raised)
            .border_1()
            .border_color(p.line)
            .rounded(px(12.))
            .shadow_lg()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        if help {
            let mut list = div().id("help-list").flex().flex_col().overflow_y_scroll().p(px(8.));
            let mut group = "";
            for shortcut in crate::app::model::SHORTCUTS {
                if shortcut.group != group {
                    group = shortcut.group;
                    list = list.child(div().px(px(8.)).pt(px(10.)).pb(px(4.)).child(kit::eyebrow(self.t(&format!("ui.help.{group}")), &p)));
                }
                list = list.child(
                    div()
                        .px(px(8.))
                        .h(px(m.menu_row_h))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(div().flex_1().truncate().child(self.t(shortcut.label_key)))
                        .child(kit::kbd(kit::shortcut(&shortcut.chord.replace("Mod+", "Ctrl+")), &p)),
                );
            }
            panel = panel
                .child(
                    div()
                        .px(px(16.))
                        .h(px(48.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(p.line)
                        .child(kit::title(self.t("shell.command.help"), 14., &p))
                        .child(kit::icon_button("help-close", IconName::X, self.t("common.close"), cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.model.shell.close_overlay();
                            cx.notify();
                        }))),
                )
                .child(list);
        } else {
            let input = self.input_for(DraftField::PaletteQuery, &self.model.text("ui.palette.placeholder"), false, window, cx);
            if !self.palette_focused {
                self.palette_focused = true;
                self.fields.focus(DraftField::PaletteQuery, window, cx);
            }
            let entries = self.model.palette_entries();
            let mut list = div().id("palette-list").flex().flex_col().overflow_y_scroll().p(px(6.)).max_h(px(380.));
            if entries.is_empty() {
                let error = self.model.catalog_error.clone();
                let mut empty = div()
                    .p(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .text_color(p.text_faint)
                    .child(self.t(self.model.palette_empty_key()));
                if self.model.shell.palette_tables_only && !error.is_empty() {
                    empty = empty
                        .child(div().id("palette-error").text_size(px(11.5)).truncate().tooltip_text(SharedString::from(error.clone())).child(SharedString::from(error)))
                        .child(kit::tool_button(
                            "palette-tree-retry",
                            Some(IconName::RefreshCw),
                            self.t("common.retry"),
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.model.shell.close_overlay();
                                this.perform("refresh-tree", cx);
                            }),
                        ));
                }
                // 面板为空时给一个能立刻继续的动作，而不是只留解释。
                empty = empty.child(kit::tool_button(
                    "palette-new-query",
                    Some(IconName::SquareTerminal),
                    self.t("shell.command.newQuery"),
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.model.shell.close_overlay();
                        this.perform("new-query", cx);
                    }),
                ));
                list = list.child(empty);
            }
            for (index, entry) in entries.into_iter().take(40).enumerate() {
                let selected = index == self.model.palette_index;
                let id = entry.id.clone();
                let chord = crate::app::COMMANDS.iter().find(|command| command.id == entry.id).and_then(|command| command.chord);
                let icon = palette_icon(&entry.id);
                let group = match entry.group {
                    "connection" => self.t("shell.paletteGroup.connection"),
                    "table" => self.t("shell.paletteGroup.table"),
                    _ => SharedString::from(""),
                };
                let detail = entry.detail.clone();
                list = list.child(
                    div()
                        .id(SharedString::from(format!("pal-{index}")))
                        .role(gpui_kit::Role::MenuItem)
                        .aria_label(SharedString::from(entry.label.clone()))
                        .aria_selected(selected)
                        .h(px(m.row_h + 2.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .rounded(px(7.))
                        .cursor_pointer()
                        .when(selected, |this| this.bg(p.accent_soft).text_color(p.accent_text))
                        .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
                        .child(kit::icon(icon, 15., if selected { p.accent_text } else { p.text_muted }))
                        .child(div().flex_1().truncate().child(SharedString::from(entry.label)))
                        .when(!detail.is_empty(), |this| this.child(div().max_w(px(160.)).truncate().text_size(px(11.)).text_color(p.text_faint).child(SharedString::from(detail))))
                        .when(!group.is_empty(), |this| this.child(div().text_size(px(11.)).text_color(p.text_faint).child(group)))
                        .when_some(chord, |this, chord| this.child(kit::kbd(kit::shortcut(&chord.replace("Mod+", "Ctrl+")), &p)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.model.shell.close_overlay();
                            this.perform(&id, cx);
                        })),
                );
            }
            panel = panel
                .child(div().p(px(10.)).border_b_1().border_color(p.line).child(input.prefix(kit::icon(IconName::Search, 14., p.text_faint))))
                .child(list);
        }
        backdrop("palette-backdrop", 0.18, cx)
            .flex()
            .flex_col()
            .items_center()
            .px(px(16.))
            .pt(px(72.))
            .child(panel)
            .into_any_element()
    }

    fn context_layer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let Some(menu) = self.model.menu.clone() else {
            return div().into_any_element();
        };
        let width = 232.;
        let viewport = self.model.viewport_width as f32;
        let mut x = f32::from(self.menu_at.x);
        let y = f32::from(self.menu_at.y);
        if x + width > viewport - 8. {
            x = (viewport - width - 8.).max(8.);
        }
        let mut list = div()
            .id("context-menu")
            .role(gpui_kit::Role::Menu)
            .aria_label(self.t(menu.place.label_key()))
            .occlude()
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(width))
            .p(px(4.))
            .flex()
            .flex_col()
            .bg(p.raised)
            .border_1()
            .border_color(p.line)
            .rounded(px(9.))
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        for (index, item) in menu.items.iter().enumerate() {
            if index > 0 && item.danger {
                list = list.child(div().h(px(1.)).my(px(4.)).mx(px(6.)).bg(p.line));
            }
            let id = item.id.to_owned();
            let label = self.t(item.label_key);
            let enabled = item.enabled;
            let selected = index == menu.index;
            let state = if enabled { "enabled" } else { "disabled" };
            let fg = if !enabled {
                p.text_faint
            } else if item.danger {
                p.danger
            } else {
                p.text
            };
            list = list.child(
                div()
                    .id(SharedString::from(format!("ctx-{id}")))
                    .role(gpui_kit::Role::MenuItem)
                    .aria_label(label.clone())
                    .aria_selected(selected)
                    .aria_description(SharedString::from(format!("{state} {}", item.reason_key.unwrap_or(item.id))))
                    .h(px(m.menu_row_h))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .text_color(fg)
                    .when(enabled, |this| this.cursor_pointer())
                    .when(enabled && selected, |this| this.bg(p.hover))
                    .when(enabled, |this| this.hover(|style| style.bg(if item.danger { p.danger_soft } else { p.hover })))
                    .child(div().flex_1().truncate().child(label))
                    .when_some(item.reason_key.filter(|_| !enabled), |this, reason| {
                        this.tooltip_text(self.t(reason))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if !enabled {
                            return;
                        }
                        this.model.menu = None;
                        this.model.shell.focus = this.model.menu_return;
                        this.perform(&id, cx);
                    })),
            );
        }
        backdrop("context-backdrop", 0., cx).child(list).into_any_element()
    }

    fn close_layer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let key = self.model.notice_key.unwrap_or("shell.closeGuard.closeWindow");
        let tab = self.model.close_prompt.as_ref().is_some_and(|prompt| matches!(prompt.scope, super::close::CloseScope::Tab));
        let title = self.t(if tab { "shell.closeGuard.closeTab" } else { "shell.closeGuard.closeWindow" });
        let leave = cx.listener(|this, _: &ClickEvent, _, cx| {
            let window_scope = this.model.close_prompt.as_ref().is_some_and(|prompt| matches!(prompt.scope, super::close::CloseScope::Window));
            this.perform("close-leave", cx);
            if window_scope && this.model.allow_system_close() {
                cx.quit();
            }
        });
        let dialog = dialog_card("close-guard", IconName::TriangleAlert, p.warn, title, self.t(key), &p)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(kit::danger_button("cmd-close-leave", None, self.t("shell.closeGuard.leave"), leave))
                    .child(kit::primary_button("cmd-close-stay", None, self.t("shell.closeGuard.stay"), act("close-stay", cx))),
            );
        backdrop("close-backdrop", 0.32, cx).flex().items_center().justify_center().px(px(20.)).child(dialog).into_any_element()
    }

    fn delete_layer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let dialog = dialog_card("delete-connection", IconName::Trash, p.danger, self.t("connections.deleteDialog.title"), self.t("connections.deleteDialog.willKeepDb"), &p).child(
            div()
                .flex()
                .justify_end()
                .gap(px(8.))
                .child(kit::secondary_button("cmd-conn-delete-cancel", None, self.t("common.cancel"), act("conn-delete-cancel", cx)))
                .child(kit::danger_button("cmd-conn-delete-confirm", Some(IconName::Trash), self.t("connections.deleteDialog.confirm"), act("conn-delete-confirm", cx))),
        );
        backdrop("delete-backdrop", 0.32, cx).flex().items_center().justify_center().px(px(20.)).child(dialog).into_any_element()
    }

    fn clear_history_layer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let dialog = dialog_card("clear-history", IconName::Trash, p.danger, self.t("ui.history.clear"), self.t("history.page.clearConfirm"), &p).child(
            div()
                .flex()
                .justify_end()
                .gap(px(8.))
                .child(kit::secondary_button("cmd-history-clear-cancel", None, self.t("common.cancel"), cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.model.history.confirm_clear = false;
                    this.model.notice_key = None;
                    cx.notify();
                })))
                .child(kit::danger_button("cmd-history-clear-confirm", Some(IconName::Trash), self.t("ui.history.clear"), act("history-clear", cx))),
        );
        backdrop("clear-backdrop", 0.32, cx).flex().items_center().justify_center().px(px(20.)).child(dialog).into_any_element()
    }

    fn delete_saved_layer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let dialog = dialog_card("delete-saved", IconName::Trash, p.danger, self.t("history.page.saved"), self.t("history.page.deleteSavedConfirm"), &p).child(
            div()
                .flex()
                .justify_end()
                .gap(px(8.))
                .child(kit::secondary_button("cmd-saved-delete-cancel", None, self.t("common.cancel"), act("saved-delete-cancel", cx)))
                .child(kit::danger_button("cmd-saved-delete-confirm", Some(IconName::Trash), self.t("ui.history.delete"), act("saved-delete", cx))),
        );
        backdrop("delete-saved-backdrop", 0.32, cx).flex().items_center().justify_center().px(px(20.)).child(dialog).into_any_element()
    }

    fn exchange_layer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let panel = div()
            .id("exchange-drawer")
            .occlude()
            .absolute()
            .top(px(m.topbar_h))
            .bottom(px(m.status_h))
            .right_0()
            .w(px(520.0_f32.min(self.model.viewport_width as f32 - 60.)))
            .flex()
            .flex_col()
            .bg(p.surface)
            .border_l_1()
            .border_color(p.line)
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_none()
                    .h(px(m.tabs_h + 8.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(p.line)
                    .child(div().flex().items_center().gap(px(8.)).child(kit::icon(IconName::ArrowDownUp, 15., p.accent)).child(kit::title(self.t("export.open"), 14., &p)))
                    .child(kit::icon_button("exchange-close", IconName::X, self.t("common.close"), act("open-exchange", cx))),
            )
            .child(div().id("exchange-scroll").flex_1().min_h(px(0.)).overflow_y_scroll().child(self.exchange.clone()));
        let entity = cx.entity();
        div()
            .id("exchange-backdrop")
            .absolute()
            .inset_0()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                entity.update(cx, |this, cx| {
                    this.model.exchange_open = false;
                    cx.notify();
                });
            })
            .child(panel)
            .into_any_element()
    }
}

// ---------------------------------------------------------------- 小部件

fn brand_mark(size: f32, p: &Palette) -> AnyElement {
    div()
        .flex_none()
        .size(px(size))
        .rounded(px(size * 0.28))
        .bg(p.accent)
        .flex()
        .items_center()
        .justify_center()
        .child(kit::icon(IconName::Database, size * 0.56, gpui_kit::hsla(0., 0., 1., 1.)))
        .into_any_element()
}

/// 活动栏条目的图标、文案与选中态；固定项之外不再画任何按钮。
fn rail_entry(id: &'static str, page: AppPage) -> Option<(IconName, &'static str, bool)> {
    Some(match id {
        "activity-workspace" => (
            IconName::Database,
            "shell.nav.workspace",
            matches!(page, AppPage::Workspace | AppPage::Structure | AppPage::Diagram),
        ),
        "activity-connections" => (IconName::Plug, "shell.nav.connections", page == AppPage::Connections),
        "activity-history" => (IconName::Clock, "shell.nav.history", page == AppPage::History),
        "activity-settings" => (IconName::Settings, "shell.nav.settings", page == AppPage::Settings),
        _ => return None,
    })
}

fn rail_item(id: &'static str, icon: IconName, label: SharedString, active: bool, p: &Palette, cx: &mut Context<Workbench>) -> AnyElement {
    let accent = p.accent;
    div()
        .id(SharedString::from(format!("cmd-{id}")))
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .aria_selected(active)
        .tooltip_text(label)
        .relative()
        .size(px(38.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(9.))
        .cursor_pointer()
        .when(active, |this| this.bg(p.accent_soft))
        .when(!active, |this| this.hover(|style| style.bg(p.hover)))
        .child(kit::icon(icon, 18., if active { p.accent_text } else { p.text_muted }))
        .when(active, |this| this.child(div().absolute().left(px(-7.)).top(px(9.)).w(px(3.)).h(px(20.)).rounded(px(2.)).bg(accent)))
        .on_click(act(id, cx))
        .into_any_element()
}

fn item_icon(kind: WorkItemKind) -> IconName {
    match kind {
        WorkItemKind::Query => IconName::Code,
        WorkItemKind::Table => IconName::Table,
        WorkItemKind::Structure => IconName::Columns3,
        WorkItemKind::Admin => IconName::Gauge,
        WorkItemKind::Redis => IconName::Key,
        WorkItemKind::Mongo => IconName::Braces,
    }
}

fn tab_chip(
    id: impl Into<SharedString>,
    icon: IconName,
    title: SharedString,
    active: bool,
    dirty: bool,
    running: bool,
    p: &Palette,
) -> impl IntoElement + gpui_kit::ParentElement + gpui_kit::StatefulInteractiveElement {
    let accent = p.accent;
    let full_title = title.clone();
    div()
        .id(gpui_kit::ElementId::Name(id.into()))
        .test_support()
        .role(gpui_kit::Role::Tab)
        .aria_label(title.clone())
        .tooltip_text(full_title)
        .aria_selected(active)
        .group("tab")
        .relative()
        .flex_none()
        .h_full()
        .max_w(px(220.))
        .pl(px(12.))
        .pr(px(4.))
        .flex()
        .items_center()
        .gap(px(7.))
        .border_r_1()
        .border_color(p.line)
        .cursor_pointer()
        .when(active, |this| this.bg(p.surface).text_color(p.text))
        .when(!active, |this| this.text_color(p.text_muted).hover(|style| style.bg(p.hover).text_color(p.text)))
        .when(active, |this| this.child(div().absolute().top_0().left_0().right_0().h(px(2.)).bg(accent)))
        .child(if running {
            Spinner::new().xsmall().color(p.accent).into_any_element()
        } else {
            kit::icon(icon, 13., if active { p.accent } else { p.text_faint }).into_any_element()
        })
        .child(div().truncate().text_size(px(12.5)).child(title))
        .when(dirty, |this| this.child(div().flex_none().size(px(6.)).rounded_full().bg(p.text_faint)))
        .test_support()
}

fn list_row(id: SharedString, p: &Palette, pad_y: f32) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .role(gpui_kit::Role::Button)
        .px(px(8.))
        .py(px(pad_y))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(px(7.))
        .cursor_pointer()
        .hover(|style| style.bg(p.hover))
}

fn section_head(title: SharedString, link: Option<(&'static str, SharedString, &'static str)>, p: &Palette, cx: &mut Context<Workbench>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(kit::title(title, 13., p))
        .when_some(link, |this, (id, label, command)| {
            this.child(Button::new(id).link().small().label(label.clone()).accessibility_label(label).on_click(act(command, cx)))
        })
        .into_any_element()
}

fn action_card(id: &'static str, icon: IconName, title: SharedString, detail: SharedString, p: &Palette, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> AnyElement {
    div()
        .id(id)
        .role(gpui_kit::Role::Button)
        .aria_label(title.clone())
        .flex_1()
        .min_w(px(200.))
        .p(px(14.))
        .flex()
        .items_start()
        .gap(px(12.))
        .rounded(px(10.))
        .border_1()
        .border_color(p.line)
        .bg(p.surface)
        .cursor_pointer()
        .hover(|style| style.border_color(p.accent).bg(p.accent_soft))
        .child(div().flex_none().size(px(34.)).rounded(px(9.)).bg(p.accent_soft).flex().items_center().justify_center().child(kit::icon(icon, 17., p.accent_text)))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.))
                .min_w(px(0.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                .child(div().text_size(px(12.)).text_color(p.text_muted).child(detail)),
        )
        .on_click(on_click)
        .into_any_element()
}

fn form_section(title: SharedString, body: impl IntoElement, p: &Palette) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(kit::eyebrow(title, p))
        .child(kit::card(p).p(px(16.)).child(body))
        .into_any_element()
}

fn banner(icon: IconName, text: SharedString, fg: gpui_kit::Hsla, bg: gpui_kit::Hsla) -> AnyElement {
    div()
        .px(px(12.))
        .py(px(8.))
        .flex()
        .items_start()
        .gap(px(8.))
        .rounded(px(8.))
        .bg(bg)
        .text_color(fg)
        .text_size(px(12.5))
        .child(kit::icon(icon, 15., fg))
        .child(div().flex_1().min_w(px(0.)).child(text))
        .into_any_element()
}

fn switch_visual(on: bool, p: &Palette) -> AnyElement {
    div()
        .flex_none()
        .w(px(34.))
        .h(px(20.))
        .p(px(2.))
        .rounded_full()
        .bg(if on { p.accent } else { p.line_strong })
        .flex()
        .when(on, |this| this.justify_end())
        .child(div().size(px(16.)).rounded_full().bg(gpui_kit::hsla(0., 0., 1., 1.)).shadow_sm())
        .into_any_element()
}

fn dialog_card(id: &'static str, icon: IconName, tone: gpui_kit::Hsla, title: SharedString, body: SharedString, p: &Palette) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .role(gpui_kit::Role::Dialog)
        .aria_label(title.clone())
        .occlude()
        .w_full()
        .max_w(px(440.))
        .min_w(px(0.))
        .p(px(20.))
        .flex()
        .flex_col()
        .gap(px(16.))
        .bg(p.raised)
        .border_1()
        .border_color(p.line)
        .rounded(px(12.))
        .shadow_lg()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .gap(px(12.))
                .child(div().flex_none().size(px(34.)).rounded_full().bg(tone.opacity(0.14)).flex().items_center().justify_center().child(kit::icon(icon, 17., tone)))
                .child(div().flex().flex_col().gap(px(6.)).min_w(px(0.)).child(kit::title(title, 15., p)).child(div().text_size(px(12.5)).text_color(p.text_muted).child(body))),
        )
}

/// 全窗口遮罩；点空白处关闭当前浮层。
fn backdrop(id: &'static str, dim: f32, cx: &mut Context<Workbench>) -> gpui_kit::Stateful<gpui_kit::Div> {
    let entity = cx.entity();
    div()
        .id(id)
        .absolute()
        .inset_0()
        .occlude()
        .bg(gpui_kit::hsla(0., 0., 0., dim))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            entity.update(cx, |this, cx| {
                let effect = this.model.dismiss();
                this.model.history.confirm_clear = false;
                this.model.history.confirm_delete = false;
                this.apply_effect(effect, cx);
                cx.notify();
            });
        })
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
}

/// 历史预览在窄内容区改为上下堆叠，避免挤压列表卡片。
pub(super) fn history_stacks_preview(content_width: f32) -> bool {
    content_width < 820.
}

fn palette_icon(id: &str) -> IconName {
    if id.starts_with("connection:") {
        return IconName::Database;
    }
    if id.starts_with("open-table:") {
        return IconName::Table;
    }
    match id {
        "new-query" | "saved-queries" => IconName::SquareTerminal,
        "execute-query" => IconName::Play,
        "open-structure" => IconName::Columns3,
        "compare" => IconName::GitCompare,
        "open-exchange" => IconName::ArrowDownUp,
        "new-connection" => IconName::Plus,
        "open-sqlite" => IconName::FolderOpen,
        "history" => IconName::Clock,
        "save-query" => IconName::Save,
        "connections" => IconName::Plug,
        "settings" => IconName::Settings,
        "sidebar" => IconName::PanelLeft,
        "theme-light" => IconName::Sun,
        "theme-dark" => IconName::Moon,
        "theme-system" => IconName::Monitor,
        "help" => IconName::Keyboard,
        _ => IconName::ChevronRight,
    }
}

/// Redis 与 MongoDB 连接对应的浏览器命令、文案键和元素 id。
pub(super) fn browser_command(row: &crate::connections::ConnectionRecord) -> Option<(String, &'static str, String)> {
    let name = row.saved.name.replace('|', " ");
    match row.saved.engine {
        Engine::Redis => {
            let database = row.saved.database.clone().unwrap_or_else(|| "0".into());
            Some((format!("open-redis:{}|{name}|{database}", row.saved.id), "redis.open", format!("side-open-redis-{}", row.saved.id)))
        }
        Engine::Mongodb => Some((format!("open-mongo:{}|{name}", row.saved.id), "mongo.open", format!("side-open-mongo-{}", row.saved.id))),
        _ => None,
    }
}

fn connection_detail(row: &crate::connections::ConnectionRecord) -> String {
    let saved = &row.saved;
    if let Some(path) = saved.file_path.as_deref().filter(|path| !path.is_empty()) {
        return path.to_owned();
    }
    let host = saved.host.clone().unwrap_or_default();
    let mut text = match saved.port {
        Some(port) if !host.is_empty() => format!("{host}:{port}"),
        _ => host,
    };
    if let Some(database) = saved.database.as_deref().filter(|database| !database.is_empty()) {
        text = format!("{text} / {database}");
    }
    if let Some(user) = saved.username.as_deref().filter(|user| !user.is_empty()) {
        text = format!("{user}@{text}");
    }
    text
}

fn one_line(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(sql: &str, limit: usize) -> String {
    let flat = sql.trim();
    if flat.chars().count() <= limit {
        return flat.to_owned();
    }
    let mut cut: String = flat.chars().take(limit).collect();
    cut.push('…');
    cut
}

#[allow(dead_code)]
fn _locale_name(locale: i18n::Locale) -> &'static str {
    match locale {
        i18n::Locale::ZhCn => "zh",
        i18n::Locale::En => "en",
    }
}
