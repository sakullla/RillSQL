//! 外壳的画法：顶栏、活动栏、资源管理器、标签条、状态栏、页面与浮层。
//! 状态与命令都在 `WorkbenchModel`；这里只把它画出来，并把点击交回 `perform`。

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, TitleBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, DragMoveEvent, FontWeight, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px,
};

use crate::app::views::connections::{engines, LinkStatus};
use crate::app::views::history::HistorySection;
use crate::app::{AppPage, FocusTarget, ThemePreference};
use crate::catalog::hierarchy::{NavKind, NavRow};
use crate::catalog::model::ObjectKind;
use crate::connections::model::{CaMode, Engine, EnvTag, SshAuthKind, TlsMode};
use crate::i18n;
use crate::ui::kit::{self, FieldSpec, Palette, Tip as _, Radius, TypeScale};

use super::model::{ConnFilterMenu, DraftField, WorkItemKind, WorkTool};
use super::view::Workbench;

/// 活动栏宽度来自布局模块，绘制与几何只保留一份。
const RAIL_W: f32 = super::layout::RAIL_WIDTH as f32;
/// 活动栏激活指示条（3px 宽的竖线）两端的圆角。线条不是控件，不取 `Radius` token；见扫描测试例外清单。
const RAIL_INDICATOR_RADIUS: f32 = 2.;

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

    /// 连接的环境标签文字。存过的标签用它的名称，不用原始 id。
    fn env_text(&self, env: &str) -> SharedString {
        SharedString::from(env_label(|key| self.model.text(key), env, &self.model.env_tags))
    }

    /// 连接的环境标签小块；没有标签（空或 none）时不画。颜色跟标签色调走。
    fn env_pill(&self, env: &str, p: &Palette) -> Option<AnyElement> {
        let tone = env_tone(env, &self.model.env_tags).to_owned();
        let label = self.env_text(env);
        env_shown(env).then(|| kit::env_tag(&tone, label, p))
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
        if self.seen_page != self.model.shell.page {
            let page = self.model.shell.page;
            self.seen_page = page;
            if matches!(
                page,
                AppPage::Settings | AppPage::Connections | AppPage::History | AppPage::Compare | AppPage::Diagram | AppPage::Structure
            ) {
                self.restore_pending = true;
            }
        }
        if std::mem::take(&mut self.restore_pending) {
            self.restore_focus(window, cx);
        }
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
            .text_size(px(TypeScale::MD))
            .font_family(cx.theme().font_family.clone())
            .on_drop({
                let entity = entity.clone();
                move |paths: &gpui_kit::ExternalPaths, _, cx| {
                    entity.update(cx, |this, cx| this.accept_exchange_drop(paths, cx));
                }
            })
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| this.overlay_key(event, window, cx)))
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
                            .track_focus(&self.focus)
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
                                // 覆盖式侧栏盖在工作区上方：鼠标事件只到侧栏，不能穿透到下面的编辑器/表格
                                // （否则右键树节点会弹出编辑器菜单，点击也会落到编辑器上）。
                                .occlude()
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
        if self.model.ddl_view.as_ref().is_some_and(|view| !view.copy_only) {
            root = root.child(self.ddl_layer(window, cx));
        }
        if self.model.danger.is_some() {
            root = root.child(self.danger_layer(window, cx));
        }
        root.into_any_element()
    }

    /// 浮层打开时，方向键、回车与 Esc 归浮层。回车经由 `perform` 执行，保证效果完整。
    fn overlay_key(&mut self, event: &gpui_kit::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        // 浮层关闭后键盘要回到打开它的地方，而不是留在已经消失的弹层上。
        let overlay_was_open = self.model.menu.is_some() || self.model.shell.palette_open;
        // 确认层与定义查看器：Esc 就是取消/关闭，不发送任何语句。
        if key == "escape" && self.model.menu.is_none() && self.model.close_prompt.is_none() {
            if self.model.danger.is_some() {
                self.perform("danger-cancel", cx);
                self.restore_focus(window, cx);
                return;
            }
            if self.model.ddl_view.as_ref().is_some_and(|view| !view.copy_only) {
                self.perform("ddl-close", cx);
                self.restore_focus(window, cx);
                return;
            }
        }
        if key == "escape" && self.model.connections.confirm_delete && self.model.menu.is_none() && self.model.close_prompt.is_none() {
            self.perform("conn-delete-cancel", cx);
            self.restore_focus(window, cx);
            return;
        }
        if key == "escape" && self.model.history.confirm_clear && self.model.menu.is_none() && self.model.close_prompt.is_none() {
            self.perform("history-clear-cancel", cx);
            self.restore_focus(window, cx);
            return;
        }
        if key == "escape" && self.model.history.confirm_delete && self.model.menu.is_none() && self.model.close_prompt.is_none() {
            self.perform("saved-delete-cancel", cx);
            self.restore_focus(window, cx);
            return;
        }
        if key == "escape" && self.model.exchange_open && self.model.menu.is_none() && !self.model.shell.palette_open {
            self.model.exchange_open = false;
            self.restore_focus(window, cx);
            cx.notify();
            return;
        }
        // 侧栏有焦点时方向键属于对象树：上下移动、左右展开/折叠、回车打开。
        if self.model.menu.is_none()
            && self.model.close_prompt.is_none()
            && !self.model.shell.palette_open
            && self.model.shell.focus == FocusTarget::Sidebar
            && self.model.shell.sidebar_open
        {
            {
                let command = match key {
                    "up" => self.model.nav_key(super::menus::NavKey::ArrowUp),
                    "down" => self.model.nav_key(super::menus::NavKey::ArrowDown),
                    "home" => self.model.nav_key(super::menus::NavKey::Home),
                    "end" => self.model.nav_key(super::menus::NavKey::End),
                    "right" => self.model.nav_step(true),
                    "left" => self.model.nav_step(false),
                    "enter" => self.model.nav_key(super::menus::NavKey::Enter),
                    _ => None,
                };
                if let Some(command) = command {
                    self.perform(&command, cx);
                }
                cx.notify();
                return;
            }
        }
        let Some(nav) = super::view::nav_key(key) else { return };
        if self.model.menu.is_none() && self.model.close_prompt.is_none() && !self.model.shell.palette_open {
            return;
        }
        if matches!(nav, super::menus::NavKey::Enter) {
            let opens = self.model.menu.as_ref().is_some_and(|menu| {
                menu.submenu.is_none() && menu.active_parent().is_some_and(|item| !item.children.is_empty() && item.enabled)
            });
            if opens {
                let _ = self.model.key(nav);
                cx.notify();
                return;
            }
            if self.model.menu.is_some() {
                let id = self.model.menu.as_ref().and_then(|menu| menu.current().map(|item| item.id.to_owned()));
                self.model.menu = None;
                self.model.shell.focus = self.model.menu_return;
                if let Some(id) = id {
                    self.perform(&id, cx);
                }
                self.restore_if_closed(overlay_was_open, window, cx);
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
                self.restore_if_closed(overlay_was_open, window, cx);
                cx.notify();
                return;
            }
        }
        let _ = self.model.key(nav);
        self.restore_if_closed(overlay_was_open, window, cx);
        cx.notify();
    }

    /// 弹层关闭后把真实键盘焦点送回触发区域；模型里的 focus 只是可读状态。
    fn restore_if_closed(&mut self, was_open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if was_open && self.model.menu.is_none() && !self.model.shell.palette_open {
            self.restore_focus(window, cx);
        }
    }

    fn restore_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.model.shell.focus {
            FocusTarget::Sidebar if self.model.shell.sidebar_open => window.focus(&self.nav_focus, cx),
            // 查询页的结果网格属于 QueryPane。公开焦点入口是编辑器；表格页才回到共享网格。
            FocusTarget::Grid if self.model.tool == super::model::WorkTool::Query => self.query.update(cx, |pane, _| pane.request_focus()),
            FocusTarget::Grid => self.table.update(cx, |pane, _| pane.request_focus()),
            FocusTarget::Editor => self.query.update(cx, |pane, _| pane.request_focus()),
            _ => window.focus(&self.focus, cx),
        }
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
                    .when(!narrow, |this| this.child(div().text_size(px(TypeScale::MD)).font_weight(FontWeight::SEMIBOLD).child("RillSQL"))),
            )
            .child(div().flex_1())
            .child(controls(
                kit::interactive_states(div().id("top-search"), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(self.t("shell.palette.title"))
                    .w(px(if narrow { 180. } else { 360. }))
                    .h(px(m.control_h + 6.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(Radius::CONTROL))
                    .border_1()
                    .border_color(p.line)
                    .bg(p.surface)
                    .hover(|style| style.border_color(p.line_strong))
                    .child(kit::icon(IconName::Search, 14., p.text_faint))
                    .child(div().flex_1().truncate().text_size(px(TypeScale::SM)).text_color(p.text_faint).child(self.t("ui.search.placeholder")))
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
            .test_support()
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
        let width = self.model.layout().sidebar_width as f32;
        let filter = self.input_for(DraftField::ObjectFilter, &self.model.text("ui.explorer.filterObjects"), false, window, cx);
        // 对象索引来自模型里的同一份快照；连接列表保持完整，连接筛选只属于连接页。
        let rows: Vec<_> = self.model.connections.rows.clone();
        let mut list = div()
            .id("explorer-list")
            .track_focus(&self.nav_focus)
            .track_scroll(&self.explorer_scroll)
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(6.))
            .pb(px(8.));
        // 列表是平铺的：每个 child 一行。数着下标，跳转定义要滚到的那一行才能定位。
        let mut children = 0usize;
        let mut reveal_at: Option<usize> = None;
        if rows.is_empty() {
            children += 1;
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
            children += 1;
            list = list.child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .mb(px(4.))
                    .rounded(px(Radius::PANEL))
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
                            .text_size(px(TypeScale::SM))
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
        if self.model.nav_searching() {
            // 搜索用同一份树索引，行显示完整路径并高亮命中；清除后目录原样回来。
            let hits = self.model.nav_rows();
            if hits.is_empty() {
                if let Some(key) = self.model.nav_empty_key("") {
                    children += 1;
                    list = list.child(div().pl(px(30.)).py(px(6.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child(self.t(key)));
                }
            }
            for row in hits {
                if self.model.reveal_node.as_deref() == Some(row.id.as_str()) {
                    reveal_at = Some(children);
                }
                children += 1;
                list = list.child(self.nav_row(&row, true, &p, cx));
            }
        } else {
            for row in rows {
                let id = row.saved.id.clone();
                let status = self.model.connections.status.get(&id).copied().unwrap_or(LinkStatus::Disconnected);
                // 展开状态只有一份：树上的连接节点。鼠标、键盘和渲染都读它。
                let expanded = self.model.nav_connection_expanded(&id) && status == LinkStatus::Connected;
                let selected = self.model.connections.selected.as_deref() == Some(id.as_str());
                children += 1;
                list = list.child(self.connection_row(&row, status, expanded, selected, &p, cx));
                if !expanded {
                    continue;
                }
                // 连接级读取失败就把原因和重试放在这条连接下面，不吞掉。
                if let Some(error) = self.model.nav.node(&crate::catalog::hierarchy::connection_node_id(&id)).and_then(|node| node.error.clone()) {
                    children += 1;
                    list = list.child(self.connection_error_row(&id, &error, &p, cx));
                }
                let nav_rows = self.model.nav_rows_for(&id);
                if nav_rows.is_empty() {
                    if let Some(key) = self.model.nav_empty_key(&id) {
                        if key != "ui.explorer.error" {
                            children += 1;
                            list = list.child(div().pl(px(42.)).py(px(4.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child(self.t(key)));
                        }
                    }
                    continue;
                }
                for nav_row in nav_rows {
                    if self.model.reveal_node.as_deref() == Some(nav_row.id.as_str()) {
                        reveal_at = Some(children);
                    }
                    children += 1;
                    list = list.child(self.nav_row(&nav_row, false, &p, cx));
                }
            }
        }
        // 跳转定义选中的行：滚进可见区（下一次布局生效），只滚一次。
        if let Some(index) = reveal_at {
            self.explorer_scroll.scroll_to_item(index);
            self.model.reveal_node = None;
        }
        let dragging = self.model.sidebar_dragging;
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
            .bg(p.chrome)
            .border_l_1()
            .border_color(p.line)
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<kit::ResizeDrag>, _, cx| {
                if event.drag(cx).key.as_ref() != "sidebar-resize" {
                    return;
                }
                let width = f32::from(event.event.position.x - event.bounds.left());
                this.model.set_sidebar_width(width);
                this.model.sidebar_dragging = true;
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                if this.model.sidebar_dragging {
                    this.model.sidebar_dragging = false;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .flex()
                    .flex_col()
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
                    .child(list),
            )
            .child({
                let entity = cx.entity().downgrade();
                kit::resize_handle("sidebar-resize", self.t("shell.sidebar.explorer"), dragging, &p, move |_, _, cx| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(cx, |this, cx| {
                            this.model.reset_sidebar_width();
                            cx.notify();
                        });
                    }
                })
            })
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
        let chevron_id = id.clone();
        kit::row_states(div().id(SharedString::from(format!("side-{id}"))), p)
            .test_support()
            .role(gpui_kit::Role::TreeItem)
            .aria_label(name.clone())
            .aria_selected(selected)
            .aria_expanded(expanded)
            // 状态不只靠圆点颜色：无障碍描述和提示都写出同一句状态文案。
            .aria_description(SharedString::from(format!("{} · {}", full_name, self.t(state_key))))
            .tooltip_text(full_name)
            .h(px(m.row_h))
            .px(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(Radius::CONTROL))
            .when(selected, |this| this.bg(p.active))
            .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
            .child(
                div()
                    .id(SharedString::from(format!("nav-chevron-{id}")))
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(self.t(if expanded { "catalog.tree.collapse" } else { "catalog.tree.expand" }))
                    .tooltip_text(self.t(if expanded { "catalog.tree.collapse" } else { "catalog.tree.expand" }))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .child(kit::icon(chevron, 13., p.text_faint))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.model.toggle_nav_connection(&chevron_id);
                        cx.notify();
                    })),
            )
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
                    .aria_label(self.t(state_key))
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

    /// 连接级的读取失败：原因写在连接下面，重试只重读这条连接。
    fn connection_error_row(&self, id: &str, error: &str, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let node_id = crate::catalog::hierarchy::connection_node_id(id);
        let full = format!("{}: {error}", self.t("catalog.tree.nodeError"));
        div()
            .id(SharedString::from(format!("nav-conn-error-{id}")))
            .test_support()
            .pl(px(42.))
            .pr(px(4.))
            .py(px(3.))
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .id(SharedString::from(format!("nav-conn-error-text-{id}")))
                    .test_support()
                    .aria_label(SharedString::from(full.clone()))
                    .tooltip_text(SharedString::from(full.clone()))
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(px(TypeScale::XS))
                    .text_color(p.danger)
                    .child(SharedString::from(full)),
            )
            .child(
                kit::icon_button(
                    SharedString::from(format!("nav-retry-conn-{id}")),
                    IconName::RefreshCw,
                    self.t("common.retry"),
                    act(format!("nav-retry:{node_id}"), cx),
                )
                .xsmall(),
            )
            .into_any_element()
    }

    fn click_connection(&mut self, id: &str, double: bool, cx: &mut Context<Self>) {
        self.model.connections.selected = Some(id.to_owned());
        self.model.shell.focus = FocusTarget::Sidebar;
        let status = self.model.connections.status.get(id).copied().unwrap_or(LinkStatus::Disconnected);
        match status {
            LinkStatus::Connected => {
                // 单击只选中这一行；双击在树上展开/折叠，Redis/MongoDB 连接双击直接开浏览器。
                // chevron 单击是展开/折叠的另一条鼠标路径，和双击、键盘右/左键走同一份状态。
                if double {
                    let specialized = self
                        .model
                        .connections
                        .rows
                        .iter()
                        .any(|row| row.saved.id == id && matches!(row.saved.engine, Engine::Redis | Engine::Mongodb));
                    if specialized {
                        if let Some((command, _, _)) = self.model.connections.rows.iter().find(|row| row.saved.id == id).and_then(browser_command) {
                            self.perform(&command, cx);
                            return;
                        }
                    }
                    self.model.toggle_nav_connection(id);
                }
                cx.notify();
            }
            LinkStatus::Connecting => {}
            LinkStatus::Disconnected | LinkStatus::Failed => {
                if self.model.connections.rows.iter().any(|row| row.saved.id == id && matches!(row.saved.engine, Engine::Redis | Engine::Mongodb)) {
                    self.pending_browser = Some(id.to_owned());
                }
                // 连接成功后 refresh_tree_on_connect 展开这一行。
                self.model.expand_nav_connection(id);
                self.model.connections.status.insert(id.to_owned(), LinkStatus::Connecting);
                self.perform("conn-connect", cx);
            }
        }
    }

    /// 树的一行：真实层级、展开状态、局部错误/续读、搜索高亮和绑定到这一行的动作。
    fn nav_row(&self, row: &NavRow, flat: bool, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let m = self.metrics();
        let id = row.id.clone();
        let label = if let Some(key) = row.label_key {
            self.t(key)
        } else {
            SharedString::from(row.label.clone())
        };
        let depth = if flat { 0 } else { row.depth };
        let indent = 18. + depth as f32 * 12.;
        let icon = nav_icon(row.kind, row.object_kind);
        let chevron = if row.expandable {
            if row.expanded { IconName::ChevronDown } else { IconName::ChevronRight }
        } else {
            IconName::Dot
        };
        let tooltip = if row.path.is_empty() {
            label.clone()
        } else {
            SharedString::from(format!("{} / {}", row.path, row.name))
        };
        let is_group = matches!(row.kind, NavKind::Group | NavKind::ColumnGroup | NavKind::IndexGroup);
        let body = match row.highlight {
            Some((start, end)) if end > start && end <= label.len() => highlight_label(&label, start, end, p),
            _ => div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::SM)).child(label.clone()).into_any_element(),
        };
        let click_id = id.clone();
        let menu_id = id.clone();
        let focus_id = id.clone();
        let toggle_id = id.clone();
        let has_error = row.error.is_some();
        kit::row_states(div().id(SharedString::from(format!("nav-{id}"))), p)
            .test_support()
            .role(gpui_kit::Role::TreeItem)
            .aria_label(label.clone())
            .aria_selected(row.focused)
            .aria_expanded(row.expanded)
            .tooltip_text(tooltip)
            .h(px(m.tree_row_h))
            .pl(px(indent))
            .pr(px(4.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(Radius::CONTROL))
            .when(row.focused, |this| this.bg(p.selection))
            .when(!row.focused, |this| this.hover(|style| style.bg(p.hover)))
            .child(
                div()
                    .id(SharedString::from(format!("nav-chevron-{id}")))
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(self.t(if row.expanded { "catalog.tree.collapse" } else { "catalog.tree.expand" }))
                    .tooltip_text(self.t(if row.expanded { "catalog.tree.collapse" } else { "catalog.tree.expand" }))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .when(row.expandable, |this| {
                        this.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.model.shell.focus = FocusTarget::Sidebar;
                            this.perform(&format!("nav-toggle:{toggle_id}"), cx);
                            cx.notify();
                        }))
                    })
                    .child(kit::icon(chevron, 12., p.text_faint)),
            )
            .child(kit::icon(icon, 13., nav_tint(row.kind, p)))
            .child(body)
            .when(is_group && row.total > 0, |this| {
                this.child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(row.total.to_string())))
            })
            .when(row.stale, |this| {
                this.child(div().text_size(px(TypeScale::XS)).text_color(p.warn).child(self.t("catalog.tree.stale")))
            })
            .when(row.loading, |this| this.child(Spinner::new().xsmall().color(p.text_muted)))
            .when_some(row.error.clone(), |this, error| {
                let full = format!("{}: {error}", self.t("catalog.tree.nodeError"));
                this.child(
                    div()
                        .id(SharedString::from(format!("nav-err-{id}")))
                        .tooltip_text(SharedString::from(full.clone()))
                        .truncate()
                        .max_w(px(120.))
                        .text_size(px(TypeScale::XS))
                        .text_color(p.danger)
                        .child(SharedString::from(full)),
                )
            })
            .when(has_error, |this| {
                this.child(
                    kit::icon_button(
                        SharedString::from(format!("nav-retry-{focus_id}")),
                        IconName::RefreshCw,
                        self.t("common.retry"),
                        act(format!("nav-retry:{focus_id}"), cx),
                    )
                    .xsmall(),
                )
            })
            .when(row.has_more, |this| {
                this.child(
                    kit::icon_button(
                        SharedString::from(format!("nav-more-{focus_id}")),
                        IconName::ChevronsDown,
                        self.t("catalog.tree.loadMore"),
                        act(format!("nav-more:{focus_id}"), cx),
                    )
                    .xsmall(),
                )
            })
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.menu_at = event.position;
                this.model.shell.focus = FocusTarget::Sidebar;
                // 菜单只属于被点的这一行：对象、列/索引或范围行各自一册。
                this.model.open_nav_menu(&menu_id);
                cx.notify();
            }))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.model.shell.focus = FocusTarget::Sidebar;
                window.focus(&this.nav_focus, cx);
                this.perform(&format!("nav-focus:{click_id}"), cx);
                if event.click_count() >= 2 {
                    // 双击容器在树上展开/折叠，对象打开自己的数据或文档页。
                    if let Some(command) = this.model.nav_toggle_command(&click_id) {
                        this.perform(&command, cx);
                    }
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
                kit::interactive_states(div().id("status-target"), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(self.t("ui.status.noTarget"))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w(px(0.))
                    .max_w(px(target_max))
                    .rounded(px(Radius::CONTROL))
                    .px(px(4.))
                    .child(kit::engine_badge(&engine, 14.))
                    .child(div().min_w(px(0.)).truncate().child(SharedString::from(if database.is_empty() { name } else { format!("{name} / {database}") })))
                    .children(self.env_pill(&env, &p))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.perform("status-target", cx);
                    this.restore_focus(window, cx);
                }))
                    .into_any_element()
            }
            _ => kit::interactive_states(div().id("status-target"), &p, false)
                .test_support()
                .role(gpui_kit::Role::Button)
                .aria_label(self.t("ui.status.noTarget"))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(Radius::CONTROL))
                .px(px(4.))
                .child(kit::status_dot(p.line_strong))
                .child(self.t("ui.status.noTarget"))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.perform("status-target", cx);
                    this.restore_focus(window, cx);
                }))
                .into_any_element(),
        };
        let notice = match self.model.notice_key {
            // 复制 DDL 失败的原因保留原文，显示时按当前语言给出。
            Some("catalog.ddl.failed") if !self.model.notice_detail.is_empty() => {
                let detail = crate::catalog::definition::error_text(&self.model.notice_detail, self.model.locale());
                format!("{} {detail}", self.model.text("catalog.ddl.failed")).trim_end().to_owned()
            }
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
            .text_size(px(TypeScale::XS))
            .text_color(p.text_muted)
            .child(target)
            .child(div().flex_1().min_w(px(0.)).truncate().child(SharedString::from(notice)))
            .when(running, |this| {
                this.child(
                    kit::interactive_states(div().id("status-jobs"), &p, false)
                        .test_support()
                        .role(gpui_kit::Role::Button)
                        .aria_label(self.t("query.status.running"))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .rounded(px(Radius::CONTROL))
                        .px(px(4.))
                        .child(Spinner::new().xsmall().color(p.accent))
                        .child(self.t("query.status.running"))
                        .on_click(act("status-jobs", cx)),
                )
            })
            .child(
                kit::interactive_states(div().id("status-locale"), &p, false)
                    .test_support()
                    .px(px(6.))
                    .rounded(px(Radius::CONTROL))
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
                            .child(div().truncate().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(detail))),
                    )
                    .children(self.env_pill(&row.saved.env, &p))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.model.shell.sidebar_open = true;
                        this.click_connection(&id, false, cx);
                    })),
            );
        }
        if rows.is_empty() {
            connections = connections.child(div().py(px(8.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child(self.t("ui.explorer.emptyHint")));
        }

        let mut recent = div().flex().flex_col().gap(px(2.));
        let runs: Vec<_> = self.model.welcome.recent(&self.model.history.runs).to_vec();
        for entry in runs.iter().take(6) {
            let id = entry.id.clone();
            recent = recent.child(
                list_row(SharedString::from(format!("welcome-run-{id}")), &p, m.row_pad_y)
                    .child(kit::icon(IconName::Clock, 14., p.text_faint))
                    .child(div().flex_1().min_w(px(0.)).truncate().font_family(cx.theme().mono_font_family.clone()).text_size(px(TypeScale::SM)).child(SharedString::from(one_line(&entry.sql))))
                    .child(
                        div()
                            .id(SharedString::from(format!("welcome-run-client-{id}")))
                            .flex_none()
                            .max_w(px(160.))
                            .truncate()
                            .tooltip_text(SharedString::from(entry.connection_name.clone()))
                            .text_size(px(TypeScale::XS))
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
            recent = recent.child(div().py(px(8.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child(self.t("ui.welcome.noRecent")));
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
                                    .child(kit::title("RillSQL", TypeScale::LG, &p)),
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
                    .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(self.t("shell.welcome.factEngines"))),
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
                kit::row_states(div().id(SharedString::from(format!("conn-row-{id}"))), &p)
                    .test_support()
                    .role(gpui_kit::Role::ListItem)
                    .aria_label(SharedString::from(label))
                    .aria_selected(selected)
                    .px(px(8.))
                    .py(px(m.row_pad_y))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(px(Radius::CONTROL))
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
                            .child(div().truncate().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(connection_detail(row)))),
                    )
                    .child(div().flex().flex_col().items_end().gap(px(4.)).children(self.env_pill(&row.saved.env, &p)).child(kit::status_dot(dot)))
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
                    .child(div().text_size(px(TypeScale::SM)).text_color(p.text_faint).child(self.t("ui.explorer.empty")))
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
                    .child(div().flex().items_center().justify_between().child(kit::title(self.t("connections.page.title"), TypeScale::LG, &p)).child(new_button))
                    .child(filter.prefix(kit::icon(IconName::Search, 13., p.text_faint)).cleanable(true))
                    .child(self.connection_filters(&p, cx)),
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
                kit::interactive_states(div().id(SharedString::from(format!("conn-engine-{name}"))), &p, false)
                    .role(gpui_kit::Role::Button)
                    .aria_label(SharedString::from(kit::engine_label(name)))
                    .aria_selected(selected)
                    .h(px(m.control_h + 6.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(Radius::CONTROL))
                    .border_1()
                    .when(selected, |this| this.border_color(p.accent).bg(p.accent_soft).text_color(p.accent_text))
                    .when(!selected, |this| this.border_color(p.line).hover(|style| style.border_color(p.line_strong)))
                    .child(kit::engine_badge(name, 16.))
                    .child(div().text_size(px(TypeScale::SM)).child(kit::engine_label(name)))
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
        let mut env_ids = vec!["dev".to_owned(), "test".to_owned(), "prod".to_owned(), "local".to_owned()];
        for tag in &self.model.env_tags {
            if !env_ids.iter().any(|id| id == &tag.id) {
                env_ids.push(tag.id.clone());
            }
        }
        for env in env_ids {
            let selected = draft.env == env;
            let label = self.env_text(&env);
            env_items.push((
                SharedString::from(format!("conn-env-{env}")),
                label,
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
                    security = security.child(self.path_field("conn-browse-ca", "connections.editor.tls.caFile", "connections.editor.tls.browseCa", ca, DraftField::CaFile, &p, cx));
                }
                let cert = self.input_for(DraftField::ClientCert, "", false, window, cx);
                let key = self.input_for(DraftField::ClientKey, "", false, window, cx);
                security = security.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(12.))
                        .child(div().flex_1().min_w(px(160.)).child(self.path_field(
                            "conn-browse-cert",
                            "connections.editor.tls.clientCert",
                            "connections.editor.tls.browseCert",
                            cert,
                            DraftField::ClientCert,
                            &p,
                            cx,
                        )))
                        .child(div().flex_1().min_w(px(160.)).child(self.path_field(
                            "conn-browse-key",
                            "connections.editor.tls.clientKey",
                            "connections.editor.tls.browseKey",
                            key,
                            DraftField::ClientKey,
                            &p,
                            cx,
                        ))),
                );
            }
            let ssh_on = draft.ssh_on;
            security = security.child(
                kit::interactive_states(div().id("conn-ssh-toggle"), &p, false)
                    .role(gpui_kit::Role::Switch)
                    .aria_label(self.t("connections.editor.ssh.title"))
                    .aria_selected(ssh_on)
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(switch_visual(ssh_on, &p))
                    .tooltip_text(self.t("ui.conn.sshHint"))
                    .child(div().font_weight(FontWeight::MEDIUM).child(self.t("connections.editor.ssh.title")))
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
                            .child(div().flex_1().min_w(px(160.)).child(self.path_field(
                                "conn-browse-ssh",
                                "connections.editor.ssh.keyFile",
                                "connections.editor.ssh.browseKey",
                                ssh_key,
                                DraftField::SshKey,
                                &p,
                                cx,
                            )))
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
                                            .child(div().id("conn-form-title").truncate().tooltip_text(heading.clone()).text_size(px(TypeScale::LG)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(heading))
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

    fn connection_filters(&mut self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let engine = self
            .model
            .connections
            .filter_engine
            .map(|engine| kit::engine_label(engine.as_str()).to_owned())
            .unwrap_or_else(|| self.model.text("connections.filter.engine"));
        let env = if self.model.connections.filter_env.is_empty() {
            self.t("connections.filter.env")
        } else {
            self.env_text(&self.model.connections.filter_env)
        };
        let active = self.model.connections.filter_engine.is_some() || !self.model.connections.filter_env.is_empty() || !self.model.connections.filter_text.trim().is_empty();
        let engine_open = self.model.conn_filter_menu == Some(ConnFilterMenu::Engine);
        let env_open = self.model.conn_filter_menu == Some(ConnFilterMenu::Env);
        let engine_options = self.engine_filter_options();
        let env_options = self.env_filter_options();
        div()
            .flex()
            .flex_wrap()
            .gap(px(6.))
            .child(self.filter_dropdown(
                "conn-filter-engine",
                IconName::Database,
                SharedString::from(engine),
                "conn-filter-engine",
                engine_open,
                engine_options,
                p,
                cx,
            ))
            .child(self.filter_dropdown(
                "conn-filter-env",
                IconName::Funnel,
                env,
                "conn-filter-env",
                env_open,
                env_options,
                p,
                cx,
            ))
            .when(active, |this| this.child(kit::tool_button("conn-filter-clear", Some(IconName::X), self.t("connections.filter.clear"), act("conn-filter-clear", cx))))
            .into_any_element()
    }

    fn engine_filter_options(&self) -> Vec<(String, SharedString, bool, String)> {
        let current = self.model.connections.filter_engine;
        let mut options = vec![(
            "conn-filter-engine-all".to_owned(),
            self.t("connections.filter.engine"),
            current.is_none(),
            "conn-filter-engine:".to_owned(),
        )];
        for engine in engines() {
            options.push((
                format!("conn-filter-engine-{}", engine.as_str()),
                SharedString::from(kit::engine_label(engine.as_str())),
                current == Some(engine),
                format!("conn-filter-engine:{}", engine.as_str()),
            ));
        }
        options
    }

    fn env_filter_options(&self) -> Vec<(String, SharedString, bool, String)> {
        let current = self.model.connections.filter_env.clone();
        let mut ids = vec!["dev".to_owned(), "test".to_owned(), "prod".to_owned(), "local".to_owned()];
        for tag in &self.model.env_tags {
            if !ids.iter().any(|id| id == &tag.id) {
                ids.push(tag.id.clone());
            }
        }
        let mut options = vec![(
            "conn-filter-env-all".to_owned(),
            self.t("connections.filter.env"),
            current.is_empty(),
            "conn-filter-env:".to_owned(),
        )];
        for id in ids {
            options.push((
                format!("conn-filter-env-{id}"),
                self.env_text(&id),
                current == id,
                format!("conn-filter-env:{id}"),
            ));
        }
        options
    }

    fn filter_dropdown(
        &mut self,
        button_id: &'static str,
        icon: IconName,
        label: SharedString,
        command: &'static str,
        open: bool,
        options: Vec<(String, SharedString, bool, String)>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut menu = div()
            .id(SharedString::from(format!("{button_id}-menu")))
            .absolute()
            .top(px(self.metrics().control_h + 4.))
            .left_0()
            .occlude()
            .min_w(px(180.))
            .p(px(4.))
            .flex()
            .flex_col()
            .bg(p.raised)
            .border_1()
            .border_color(p.line)
            .rounded(px(Radius::PANEL))
            .shadow_lg();
        for (id, text, selected, action) in options {
            menu = menu.child(
                kit::interactive_states(div().id(SharedString::from(id)), p, false)
                    .test_support()
                    .role(gpui_kit::Role::MenuItem)
                    .aria_label(text.clone())
                    .aria_selected(selected)
                    .h(px(self.metrics().menu_row_h))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(Radius::CONTROL))
                    .text_size(px(TypeScale::SM))
                    .when(selected, |this| this.bg(p.accent_soft).text_color(p.accent_text))
                    .child(text)
                    .on_click(act(action, cx)),
            );
        }
        div()
            .relative()
            .child(kit::tool_button(button_id, Some(icon), label, act(command, cx)))
            .when(open, |this| this.child(menu))
            .into_any_element()
    }

    fn path_field(
        &mut self,
        id: &'static str,
        label_key: &'static str,
        browse_key: &'static str,
        input: impl IntoElement,
        field: DraftField,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .items_end()
            .gap(px(6.))
            .min_w(px(0.))
            .child(div().flex_1().min_w(px(0.)).child(kit::form_row(self.t(label_key), input, p)))
            .child(kit::secondary_button(id, Some(IconName::FolderOpen), self.t(browse_key), cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.prompt_field_file(field, cx);
            })))
            .into_any_element()
    }

    /// 浏览证书或私钥。取消保留原路径；打不开的文件只写错误，不改 TLS 校验，也不写入钥匙串。
    fn prompt_field_file(&mut self, field: DraftField, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(self.t("ui.conn.browse")),
        });
        cx.spawn(async move |this, cx| {
            let chosen = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let Some(path) = chosen else { return };
            let accepted = crate::app::accept_open_path(&path);
            cx.update(|cx| {
                let _ = this.update(cx, |view, cx| {
                    match accepted {
                        Ok(path) => view.model.set_field(field, path.display().to_string()),
                        Err(detail) => {
                            view.model.notice_key = Some("connections.status.error");
                            view.model.notice_detail = detail;
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn prompt_draft_file(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(self.t("connections.validate.chooseFile")),
        });
        cx.spawn(async move |this, cx| {
            let chosen = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            // 取消保留原来的文件路径。
            let Some(path) = chosen else { return };
            let accepted = crate::app::accept_open_path(&path);
            cx.update(|cx| {
                let _ = this.update(cx, |view, cx| {
                    match accepted {
                        Ok(path) => {
                            if let Some(editor) = view.model.connections.editor.as_mut() {
                                if editor.name.trim().is_empty() {
                                    editor.name = path.file_stem().and_then(|name| name.to_str()).unwrap_or("sqlite").to_owned();
                                }
                            }
                            view.model.set_field(DraftField::File, path.display().to_string());
                        }
                        Err(detail) => {
                            view.model.notice_key = Some("connections.status.error");
                            view.model.notice_detail = detail;
                        }
                    }
                    cx.notify();
                });
            });
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
                    .rounded(px(Radius::PANEL))
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
                    .when(!detail.is_empty(), |this| this.child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(detail)))),
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
                    kit::row_states(div().id(SharedString::from(format!("run-{id}"))), &p)
                        .group("run-row")
                        .role(gpui_kit::Role::ListItem)
                        .aria_label(SharedString::from(entry.sql.clone()))
                        .aria_selected(selected)
                        .p(px(m.row_pad_y + 5.))
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .rounded(px(Radius::PANEL))
                        .border_1()
                        .when(selected, |this| this.border_color(p.accent).bg(p.accent_soft))
                        .when(!selected, |this| this.border_color(p.line).hover(|style| style.border_color(p.line_strong)))
                        .child(
                            div()
                                .min_w(px(0.))
                                .font_family(mono.clone())
                                .text_size(px(TypeScale::SM))
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
                                .text_size(px(TypeScale::XS))
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
                    kit::row_states(div().id(SharedString::from(format!("saved-{id}"))), &p)
                        .group("saved-row")
                        .role(gpui_kit::Role::ListItem)
                        .aria_label(SharedString::from(item.name.clone()))
                        .aria_selected(selected)
                        .p(px(m.row_pad_y + 5.))
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .rounded(px(Radius::PANEL))
                        .border_1()
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
                                        .text_size(px(TypeScale::XS))
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
                        .child(div().min_w(px(0.)).font_family(mono.clone()).text_size(px(TypeScale::SM)).text_color(p.text_muted).max_h(px(36.)).truncate().child(SharedString::from(clip(&item.sql, 200))))
                        .on_mouse_down(MouseButton::Right, cx.listener({
                            let menu = id.clone();
                            move |this, event: &MouseDownEvent, _, cx| {
                                this.menu_at = event.position;
                                this.model.history.section = HistorySection::Saved;
                                this.model.history.selected_saved = Some(menu.clone());
                                this.perform("menu-history", cx);
                            }
                        }))
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
            self.history_preview(window, &known, &p, narrow, cx)
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
                    .child(kit::title(self.t("history.page.titleRuns"), TypeScale::LG, &p))
                    .child(toolbar),
            )
            .child(rename_bar)
            .child(body)
            .into_any_element()
    }

    #[inline(never)]
    fn history_preview(&mut self, window: &mut Window, known: &std::collections::BTreeSet<String>, p: &Palette, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(plan) = self.model.history.preview_plan(known) else {
            return div().into_any_element();
        };
        let status = if self.model.history.section == HistorySection::Runs {
            self.model.history.selected_run_entry().map(|entry| entry.status.clone())
        } else {
            None
        };
        let error = self.model.history.preview_error().map(str::to_owned);
        let target = if plan.needs_target {
            self.model.text("history.plan.deletedNotice")
        } else if plan.database.as_deref().is_some_and(|database| !database.is_empty()) {
            format!("{} / {}", plan.connection_name, plan.database.unwrap_or_default())
        } else {
            plan.connection_name.clone()
        };
        let preview_width = self.model.history_preview_px();
        let editor_key = format!(
            "{}:{}",
            self.model.history.selected_run.as_deref().or(self.model.history.selected_saved.as_deref()).unwrap_or(""),
            plan.sql
        );
        let editor = match &self.history_editor {
            Some((key, editor)) if key == &editor_key => editor.clone(),
            _ => {
                let sql = plan.sql.clone();
                let editor = gpui_kit::AppContext::new(cx, |cx| {
                    gpui_kit::component::input::EditorState::new(window, cx).language("sql").line_number(true).default_value(sql)
                });
                self.history_editor = Some((editor_key, editor.clone()));
                editor
            }
        };
        let status_label = status.map(|status| {
            let ok = status == "ok" || status == "success" || status == "done";
            (if ok { self.t("history.status.ok") } else { self.t("history.status.failed") }, ok)
        });
        let dragging = self.model.history_dragging;
        let reset = cx.entity().downgrade();
        let mut panel = div()
            .id("history-preview")
            .role(gpui_kit::Role::Group)
            .flex_none()
            .min_w(px(0.))
            .min_h(px(0.))
            .when(narrow, |this| this.h_full().w_full().border_t_1())
            .when(!narrow, |this| this.w(px(preview_width)).border_l_1())
            .border_color(p.line)
            .flex()
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<kit::ResizeDrag>, _, cx| {
                if event.drag(cx).key.as_ref() != "history-preview-resize" {
                    return;
                }
                let width = f32::from(event.bounds.right() - event.event.position.x);
                this.model.set_history_preview_width(width);
                this.model.history_dragging = true;
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                if this.model.history_dragging {
                    this.model.history_dragging = false;
                    cx.notify();
                }
            }));
        if !narrow {
            panel = panel.child(kit::resize_handle("history-preview-resize", self.t("history.page.sqlPreview"), dragging, p, move |_, _, cx| {
                if let Some(reset) = reset.upgrade() {
                    reset.update(cx, |this, cx| {
                        this.model.reset_history_preview_width();
                        cx.notify();
                    });
                }
            }));
        }
        panel
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .min_h(px(0.))
                    .p(px(if narrow { 12. } else { 14. }))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.))
                            .child(kit::title(self.t("history.page.sqlPreview"), TypeScale::MD, p))
                            .child(div().flex_1())
                            .child(kit::primary_button("history-preview-load", Some(IconName::SquarePen), self.t("history.page.loadToEditor"), act("history-load", cx)))
                            .child(kit::tool_button("history-preview-copy", Some(IconName::Copy), self.t("history.preview.copy"), act("history-copy", cx)))
                            .child(kit::tool_button("history-preview-save", Some(IconName::Save), self.t("history.preview.saveQuery"), act("history-save-preview", cx)))
                            .child(kit::icon_button("history-preview-close", IconName::X, self.t("history.page.previewClose"), act("history-preview-close", cx))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(kit::engine_badge(&plan.engine, 14.))
                            .child(div().text_size(px(TypeScale::XS)).text_color(if plan.needs_target { p.warn } else { p.text_muted }).child(SharedString::from(target))),
                    )
                    .when_some(status_label, |this, (label, ok)| {
                        this.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .text_size(px(TypeScale::XS))
                                .text_color(if ok { p.ok } else { p.danger })
                                .child(kit::status_dot(if ok { p.ok } else { p.danger }))
                                .child(label),
                        )
                    })
                    .when_some(error.filter(|text| !text.trim().is_empty()), |this, error| {
                        this.child(
                            div()
                                .id("history-preview-error")
                                .test_support()
                                .px(px(8.))
                                .py(px(6.))
                                .rounded(px(Radius::CONTROL))
                                .bg(p.danger_soft)
                                .text_size(px(TypeScale::SM))
                                .text_color(p.danger)
                                .child(SharedString::from(error)),
                        )
                    })
                    .child(
                        div()
                            .id("history-preview-sql")
                            .flex_1()
                            .min_h(px(0.))
                            .rounded(px(Radius::PANEL))
                            .border_1()
                            .border_color(p.line)
                            .bg(p.surface)
                            .overflow_hidden()
                            .child(Editor::new(&editor).readonly(true).h(gpui_kit::relative(1.)).bordered(false).aria_label(self.t("history.page.sqlPreview"))),
                    ),
            )
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
        let updates = kit::interactive_states(div().id("toggle-updates"), &p, false)
            .role(gpui_kit::Role::Switch)
            .aria_label(self.t("shell.update.autoCheck"))
            .aria_selected(s.auto_check_updates)
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
                                .when_some(hint, |this, hint| this.child(div().text_size(px(TypeScale::SM)).text_color(p.text_faint).child(hint))),
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
                    .child(kit::title(self.t("shell.settings.title"), TypeScale::LG, &p))
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
                    .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(format!("RillSQL {}", env!("CARGO_PKG_VERSION"))))),
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
            .rounded(px(Radius::PANEL))
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
                        .h(px(m.topbar_h + 4.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(p.line)
                        .child(kit::title(self.t("shell.command.help"), TypeScale::LG, &p))
                        .child(kit::icon_button("help-close", IconName::X, self.t("common.close"), cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.model.shell.close_overlay();
                            this.restore_focus(window, cx);
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
                        .child(div().id("palette-error").text_size(px(TypeScale::XS)).truncate().tooltip_text(SharedString::from(error.clone())).child(SharedString::from(error)))
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
                    kit::row_states(div().id(SharedString::from(format!("pal-{index}"))), &p)
                        .test_support()
                        .role(gpui_kit::Role::MenuItem)
                        .aria_label(SharedString::from(entry.label.clone()))
                        .aria_selected(selected)
                        .h(px(m.row_h + 2.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .rounded(px(Radius::CONTROL))
                        .when(selected, |this| this.bg(p.accent_soft).text_color(p.accent_text))
                        .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
                        .child(kit::icon(icon, 15., if selected { p.accent_text } else { p.text_muted }))
                        .child(div().flex_1().truncate().child(SharedString::from(entry.label)))
                        .when(!detail.is_empty(), |this| this.child(div().max_w(px(160.)).truncate().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(detail))))
                        .when(!group.is_empty(), |this| this.child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(group)))
                        .when_some(chord, |this, chord| this.child(kit::kbd(kit::shortcut(&chord.replace("Mod+", "Ctrl+")), &p)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.model.shell.close_overlay();
                            this.perform(&id, cx);
                            if !this.model.shell.palette_open && !this.model.shell.help_open {
                                this.restore_focus(window, cx);
                            }
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

    #[inline(never)]
    fn context_layer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let Some(menu) = self.model.menu.clone() else {
            return div().into_any_element();
        };
        let width = 248.;
        let row_h = m.menu_row_h;
        let height = menu_block_height(&menu.items, row_h);
        let view_w = self.model.viewport_width as f32;
        let view_h = self.model.viewport_height.max(1) as f32;
        let anchor_x = f32::from(self.menu_at.x);
        let anchor_y = f32::from(self.menu_at.y);
        let (x, y) = super::menus::place_popup(anchor_x, anchor_y, width, height, view_w, view_h);
        let list = self.context_menu_box("context-menu", menu.place.label_key(), x, y, width, &menu.items, menu.index, None, &p, cx);
        let mut layer = backdrop("context-backdrop", 0., cx).child(list);
        if let Some((parent, child)) = menu.submenu {
            if let Some(parent_item) = menu.items.get(parent) {
                if !parent_item.children.is_empty() {
                    let sub_w = 232.;
                    let sub_h = menu_block_height(&parent_item.children, row_h);
                    let parent_y = y + 4. + parent as f32 * (row_h + 2.);
                    let (sub_x, sub_y) = super::menus::place_submenu(x, parent_y, width, sub_w, sub_h, view_w, view_h);
                    layer = layer.child(self.context_menu_box(
                        "context-submenu",
                        parent_item.label_key,
                        sub_x,
                        sub_y,
                        sub_w,
                        &parent_item.children,
                        child,
                        Some(parent),
                        &p,
                        cx,
                    ));
                }
            }
        }
        layer.into_any_element()
    }

    #[inline(never)]
    fn context_menu_box(
        &mut self,
        element_id: &'static str,
        label_key: &'static str,
        x: f32,
        y: f32,
        width: f32,
        items: &[super::menus::MenuItem],
        selected_index: usize,
        parent: Option<usize>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let m = self.metrics();
        let mut list = div()
            .id(element_id)
            .role(gpui_kit::Role::Menu)
            .aria_label(self.t(label_key))
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
            .rounded(px(Radius::PANEL))
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        for (index, item) in items.iter().enumerate() {
            if item.separator_before || (index > 0 && item.danger && !items[index - 1].danger) {
                list = list.child(div().h(px(1.)).my(px(4.)).mx(px(6.)).bg(p.line));
            }
            let id = item.id.to_owned();
            let has_children = !item.children.is_empty();
            let label = self.t(item.label_key);
            let shortcut = item.shortcut.map(SharedString::from);
            let enabled = item.enabled;
            let selected = index == selected_index;
            let state = if enabled { "enabled" } else { "disabled" };
            let fg = if !enabled {
                p.text_faint
            } else if item.danger {
                p.danger
            } else {
                p.text
            };
            let element = if parent.is_some() { format!("ctx-sub-{id}") } else { format!("ctx-{id}") };
            list = list.child(
                div()
                    .id(SharedString::from(element))
                    .role(gpui_kit::Role::MenuItem)
                    .aria_label(label.clone())
                    .aria_selected(selected)
                    .aria_description(SharedString::from(format!("{state} {}", item.reason_key.unwrap_or(item.id))))
                    .h(px(m.menu_row_h))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(Radius::CONTROL))
                    .text_color(fg)
                    .when(enabled, |this| this.cursor_pointer())
                    .when(enabled && selected, |this| this.bg(p.hover))
                    .when(enabled, |this| this.hover(|style| style.bg(if item.danger { p.danger_soft } else { p.hover })))
                    .when_some(item.icon, |this, icon| this.child(kit::icon(icon, 14., fg)))
                    .child(div().flex_1().min_w(px(0.)).truncate().child(label))
                    .when_some(shortcut, |this, shortcut| this.child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(shortcut)))
                    .when(has_children, |this| this.child(kit::icon(IconName::ChevronRight, 12., p.text_faint)))
                    .when_some(item.reason_key.filter(|_| !enabled), |this, reason| this.tooltip_text(self.t(reason)))
                    .on_mouse_move(cx.listener(move |this, _: &gpui_kit::MouseMoveEvent, _, cx| {
                        let Some(menu) = this.model.menu.as_mut() else { return };
                        if parent.is_some() {
                            return;
                        }
                        if menu.index != index || menu.submenu.map(|pair| pair.0) != has_children.then_some(index) {
                            menu.hover(index);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if !enabled {
                            return;
                        }
                        if has_children {
                            if let Some(menu) = this.model.menu.as_mut() {
                                menu.index = index;
                                let _ = menu.open_submenu();
                            }
                            cx.notify();
                            return;
                        }
                        this.model.menu = None;
                        this.model.shell.focus = this.model.menu_return;
                        this.perform(&id, cx);
                        this.restore_focus(window, cx);
                    })),
            );
        }
        list.into_any_element()
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

    /// 对象定义查看器：只读、SQL 高亮。重建的定义标明来源并列出没有包含的内容。
    fn ddl_layer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let Some(view) = self.model.ddl_view.clone() else {
            return div().into_any_element();
        };
        let name = super::nav::qualified_name(&view.object);
        let title = SharedString::from(format!("{} · {name}", self.model.text("catalog.ddl.title")));
        let body: AnyElement = if view.loading {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_color(p.text_muted)
                .child(Spinner::new().small().color(p.accent))
                .child(self.t("catalog.ddl.loading"))
                .into_any_element()
        } else if let Some(text) = view.text.clone() {
            let editor = match &self.ddl_editor {
                Some((epoch, editor)) if *epoch == view.epoch => editor.clone(),
                _ => {
                    let editor = gpui_kit::AppContext::new(cx, |cx| EditorState::new(window, cx).language("sql").line_number(true).default_value(text.clone()));
                    self.ddl_editor = Some((view.epoch, editor.clone()));
                    editor
                }
            };
            div()
                .id("ddl-viewer-text")
                .test_support()
                .h(px(320.))
                .rounded(px(Radius::PANEL))
                .border_1()
                .border_color(p.line)
                .bg(p.surface)
                .overflow_hidden()
                .child(Editor::new(&editor).readonly(true).h(gpui_kit::relative(1.)).bordered(false).aria_label(title.clone()))
                .into_any_element()
        } else {
            // 模型里留原文，显示时去掉重复的“读取定义失败”前缀并按当前语言给出。
            let error = crate::catalog::definition::error_text(view.error.as_deref().unwrap_or_default(), self.model.locale());
            let text = format!("{} {error}", self.model.text("catalog.ddl.failed")).trim_end().to_owned();
            div()
                .id("ddl-viewer-error")
                .test_support()
                .aria_label(SharedString::from(text.clone()))
                .child(banner(IconName::TriangleAlert, SharedString::from(text), p.danger, p.danger_soft))
                .into_any_element()
        };
        let rebuilt = view.rebuilt.then(|| {
            let mut text = self.model.text("catalog.ddl.rebuilt");
            if !view.missing.is_empty() {
                // 横幅逐项英文化；编辑区里重建 SQL 的注释保持生成时的原样。
                let items = crate::catalog::definition::missing_text(&view.missing, self.model.locale()).join(", ");
                let missing = i18n::fill(&self.model.text("catalog.ddl.missing"), &[("items", &items)]);
                text = format!("{text} {missing}");
            }
            div()
                .id("ddl-viewer-rebuilt")
                .test_support()
                .aria_label(SharedString::from(text.clone()))
                .child(banner(IconName::Info, SharedString::from(text), p.warn, p.warn_soft))
        });
        let has_text = view.text.is_some();
        let dialog = div()
            .id("ddl-viewer")
            .test_support()
            .role(gpui_kit::Role::Dialog)
            .aria_label(title.clone())
            .occlude()
            .w_full()
            .max_w(px(760.))
            .min_w(px(0.))
            .p(px(20.))
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
                    .items_center()
                    .gap(px(8.))
                    .child(kit::icon(IconName::Code, 16., p.accent))
                    .child(div().flex_1().min_w(px(0.)).truncate().child(kit::title(title, TypeScale::LG, &p))),
            )
            .when_some(rebuilt, |this, rebuilt| this.child(rebuilt))
            .child(body)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(kit::secondary_button("cmd-ddl-close", None, self.t("common.close"), act("ddl-close", cx)))
                    .when(has_text, |this| {
                        this.child(kit::primary_button("cmd-ddl-copy", Some(IconName::Copy), self.t("catalog.tree.copyDdl"), act("ddl-copy", cx)))
                    }),
            );
        backdrop("ddl-backdrop", 0.32, cx).flex().items_center().justify_center().px(px(20.)).child(dialog).into_any_element()
    }

    /// 对象级危险操作的确认层：目标、环境、将执行的语句与风险；第一次点执行只待确认，
    /// 生产环境还要输入对象名。
    fn danger_layer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let Some(prompt) = self.model.danger.clone() else {
            return div().into_any_element();
        };
        let name = super::nav::qualified_name(&prompt.object);
        let title = self.t(prompt.kind.title_key());
        let env = if prompt.env.trim().is_empty() || prompt.env == crate::connections::model::ENV_NONE {
            "—".to_owned()
        } else {
            prompt.env_label.clone()
        };
        let facts = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .text_size(px(TypeScale::SM))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(div().w(px(84.)).flex_none().text_color(p.text_muted).child(self.t("catalog.danger.target")))
                    .child(div().id("danger-target").min_w(px(0.)).truncate().child(SharedString::from(name.clone()))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(div().w(px(84.)).flex_none().text_color(p.text_muted).child(self.t("catalog.danger.env")))
                    .child(
                        div()
                            .id("danger-env")
                            .text_color(if prompt.prod { p.danger } else { p.text })
                            .when(prompt.prod, |this| this.font_weight(FontWeight::SEMIBOLD))
                            .child(SharedString::from(env)),
                    ),
            );
        let mut content = div().flex().flex_col().gap(px(10.)).child(facts);
        if prompt.kind == super::danger::DangerKind::Rename {
            let input = self.input_for(DraftField::DangerNewName, &self.model.text("catalog.danger.newName"), false, window, cx);
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w(px(0.)).child(input))
                    .child(kit::secondary_button("cmd-danger-preview", Some(IconName::Eye), self.t("catalog.danger.preview"), act("danger-preview", cx)).disabled(prompt.running)),
            );
        }
        if prompt.loading {
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_color(p.text_muted)
                    .child(Spinner::new().small().color(p.accent))
                    .child(self.t("catalog.danger.loading")),
            );
        } else if prompt.plan.is_none() && prompt.error.is_none() && prompt.kind == super::danger::DangerKind::Rename {
            content = content.child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(self.t("catalog.danger.renameHint")));
        }
        // 计划文字与错误由执行层按中文生成；英文界面按句型表逐条换成英文（数据库原文不动）。
        let locale = if self.model.shell.locale == crate::app::Locale::En { i18n::Locale::En } else { i18n::Locale::ZhCn };
        if let Some(plan) = prompt.plan.as_ref() {
            if !plan.sql.is_empty() {
                let mono = cx.theme().mono_font_family.clone();
                content = content.child(kit::eyebrow(self.t("catalog.danger.sql"), &p)).child(
                    div()
                        .id("danger-sql")
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(px(Radius::PANEL))
                        .bg(p.surface)
                        .border_1()
                        .border_color(p.line)
                        .font_family(mono)
                        .text_size(px(TypeScale::SM))
                        .child(SharedString::from(plan.sql.join("\n"))),
                );
            }
            let mut risks = div().id("danger-risks").flex().flex_col().gap(px(4.));
            for (index, (level, text)) in plan.risks.iter().enumerate() {
                let (fg, bg) = if level == "danger" { (p.danger, p.danger_soft) } else if level == "warn" { (p.warn, p.warn_soft) } else { (p.text_muted, p.hover) };
                let text = crate::ddl::english::localize(text, locale);
                risks = risks.child(
                    div()
                        .id(SharedString::from(format!("danger-risk-{index}")))
                        .test_support()
                        .aria_label(SharedString::from(text.clone()))
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(Radius::PANEL))
                        .bg(bg)
                        .text_color(fg)
                        .text_size(px(TypeScale::SM))
                        .child(SharedString::from(text)),
                );
            }
            content = content.child(risks);
            if !plan.blocked.is_empty() {
                let blocked: Vec<String> = plan.blocked.iter().map(|text| crate::ddl::english::localize(text, locale)).collect();
                let blocked = blocked.join(" ");
                content = content.child(
                    div()
                        .id("danger-blocked")
                        .test_support()
                        .aria_label(SharedString::from(blocked.clone()))
                        .child(banner(IconName::Ban, SharedString::from(blocked), p.danger, p.danger_soft)),
                );
            }
        }
        if let Some(error) = prompt.error.as_ref() {
            let error = prompt.error_text(locale).unwrap_or_else(|| error.clone());
            content = content.child(
                div()
                    .id("danger-error")
                    .test_support()
                    .aria_label(SharedString::from(error.clone()))
                    .child(banner(IconName::TriangleAlert, SharedString::from(error), p.danger, p.danger_soft)),
            );
        }
        if prompt.prod {
            let hint = i18n::fill(&self.model.text("catalog.danger.prodHint"), &[("name", &prompt.object.name)]);
            let input = self.input_for(DraftField::DangerConfirm, &prompt.object.name, false, window, cx);
            content = content
                .child(div().text_size(px(TypeScale::SM)).text_color(p.danger).child(SharedString::from(hint)))
                .child(input);
        }
        if prompt.running {
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_color(p.text_muted)
                    .child(Spinner::new().small().color(p.accent))
                    .child(self.t("catalog.danger.running")),
            );
        }
        let ready = prompt.execute_ready();
        let label = if prompt.armed { self.t("catalog.danger.confirmAgain") } else { self.t("catalog.danger.execute") };
        let dialog = dialog_card("danger-confirm", IconName::TriangleAlert, p.danger, title, SharedString::from(name), &p)
            .test_support()
            .max_w(px(560.))
            .child(div().id("danger-body").max_h(px(420.)).overflow_y_scroll().child(content))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(kit::secondary_button("cmd-danger-cancel", None, self.t("common.cancel"), act("danger-cancel", cx)))
                    .child(kit::danger_button("cmd-danger-execute", Some(IconName::TriangleAlert), label, act("danger-execute", cx)).disabled(!ready)),
            );
        backdrop("danger-backdrop", 0.32, cx).flex().items_center().justify_center().px(px(20.)).child(dialog).into_any_element()
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
                    .child(div().flex().items_center().gap(px(8.)).child(kit::icon(IconName::ArrowDownUp, 15., p.accent)).child(kit::title(self.t("export.open"), TypeScale::LG, &p)))
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

