//! 外壳的画法：顶栏、活动栏、资源管理器、标签条、状态栏、页面与浮层。
//! 状态与命令都在 `WorkbenchModel`；这里只把它画出来，并把点击交回 `perform`。

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::Input;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Editor, EditorState, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, TitleBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, DragMoveEvent, Entity, FocusHandle, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, point, px,
};

use crate::app::views::connections::{engines, LinkStatus};
use crate::app::views::history::{HistorySection, ListPhase};
use crate::app::views::settings::{SaveState, SettingsSection};
use crate::app::{AppPage, FocusTarget, ThemePreference};
use crate::catalog::hierarchy::{NavKind, NavRow};
use crate::catalog::model::ObjectKind;
use crate::connections::model::{CaMode, Engine, EnvTag, SshAuthKind, TlsMode};
use crate::i18n;
use crate::ui::kit::{self, FieldSpec, Palette, Tip as _, Radius, TypeScale};

use gpui_kit::base::FocusTrapElement as _;

use super::conn_modal;
use super::model::{ConnFilterMenu, DraftField, WorkItemKind, WorkTool};
use super::view::Workbench;

/// 活动栏宽度来自布局模块，绘制与几何只保留一份。
const RAIL_W: f32 = super::layout::RAIL_WIDTH as f32;
/// 活动栏激活指示条（3px 宽的竖线）两端的圆角。线条不是控件，不取 `Radius` token；见扫描测试例外清单。
const RAIL_INDICATOR_RADIUS: f32 = 2.;

/// 拖拽连接时的拖动图形：不画内容，避免挡住目标分组标题。
struct ConnectionDragGhost;

