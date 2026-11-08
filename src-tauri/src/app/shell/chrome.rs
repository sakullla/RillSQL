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
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::app::views::connections::{engines, LinkStatus};
use crate::app::views::history::HistorySection;
use crate::app::{AppPage, ThemePreference};
use crate::connections::model::{CaMode, Engine, SshAuthKind, TlsMode};
use crate::i18n;
use crate::ui::kit::{self, FieldSpec, Palette, Tip as _};

use super::model::{DraftField, WorkItemKind, WorkTool};
use super::view::Workbench;

const TOPBAR_H: f32 = 44.;
const RAIL_W: f32 = 52.;
const TABS_H: f32 = 36.;
const STATUS_H: f32 = 26.;

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
        root.into_any_element()
    }

    /// 浮层打开时，方向键、回车与 Esc 归浮层。回车经由 `perform` 执行，保证效果完整。
    fn overlay_key(&mut self, event: &gpui_kit::KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if key == "escape" && self.model.connections.confirm_delete && self.model.menu.is_none() && self.model.close_prompt.is_none() {
            self.perform("conn-delete-cancel", cx);
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
        let dark = matches!(self.model.shell.resolved_theme(), crate::app::ResolvedTheme::Dark);
        let theme_next = if dark { "theme-light" } else { "theme-dark" };
        let narrow = self.model.viewport_width <= 760;
        let entity = cx.entity();
        // 顶栏就是窗口标题栏：空白处拖动窗口、双击最大化，Windows/Linux 在右侧画最小化/最大化/关闭。
        // 按钮包在 occlude 里，命中测试才不会把点击当成拖动标题栏。
        let controls = |child: AnyElement| div().flex().items_center().gap(px(8.)).occlude().child(child);
        let row = div()
            .id("toolbar")
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
            .h(px(TOPBAR_H))
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
        let workspace = matches!(page, AppPage::Workspace | AppPage::Structure | AppPage::Diagram);
        let items: [(&str, IconName, &str, bool); 5] = [
            ("activity-workspace", IconName::Database, "shell.nav.workspace", workspace),
            ("activity-connections", IconName::Plug, "shell.nav.connections", page == AppPage::Connections),
            ("activity-history", IconName::Clock, "shell.nav.history", page == AppPage::History),
            ("compare", IconName::GitCompare, "shell.command.compare", page == AppPage::Compare),
            ("open-exchange", IconName::ArrowDownUp, "export.open", self.model.exchange_open),
        ];
        let mut top = div().flex().flex_col().items_center().gap(px(4.));
        for (id, icon, key, active) in items {
            top = top.child(rail_item(id, icon, self.t(key), active, &p, cx));
        }
        let bottom = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.))
            .child(rail_item("help", IconName::Keyboard, self.t("shell.command.help"), self.model.shell.help_open, &p, cx))
            .child(rail_item("activity-settings", IconName::Settings, self.t("shell.nav.settings"), page == AppPage::Settings, &p, cx));
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
        let width = self.model.layout().sidebar_width.max(220) as f32;
        let filter = self.input_for(DraftField::Filter, &self.model.text("ui.explorer.filter"), false, window, cx);
        let objects = self.query.read(cx).tree_objects().to_vec();
        let tree_error = self.query.read(cx).tree_error().to_owned();
        let rows: Vec<_> = self.model.connections.filtered().into_iter().cloned().collect();
        let mut list = div().id("explorer-list").flex().flex_col().flex_1().min_h(px(0.)).overflow_y_scroll().px(px(6.)).pb(px(8.));
        if rows.is_empty() {
            list = list.child(
                kit::empty_state(IconName::Database, self.t("ui.explorer.empty"), self.t("ui.explorer.emptyHint"), &p)
                    .child(kit::primary_button("explorer-new", Some(IconName::Plus), self.t("common.newConnection"), act("new-connection", cx)))
                    .child(kit::tool_button("explorer-sqlite", Some(IconName::FolderOpen), self.t("common.openSqlite"), act("open-sqlite", cx))),
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
                    let mine: Vec<_> = objects.iter().filter(|object| object.connection_id == id).cloned().collect();
                    if mine.is_empty() {
                        let note = if tree_error.is_empty() { self.t("query.status.noObjects") } else { SharedString::from(tree_error.clone()) };
                        list = list.child(div().pl(px(42.)).py(px(4.)).text_size(px(12.)).text_color(p.text_faint).child(note));
                    }
                    let environment = row.saved.env.clone();
                    for object in mine {
                        list = list.child(self.object_row(&object, &environment, &p, cx));
                    }
                }
            }
        }
        div()
            .id("sidebar")
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
                    .h(px(TABS_H))
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
        let id = row.saved.id.clone();
        let name = SharedString::from(row.saved.name.clone());
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
            .h(px(30.))
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
        let spec = crate::app::views::query::object_spec(object, environment);
        let key = format!("{}:{}", object.connection_id, object.name);
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
        let query_id = format!("object:{key}");
        let structure_id = format!("open-structure-for:{spec}");
        div()
            .id(SharedString::from(format!("obj-{key}")))
            .group("obj-row")
            .role(gpui_kit::Role::TreeItem)
            .aria_label(label.clone())
            .aria_selected(selected)
            .h(px(26.))
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
        // 标签列表独立滚动，新建入口固定在右侧；当前标签可通过横向滚动或 Ctrl+Tab 到达。
        let mut list = div()
            .id("tab-list")
            .flex()
            .items_end()
            .flex_1()
            .min_w(px(0.))
            .overflow_x_scroll();
        for tab in self.model.tabs.clone() {
            let active = self.model.active_tab.as_deref() == Some(tab.id.as_str());
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
            .role(gpui_kit::Role::TabList)
            .aria_label(self.t("shell.tabs.tablist"))
            .flex_none()
            .h(px(TABS_H))
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
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(kit::engine_badge(&engine, 14.))
                    .child(SharedString::from(if database.is_empty() { name } else { format!("{name} / {database}") }))
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
            .role(gpui_kit::Role::Status)
            .aria_label(SharedString::from(notice.clone()))
            .flex_none()
            .h(px(STATUS_H))
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
                list_row(SharedString::from(format!("welcome-conn-{id}")), &p)
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
                list_row(SharedString::from(format!("welcome-run-{id}")), &p)
                    .child(kit::icon(IconName::Clock, 14., p.text_faint))
                    .child(div().flex_1().min_w(px(0.)).truncate().font_family(cx.theme().mono_font_family.clone()).text_size(px(12.)).child(SharedString::from(one_line(&entry.sql))))
                    .child(div().flex_none().text_size(px(11.)).text_color(p.text_faint).child(SharedString::from(entry.connection_name.clone())))
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
                list_row(SharedString::from(format!("welcome-saved-{}", item.id)), &p)
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
                    .px(px(32.))
                    .py(px(48.))
                    .flex()
                    .flex_col()
                    .gap(px(28.))
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
                                    .p(px(14.))
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
                                    .p(px(14.))
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
                    .role(gpui_kit::Role::ListItem)
                    .aria_label(SharedString::from(label))
                    .aria_selected(selected)
                    .px(px(8.))
                    .py(px(7.))
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
            list = list.child(div().p(px(16.)).text_size(px(12.)).text_color(p.text_faint).child(self.t("ui.explorer.empty")));
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
                .gap(px(8.))
                .items_end()
                .child(div().flex_1().child(kit::form_row(self.t("ui.conn.url"), url, &p)))
                .child(kit::secondary_button("conn-apply-url", Some(IconName::WandSparkles), self.t("ui.conn.parse"), act("conn-apply-url", cx))),
        );
        general = general.child(
            div()
                .flex()
                .gap(px(12.))
                .child(div().flex_1().child(kit::form_row(self.t("ui.conn.name"), name, &p)))
                .child(kit::form_row(self.t("ui.conn.env"), kit::segmented(env_items, &p), &p)),
        );
        if file_based {
            let file = self.input_for(DraftField::File, &t(self, "connections.validate.chooseFile"), false, window, cx);
            general = general.child(
                div()
                    .flex()
                    .gap(px(8.))
                    .items_end()
                    .child(div().flex_1().child(kit::form_row(self.t("ui.conn.file"), file, &p)))
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
                        .gap(px(12.))
                        .child(div().flex_1().child(kit::form_row(self.t("ui.conn.host"), host, &p)))
                        .child(div().w(px(110.)).child(kit::form_row(self.t("ui.conn.port"), port, &p))),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(12.))
                        .child(div().flex_1().child(kit::form_row(self.t("ui.conn.user"), user, &p)))
                        .child(div().flex_1().child(kit::form_row(self.t("ui.conn.password"), password.mask_toggle(), &p))),
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
            security = security.child(kit::form_row(self.t("connections.editor.tls.title"), kit::segmented(tls_items, &p), &p));
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
                security = security.child(kit::form_row(self.t("connections.editor.tls.caLabel"), kit::segmented(ca_items, &p), &p));
                if draft.ca_mode == CaMode::File {
                    let ca = self.input_for(DraftField::CaFile, "ca.pem", false, window, cx);
                    security = security.child(kit::form_row(self.t("connections.editor.tls.caFile"), ca, &p));
                }
                let cert = self.input_for(DraftField::ClientCert, "", false, window, cx);
                let key = self.input_for(DraftField::ClientKey, "", false, window, cx);
                security = security.child(
                    div()
                        .flex()
                        .gap(px(12.))
                        .child(div().flex_1().child(kit::form_row(self.t("connections.editor.tls.clientCert"), cert, &p)))
                        .child(div().flex_1().child(kit::form_row(self.t("connections.editor.tls.clientKey"), key, &p))),
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
                    .pl(px(46.))
                    .child(
                        div()
                            .flex()
                            .gap(px(12.))
                            .child(div().flex_1().child(kit::form_row(self.t("ui.conn.sshHost"), ssh_host, &p)))
                            .child(div().w(px(90.)).child(kit::form_row(self.t("connections.editor.ssh.port"), ssh_port, &p))),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(12.))
                            .items_end()
                            .child(div().flex_1().child(kit::form_row(self.t("ui.conn.sshUser"), ssh_user, &p)))
                            .child(kit::form_row(self.t("connections.editor.ssh.authLabel"), kit::segmented(auth_items, &p), &p)),
                    );
                if draft.ssh_auth == SshAuthKind::Key {
                    let ssh_key = self.input_for(DraftField::SshKey, "~/.ssh/id_ed25519", false, window, cx);
                    let passphrase = self.input_for(DraftField::Passphrase, &t(self, "ui.conn.optional"), true, window, cx);
                    ssh = ssh.child(
                        div()
                            .flex()
                            .gap(px(12.))
                            .child(div().flex_1().child(kit::form_row(self.t("connections.editor.ssh.keyFile"), ssh_key, &p)))
                            .child(div().flex_1().child(kit::form_row(self.t("ui.conn.passphrase"), passphrase, &p))),
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
        let mut footer = div().flex().items_center().gap(px(8.)).pt(px(4.));
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
        div()
            .id("conn-form-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .max_w(px(720.))
                    .mx_auto()
                    .px(px(28.))
                    .py(px(24.))
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .child(kit::engine_badge(engine.as_str(), 36.))
                            .child(div().flex().flex_col().child(kit::title(heading, 17., &p)).child(kit::muted(SharedString::from(kit::engine_label(engine.as_str())), &p))),
                    )
                    .child(form_section(self.t("ui.conn.pickEngine"), engine_row, &p))
                    .child(form_section(self.t("ui.conn.general"), general, &p))
                    .when(!file_based, |this| this.child(form_section(self.t("ui.conn.security"), security, &p)))
                    .child(notes)
                    .child(footer),
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
        let section = self.model.history.section;
        let search = self.input_for(DraftField::HistorySearch, &self.model.text("ui.history.search"), false, window, cx);
        let save_name = self.input_for(DraftField::SaveName, &self.model.text("ui.history.saveAs"), false, window, cx);
        let tabs = kit::segmented(
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
        );
        let mono = cx.theme().mono_font_family.clone();
        let mut list = div().id("history-list").flex().flex_col().gap(px(6.)).flex_1().min_h(px(0.)).overflow_y_scroll().px(px(24.)).pb(px(24.));
        if section == HistorySection::Runs {
            let runs: Vec<_> = self.model.history.filtered_runs().into_iter().take(200).cloned().collect();
            if runs.is_empty() {
                list = list.child(kit::empty_state(IconName::Clock, self.t("ui.history.empty"), self.t("history.page.loadOnlySql"), &p));
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
                list = list.child(
                    div()
                        .id(SharedString::from(format!("run-{id}")))
                        .group("run-row")
                        .role(gpui_kit::Role::ListItem)
                        .aria_label(SharedString::from(entry.sql.clone()))
                        .aria_selected(selected)
                        .p(px(12.))
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
                                .font_family(mono.clone())
                                .text_size(px(12.5))
                                .text_color(p.text)
                                .when(!selected, |this| this.max_h(px(40.)).overflow_hidden())
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
                                .child(SharedString::from(if entry.database_name.is_empty() { entry.connection_name.clone() } else { format!("{} / {}", entry.connection_name, entry.database_name) }))
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
                list = list.child(kit::empty_state(IconName::Bookmark, self.t("ui.history.emptySaved"), self.t("history.savedPanel.openNote"), &p));
            }
            for item in saved {
                let id = item.id.clone();
                let selected = self.model.history.selected_saved.as_deref() == Some(id.as_str());
                let select = id.clone();
                let delete = id.clone();
                list = list.child(
                    div()
                        .id(SharedString::from(format!("saved-{id}")))
                        .group("saved-row")
                        .role(gpui_kit::Role::ListItem)
                        .aria_label(SharedString::from(item.name.clone()))
                        .aria_selected(selected)
                        .p(px(12.))
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
                                .child(div().flex_1().truncate().font_weight(FontWeight::MEDIUM).child(SharedString::from(item.name.clone())))
                                .child(kit::engine_badge(&item.engine, 14.))
                                .child(div().text_size(px(11.5)).text_color(p.text_faint).child(SharedString::from(item.connection_name.clone())))
                                .child(
                                    div()
                                        .flex()
                                        .gap(px(4.))
                                        .when(!selected, |this| this.invisible().group_hover("saved-row", |style| style.visible()))
                                        .child(kit::icon_button(SharedString::from(format!("saved-del-{id}")), IconName::Trash, self.t("ui.history.delete"), cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            this.model.history.section = HistorySection::Saved;
                                            this.model.history.selected_saved = Some(delete.clone());
                                            this.perform("saved-delete", cx);
                                        })).xsmall()),
                                ),
                        )
                        .child(div().font_family(mono.clone()).text_size(px(12.)).text_color(p.text_muted).max_h(px(36.)).overflow_hidden().child(SharedString::from(clip(&item.sql, 200))))
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            this.model.history.section = HistorySection::Saved;
                            this.model.history.selected_saved = Some(select.clone());
                            if event.click_count() >= 2 {
                                this.perform("history-load", cx);
                            }
                            cx.notify();
                        })),
                );
            }
        }

        let can_save = self.model.active_tab().is_some_and(|tab| tab.kind.is_query());
        let toolbar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(10.))
            .child(tabs)
            .child(div().w(px(260.)).child(search.prefix(kit::icon(IconName::Search, 13., p.text_faint)).cleanable(true)))
            .child(div().flex_1())
            .when(section == HistorySection::Runs, |this| {
                this.child(kit::tool_button("history-clear", Some(IconName::Trash), self.t("ui.history.clear"), act("history-clear", cx)))
                    .child(kit::tool_button("history-refresh", Some(IconName::RefreshCw), self.t("redis.tab.refresh"), act("history-refresh", cx)))
            })
            .when(section == HistorySection::Saved && can_save, |this| {
                this.child(div().w(px(220.)).child(save_name)).child(kit::primary_button("history-save-named", Some(IconName::Save), self.t("history.savedPanel.save"), act("history-save-named", cx)))
            });

        div()
            .id("history-page")
            .role(gpui_kit::Role::Group)
            .aria_label(self.t("history.page.titleRuns"))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(24.))
                    .pt(px(20.))
                    .pb(px(14.))
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .child(div().flex().flex_col().gap(px(2.)).child(kit::title(self.t("history.page.titleRuns"), 17., &p)).child(kit::muted(self.t("history.page.loadOnlySql"), &p)))
                    .child(toolbar),
            )
            .child(list)
            .into_any_element()
    }

    // ------------------------------------------------------------ 设置

    fn settings_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let s = self.model.settings.clone();
        let theme = self.model.shell.theme;
        let seg = |this: &mut Self, items: &[(&str, &str, bool)], cx: &mut Context<Self>| {
            let built = items
                .iter()
                .map(|(id, key, selected)| (SharedString::from(id.to_string()), this.t(key), *selected, kit::click(act(id.to_string(), cx))))
                .collect::<Vec<_>>();
            kit::segmented(built, &Palette::of(cx))
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
            .child(kit::secondary_button("editor-theme-next", Some(IconName::RefreshCw), self.t("ui.settings.change"), act("editor-theme-next", cx)));
        let stepper = |this: &Self, id: &'static str, value: String, cx: &mut Context<Self>| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().font_weight(FontWeight::MEDIUM).child(SharedString::from(value)))
                .child(kit::secondary_button(id, Some(IconName::Plus), this.t("ui.settings.increase"), act(id, cx)))
        };
        let page_size = stepper(self, "page-size-up", s.page_size.to_string(), cx);
        let preview = stepper(self, "preview-up", format!("{} KB", s.preview_kb), cx);
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
                        .py(px(12.))
                        .flex()
                        .items_center()
                        .gap(px(16.))
                        .when(index + 1 < count, |this| this.border_b_1().border_color(p.line_soft))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
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
            .role(gpui_kit::Role::Group)
            .aria_label(self.t("shell.settings.title"))
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .max_w(px(760.))
                    .mx_auto()
                    .px(px(28.))
                    .py(px(28.))
                    .flex()
                    .flex_col()
                    .gap(px(24.))
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
        let help = self.model.shell.help_open && !self.model.shell.palette_open;
        let mut panel = div()
            .id("palette")
            .role(gpui_kit::Role::Dialog)
            .aria_label(self.t(if help { "shell.command.help" } else { "shell.palette.title" }))
            .aria_expanded(self.model.shell.palette_open)
            .key_context("Palette")
            .occlude()
            .w(px(560.))
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
                        .h(px(30.))
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
                list = list.child(div().p(px(16.)).text_color(p.text_faint).child(self.t("ui.palette.empty")));
            }
            for (index, entry) in entries.into_iter().take(40).enumerate() {
                let selected = index == self.model.palette_index;
                let id = entry.id.clone();
                let chord = crate::app::COMMANDS.iter().find(|command| command.id == entry.id).and_then(|command| command.chord);
                let icon = palette_icon(&entry.id);
                let group = if entry.group == "connection" { self.t("shell.paletteGroup.connection") } else { SharedString::from("") };
                list = list.child(
                    div()
                        .id(SharedString::from(format!("pal-{index}")))
                        .role(gpui_kit::Role::MenuItem)
                        .aria_label(SharedString::from(entry.label.clone()))
                        .aria_selected(selected)
                        .h(px(34.))
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
            .pt(px(72.))
            .child(panel)
            .into_any_element()
    }

    fn context_layer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
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
                    .h(px(28.))
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
        backdrop("close-backdrop", 0.32, cx).flex().items_center().justify_center().child(dialog).into_any_element()
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
        backdrop("delete-backdrop", 0.32, cx).flex().items_center().justify_center().child(dialog).into_any_element()
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
        backdrop("clear-backdrop", 0.32, cx).flex().items_center().justify_center().child(dialog).into_any_element()
    }

    fn exchange_layer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let panel = div()
            .id("exchange-drawer")
            .occlude()
            .absolute()
            .top(px(TOPBAR_H))
            .bottom(px(STATUS_H))
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
                    .h(px(TABS_H + 8.))
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

fn rail_item(id: &'static str, icon: IconName, label: SharedString, active: bool, p: &Palette, cx: &mut Context<Workbench>) -> AnyElement {
    let accent = p.accent;
    div()
        .id(SharedString::from(format!("cmd-{id}")))
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

fn tab_chip(id: impl Into<SharedString>, icon: IconName, title: SharedString, active: bool, dirty: bool, running: bool, p: &Palette) -> gpui_kit::Stateful<gpui_kit::Div> {
    let accent = p.accent;
    div()
        .id(gpui_kit::ElementId::Name(id.into()))
        .role(gpui_kit::Role::Tab)
        .aria_label(title.clone())
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
}

fn list_row(id: SharedString, p: &Palette) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .role(gpui_kit::Role::Button)
        .px(px(8.))
        .py(px(6.))
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
        .w(px(440.))
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
                this.apply_effect(effect, cx);
                cx.notify();
            });
        })
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
}

fn palette_icon(id: &str) -> IconName {
    if id.starts_with("connection:") {
        return IconName::Database;
    }
    match id {
        "new-query" | "saved-queries" => IconName::SquareTerminal,
        "execute-query" => IconName::Play,
        "open-structure" => IconName::Columns3,
        "compare" => IconName::GitCompare,
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