/// 搜索命中的一段：前缀与后缀保持普通文本，中间用当前主题的选中底色强调。
fn highlight_label(label: &SharedString, start: usize, end: usize, p: &Palette) -> AnyElement {
    let text = label.as_ref();
    let head = if start > 0 { SharedString::from(text[..start].to_owned()) } else { SharedString::from("") };
    let hit = SharedString::from(text[start..end].to_owned());
    let tail = if end < text.len() { SharedString::from(text[end..].to_owned()) } else { SharedString::from("") };
    div()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .items_center()
        .overflow_hidden()
        .text_size(px(TypeScale::SM))
        .child(head)
        .child(div().bg(p.selection).rounded(px(Radius::CONTROL)).font_weight(FontWeight::MEDIUM).child(hit))
        .child(tail)
        .into_any_element()
}

fn nav_icon(kind: NavKind, object: Option<ObjectKind>) -> IconName {
    match kind {
        NavKind::Connection | NavKind::Database => IconName::Database,
        NavKind::Schema => IconName::FolderOpen,
        NavKind::Keyspace => IconName::Key,
        NavKind::Group => IconName::Layers,
        NavKind::ColumnGroup => IconName::Columns3,
        NavKind::Column => IconName::Dot,
        NavKind::IndexGroup | NavKind::Index => IconName::Key,
        NavKind::Object => match object.unwrap_or(ObjectKind::Table) {
            ObjectKind::Table => IconName::Table,
            ObjectKind::View => IconName::Eye,
            ObjectKind::Function | ObjectKind::Procedure => IconName::Code,
            ObjectKind::Trigger => IconName::Zap,
            ObjectKind::Sequence => IconName::ListOrdered,
            ObjectKind::Enum => IconName::ListFilter,
            ObjectKind::Domain => IconName::Hash,
            ObjectKind::Extension => IconName::Plug,
            ObjectKind::Event => IconName::Clock,
        },
    }
}