impl Render for ConnectionDragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(0.))
    }
}

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
        self.field_input(field, placeholder, masked, window, cx).0
    }

    /// 同 `input_for`，另外把输入框实体交出来：打开模态和 Tab 换位时要用它的焦点句柄。
    pub(super) fn field_input(
        &mut self,
        field: DraftField,
        placeholder: &str,
        masked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Input, Entity<InputState>) {
        let value = self.model.field_value(field);
        let state = self.fields.bind(field, &value, FieldSpec { placeholder, masked }, window, cx, field_changed);
        let label = if placeholder.is_empty() { format!("{field:?}") } else { placeholder.to_owned() };
        let input = kit::text_input(SharedString::from(format!("field-{field:?}")), &state, label);
        (input, state)
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
        // 模态关闭后把真实焦点还给触发它的控件；触发控件已不在时退回来源区域。
        if self.model.conn_modal.restore_origin {
            self.model.conn_modal.restore_origin = false;
            match self.model.conn_modal.last_control.take() {
                Some(super::conn_modal::ModalControl::LauncherNew) => window.focus(&self.conn_launcher_focus, cx),
                Some(super::conn_modal::ModalControl::SidebarNew) => window.focus(&self.conn_sidebar_focus, cx),
                None => self.restore_focus(window, cx),
            }
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
                        let entity = cx.entity();
                        // 覆盖式侧栏下面压一层轻遮罩：看得出侧栏浮在内容上方，点遮罩收起侧栏。
                        let scrim = div()
                            .id("sidebar-scrim")
                            .test_support()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(RAIL_W))
                            .right_0()
                            .bg(gpui_kit::hsla(0., 0., 0., if p.dark { 0.45 } else { 0.18 }))
                            .occlude()
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                entity.update(cx, |this, cx| {
                                    this.model.shell.sidebar_open = false;
                                    this.persist_if_needed();
                                    cx.notify();
                                });
                            });
                        this.child(scrim).child(
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
        if self.model.conn_filter_menu.is_some() {
            root = root.child(self.conn_filter_layer(cx));
        }
        if self.model.menu.is_some() {
            root = root.child(self.context_layer(cx));
        }
        // 连接模态盖在最上层：backdrop 吞掉下面控件的点击，Esc 与 Tab 也归它。
        if self.model.conn_modal.open {
            root = root.child(self.connection_modal_layer(window, cx));
        }
        if self.model.close_prompt.is_some() {
            root = root.child(self.close_layer(cx));
        }
        if self.model.connections.confirm_delete {
            root = root.child(self.delete_layer(cx));
        }
        if self.model.group_delete.is_some() {
            root = root.child(self.group_delete_layer(cx));
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
        // 连接模态打开时，Esc 关闭、Tab 在模态里循环、回车等于当前页脚动作。
        if self.model.conn_modal.open && self.model.menu.is_none() && !self.model.shell.palette_open && self.model.danger.is_none() {
            if key == "escape" {
                self.perform("conn-cancel", cx);
                cx.notify();
                return;
            }
            // Tab/Shift+Tab 由面板上的 GPUI focus trap 处理（Root 的 Tab 动作在容器内成环），
            // 这里不再自绘序列——那会被 Root 的绑定抢先吃掉。
            // 回车落在已登记的控件上就激活它；落在输入框上时交还给输入框。
            if key == "enter" && self.modal_activate_focused(window, cx) {
                cx.notify();
                return;
            }
            if super::view::nav_key(key).is_some() {
                // 模态打开时方向键/回车不再驱动下面的对象树与页面。
                return;
            }
        }
        if key == "escape" && self.model.conn_filter_menu.is_some() && self.model.menu.is_none() {
            self.model.conn_filter_menu = None;
            self.restore_focus(window, cx);
            cx.notify();
            return;
        }
        // 分组输入面板与删除确认：Esc 只取消这一层，不改任何数据。
        if key == "escape" && self.model.group_delete.is_some() && self.model.menu.is_none() {
            self.perform("group-delete-cancel", cx);
            self.restore_focus(window, cx);
            cx.notify();
            return;
        }
        if key == "escape" && self.model.group_prompt.is_some() && self.model.menu.is_none() && !self.model.conn_modal.open {
            self.perform("group-prompt-cancel", cx);
            self.restore_focus(window, cx);
            cx.notify();
            return;
        }
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
        // 清空历史 / 删除保存查询的确认层开着时，页面级键一律让路：确认层自己不管键盘焦点，
        // 键会落到侧栏对象树或历史列表上，那样"确认"的就会是另一个对象。
        // 这一条放在所有页面级分支之前，侧栏与列表两条路径共用同一个判断。
        if self.model.menu.is_none()
            && self.model.close_prompt.is_none()
            && !self.model.shell.palette_open
            && self.model.history.confirm_open()
        {
            // Esc 已在上面按确认层取消；其余键交给确认条自己的按钮。
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
        // 历史页的方向键属于列表：上下/Home/End 在筛选后的可见行里选，
        // 回车只载入编辑器（不执行），Delete 删除当前条目（与条目右键同一条命令）。
        // 有键位绑定时 Delete 作为动作到 perform，没有时在这里直接落到同一条命令。
        // 清空历史 / 删除保存查询的确认层开着时上面已经统一让路，这里不再重复判断。
        if self.model.menu.is_none()
            && self.model.close_prompt.is_none()
            && !self.model.shell.palette_open
            && self.model.shell.page == AppPage::History
            && self.model.shell.focus != FocusTarget::Sidebar
        {
            if key == "delete" {
                if let Some(command) = self.model.history.delete_command() {
                    self.perform(command, cx);
                    cx.notify();
                    return;
                }
            }
            let nav = match key {
                "up" => Some(super::menus::NavKey::ArrowUp),
                "down" => Some(super::menus::NavKey::ArrowDown),
                "home" => Some(super::menus::NavKey::Home),
                "end" => Some(super::menus::NavKey::End),
                "enter" => Some(super::menus::NavKey::Enter),
                _ => None,
            };
            if let Some(nav) = nav {
                if let Some(command) = self.model.history_key(nav) {
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
                let id = self.model.menu.as_ref().and_then(|menu| menu.current().map(|item| item.command()));
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
        let group_prompt = self.group_prompt_panel(window, cx);
        // 对象索引来自模型里的同一份快照；连接列表保持完整，连接筛选只属于连接页。
        // 可见行先拍成扁平序列（分组标题 → 连接 → 树行），渲染只创建视口窗口里的行。
        let mut list = div()
            .id("explorer-list")
            .test_support()
            .track_focus(&self.nav_focus)
            .track_scroll(&self.explorer_scroll)
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(6.))
            .pb(px(8.));
        if self.model.connections.rows.is_empty() {
            list = list.child(kit::empty_state_with_action(
                IconName::Database,
                self.t("ui.explorer.empty"),
                self.t("ui.explorer.emptyHint"),
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(4.))
                    .child(self.launcher_action(
                        "explorer-new",
                        IconName::Plus,
                        self.t("common.newConnection"),
                        Some(&self.conn_sidebar_focus.clone()),
                        "conn-new-sidebar",
                        true,
                        &p,
                        cx,
                    ))
                    .child(kit::tool_button(
                        "explorer-sqlite",
                        Some(IconName::FolderOpen),
                        self.t("common.openSqlite"),
                        act("open-sqlite", cx),
                    )),
                &p,
            ));
        } else {
            let items = self.model.explorer_rows();
            let heights = self.model.explorer_heights(&items);
            let viewport = f32::from(self.explorer_scroll.bounds().size.height);
            // 跳转定义与键盘定位：把目标行滚进视口一次，滚过就清掉。
            let reveal = self
                .model
                .reveal_row
                .take()
                .or_else(|| self.model.reveal_node.take().map(|id| format!("nav:{id}")));
            if let Some(reveal) = reveal {
                if let Some(index) = items.iter().position(|item| item.id() == reveal) {
                    let offset = super::explorer::scroll_offset_for(&heights, index, viewport);
                    self.explorer_scroll.set_offset(point(px(0.), px(-offset)));
                }
            }
            let scroll_top = -f32::from(self.explorer_scroll.offset().y);
            let window = super::explorer::window_rows(&heights, scroll_top, viewport, super::explorer::OVERSCAN_ROWS);
            if window.pad_top > 0. {
                list = list.child(div().id("explorer-pad-top").flex_none().h(px(window.pad_top)));
            }
            for item in &items[window.range.clone()] {
                list = list.child(self.explorer_row(item, &p, cx));
            }
            if window.pad_bottom > 0. {
                list = list.child(div().id("explorer-pad-bottom").flex_none().h(px(window.pad_bottom)));
            }
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
                                    // 分组入口就在树旁边：新建组、右键组改名/删除、拖连接归组。
                                    .child(kit::icon_button("explorer-group-add", IconName::Folder, self.t("ui.group.new"), act("group-new", cx)).xsmall())
                                    .child(kit::icon_button("explorer-add", IconName::Plus, self.t("common.newConnection"), act("new-connection", cx)).xsmall()),
                            ),
                    )
                    // 没有连接时没有可筛的对象，筛选框不出现。
                    .when(!self.model.connections.rows.is_empty(), |this| {
                        this.child(div().px(px(10.)).pb(px(8.)).child(filter.prefix(kit::icon(IconName::Search, 13., p.text_faint)).cleanable(true)))
                    })
                    .when_some(group_prompt, |this, prompt| this.child(prompt))
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

    /// 扁平序列里的一行。每行都用稳定 id 绑定动作：图形不随滚动错位，命令也不按下标走。
    fn explorer_row(&self, item: &super::explorer::ExplorerItem, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        use super::explorer::ExplorerItem;
        match item {
            ExplorerItem::Group { id, collapsed, members } => self.group_row(id, *collapsed, *members, p, cx),
            ExplorerItem::Connection { id } => {
                let status = self.model.connections.status.get(id).copied().unwrap_or(LinkStatus::Disconnected);
                let expanded = self.model.nav_connection_expanded(id) && status == LinkStatus::Connected;
                let selected = self.model.connections.selected.as_deref() == Some(id.as_str());
                match self.model.connections.rows.iter().find(|row| row.saved.id == *id) {
                    Some(row) => self.connection_row(row, status, expanded, selected, p, cx),
                    // 列表刚变、这一帧找不到记录：画一个占位行，不做越界索引。
                    None => div().id(SharedString::from(format!("side-missing-{id}"))).h(px(self.metrics().row_h)).into_any_element(),
                }
            }
            ExplorerItem::ConnectionError { connection_id, error } => self.connection_error_row(connection_id, error, p, cx),
            ExplorerItem::Note { connection_id, key } => {
                let m = self.metrics();
                div()
                    .id(SharedString::from(format!("nav-note-{connection_id}")))
                    .test_support()
                    .flex_none()
                    .h(px(m.tree_row_h))
                    .pl(px(42.))
                    .flex()
                    .items_center()
                    .text_size(px(TypeScale::SM))
                    .text_color(p.text_faint)
                    .child(self.t(key))
                    .into_any_element()
            }
            ExplorerItem::Nav(row) => self.nav_row(row, false, p, cx),
            ExplorerItem::CatalogError { message } => {
                let m = self.metrics();
                let error = self.model.text(message);
                let full_error = format!("{}: {}", self.model.text("ui.explorer.error"), error);
                div()
                    .id("explorer-error")
                    .test_support()
                    .flex_none()
                    .h(px(m.row_h))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(Radius::CONTROL))
                    .bg(p.danger_soft)
                    .child(
                        div()
                            .id("explorer-error-text")
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
                    ))
                    .into_any_element()
            }
            ExplorerItem::NoMatch => {
                let m = self.metrics();
                div()
                    .id("explorer-search-empty")
                    .test_support()
                    .flex_none()
                    .h(px(m.tree_row_h))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .text_size(px(TypeScale::SM))
                    .text_color(p.text_faint)
                    .child(self.t("ui.explorer.noMatch"))
                    .into_any_element()
            }
        }
    }

    /// 分组标题：折叠箭头、名称、成员数量。拖拽到这里才归组，右键只属于这一个分组。
    fn group_row(&self, id: &str, collapsed: bool, members: usize, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let m = self.metrics();
        let name = self.model.group(id).map(|group| group.name.clone()).unwrap_or_default();
        let focused = matches!(&self.model.explorer_current, Some(super::explorer::ExplorerRow::Group(current)) if current == id);
        let chevron = if collapsed { IconName::ChevronRight } else { IconName::ChevronDown };
        let toggle_id = id.to_owned();
        let click_id = id.to_owned();
        let menu_id = id.to_owned();
        let drop_id = id.to_owned();
        let drop_soft = p.accent_soft;
        let entity = cx.entity().downgrade();
        kit::row_states(div().id(SharedString::from(format!("side-group-{id}"))), p)
            .test_support()
            .role(gpui_kit::Role::TreeItem)
            .aria_label(SharedString::from(name.clone()))
            .aria_selected(focused)
            .aria_expanded(!collapsed)
            .aria_description(SharedString::from(format!("{} · {members}", self.t("ui.group.members"))))
            .flex_none()
            .h(px(m.tree_row_h))
            .px(px(4.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(Radius::CONTROL))
            .when(focused, |this| this.bg(p.selection))
            .when(!focused, |this| this.hover(|style| style.bg(p.hover)))
            // 拖拽悬停：拖连接经过这一行时整行高亮，放手才真的移动。
            .drag_over::<super::explorer::ConnectionDrag>(move |style, _, _, _| style.bg(drop_soft))
            .on_drop::<super::explorer::ConnectionDrag>(move |drag, _, cx| {
                let connection = drag.connection.clone();
                let group = drop_id.clone();
                if let Some(entity) = entity.upgrade() {
                    entity.update(cx, |this, cx| {
                        this.model.shell.focus = FocusTarget::Sidebar;
                        this.model.assign_connection_group(&connection, Some(&group));
                        cx.notify();
                    });
                }
            })
            .child(
                div()
                    .id(SharedString::from(format!("group-chevron-{id}")))
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(self.t(if collapsed { "catalog.tree.expand" } else { "catalog.tree.collapse" }))
                    .tooltip_text(self.t(if collapsed { "catalog.tree.expand" } else { "catalog.tree.collapse" }))
                    .flex_none()
                    .w(px(12.))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .child(kit::icon(chevron, 12., p.text_faint))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.perform(&format!("group-toggle:{toggle_id}"), cx);
                        cx.notify();
                    })),
            )
            .child(kit::icon(IconName::Folder, 13., p.text_muted))
            .child(div().flex_1().min_w(px(0.)).truncate().font_weight(FontWeight::MEDIUM).child(SharedString::from(name.clone())))
            .child(
                div()
                    .flex_none()
                    .px(px(5.))
                    .rounded(px(Radius::TAG))
                    .bg(p.hover)
                    .text_size(px(TypeScale::XS))
                    .text_color(p.text_muted)
                    .child(SharedString::from(members.to_string())),
            )
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.menu_at = event.position;
                this.model.shell.focus = FocusTarget::Sidebar;
                this.model.set_explorer_current(super::explorer::ExplorerRow::Group(menu_id.clone()));
                this.model.open_group_menu(&menu_id);
                cx.notify();
            }))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.model.shell.focus = FocusTarget::Sidebar;
                window.focus(&this.nav_focus, cx);
                this.model.set_explorer_current(super::explorer::ExplorerRow::Group(click_id.clone()));
                this.perform(&format!("group-toggle:{click_id}"), cx);
                cx.notify();
            }))
            .into_any_element()
    }

    /// 分组名输入面板：新建与重命名共用，Esc 取消（由外壳统一处理）。
    fn group_prompt_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let prompt = self.model.group_prompt.clone()?;
        let p = Palette::of(cx);
        let title = if prompt.id.is_some() { "ui.group.rename" } else { "ui.group.new" };
        let input = self.input_for(DraftField::GroupName, &self.model.text("ui.group.namePlaceholder"), false, window, cx);
        Some(
            div()
                .id("group-prompt")
                .test_support()
                .flex()
                .flex_col()
                .gap(px(6.))
                .px(px(10.))
                .pb(px(8.))
                .child(kit::eyebrow(self.t(title), &p))
                .child(input.cleanable(true))
                .when_some(prompt.error.clone(), |this, error| {
                    this.child(div().text_size(px(TypeScale::XS)).text_color(p.danger).child(SharedString::from(error)))
                })
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(6.))
                        .child(kit::secondary_button("group-prompt-cancel", None, self.t("common.cancel"), act("group-prompt-cancel", cx)))
                        .child(kit::primary_button("group-prompt-save", None, self.t("common.save"), act("group-save", cx))),
                )
                .into_any_element(),
        )
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
            .when(self.model.menu.is_none(), |this| this.tooltip_text(full_name))
            .flex_none()
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
                        // 展开一条还没连上的连接先按需连接，连上后自动展开：不再只有点整行才建连。
                        if this.model.connections.row_status(&chevron_id) == LinkStatus::Connected {
                            this.model.toggle_nav_connection(&chevron_id);
                        } else {
                            this.model.queue_connect(super::conn_modal::ConnectIntent::new(
                                chevron_id.clone(),
                                super::conn_modal::ConnectAction::Expand,
                                None,
                            ));
                        }
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
                this.model.set_explorer_current(super::explorer::ExplorerRow::Connection(menu_id.clone()));
                this.perform("menu-connection", cx);
            }))
            // 拖拽归组：拖到分组标题上放手，落库路径与右键“移动到分组”完全相同。
            .on_drag(
                super::explorer::ConnectionDrag { connection: id.clone() },
                |_, _, _, cx| cx.new(|_| ConnectionDragGhost),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.model.set_explorer_current(super::explorer::ExplorerRow::Connection(click_id.clone()));
                this.click_connection(&click_id, event.click_count() >= 2, cx);
            }))
            .into_any_element()
    }

    /// 连接级的读取失败：原因写在连接下面，重试只重读这条连接。
    fn connection_error_row(&self, id: &str, error: &str, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let error = self.model.text(error);
        let node_id = crate::catalog::hierarchy::connection_node_id(id);
        let full = format!("{}: {error}", self.t("catalog.tree.nodeError"));
        div()
            .id(SharedString::from(format!("nav-conn-error-{id}")))
            .test_support()
            .flex_none()
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
                // 按需连接：连上后展开这一行（Redis/MongoDB 由续接动作打开浏览器），失败保留重试。
                self.model.expand_nav_connection(id);
                self.model.queue_connect(super::conn_modal::ConnectIntent::new(
                    id.to_owned(),
                    super::conn_modal::ConnectAction::Expand,
                    None,
                ));
                cx.notify();
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
        let leaf_column = row.kind == NavKind::Column;
        let (column_type, column_marks) = split_column_detail(&row.detail);
        let icon = if leaf_column { column_icon(&column_type, row.primary) } else { nav_icon(row.kind, row.object_kind) };
        let icon_tint = if leaf_column && row.primary { p.accent } else { nav_tint(row.kind, p) };
        let chevron = if row.expanded { IconName::ChevronDown } else { IconName::ChevronRight };
        let detail_text = match row.kind {
            NavKind::Column => column_type.to_lowercase(),
            NavKind::Index => row.detail.clone(),
            _ => String::new(),
        };
        let tooltip = if leaf_column && !row.detail.is_empty() {
            SharedString::from(format!("{} — {}", row.name, row.detail))
        } else if row.path.is_empty() {
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
            // 右键菜单打开时不再挂悬停提示：提示浮层会盖住菜单的第一项。
            .when(self.model.menu.is_none(), |this| this.tooltip_text(tooltip))
            .flex_none()
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
                    .flex_none()
                    .w(px(12.))
                    .when(row.expandable, |this| {
                        this.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.model.shell.focus = FocusTarget::Sidebar;
                            this.perform(&format!("nav-toggle:{toggle_id}"), cx);
                            cx.notify();
                        }))
                        .child(kit::icon(chevron, 12., p.text_faint))
                    }),
            )
            .child(kit::icon(icon, 13., icon_tint))
            .child(body)
            .when(!detail_text.is_empty(), |this| {
                this.child(
                    div()
                        .flex_none()
                        .max_w(px(120.))
                        .truncate()
                        .text_size(px(TypeScale::XS))
                        .text_color(p.text_faint)
                        .child(SharedString::from(detail_text.clone())),
                )
            })
            .when(leaf_column && column_marks.contains("not null") && !row.primary, |this| {
                this.child(div().flex_none().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from("*")))
            })
            .when(is_group && !row.unloaded && row.error.is_none(), |this| {
                this.child(
                    div()
                        .flex_none()
                        .px(px(5.))
                        .rounded(px(Radius::TAG))
                        .bg(p.hover)
                        .text_size(px(TypeScale::XS))
                        .text_color(p.text_muted)
                        .child(SharedString::from(row.total.to_string())),
                )
            })
            .when(row.stale, |this| {
                this.child(div().text_size(px(TypeScale::XS)).text_color(p.warn).child(self.t("catalog.tree.stale")))
            })
            .when(row.loading, |this| this.child(Spinner::new().xsmall().color(p.text_muted)))
            .when_some(row.error.clone(), |this, error| {
                let error = self.model.text(&error);
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
            self.welcome_page(window, cx)
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
            // 设置写盘失败是全局事实：不只在设置页可见，状态栏常驻入口把它带回设置页。
            .when(self.model.settings_save.failed(), |this| {
                this.child(
                    kit::interactive_states(div().id("status-settings-save"), &p, false)
                        .test_support()
                        .role(gpui_kit::Role::Button)
                        .aria_label(self.t("ui.settings.saveFailed"))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .rounded(px(Radius::CONTROL))
                        .px(px(6.))
                        .text_color(p.danger)
                        .child(kit::icon(IconName::CircleAlert, 12., p.danger))
                        .child(self.t("ui.settings.saveFailed"))
                        .on_click(act("settings", cx)),
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

    fn welcome_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        // 欢迎区与连接页是同一个启动台：同一份最近/全部连接、同一组入口、同一套行操作。
        let recent_runs: Vec<_> = self.model.welcome.recent(&self.model.history.runs).to_vec();
        let mut history_rows = div().flex().flex_col().gap(px(2.));
        for entry in recent_runs.iter() {
            let id = entry.id.clone();
            history_rows = history_rows.child(
                list_row(SharedString::from(format!("welcome-run-{id}")), &p, m.row_pad_y)
                    .child(kit::icon(IconName::Clock, 14., p.text_faint))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(TypeScale::SM))
                            .child(SharedString::from(one_line(&entry.sql))),
                    )
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

        div()
            .id("welcome-page")
            .role(gpui_kit::Role::Group)
            .aria_label(self.t("shell.welcome.title"))
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .px(px(m.page_pad))
                    .pt(px(m.page_pad_y))
                    .pb(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(brand_mark(36., &p))
                    // 欢迎区主标题不超过 16px：它标识产品，不做宣传口号（S8.6）。
                    .child(kit::title("RillSQL", TypeScale::LG, &p))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("welcome-history")
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text_muted)
                            .child(kit::icon(IconName::Clock, 13., p.text_faint))
                            .child(self.t("ui.welcome.allHistory"))
                            .on_click(act("history", cx)),
                    ),
            )
            .child(self.launcher_header(window, cx))
            .child(self.launcher_body(cx))
            // 没有执行记录时不留一张只写着“没有记录”的卡片。
            .when(!recent_runs.is_empty(), |this| this.child(
                kit::card(&p)
                    .flex_none()
                    .mx(px(m.page_pad))
                    .mb(px(10.))
                    .p(px(8.))
                    .max_h(px(168.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(section_label(self.t("shell.welcome.recent"), &p))
                    .child(div().id("welcome-recent-runs").flex_1().min_h(px(0.)).overflow_y_scroll().child(history_rows)),
            ))
            .into_any_element()
    }


    // ------------------------------------------------------------ 启动台

    /// 连接页与欢迎区共用的启动台：最近连接、全部连接、紧凑行与全部入口。
    /// 新建/编辑不再在页面里开第二块编辑区，而是打开同一个全局模态。
    fn connections_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
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
            .child(self.launcher_body(cx))
            .child(
                div()
                    .flex_none()
                    .px(px(m.page_pad))
                    .py(px(8.))
                    .border_t_1()
                    .border_color(p.line)
                    .text_size(px(TypeScale::XS))
                    .text_color(p.text_faint)
                    .child(self.t("connections.page.note")),
            )
            .into_any_element()
    }

    /// 启动台头部：标题、主入口、搜索与筛选。三个入口都只有一条命令。
    fn launcher_header(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let filter = self.input_for(DraftField::Filter, &self.model.text("ui.conn.search"), false, window, cx);
        let shown = self.model.connections.filtered().len();
        let total = self.model.connections.rows.len();
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
                    .child(kit::muted(SharedString::from(format!("{shown} / {total}")), &p))
                    .child(div().flex_1())
                    .child(self.launcher_action(
                        "conn-new",
                        IconName::Plus,
                        self.t("common.newConnection"),
                        Some(&self.conn_launcher_focus.clone()),
                        "conn-new-launcher",
                        true,
                        &p,
                        cx,
                    ))
                    .child(self.launcher_action(
                        "launcher-sqlite",
                        IconName::FolderOpen,
                        self.t("common.openSqlite"),
                        None,
                        "open-sqlite",
                        false,
                        &p,
                        cx,
                    ))
                    .child(self.launcher_action(
                        "launcher-import",
                        IconName::Download,
                        self.t("ui.conn.import"),
                        None,
                        "conn-import",
                        false,
                        &p,
                        cx,
                    ))
                    .child(self.launcher_action(
                        "launcher-export",
                        IconName::Upload,
                        self.t("ui.conn.export"),
                        None,
                        "conn-export",
                        false,
                        &p,
                        cx,
                    )),
            )
            // 一条连接都没有时，搜索与筛选没有可作用的对象，不占一行。
            .when(total > 0, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .child(div().flex_1().min_w(px(160.)).child(filter.prefix(kit::icon(IconName::Search, 13., p.text_faint)).cleanable(true)))
                        .child(self.connection_filters(&p, cx)),
                )
            })
            .into_any_element()
    }

    /// 启动台主体：最近连接（按持久化用量排序）与全部连接。滚动只发生在这一块。
    fn launcher_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let all: Vec<crate::connections::ConnectionRecord> = self.model.connections.launcher_all().into_iter().cloned().collect();
        let recent: Vec<crate::connections::ConnectionRecord> =
            self.model.connections.launcher_recent(6).into_iter().cloned().collect();
        let filtering = !self.model.connections.filter_text.trim().is_empty()
            || self.model.connections.filter_engine.is_some()
            || !self.model.connections.filter_env.is_empty();
        let mut body = div()
            .id("launcher-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(m.page_pad))
            .py(px(10.))
            .gap(px(2.));
        if self.model.connections.rows.is_empty() {
            // 一条连接都没有：空态给下一步，而不是只留一句"没有连接"。
            // 头部已经给出新建连接与打开 SQLite 两个入口；空态只说明，不在同一屏再放一个同名主按钮。
            body = body.child(kit::empty_state(IconName::Database, self.t("ui.explorer.empty"), self.t("ui.explorer.emptyHint"), &p));
            return body.into_any_element();
        }
        // 连接失败的待办动作：原因与重试入口留在启动台上，不把用户想打开的对象丢掉。
        if let Some(retry) = self.model.connect_retry.clone() {
            // 用连接名而不是 id：失败说明要能指回用户那一行。
            let name = self
                .model
                .connections
                .rows
                .iter()
                .find(|row| row.saved.id == retry.connection_id)
                .map(|row| row.saved.name.clone())
                .unwrap_or_else(|| retry.connection_id.clone());
            body = body.child(
                div()
                    .id("launcher-connect-retry")
                    .test_support()
                    .mb(px(6.))
                    .px(px(8.))
                    .py(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(Radius::PANEL))
                    .bg(p.danger_soft)
                    .child(kit::icon(IconName::CircleAlert, 13., p.danger))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(TypeScale::SM))
                            .text_color(p.danger)
                            .child(SharedString::from(format!("{} · {name}", self.model.text("connections.status.error")))),
                    )
                    .child(kit::tool_button(
                        "launcher-connect-retry-action",
                        Some(IconName::RefreshCw),
                        self.t("common.retry"),
                        act("conn-retry", cx),
                    )),
            );
        }
        // 最近一栏只在它是全部连接的子集时才有意义；连接不多时同一行列两遍只是噪音。
        if !recent.is_empty() && !filtering && all.len() > recent.len() {
            body = body.child(section_label(self.t("connections.launcher.recent"), &p));
            for row in &recent {
                body = body.child(self.launcher_row(row, &p, cx, "recent"));
            }
        }
        body = body.child(section_label(self.t("connections.launcher.all"), &p));
        if all.is_empty() {
            // 有连接但筛选无命中：与"一条都没有"分开呈现，并给出清除筛选。
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
        for row in &all {
            body = body.child(self.launcher_row(row, &p, cx, "all"));
        }
        body.into_any_element()
    }

    /// 启动台的一行：徽标 / 名称 + 环境 / 地址与库 / 状态文字 / 操作组。
    /// 双击走按需连接续接进入工作区；编辑与测试同样作用于这一行。
    fn launcher_row(
        &mut self,
        row: &crate::connections::ConnectionRecord,
        p: &Palette,
        cx: &mut Context<Self>,
        prefix: &'static str,
    ) -> AnyElement {
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
        // 全部连接沿用 conn-row-<id> 这个稳定锚点；最近一栏单独加前缀，避免同一行注册两次。
        let row_id = if prefix == "all" { format!("conn-row-{id}") } else { format!("launcher-{prefix}-{id}") };
        let select_id = id.clone();
        let menu_id = id.clone();
        let edit_id = id.clone();
        let test_id = id.clone();
        kit::row_states(div().id(SharedString::from(row_id)), p)
            .test_support()
            .role(gpui_kit::Role::ListItem)
            .aria_label(SharedString::from(label))
            .aria_selected(selected)
            .aria_description(SharedString::from(aware.clone()))
            .tooltip_text(SharedString::from(aware))
            .h(px(m.row_h))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(Radius::CONTROL))
            .when(selected, |this| this.bg(p.active))
            .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
            .child(kit::engine_badge(row.saved.engine.as_str(), 18.))
            .child(
                div()
                    .min_w(px(0.))
                    .flex_1()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(SharedString::from(row.saved.name.clone())),
            )
            .children(self.env_pill(&row.saved.env, p))
            .child(
                div()
                    .flex_none()
                    .max_w(px(280.))
                    .truncate()
                    .text_size(px(TypeScale::XS))
                    .text_color(p.text_faint)
                    .child(SharedString::from(shown_detail)),
            )
            // 状态不只靠圆点：文字与 tooltip 说同一句话。
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .child(kit::status_dot(dot))
                    .child(div().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(self.model.text(state_key))),
            )
            .child(
                kit::icon_button(
                    SharedString::from(format!("launcher-edit-{id}")),
                    IconName::Pencil,
                    self.model.text("connections.page.menuEdit"),
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.model.connections.selected = Some(edit_id.clone());
                        this.perform("conn-edit", cx);
                    }),
                )
                .xsmall(),
            )
            .child(
                kit::icon_button(
                    SharedString::from(format!("launcher-test-{id}")),
                    IconName::Zap,
                    self.model.text("connections.editor.test.button"),
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.model.connections.selected = Some(test_id.clone());
                        this.perform("conn-test", cx);
                    }),
                )
                .xsmall(),
            )
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.menu_at = event.position;
                this.model.connections.selected = Some(menu_id.clone());
                this.perform("menu-connection", cx);
            }))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.model.connections.selected = Some(select_id.clone());
                if event.click_count() >= 2 {
                    let command = format!("connection:{}", select_id);
                    this.perform(&command, cx);
                } else {
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    /// 启动台的入口按钮。带焦点句柄的那些在模态关闭后接回焦点。
    #[allow(clippy::too_many_arguments)]
    fn launcher_action(
        &mut self,
        id: &'static str,
        icon: IconName,
        label: SharedString,
        handle: Option<&FocusHandle>,
        command: &'static str,
        primary: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let height = self.metrics().control_h;
        let mut element = kit::interactive_states(div().id(id), p, false)
            .test_support()
            .role(gpui_kit::Role::Button)
            .aria_label(label.clone())
            .h(px(height))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(Radius::CONTROL))
            .border_1()
            .text_size(px(TypeScale::SM))
            .when(primary, |this| this.bg(p.accent).text_color(p.on_accent).border_color(p.accent).hover(|style| style.bg(p.accent_hover).border_color(p.accent_hover)).active(|style| style.bg(p.accent_active)))
            .when(!primary, |this| this.border_color(p.line).text_color(p.text).hover(|style| style.border_color(p.line_strong)))
            .child(kit::icon(icon, 14., if primary { p.on_accent } else { p.text_muted }))
            .child(label)
            .on_click(act(command, cx));
        if let Some(handle) = handle {
            element = element.track_focus(handle);
        }
        element.into_any_element()
    }

    // ------------------------------------------------------------ 连接模态

    /// 新建/编辑连接的全局模态：头部、仅主体滚动、固定页脚。
    ///
    /// 打开它的入口有启动台、命令搜索、对象树空态与连接行操作，屏幕上也只有一个编辑面。
    /// 面板本身是一个 GPUI focus trap：Root 的 Tab 动作在容器内成环，Tab/Shift+Tab
    /// 走的是真实焦点，不会逃到下面的页面。
    fn connection_modal_layer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !self.model.conn_modal.open {
            return div().into_any_element();
        }
        let p = Palette::of(cx);
        let m = self.metrics();
        let Some(draft) = self.model.connections.editor.clone() else {
            return div().into_any_element();
        };
        // 每一帧重建控件登记：这一帧画了什么，Tab 与回车就认什么。
        self.model.conn_modal.rendered.clear();
        self.modal_actions.clear();
        let existing = draft.id.is_some();
        let engine = draft.engine;
        let file_based = engine == Engine::Sqlite;
        let narrow = self.model.viewport_width <= 900;
        let view_w = self.model.viewport_width.max(240) as f32;
        let view_h = self.model.viewport_height.max(240) as f32;
        let panel_w = if narrow { (view_w - 40.).max(240.) } else { 640_f32.min(view_w - 80.) };
        // 顶部锚定：换引擎时表单高度会变，居中会让整块上下跳；固定上沿只让下沿伸缩。
        let panel_top = (view_h * 0.1).clamp(20., 96.);
        let panel_h = (view_h - panel_top - 40.).max(200.);
        let pad = if narrow { 16. } else { m.page_pad };
        let focus_first = std::mem::take(&mut self.model.conn_modal.focus_first_pending);

        // 引擎选择：一排徽标，每个都是 Tab 停靠点。
        let mut engine_row = div().flex().flex_wrap().gap(px(6.));
        for candidate in engines() {
            let selected = candidate == engine;
            engine_row = engine_row.child(self.modal_engine_item(candidate, selected, &p, cx));
        }

        let envs = conn_modal::env_ids(&self.model.env_tags);
        let mut env_items = segment_track(&p);
        for env in &envs {
            env_items = env_items.child(self.modal_segment_item(
                conn_modal::env_control_id(env),
                self.env_text(env),
                draft.env == *env,
                conn_modal::ModalAction::Env(env.clone()),
                &p,
                cx,
            ));
        }

        // 首个字段就是 URL：打开模态后焦点直接落在它上面。
        let (url_hint, url_masked) = self.field_hint(DraftField::Url);
        let (url_input, first_field) = self.field_input(DraftField::Url, &url_hint, url_masked, window, cx);
        let apply_url = self.modal_button(
            conn_modal::APPLY_URL,
            Some(IconName::WandSparkles),
            self.t("ui.conn.parse"),
            false,
            conn_modal::ModalAction::Command("conn-apply-url"),
            &p,
            cx,
        );
        let mut general = div().flex().flex_col().gap(px(14.));
        general = general.child(
            div()
                .flex()
                .flex_wrap()
                .items_end()
                .gap(px(8.))
                .child(div().flex_1().min_w(px(180.)).child(self.field_row(self.t("ui.conn.url"), "url", url_input, &p)))
                .child(apply_url),
        );
        let (name_hint, name_masked) = self.field_hint(DraftField::Name);
        let (name, _) = self.field_input(DraftField::Name, &name_hint, name_masked, window, cx);
        general = general.child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(12.))
                .child(div().flex_1().min_w(px(180.)).child(self.field_row(self.t("ui.conn.name"), "name", name, &p)))
                .child(kit::form_row(self.t("ui.conn.env"), env_items, &p)),
        );
        if file_based {
            let (file_hint, file_masked) = self.field_hint(DraftField::File);
            let (file, _) = self.field_input(DraftField::File, &file_hint, file_masked, window, cx);
            let browse = self.modal_button(
                conn_modal::BROWSE_FILE,
                Some(IconName::FolderOpen),
                self.t("ui.conn.browse"),
                false,
                conn_modal::ModalAction::DraftFile,
                &p,
                cx,
            );
            general = general.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_end()
                    .gap(px(8.))
                    .child(div().flex_1().min_w(px(180.)).child(self.field_row(self.t("ui.conn.file"), "file_path", file, &p)))
                    .child(browse),
            );
        } else {
            let (host_hint, host_masked) = self.field_hint(DraftField::Host);
            let (host, _) = self.field_input(DraftField::Host, &host_hint, host_masked, window, cx);
            let (port_hint, port_masked) = self.field_hint(DraftField::Port);
            let (port, _) = self.field_input(DraftField::Port, &port_hint, port_masked, window, cx);
            let (db_hint, db_masked) = self.field_hint(DraftField::Database);
            let (database, _) = self.field_input(DraftField::Database, &db_hint, db_masked, window, cx);
            let (user_hint, user_masked) = self.field_hint(DraftField::Username);
            let (user, _) = self.field_input(DraftField::Username, &user_hint, user_masked, window, cx);
            let (password_hint, password_masked) = self.field_hint(DraftField::Password);
            let (password, _) = self.field_input(DraftField::Password, &password_hint, password_masked, window, cx);
            general = general
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(12.))
                        .child(div().flex_1().min_w(px(180.)).child(self.field_row(self.t("ui.conn.host"), "host", host, &p)))
                        .child(div().w(px(110.)).child(self.field_row(self.t("ui.conn.port"), "port", port, &p))),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(12.))
                        .child(div().flex_1().min_w(px(160.)).child(self.field_row(self.t("ui.conn.user"), "username", user, &p)))
                        .child(div().flex_1().min_w(px(160.)).child(self.field_row(self.t("ui.conn.password"), "password", password.mask_toggle(), &p))),
                );
            // 服务端拒绝凭据时分不清用户名还是密码：结论单独占一行，落在这一组下方，
            // 而不是塞进用户名那一格（见 T3 review 的 advisory）。
            if let Some(issue) = self.model.connections.issues.iter().find(|issue| issue.field == "credentials").cloned() {
                general = general.child(credentials_row(&issue, &self.model.text("connections.status.error"), &p));
            }
            general = general.child(self.field_row(self.t("ui.conn.database"), "database", database, &p));
            if engine == Engine::Mongodb {
                let (replica_hint, replica_masked) = self.field_hint(DraftField::Replica);
                let (replica, _) = self.field_input(DraftField::Replica, &replica_hint, replica_masked, window, cx);
                general = general.child(self.field_row(self.t("connections.editor.replicaSet"), "replica", replica, &p));
            }
        }

        // 安全：TLS 与 SSH。
        let mut security = div().flex().flex_col().gap(px(14.));
        if !file_based {
            let mut tls_items = segment_track(&p);
            for (mode, key) in [(TlsMode::Off, "connections.page.tlsOff"), (TlsMode::Require, "ui.conn.tlsRequire"), (TlsMode::Verify, "ui.conn.tlsVerify")] {
                tls_items = tls_items.child(self.modal_segment_item(
                    conn_modal::tls_control_id(mode),
                    self.t(key),
                    draft.tls_mode == mode,
                    conn_modal::ModalAction::Tls(mode),
                    &p,
                    cx,
                ));
            }
            security = security.child(kit::form_row(self.t("connections.editor.tls.title"), tls_items, &p));
            if draft.tls_mode == TlsMode::Verify {
                let mut ca_items = segment_track(&p);
                for (mode, key) in [(CaMode::System, "connections.editor.tls.caSystem"), (CaMode::File, "connections.editor.tls.caFile")] {
                    ca_items = ca_items.child(self.modal_segment_item(
                        conn_modal::ca_control_id(mode),
                        self.t(key),
                        draft.ca_mode == mode,
                        conn_modal::ModalAction::Ca(mode),
                        &p,
                        cx,
                    ));
                }
                security = security.child(kit::form_row(self.t("connections.editor.tls.caLabel"), ca_items, &p));
                if draft.ca_mode == CaMode::File {
                    let (ca_hint, ca_masked) = self.field_hint(DraftField::CaFile);
                    let (ca, _) = self.field_input(DraftField::CaFile, &ca_hint, ca_masked, window, cx);
                    security = security.child(self.path_row(
                        self.t("connections.editor.tls.caFile"),
                        "ca_file",
                        conn_modal::BROWSE_CA,
                        self.t("connections.editor.tls.browseCa"),
                        conn_modal::ModalAction::Path(DraftField::CaFile),
                        ca,
                        &p,
                        cx,
                    ));
                }
                let (cert_hint, cert_masked) = self.field_hint(DraftField::ClientCert);
                let (cert, _) = self.field_input(DraftField::ClientCert, &cert_hint, cert_masked, window, cx);
                let (key_hint, key_masked) = self.field_hint(DraftField::ClientKey);
                let (key, _) = self.field_input(DraftField::ClientKey, &key_hint, key_masked, window, cx);
                security = security.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(12.))
                        .child(div().flex_1().min_w(px(160.)).child(self.path_row(
                            self.t("connections.editor.tls.clientCert"),
                            "client_cert",
                            conn_modal::BROWSE_CERT,
                            self.t("connections.editor.tls.browseCert"),
                            conn_modal::ModalAction::Path(DraftField::ClientCert),
                            cert,
                            &p,
                            cx,
                        )))
                        .child(div().flex_1().min_w(px(160.)).child(self.path_row(
                            self.t("connections.editor.tls.clientKey"),
                            "client_key",
                            conn_modal::BROWSE_KEY,
                            self.t("connections.editor.tls.browseKey"),
                            conn_modal::ModalAction::Path(DraftField::ClientKey),
                            key,
                            &p,
                            cx,
                        ))),
                );
            }
            let ssh_on = draft.ssh_on;
            let ssh_handle = self.modal_control(cx, conn_modal::SSH_TOGGLE, conn_modal::ModalAction::Command("conn-ssh-toggle"));
            security = security.child(
                kit::interactive_states(div().id(conn_modal::SSH_TOGGLE), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::Switch)
                    .aria_label(self.t("connections.editor.ssh.title"))
                    .aria_selected(ssh_on)
                    .track_focus(&ssh_handle)
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(switch_visual(ssh_on, &p))
                    .tooltip_text(self.t("ui.conn.sshHint"))
                    .child(div().font_weight(FontWeight::MEDIUM).child(self.t("connections.editor.ssh.title")))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.activate_modal_control(conn_modal::SSH_TOGGLE, cx);
                    })),
            );
            if ssh_on {
                let (ssh_host_hint, ssh_host_masked) = self.field_hint(DraftField::SshHost);
                let (ssh_host, _) = self.field_input(DraftField::SshHost, &ssh_host_hint, ssh_host_masked, window, cx);
                let (ssh_port_hint, ssh_port_masked) = self.field_hint(DraftField::SshPort);
                let (ssh_port, _) = self.field_input(DraftField::SshPort, &ssh_port_hint, ssh_port_masked, window, cx);
                let (ssh_user_hint, ssh_user_masked) = self.field_hint(DraftField::SshUser);
                let (ssh_user, _) = self.field_input(DraftField::SshUser, &ssh_user_hint, ssh_user_masked, window, cx);
                let mut auth_items = segment_track(&p);
                for (kind, key) in [
                    (SshAuthKind::Key, "connections.editor.ssh.authKey"),
                    (SshAuthKind::Password, "connections.editor.ssh.authPassword"),
                ] {
                    auth_items = auth_items.child(self.modal_segment_item(
                        conn_modal::ssh_auth_control_id(kind),
                        self.t(key),
                        draft.ssh_auth == kind,
                        conn_modal::ModalAction::SshAuth(kind),
                        &p,
                        cx,
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
                            .child(div().flex_1().min_w(px(160.)).child(self.field_row(self.t("ui.conn.sshHost"), "ssh_host", ssh_host, &p)))
                            .child(div().w(px(90.)).child(self.field_row(self.t("connections.editor.ssh.port"), "ssh_port", ssh_port, &p))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_end()
                            .gap(px(12.))
                            .child(div().flex_1().min_w(px(160.)).child(self.field_row(self.t("ui.conn.sshUser"), "ssh_user", ssh_user, &p)))
                            .child(kit::form_row(self.t("connections.editor.ssh.authLabel"), auth_items, &p)),
                    );
                if draft.ssh_auth == SshAuthKind::Key {
                    let (ssh_key_hint, ssh_key_masked) = self.field_hint(DraftField::SshKey);
                    let (ssh_key, _) = self.field_input(DraftField::SshKey, &ssh_key_hint, ssh_key_masked, window, cx);
                    let (passphrase_hint, passphrase_masked) = self.field_hint(DraftField::Passphrase);
                    let (passphrase, _) = self.field_input(DraftField::Passphrase, &passphrase_hint, passphrase_masked, window, cx);
                    ssh = ssh.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(12.))
                            .child(div().flex_1().min_w(px(160.)).child(self.path_row(
                                self.t("connections.editor.ssh.keyFile"),
                                "ssh_key",
                                conn_modal::BROWSE_SSH,
                                self.t("connections.editor.ssh.browseKey"),
                                conn_modal::ModalAction::Path(DraftField::SshKey),
                                ssh_key,
                                &p,
                                cx,
                            )))
                            .child(div().flex_1().min_w(px(160.)).child(self.field_row(self.t("ui.conn.passphrase"), "passphrase", passphrase.mask_toggle(), &p))),
                    );
                } else {
                    let (ssh_password_hint, ssh_password_masked) = self.field_hint(DraftField::SshPassword);
                    let (ssh_password, _) = self.field_input(DraftField::SshPassword, &ssh_password_hint, ssh_password_masked, window, cx);
                    ssh = ssh.child(self.field_row(self.t("ui.conn.sshPassword"), "ssh_password", ssh_password.mask_toggle(), &p));
                }
                security = security.child(ssh);
            }
        }

        // 测试/打开结果与无处归属的问题：集中在主体末尾，每条只说一次。
        let mut notes = div().flex().flex_col().gap(px(6.));
        for issue in self.model.connections.issues.iter().filter(|issue| !field_issue_placed(&issue.field)) {
            notes = notes.child(banner(IconName::CircleAlert, self.t(issue.key), p.danger, p.danger_soft));
        }
        if let Some(key) = self.model.connections.message_key {
            let ok = key == "connections.status.connected" || key == "connections.editor.test.ok";
            let detail = self.model.connections.message_detail.clone();
            let text = if detail.is_empty() {
                self.model.text(key)
            } else {
                format!("{} · {}", self.model.text(key), detail)
            };
            notes = notes.child(if ok {
                banner(IconName::CircleCheck, SharedString::from(text), p.ok, p.ok_soft)
            } else {
                banner(IconName::CircleAlert, SharedString::from(text), p.danger, p.danger_soft)
            });
        }

        let title = if existing {
            SharedString::from(draft.name.clone())
        } else {
            self.t("connections.editor.newTitle")
        };
        let subtitle = SharedString::from(kit::engine_label(engine.as_str()));
        let running = self.model.connections.test_running();
        let close = self.modal_icon_button(
            conn_modal::CLOSE,
            IconName::X,
            self.t("common.close"),
            conn_modal::ModalAction::Command("conn-cancel"),
            &p,
            cx,
        );

        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(pad))
            .py(px(12.))
            .border_b_1()
            .border_color(p.line)
            .child(kit::engine_badge(engine.as_str(), 28.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w(px(0.))
                    .child(div().id("conn-form-title").truncate().tooltip_text(title.clone()).text_size(px(TypeScale::LG)).font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(kit::muted(subtitle, &p)),
            )
            .child(div().flex_1())
            .child(close);

        let body = div()
            .id("conn-modal-scroll")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(
                div()
                    .px(px(pad))
                    .py(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .child(form_section(self.t("ui.conn.pickEngine"), engine_row, &p))
                    .child(form_section(self.t("ui.conn.general"), general, &p))
                    .when(!file_based, |this| this.child(form_section(self.t("ui.conn.security"), security, &p)))
                    .child(notes),
            );

        // 固定页脚：保存、保存并连接、测试和取消始终在同一位置，不必先滚到底。
        let intro = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .min_w(px(0.))
            .when(running, |this| {
                this.child(Spinner::new().xsmall().color(p.accent)).child(
                    div()
                        .id("conn-test-running")
                        .text_size(px(TypeScale::XS))
                        .text_color(p.text_muted)
                        .child(self.model.text("connections.editor.test.running")),
                )
            });
        let cancel = self.modal_button(
            "conn-cancel",
            None,
            self.t("common.cancel"),
            false,
            conn_modal::ModalAction::Command("conn-cancel"),
            &p,
            cx,
        );
        let test = self.modal_button(
            "conn-test",
            None,
            self.t("connections.editor.test.button"),
            false,
            conn_modal::ModalAction::Command("conn-test"),
            &p,
            cx,
        );
        let save = self.modal_button(
            "conn-save",
            None,
            self.t("common.save"),
            false,
            conn_modal::ModalAction::Command("conn-save"),
            &p,
            cx,
        );
        let save_connect = self.modal_button(
            "conn-save-connect",
            None,
            self.t("connections.editor.saveConnect"),
            true,
            conn_modal::ModalAction::Command("conn-save-connect"),
            &p,
            cx,
        );
        let footer = div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .px(px(pad))
            .py(px(10.))
            .border_t_1()
            .border_color(p.line)
            .bg(p.surface)
            .child(intro)
            .child(div().flex_1())
            .child(cancel)
            .child(test)
            .child(save)
            .child(save_connect);

        let trap = self.modal_focus.clone();
        let content = div()
            // 保留 conn-form 这个 test-support 锚点：它就是“屏幕上那个连接表单”。
            .id("conn-form")
            .test_support()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .child(header)
            .child(body)
            .child(footer);
        let panel = div()
            .id("conn-modal-panel")
            .role(gpui_kit::Role::Group)
            .aria_label(self.t("connections.editor.newTitle"))
            .occlude()
            // 模态的焦点陷阱交给 GPUI：Root 的 Tab/Shift+Tab 在这个容器内成环，
            // 走的是真实焦点，不会像自绘序列那样被 Root 的绑定抢先吃掉。
            .focus_trap("conn-modal", &trap)
            .w(px(panel_w))
            .max_h(px(panel_h))
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(p.surface)
            .border_1()
            .border_color(p.line)
            .rounded(px(Radius::PANEL))
            .shadow_lg()
            .child(content);

        if focus_first {
            // 打开就把键盘交给首个字段：不用先点一下才能输入。
            let handle = first_field.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }

        backdrop("conn-modal-backdrop", 0.32, cx)
            .flex()
            .items_start()
            .justify_center()
            .px(px(20.))
            .pt(px(panel_top))
            .pb(px(20.))
            .child(panel)
            .into_any_element()
    }

    /// 登记一个模态控件：拿到稳定焦点句柄、把 id 记进这一帧的渲染清单、记下它的动作。
    fn modal_control(&mut self, cx: &mut Context<Self>, id: &str, action: conn_modal::ModalAction) -> FocusHandle {
        self.model.conn_modal.note_rendered(id.to_owned());
        self.modal_actions.insert(id.to_owned(), action);
        if let Some(handle) = self.modal_controls.get(id) {
            return handle.clone();
        }
        let handle = cx.focus_handle().tab_stop(true);
        self.modal_controls.insert(id.to_owned(), handle.clone());
        handle
    }

    /// 点一下或按回车：都走这一处，因此键盘与鼠标不会各做一套。
    fn activate_modal_control(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let Some(action) = self.modal_actions.get(id).cloned() else { return false };
        match action {
            conn_modal::ModalAction::Engine(engine) => {
                if let Some(editor) = self.model.connections.editor.as_mut() {
                    if editor.engine != engine {
                        let keep_name = editor.name.clone();
                        let keep_id = editor.id.clone();
                        let mut fresh = crate::app::views::connections::blank_draft(engine);
                        fresh.name = keep_name;
                        fresh.id = keep_id;
                        *editor = fresh;
                    }
                }
            }
            conn_modal::ModalAction::Env(env) => {
                if let Some(editor) = self.model.connections.editor.as_mut() {
                    editor.env = env;
                }
            }
            conn_modal::ModalAction::Tls(mode) => {
                if let Some(editor) = self.model.connections.editor.as_mut() {
                    editor.tls_mode = mode;
                }
            }
            conn_modal::ModalAction::Ca(mode) => {
                if let Some(editor) = self.model.connections.editor.as_mut() {
                    editor.ca_mode = mode;
                }
            }
            conn_modal::ModalAction::SshAuth(kind) => {
                if let Some(editor) = self.model.connections.editor.as_mut() {
                    editor.ssh_auth = kind;
                }
            }
            conn_modal::ModalAction::Command(command) => {
                self.perform(command, cx);
                return true;
            }
            conn_modal::ModalAction::DraftFile => self.prompt_draft_file(cx),
            conn_modal::ModalAction::Path(field) => self.prompt_field_file(field, cx),
        }
        cx.notify();
        true
    }

    /// 回车落在哪个已登记的控件上就激活它；落在输入框上时返回 false，交还给输入框。
    fn modal_activate_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(focused) = window.focused(cx) else { return false };
        let Some(id) = self
            .modal_controls
            .iter()
            .find(|(_, handle)| **handle == focused)
            .map(|(id, _)| id.clone())
        else {
            return false;
        };
        if !self.modal_actions.contains_key(&id) {
            return false;
        }
        self.activate_modal_control(&id, cx)
    }

    /// 引擎徽标：一整块可点，也是 Tab 停靠点。
    fn modal_engine_item(&mut self, engine: Engine, selected: bool, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = conn_modal::engine_control_id(engine);
        let handle = self.modal_control(cx, &id, conn_modal::ModalAction::Engine(engine));
        let name = engine.as_str();
        let label = SharedString::from(kit::engine_label(name));
        kit::interactive_states(div().id(SharedString::from(id)), p, false)
            .test_support()
            .role(gpui_kit::Role::Button)
            .aria_label(label)
            .aria_selected(selected)
            .track_focus(&handle)
            .h(px(self.metrics().control_h))
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
                let id = conn_modal::engine_control_id(engine);
                this.activate_modal_control(&id, cx);
            }))
            .into_any_element()
    }

    /// 分段控件里的一项：与 kit::segmented_sized 同一套视觉，但每项都有自己的焦点句柄。
    fn modal_segment_item(
        &mut self,
        id: impl Into<String>,
        label: SharedString,
        selected: bool,
        action: conn_modal::ModalAction,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = id.into();
        let handle = self.modal_control(cx, &id, action);
        // 分段项放在 2px 内边距的槽里：整组高度与旁边的输入框一致。
        let height = self.metrics().control_h - 4.;
        let click_id = id.clone();
        kit::interactive_states(div().id(SharedString::from(id)), p, false)
            .test_support()
            .role(gpui_kit::Role::Button)
            .aria_label(label.clone())
            .aria_selected(selected)
            .track_focus(&handle)
            .h(px(height))
            .px(px(10.))
            .flex()
            .items_center()
            .rounded(px(Radius::CONTROL))
            .text_size(px(TypeScale::SM))
            .when(selected, |this| this.bg(p.surface).text_color(p.text).font_weight(FontWeight::MEDIUM).shadow_sm())
            .when(!selected, |this| this.text_color(p.text_muted).hover(|style| style.text_color(p.text)))
            .child(label)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.activate_modal_control(&click_id, cx);
            }))
            .into_any_element()
    }

    /// 模态里的文字按钮（页脚动作、解析、浏览）。
    #[allow(clippy::too_many_arguments)]
    fn modal_button(
        &mut self,
        id: &str,
        icon: Option<IconName>,
        label: SharedString,
        primary: bool,
        action: conn_modal::ModalAction,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let handle = self.modal_control(cx, id, action);
        let click_id = id.to_owned();
        let element_id = SharedString::from(id);
        let height = self.metrics().control_h;
        let mut button = kit::interactive_states(div().id(element_id), p, false)
            .test_support()
            .role(gpui_kit::Role::Button)
            .aria_label(label.clone())
            .track_focus(&handle)
            .h(px(height))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(Radius::CONTROL))
            .border_1()
            .text_size(px(TypeScale::SM))
            .when(primary, |this| this.bg(p.accent).text_color(p.on_accent).border_color(p.accent).hover(|style| style.bg(p.accent_hover).border_color(p.accent_hover)).active(|style| style.bg(p.accent_active)))
            .when(!primary, |this| this.border_color(p.line).text_color(p.text).hover(|style| style.border_color(p.line_strong)));
        if let Some(icon) = icon {
            button = button.child(kit::icon(icon, 13., if primary { p.on_accent } else { p.text_muted }));
        }
        button
            .child(label)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.activate_modal_control(&click_id, cx);
            }))
            .into_any_element()
    }

    /// 头部关闭图标：同样是 Tab 停靠点。
    fn modal_icon_button(
        &mut self,
        id: &str,
        icon: IconName,
        label: SharedString,
        action: conn_modal::ModalAction,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let handle = self.modal_control(cx, id, action);
        let click_id = id.to_owned();
        let element_id = SharedString::from(id);
        let size = self.metrics().control_h;
        kit::interactive_states(div().id(element_id), p, false)
            .test_support()
            .role(gpui_kit::Role::Button)
            .aria_label(label)
            .track_focus(&handle)
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(Radius::CONTROL))
            .hover(|style| style.bg(p.hover))
            .child(kit::icon(icon, 14., p.text_muted))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.activate_modal_control(&click_id, cx);
            }))
            .into_any_element()
    }

    /// 字段旁的错误：同一字段的结论就显示在它旁边，不再汇总到底部。
    fn field_row(&self, label: SharedString, field: &str, control: impl IntoElement, p: &Palette) -> AnyElement {
        let issue = self.model.connections.issues.iter().find(|issue| issue.field == field);
        let mut row = div().flex().flex_col().gap(px(4.)).min_w(px(0.)).child(kit::form_row(label, control, p));
        if let Some(issue) = issue {
            row = row.child(
                div()
                    .id(SharedString::from(format!("conn-issue-{field}")))
                    .test_support()
                    .aria_label(SharedString::from(field_issue_text(issue, &|key| self.model.text(key))))
                    .text_size(px(TypeScale::XS))
                    .text_color(p.danger)
                    .child(SharedString::from(field_issue_text(issue, &|key| self.model.text(key)))),
            );
        }
        row.into_any_element()
    }

    /// 带“浏览”按钮的路径字段：错误同样落在字段旁。
    #[allow(clippy::too_many_arguments)]
    fn path_row(
        &mut self,
        label: SharedString,
        field: &str,
        browse_id: &str,
        browse_label: SharedString,
        browse_action: conn_modal::ModalAction,
        input: impl IntoElement,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let browse = self.modal_button(browse_id, Some(IconName::FolderOpen), browse_label, false, browse_action, p, cx);
        let mut row = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(6.))
                    .min_w(px(0.))
                    .child(div().flex_1().min_w(px(0.)).child(kit::form_row(label, input, p)))
                    .child(browse),
            );
        if let Some(issue) = self.model.connections.issues.iter().find(|issue| issue.field == field) {
            row = row.child(
                div()
                    .id(SharedString::from(format!("conn-issue-{field}")))
                    .test_support()
                    .text_size(px(TypeScale::XS))
                    .text_color(p.danger)
                    .child(SharedString::from(field_issue_text(issue, &|key| self.model.text(key)))),
            );
        }
        row.into_any_element()
    }

    /// 每个草稿字段的占位符与掩码：渲染与 Tab 聚焦读同一份，避免两处写不同文案。
    fn field_hint(&self, field: DraftField) -> (String, bool) {
        let existing = self.model.connections.editor.as_ref().is_some_and(|draft| draft.id.is_some());
        let engine = self.model.connections.editor.as_ref().map(|draft| draft.engine);
        let redis = engine == Some(Engine::Redis);
        let key = match field {
            DraftField::Url => "ui.conn.urlHint",
            DraftField::Name => "ui.conn.namePlaceholder",
            DraftField::File => "connections.validate.chooseFile",
            DraftField::Database if redis => "ui.conn.redisDb",
            DraftField::Database => "ui.conn.optional",
            DraftField::CaFile => "",
            DraftField::ClientCert | DraftField::ClientKey => "",
            DraftField::Passphrase => "ui.conn.optional",
            _ => "",
        };
        let placeholder = match field {
            DraftField::Host => "localhost".to_owned(),
            DraftField::Port => String::new(),
            DraftField::Replica => "rs0".to_owned(),
            DraftField::SshHost => "bastion.example.com".to_owned(),
            DraftField::SshPort => "22".to_owned(),
            DraftField::SshKey => "~/.ssh/id_ed25519".to_owned(),
            DraftField::CaFile => "ca.pem".to_owned(),
            DraftField::Password if existing => self.model.text("ui.conn.passwordKeep"),
            // 示例跟着当前引擎走：选了 SQLite 不再提示一个 postgres 连接串。
            DraftField::Url => format!("{} {}", self.model.text(key), url_example(engine.unwrap_or(Engine::Postgres))),
            _ if key.is_empty() => String::new(),
            _ => self.model.text(key),
        };
        let masked = matches!(
            field,
            DraftField::Password | DraftField::SshPassword | DraftField::Passphrase
        );
        (placeholder, masked)
    }

    /// 引擎/环境筛选的两个按钮。点击记下位置，浮层在根层弹出并自动避开视口边缘。
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
        let active = self.model.connections.filter_engine.is_some()
            || !self.model.connections.filter_env.is_empty()
            || !self.model.connections.filter_text.trim().is_empty();
        let engine_open = self.model.conn_filter_menu == Some(ConnFilterMenu::Engine);
        let env_open = self.model.conn_filter_menu == Some(ConnFilterMenu::Env);
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .child(self.filter_button("conn-filter-engine", IconName::Database, SharedString::from(engine), engine_open, "conn-filter-engine", p, cx))
            .child(self.filter_button("conn-filter-env", IconName::Funnel, env, env_open, "conn-filter-env", p, cx))
            .when(active, |this| {
                this.child(kit::tool_button(
                    "conn-filter-clear",
                    Some(IconName::X),
                    self.t("connections.filter.clear"),
                    act("conn-filter-clear", cx),
                ))
            })
            .into_any_element()
    }

    /// 一个筛选按钮：展开时用强调底，点击同时记住锚点位置供浮层定位。
    #[allow(clippy::too_many_arguments)]
    fn filter_button(
        &mut self,
        id: &'static str,
        icon: IconName,
        label: SharedString,
        open: bool,
        command: &'static str,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let height = self.metrics().control_h;
        kit::interactive_states(div().id(id), p, false)
            .test_support()
            .role(gpui_kit::Role::Button)
            .aria_label(label.clone())
            .aria_expanded(open)
            .h(px(height))
            .px(px(9.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(Radius::CONTROL))
            .border_1()
            .text_size(px(TypeScale::SM))
            .when(open, |this| this.border_color(p.accent).bg(p.accent_soft).text_color(p.accent_text))
            .when(!open, |this| this.border_color(p.line).text_color(p.text).hover(|style| style.border_color(p.line_strong)))
            .child(kit::icon(IconName::ChevronDown, 12., p.text_faint))
            .child(kit::icon(icon, 13., p.text_muted))
            .child(label)
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                // 浮层以按钮的左下角为锚点：位置来自这次真实点击，不靠猜元素坐标。
                this.conn_filter_at = event.position();
                this.perform(command, cx);
            }))
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

    /// 筛选浮层：Esc、外点关闭，近右/下边缘自动翻转，过高时自身滚动，绝不压住别的内容。
    fn conn_filter_layer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let Some(which) = self.model.conn_filter_menu else {
            return div().into_any_element();
        };
        let options = match which {
            ConnFilterMenu::Engine => self.engine_filter_options(),
            ConnFilterMenu::Env => self.env_filter_options(),
        };
        let width = 220.;
        let row_h = m.menu_row_h;
        let height = options.len() as f32 * (row_h + 2.) + 8.;
        let view_w = self.model.viewport_width.max(240) as f32;
        let view_h = self.model.viewport_height.max(240) as f32;
        let anchor_x = f32::from(self.conn_filter_at.x);
        let anchor_y = f32::from(self.conn_filter_at.y);
        let (x, y) = super::menus::place_popup(anchor_x, anchor_y, width, height, view_w, view_h);
        let label = match which {
            ConnFilterMenu::Engine => self.t("connections.filter.engine"),
            ConnFilterMenu::Env => self.t("connections.filter.env"),
        };
        let mut menu = div()
            .id("conn-filter-menu")
            .test_support()
            .role(gpui_kit::Role::Menu)
            .aria_label(label)
            .occlude()
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(width))
            .max_h(px(view_h - 24.))
            .overflow_y_scroll()
            .p(px(4.))
            .flex()
            .flex_col()
            .bg(p.raised)
            .border_1()
            .border_color(p.line)
            .rounded(px(Radius::PANEL))
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());
        for (id, text, selected, action) in options {
            menu = menu.child(
                kit::interactive_states(div().id(SharedString::from(id)), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::MenuItem)
                    .aria_label(text.clone())
                    .aria_selected(selected)
                    .h(px(row_h))
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
        backdrop("conn-filter-backdrop", 0., cx).child(menu).into_any_element()
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
        let phase = self.model.history.phase_for(section);
        // 读取失败与空态不是一回事：失败给原因和“重新读取”，旧行仍留在下面可读。
        if let Some(key) = self.model.history.failure_key {
            let detail = self.model.history.failure_detail.clone();
            let detail = if detail.trim().is_empty() { self.t("history.page.retryLoad") } else { SharedString::from(detail) };
            list = list.child(
                div().id("history-failure").test_support().child(kit::error_state_with_action(
                    self.t(key),
                    detail,
                    kit::secondary_button("history-retry", Some(IconName::RefreshCw), self.t("history.page.retryLoad"), act("history-refresh", cx)),
                    &p,
                )),
            );
        }
        if section == HistorySection::Runs {
            let runs: Vec<_> = self.model.history.filtered_runs().into_iter().take(200).cloned().collect();
            if runs.is_empty() {
                // 读取中先给骨架，读到才判断“无数据”还是“无命中”。
                list = list.child(match phase {
                    ListPhase::Loading => kit::skeleton_state(5, &m, &p).into_any_element(),
                    ListPhase::Empty => kit::empty_state_with_action(
                        IconName::Clock,
                        self.t("ui.history.empty"),
                        self.t("history.page.loadOnlySql"),
                        kit::primary_button("history-empty-query", Some(IconName::SquareTerminal), self.t("shell.command.newQuery"), act("new-query", cx)),
                        &p,
                    )
                    .into_any_element(),
                    ListPhase::NoMatch => kit::no_match_state_with_action(
                        IconName::Search,
                        self.t("history.page.noMatch"),
                        self.t("history.filter.reset"),
                        kit::secondary_button("history-empty-reset", Some(IconName::X), self.t("history.filter.reset"), act("history-filter-reset", cx)),
                        &p,
                    )
                    .into_any_element(),
                    ListPhase::Failed | ListPhase::Rows => div().into_any_element(),
                });
            } else if self.model.history.loading {
                // 刷新时旧行继续显示，只在列表顶部说明还在读，不整块换成骨架。
                list = list.child(div().id("history-loading").test_support().child(kit::inline_loading(self.t("history.page.loading"), &m, &p)));
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
                        .test_support()
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
                list = list.child(match phase {
                    ListPhase::Loading => kit::skeleton_state(4, &m, &p).into_any_element(),
                    ListPhase::Empty => kit::empty_state_with_action(
                        IconName::Bookmark,
                        self.t("ui.history.emptySaved"),
                        self.t("history.savedPanel.openNote"),
                        kit::primary_button("history-empty-saved-query", Some(IconName::SquareTerminal), self.t("shell.command.newQuery"), act("new-query", cx)),
                        &p,
                    )
                    .into_any_element(),
                    ListPhase::NoMatch => kit::no_match_state_with_action(
                        IconName::Search,
                        self.t("history.page.noMatchSaved"),
                        self.t("history.filter.reset"),
                        kit::secondary_button("history-empty-saved-reset", Some(IconName::X), self.t("history.filter.reset"), act("history-filter-reset", cx)),
                        &p,
                    )
                    .into_any_element(),
                    ListPhase::Failed | ListPhase::Rows => div().into_any_element(),
                });
            } else if self.model.history.loading {
                list = list.child(div().id("history-loading").test_support().child(kit::inline_loading(self.t("history.page.loadingSaved"), &m, &p)));
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
                        .test_support()
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

        // 键盘可用性写在列表末尾：方向键选行、回车载入、Delete 删除，与右键菜单同一批命令。
        if phase == ListPhase::Rows {
            list = list.child(
                div()
                    .id("history-keyboard-hint")
                    .test_support()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .pt(px(4.))
                    .text_size(px(TypeScale::XS))
                    .text_color(p.text_faint)
                    .child(self.t("history.page.keyboardHint"))
                    .child(kit::kbd("↑↓", &p))
                    .child(kit::kbd("Home/End", &p))
                    .child(kit::kbd("Enter", &p))
                    .child(kit::kbd("Delete", &p)),
            );
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
                    // 标题行只放标题与关闭；动作单独一行左对齐。预览面板窄，四个按钮挤在标题旁会把关闭按钮折到下一行。
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(kit::title(self.t("history.page.sqlPreview"), TypeScale::MD, p))
                            .child(div().flex_1())
                            .child(kit::icon_button("history-preview-close", IconName::X, self.t("history.page.previewClose"), act("history-preview-close", cx))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(6.))
                            .child(kit::primary_button("history-preview-load", Some(IconName::SquarePen), self.t("history.page.loadToEditor"), act("history-load", cx)))
                            .child(kit::tool_button("history-preview-copy", Some(IconName::Copy), self.t("history.preview.copy"), act("history-copy", cx)))
                            .child(kit::tool_button("history-preview-save", Some(IconName::Save), self.t("history.preview.saveQuery"), act("history-save-preview", cx))),
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

        // 一组设置的卡片：标题 + 逐行“名称/说明 + 控件”。行内换行，
        // 窄窗口里控件掉到下一行而不是把名称挤没。
        let group = |id: &'static str, title: SharedString, rows: Vec<(SharedString, Option<SharedString>, AnyElement)>, p: &Palette| {
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
                                .min_w(px(160.))
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(div().font_weight(FontWeight::MEDIUM).child(label))
                                .when_some(hint, |this, hint| this.child(div().text_size(px(TypeScale::SM)).text_color(p.text_faint).child(hint))),
                        )
                        .child(control),
                );
            }
            div()
                .id(SharedString::from(id))
                .test_support()
                .role(gpui_kit::Role::Group)
                .aria_label(title.clone())
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(kit::eyebrow(title, p))
                .child(card)
        };

        // 只构建当前分区的表单：另一个分区的控件不进元素树，Tab 也不会走到看不见的控件上。
        let section = self.model.settings_section;
        let section_body = match section {
            SettingsSection::Appearance => div()
                .flex()
                .flex_col()
                .gap(px(m.section_gap()))
                .child(group(
                    "settings-group-appearance",
                    self.t("ui.settings.appearance"),
                    vec![
                        (self.t("shell.settings.theme"), None, theme_seg),
                        (self.t("shell.settings.density"), None, density_seg),
                        (self.t("shell.settings.language"), None, locale_seg),
                    ],
                    &p,
                ))
                .child(group(
                    "settings-group-editor",
                    self.t("ui.settings.editor"),
                    vec![
                        (self.t("shell.settings.editorFont"), None, font.into_any_element()),
                        (self.t("shell.settings.editorTheme"), Some(self.t("shell.settings.editorThemeAuto")), editor_theme.into_any_element()),
                    ],
                    &p,
                )),
            SettingsSection::Data => div()
                .flex()
                .flex_col()
                .gap(px(m.section_gap()))
                .child(group(
                    "settings-group-data",
                    self.t("ui.settings.data"),
                    vec![
                        (self.t("shell.settings.rowsPerPage"), None, page_size.into_any_element()),
                        (self.t("shell.settings.previewLimit"), None, preview.into_any_element()),
                        (self.t("shell.settings.objectPage"), None, object_page.into_any_element()),
                        (self.t("shell.settings.nullCopy"), None, null_seg),
                        (self.t("shell.settings.timeout"), None, timeout_seg),
                    ],
                    &p,
                )),
            SettingsSection::Local => div()
                .flex()
                .flex_col()
                .gap(px(m.section_gap()))
                .child(group(
                    "settings-group-local",
                    self.t("ui.settings.general"),
                    vec![(self.t("shell.update.autoCheck"), None, updates.into_any_element())],
                    &p,
                ))
                .child(
                    div()
                        .id("settings-local-hint")
                        .text_size(px(TypeScale::SM))
                        .text_color(p.text_muted)
                        .child(self.t("ui.settings.localHint")),
                )
                .child(
                    div()
                        .text_size(px(TypeScale::XS))
                        .text_color(p.text_faint)
                        .child(SharedString::from(format!("RillSQL {}", env!("CARGO_PKG_VERSION")))),
                ),
        };
        let form = div()
            .id("settings-form")
            .test_support()
            .role(gpui_kit::Role::Group)
            .aria_label(self.t("shell.settings.title"))
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(
                div()
                    .max_w(px(760.))
                    .min_w(px(0.))
                    .px(px(m.page_pad))
                    .py(px(m.page_pad_y))
                    .flex()
                    .flex_col()
                    .gap(px(m.section_gap()))
                    .child(kit::title(self.t(section.label_key()), TypeScale::MD, &p))
                    .child(section_body)
                    // 写盘反馈贴着表单末尾：改动立即生效，保存结果就地可读。
                    .child(self.settings_save_row(&p, &m, cx)),
            );

        // 分区导航：宽窗口在左侧竖排，窄窗口折成顶部一行，右侧表单始终可滚动到底。
        let nav_items: Vec<_> = SettingsSection::ALL
            .iter()
            .map(|item| (SharedString::from(item.command()), self.t(item.label_key()), *item == section, kit::click(act(item.command(), cx))))
            .collect();
        let side_by_side = settings_side_by_side(self.model.layout().content_width as f32);
        let nav: AnyElement = if side_by_side {
            let mut column = div()
                .id("settings-nav")
                .test_support()
                .role(gpui_kit::Role::Navigation)
                .aria_label(self.t("ui.settings.nav"))
                .flex_none()
                .w(px(180.))
                .py(px(m.page_pad_y))
                .px(px(m.page_pad))
                .border_r_1()
                .border_color(p.line)
                .flex()
                .flex_col()
                .gap(px(2.));
            for (id, label, selected, click) in nav_items {
                column = column.child(
                    kit::interactive_states(div().id(id), &p, false)
                        .test_support()
                        .role(gpui_kit::Role::Button)
                        .aria_label(label.clone())
                        .aria_selected(selected)
                        .h(px(m.row_h + 6.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .rounded(px(Radius::CONTROL))
                        .text_size(px(TypeScale::SM))
                        .when(selected, |this| this.bg(p.accent_soft).text_color(p.accent).font_weight(FontWeight::MEDIUM))
                        .when(!selected, |this| this.text_color(p.text_muted))
                        .child(label)
                        .on_click(click),
                );
            }
            column.into_any_element()
        } else {
            div()
                .id("settings-nav")
                .test_support()
                .role(gpui_kit::Role::Navigation)
                .aria_label(self.t("ui.settings.nav"))
                .flex_none()
                .px(px(m.page_pad))
                .pt(px(m.page_pad_y))
                .child(kit::segmented_sized(nav_items, &p, m.control_h))
                .into_any_element()
        };

        let body = div()
            .flex()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .when(side_by_side, |this| this.flex_row())
            .when(!side_by_side, |this| this.flex_col())
            .child(nav)
            .child(form);

        div()
            .id("settings-page")
            .test_support()
            .role(gpui_kit::Role::Group)
            .aria_label(self.t("shell.settings.title"))
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(m.page_pad))
                    .pt(px(m.page_pad_y))
                    .pb(px(10.))
                    .flex_none()
                    .child(kit::title(self.t("shell.settings.title"), TypeScale::LG, &p)),
            )
            .child(body)
            .into_any_element()
    }

    /// 设置写盘的可见结果：失败给原因与就地重试，不把错误吞在后台任务里。
    fn settings_save_row(&self, p: &Palette, m: &kit::Metrics, cx: &mut Context<Self>) -> AnyElement {
        match self.model.settings_save.clone() {
            SaveState::Idle => div().into_any_element(),
            SaveState::Saving => div()
                .id("settings-save-state")
                .test_support()
                .child(kit::inline_loading(self.t("ui.settings.saving"), m, p))
                .into_any_element(),
            SaveState::Saved => div()
                .id("settings-save-state")
                .test_support()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(kit::icon(IconName::Check, 14., p.ok))
                .child(div().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(self.t("ui.settings.saved")))
                .into_any_element(),
            SaveState::Failed { detail } => {
                let hint = self.t("ui.settings.saveFailedHint");
                let detail = if detail.trim().is_empty() { hint } else { SharedString::from(format!("{hint} {detail}")) };
                kit::error_state_with_action(
                    self.t("ui.settings.saveFailed"),
                    detail,
                    kit::secondary_button("settings-retry-save", Some(IconName::RefreshCw), self.t("ui.settings.retrySave"), act("settings-retry-save", cx)),
                    p,
                )
                .into_any_element()
            }
        }
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
            .max_w(px(620.))
            .max_h(px(480.))
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
                let error = self.model.palette_error().to_owned();
                let empty_key = self.model.palette_empty_key();
                let mut empty = div()
                    .p(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .text_color(p.text_faint)
                    .child(self.t(empty_key));
                // 目录读取失败与“没有命中”是两回事：只有失败才带原因和重试。
                if empty_key == "ui.palette.error" && !error.is_empty() {
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
            let mut last_group = "";
            for (index, entry) in entries.into_iter().take(40).enumerate() {
                let selected = index == self.model.palette_index;
                let id = entry.id.clone();
                // 快捷键来自菜单同源（COMMANDS → SHORTCUTS），类型标注来自条目种类。
                let chord = entry.shortcut;
                let type_label = self.t(super::model::palette_kind_key(entry.group));
                let detail = entry.detail.clone();
                let icon = palette_icon(&entry.id);
                // 分组标题只在组变化时出现一次；类型标注仍在行内，滚动到中段也认得出。
                if entry.group != last_group {
                    last_group = entry.group;
                    let heading = self.t(super::model::palette_kind_key(entry.group));
                    list = list.child(
                        div()
                            .px(px(10.))
                            .pt(px(if index == 0 { 4. } else { 10. }))
                            .pb(px(4.))
                            .text_size(px(TypeScale::XS))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.text_faint)
                            .child(heading),
                    );
                }
                list = list.child(
                    kit::row_states(div().id(SharedString::from(format!("pal-{index}"))), &p)
                        .test_support()
                        .role(gpui_kit::Role::MenuItem)
                        .aria_label(SharedString::from(entry.label.clone()))
                        .aria_selected(selected)
                        .flex_none()
                        .h(px(m.row_h + 4.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .rounded(px(Radius::CONTROL))
                        .when(selected, |this| this.bg(p.accent_soft).text_color(p.accent_text))
                        .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
                        .child(
                            div()
                                .flex_none()
                                .size(px(22.))
                                .rounded(px(Radius::CONTROL))
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(if selected { p.raised } else { p.hover })
                                .child(kit::icon(icon, 13., if selected { p.accent_text } else { p.text_muted })),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(TypeScale::MD)).child(SharedString::from(entry.label)))
                                // 上下文（库.模式 · 连接 / 引擎 · 库）：选中后去哪一目了然。
                                .when(!detail.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .id(SharedString::from(format!("pal-detail-{index}")))
                                            .test_support()
                                            .flex_none()
                                            .max_w(px(190.))
                                            .truncate()
                                            .text_size(px(TypeScale::XS))
                                            .text_color(if selected { p.accent_text } else { p.text_faint })
                                            .child(SharedString::from(detail)),
                                    )
                                })
                                // 类型标注：表 / 视图 / 连接 / 库 / 保存的查询 / 命令。
                                // 也是无障碍名称的一部分：读屏能听到这一行是什么类型。
                                .child(
                                    div()
                                        .id(SharedString::from(format!("pal-kind-{index}")))
                                        .test_support()
                                        .flex_none()
                                        .text_size(px(TypeScale::XS))
                                        .text_color(if selected { p.accent_text } else { p.text_faint })
                                        .aria_label(type_label.clone())
                                        .child(type_label),
                                ),
                        )
                        .when_some(chord, |this, chord| this.child(kit::kbd(kit::shortcut(&chord.replace("Mod+", "Ctrl+")), &p)))
                        .when(selected && chord.is_none(), |this| this.child(kit::icon(IconName::CornerDownLeft, 12., p.accent_text)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.model.shell.close_overlay();
                            this.perform(&id, cx);
                            if !this.model.shell.palette_open && !this.model.shell.help_open {
                                this.restore_focus(window, cx);
                            }
                        })),
                );
            }
            let hint = |keys: &'static str, label: SharedString| {
                div().flex().items_center().gap(px(4.)).child(kit::kbd(keys, &p)).child(label)
            };
            let footer = div()
                .id("palette-footer")
                .flex_none()
                .h(px(m.row_h + 4.))
                .px(px(12.))
                .flex()
                .items_center()
                .gap(px(14.))
                .border_t_1()
                .border_color(p.line)
                .bg(p.chrome)
                .text_size(px(TypeScale::XS))
                .text_color(p.text_faint)
                .child(hint("↑↓", self.t("shell.palette.hintMove")))
                .child(hint("Enter", self.t("shell.palette.hintRun")))
                .child(hint("Esc", self.t("shell.palette.hintClose")));
            panel = panel
                .child(
                    div()
                        .px(px(12.))
                        .py(px(10.))
                        .border_b_1()
                        .border_color(p.line)
                        .child(input.appearance(false).prefix(kit::icon(IconName::Search, 15., p.text_faint))),
                )
                .child(list.flex_1().min_h(px(0.)))
                .child(footer);
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
            let id = item.command();
            let has_children = !item.children.is_empty();
            let label = match &item.text {
                Some(text) if !text.is_empty() => SharedString::from(text.clone()),
                _ => self.t(item.label_key),
            };
            let shortcut = item.shortcut.map(|chord| SharedString::from(kit::shortcut(chord)));
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
                    .test_support()
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

    /// 删除分组确认：只删分组记录与成员关系，连接回到顶层，配置与机密都不动。
    fn group_delete_layer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let name = self
            .model
            .group_delete
            .as_deref()
            .and_then(|id| self.model.group(id))
            .map(|group| group.name.clone())
            .unwrap_or_default();
        let body = super::explorer::full_label(&self.t("ui.group.deleteHint"), &name);
        let dialog = dialog_card("delete-group", IconName::Trash, p.danger, self.t("ui.group.delete"), SharedString::from(body), &p).child(
            div()
                .flex()
                .justify_end()
                .gap(px(8.))
                .child(kit::secondary_button("cmd-group-delete-cancel", None, self.t("common.cancel"), act("group-delete-cancel", cx)))
                .child(kit::danger_button("cmd-group-delete-confirm", Some(IconName::Trash), self.t("ui.group.delete"), act("group-delete-confirm", cx))),
        );
        backdrop("group-delete-backdrop", 0.32, cx).flex().items_center().justify_center().px(px(20.)).child(dialog).into_any_element()
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