fn nav_tint(kind: NavKind, p: &Palette) -> gpui_kit::Hsla {
    match kind {
        NavKind::Object => p.text_muted,
        NavKind::Column | NavKind::ColumnGroup | NavKind::Index | NavKind::IndexGroup => p.text_faint,
        NavKind::Group => p.text_muted,
        _ => p.text_faint,
    }
}

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
    kit::interactive_states(div().id(SharedString::from(format!("cmd-{id}"))), p, false)
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
        .rounded(px(Radius::CONTROL))
        .when(active, |this| this.bg(p.accent_soft))
        .when(!active, |this| this.hover(|style| style.bg(p.hover)))
        .child(kit::icon(icon, 18., if active { p.accent_text } else { p.text_muted }))
        .when(active, |this| this.child(div().absolute().left(px(-7.)).top(px(9.)).w(px(3.)).h(px(20.)).rounded(px(RAIL_INDICATOR_RADIUS)).bg(accent)))
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
    kit::interactive_states(div().id(gpui_kit::ElementId::Name(id.into())), p, false)
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
        .when(active, |this| this.bg(p.surface).text_color(p.text))
        .when(!active, |this| this.text_color(p.text_muted).hover(|style| style.bg(p.hover).text_color(p.text)))
        .when(active, |this| this.child(div().absolute().top_0().left_0().right_0().h(px(2.)).bg(accent)))
        .child(if running {
            Spinner::new().xsmall().color(p.accent).into_any_element()
        } else {
            kit::icon(icon, 13., if active { p.accent } else { p.text_faint }).into_any_element()
        })
        .child(div().truncate().text_size(px(TypeScale::SM)).child(title))
        .when(dirty, |this| this.child(div().flex_none().size(px(6.)).rounded_full().bg(p.text_faint)))
        .test_support()
}