/// 列节点的 detail 形如 "INTEGER · pk · not null"：拆成类型与约束标记。
fn split_column_detail(detail: &str) -> (String, String) {
    let mut parts = detail.split(" · ");
    let first = parts.next().unwrap_or_default();
    if first == "pk" || first == "not null" {
        return (String::new(), detail.to_owned());
    }
    (first.to_owned(), parts.collect::<Vec<_>>().join(" · "))
}

/// 列图标按类型族区分：主键、数字、文本、时间、布尔、二进制、JSON。
fn column_icon(data_type: &str, primary: bool) -> IconName {
    if primary {
        return IconName::KeyRound;
    }
    let ty = data_type.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| ty.contains(word));
    if has(&["json"]) {
        IconName::Braces
    } else if has(&["bool", "bit"]) {
        IconName::ToggleLeft
    } else if has(&["date", "time", "year", "interval"]) {
        IconName::CalendarClock
    } else if has(&["blob", "binary", "bytea", "image"]) {
        IconName::Binary
    } else if has(&["int", "num", "dec", "real", "float", "double", "serial", "money"]) {
        IconName::Hash
    } else if has(&["char", "text", "clob", "string", "uuid", "enum", "xml"]) {
        IconName::CaseSensitive
    } else {
        IconName::Type
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

/// 启动台里的小节标题：11px、粗体、弱色，与 S4.1 的分区标签一致。
fn section_label(text: SharedString, p: &Palette) -> AnyElement {
    div()
        .pt(px(8.))
        .pb(px(2.))
        .text_size(px(TypeScale::XS))
        .font_weight(FontWeight::BOLD)
        .text_color(p.text_faint)
        .child(text)
        .into_any_element()
}

/// 连接状态的颜色与文案键。颜色不是唯一载体：调用处同时渲染文字。
fn status_parts(status: LinkStatus, p: &Palette) -> (gpui_kit::Hsla, &'static str) {
    match status {
        LinkStatus::Connected => (p.ok, "connections.status.connected"),
        LinkStatus::Connecting => (p.warn, "ui.conn.connecting"),
        LinkStatus::Failed => (p.danger, "connections.status.error"),
        LinkStatus::Disconnected => (p.line_strong, "ui.conn.notConnected"),
    }
}

/// 凭据被服务端拒绝：结论单独占一行、落在用户名+密码这一组下方，
/// 而不是塞进用户名那一格（分不清是哪一个错的时候不该只怪其中一个）。
fn credentials_row(issue: &crate::app::views::connections::FieldIssue, heading: &str, p: &Palette) -> AnyElement {
    let detail = issue.detail.trim();
    let text = if detail.is_empty() || detail == heading {
        heading.to_owned()
    } else {
        format!("{heading} · {detail}")
    };
    div()
        .id("conn-issue-credentials")
        .test_support()
        .role(gpui_kit::Role::Group)
        .aria_label(SharedString::from(text.clone()))
        .flex()
        .items_start()
        .gap(px(6.))
        .child(kit::icon(IconName::CircleAlert, 13., p.danger))
        .child(div().flex_1().min_w(px(0.)).text_size(px(TypeScale::XS)).text_color(p.danger).child(SharedString::from(text)))
        .into_any_element()
}

/// 这些字段的错误已经画在字段旁，不再进主体末尾的汇总。
fn field_issue_placed(field: &str) -> bool {
    matches!(
        field,
        "name"
            | "url"
            | "host"
            | "port"
            | "database"
            | "replica"
            | "username"
            | "password"
            | "credentials"
            | "file_path"
            | "ca_file"
            | "client_cert"
            | "client_key"
            | "ssh_host"
            | "ssh_port"
            | "ssh_user"
            | "ssh_key"
            | "ssh_password"
            | "passphrase"
    )
}

/// 字段错误的显示文本：通用文案 + 服务端原文（有就带上）。
fn field_issue_text(issue: &crate::app::views::connections::FieldIssue, text: &dyn Fn(&str) -> String) -> String {
    let key = text(issue.key);
    let detail = issue.detail.trim();
    if detail.is_empty() {
        key
    } else {
        format!("{key} · {detail}")
    }
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
/// 模态里分段选择的槽：浅底圆角，选中项浮起（与 kit::segmented_sized 同一外观）。
/// 选项多时整槽换行，不被裁切。
fn segment_track(p: &Palette) -> gpui_kit::Div {
    div().flex().flex_wrap().flex_none().self_start().max_w_full().p(px(2.)).gap(px(2.)).rounded(px(Radius::CONTROL)).bg(p.hover)
}

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

/// 设置页的分区导航在宽内容区放左侧，窄内容区折成顶部一行；
/// 180 的导航加上表单与两侧页边距在 820 以下会把控件挤到换行以外。
pub(super) fn settings_side_by_side(content_width: f32) -> bool {
    content_width >= 820.
}

fn palette_icon(id: &str) -> IconName {
    if id.starts_with("connection:") || id.starts_with("palette-connection:") {
        return IconName::Database;
    }
    if id.starts_with("palette-database:") {
        return IconName::FolderOpen;
    }
    if id.starts_with("palette-saved:") {
        return IconName::SquareTerminal;
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

/// 连接串占位示例：与解析器支持的写法一致。
fn url_example(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "postgres://user@host:5432/db",
        Engine::Mysql => "mysql://user@host:3306/db",
        Engine::Mariadb => "mariadb://user@host:3306/db",
        Engine::Sqlserver => "sqlserver://user@host:1433/db",
        Engine::Sqlite => "sqlite:///path/to/file.db",
        Engine::Redis => "redis://host:6379/0",
        Engine::Mongodb => "mongodb://user@host:27017/db",
    }
}

/// 列表里显示的文件路径：长路径只留最后两段（目录/文件名），完整路径放在提示里。
fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|part| !part.is_empty()).collect();
    if parts.len() <= 2 || path.chars().count() <= 40 {
        return path.to_owned();
    }
    format!("…/{}/{}", parts[parts.len() - 2], parts[parts.len() - 1])
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