fn list_row(id: SharedString, p: &Palette, pad_y: f32) -> gpui_kit::Stateful<gpui_kit::Div> {
    kit::interactive_states(div().id(id), p, false)
        .role(gpui_kit::Role::Button)
        .px(px(8.))
        .py(px(pad_y))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(px(Radius::CONTROL))
        .hover(|style| style.bg(p.hover))
}

fn section_head(title: SharedString, link: Option<(&'static str, SharedString, &'static str)>, p: &Palette, cx: &mut Context<Workbench>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(kit::title(title, TypeScale::MD, p))
        .when_some(link, |this, (id, label, command)| {
            this.child(Button::new(id).link().small().label(label.clone()).accessibility_label(label).on_click(act(command, cx)))
        })
        .into_any_element()
}

fn action_card(id: &'static str, icon: IconName, title: SharedString, detail: SharedString, p: &Palette, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> AnyElement {
    kit::interactive_states(div().id(id), p, false)
        .role(gpui_kit::Role::Button)
        .aria_label(title.clone())
        .flex_1()
        .min_w(px(200.))
        .p(px(14.))
        .flex()
        .items_start()
        .gap(px(12.))
        .rounded(px(Radius::PANEL))
        .border_1()
        .border_color(p.line)
        .bg(p.surface)
        .hover(|style| style.border_color(p.accent).bg(p.accent_soft))
        .child(div().flex_none().size(px(34.)).rounded(px(Radius::PANEL)).bg(p.accent_soft).flex().items_center().justify_center().child(kit::icon(icon, 17., p.accent_text)))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.))
                .min_w(px(0.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(detail)),
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
        .rounded(px(Radius::PANEL))
        .bg(bg)
        .text_color(fg)
        .text_size(px(TypeScale::SM))
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
        .rounded(px(Radius::PANEL))
        .shadow_lg()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .gap(px(12.))
                .child(div().flex_none().size(px(34.)).rounded_full().bg(tone.opacity(0.14)).flex().items_center().justify_center().child(kit::icon(icon, 17., tone)))
                .child(div().flex().flex_col().gap(px(6.)).min_w(px(0.)).child(kit::title(title, TypeScale::LG, p)).child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(body))),
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
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            entity.update(cx, |this, cx| {
                let effect = this.model.dismiss();
                this.model.history.confirm_clear = false;
                this.model.history.confirm_delete = false;
                this.apply_effect(effect, cx);
                this.restore_focus(window, cx);
                cx.notify();
            });
        })
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
}

fn menu_block_height(items: &[super::menus::MenuItem], row_h: f32) -> f32 {
    let rows = items.len() as f32;
    let separators = items.iter().filter(|item| item.separator_before || item.danger).count() as f32;
    8. + rows * (row_h + 2.) + separators * 6.
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

/// 是否画环境标签：未打标签（空或 `none`）的连接不显示标签小块。
pub(super) fn env_shown(env: &str) -> bool {
    let env = env.trim();
    !env.is_empty() && env != crate::connections::model::ENV_NONE
}

/// 存过名称的标签用 `EnvTag.label`。没有记录时，内置环境走字典；字典也没有才显示 id。
pub(super) fn env_label(text: impl Fn(&str) -> String, env: &str, tags: &[EnvTag]) -> String {
    if let Some(tag) = tags.iter().find(|tag| tag.id == env) {
        let label = tag.label.trim();
        if !label.is_empty() {
            return label.to_owned();
        }
    }
    let key = format!("ui.conn.env.{env}");
    let value = text(&key);
    if value == key { env.to_owned() } else { value }
}

/// 色块用 `EnvTag.tone`。没有色调时退回环境 id，内置色仍按 dev/test/prod 取。
pub(super) fn env_tone<'a>(env: &'a str, tags: &'a [EnvTag]) -> &'a str {
    if let Some(tag) = tags.iter().find(|tag| tag.id == env) {
        let tone = tag.tone.trim();
        if !tone.is_empty() {
            return tone;
        }
    }
    env
}
