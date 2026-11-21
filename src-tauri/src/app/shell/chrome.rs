//! 外壳的画法：顶栏、活动栏、资源管理器、标签条、状态栏、页面与浮层。
//! 状态与命令都在 `WorkbenchModel`；这里只把它画出来，并把点击交回 `perform`。

use gpui_kit::component::input::Input;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Editor, EditorState, InputState};
use gpui_kit::component::{ActiveTheme as _, Size, TitleBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, DragMoveEvent, Entity, FocusHandle, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, point, px,
};

use crate::app::views::connections::{engines, LinkStatus};
use crate::app::views::history::{HistorySection, ListPhase};
use crate::app::views::settings::{
    SaveState, SettingsSection, FONT_RANGE, OBJECT_PAGE_RANGE, PAGE_SIZE_RANGE, PREVIEW_RANGE,
};
use crate::app::{AppPage, FocusTarget, ThemePreference};
use crate::catalog::hierarchy::{NavKind, NavRow};
use crate::catalog::model::ObjectKind;
use crate::connections::model::{CaMode, Engine, EnvTag, SshAuthKind, TlsMode};
use crate::i18n;
use crate::ui::kit::{self, ControlSize, FieldSpec, Palette, Tip as _, Radius, Space, TypeScale};
use crate::ui::kit::ScrollAreaExt as _;
use crate::ui::tabs;
use crate::ui::read_table::RowPick;

use gpui_kit::base::FocusTrapElement as _;

use super::conn_modal;
use super::model::{ConnFilterMenu, DraftField, PaletteScope, WorkItemKind, WorkTool};
use super::transaction::{self, GuardDecision, TransactionActions, TransactionMenu};
use super::view::Workbench;
use crate::adapters::Workbench as WorkbenchKind;
use crate::transaction::TransactionCapabilities;

use crate::ui::kit::stack_frame;

/// 活动栏宽度来自布局模块，绘制与几何只保留一份。
const RAIL_W: f32 = super::layout::RAIL_WIDTH as f32;
/// 活动栏激活指示条（3px 宽的竖线）两端的圆角。线条不是控件，不取 `Radius` token；见扫描测试例外清单。
const RAIL_INDICATOR_RADIUS: f32 = 2.;

/// 拖拽连接时的拖动图形：不画内容，避免挡住目标分组标题。
pub(super) struct ConnectionDragGhost;

impl Render for ConnectionDragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(0.))
    }
}

/// 点击后执行一条外壳命令。
pub(super) fn act<S: Into<String>>(id: S, cx: &mut Context<Workbench>) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static + use<S> {
    let id: String = id.into();
    let entity = cx.entity().downgrade();
    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        if let Some(entity) = entity.upgrade() {
            entity.update(cx, |this, cx| this.perform(&id, cx));
        }
    }
}

fn field_changed(this: &mut Workbench, field: DraftField, value: String, cx: &mut Context<Workbench>) {
    let setting = matches!(field, DraftField::IndexNamingNormal | DraftField::IndexNamingUnique | DraftField::IndexNamingJoiner);
    this.model.set_field(field, value);
    // 索引命名模板是设置：改了就写盘（带保存反馈），结构页下一次整理时拿到新模板。
    if setting {
        this.persist_settings(cx);
    }
}

impl Workbench {
    pub(super) fn t(&self, key: &str) -> SharedString {
        SharedString::from(self.model.text(key))
    }

    /// 连接的环境标签文字。存过的标签用它的名称，不用原始 id。
    pub(super) fn env_text(&self, env: &str) -> SharedString {
        SharedString::from(env_label(|key| self.model.text(key), env, &self.model.env_tags))
    }

    /// 连接的环境标签小块；没有标签（空或 none）时不画。颜色跟标签色调走。
    pub(super) fn env_pill(&self, env: &str, p: &Palette) -> Option<AnyElement> {
        let tone = env_tone(env, &self.model.env_tags).to_owned();
        let label = self.env_text(env);
        env_shown(env).then(|| kit::env_tag(&tone, label, p))
    }

    /// 当前密度的尺寸指标；绘制只读这一处。
    pub(super) fn metrics(&self) -> kit::Metrics {
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
            self.palette_scroll_seen = None;
        }
        if !help_open {
            self.help_focused = false;
        }
        if self.model.danger.is_none() {
            self.danger_focused_epoch = None;
        }
        if self.model.password_prompt.is_none() {
            self.password_focused_epoch = None;
        }
        if page != AppPage::History || !self.model.history.rename_open {
            self.history_rename_focused = false;
        }
        let sidebar_visible = self.model.shell.sidebar_open && matches!(page, AppPage::Workspace | AppPage::Structure | AppPage::Compare | AppPage::Diagram);
        let overlay_sidebar = sidebar_visible && frame.sidebar_overlay;
        let docked_sidebar = sidebar_visible && !frame.sidebar_overlay;

        let content: AnyElement = match page {
            AppPage::Connections => self.connections_page(window, cx),
            AppPage::History => self.history_page(window, cx),
            AppPage::Settings => self.settings_page(window, cx),
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
                    .min_h_0()
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
                            .min_w_0()
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
                                    this.persist_if_needed(cx);
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

        // 盖在工作区上的浮层（模态、命令面板、右键菜单……）。编辑器贴着文本的浮层（行尾错误标记、错误弹层、
        // 签名提示）是 deferred 绘制，会画在这些普通层之上；有浮层时让它们这一帧不画。
        let mut covered = false;
        if self.model.exchange_open {
            covered = true;
            root = root.child(self.exchange_layer(cx));
        }
        if palette_open || help_open {
            covered = true;
            root = root.child(self.palette_layer(window, cx));
        }
        if self.model.conn_filter_menu.is_some() {
            covered = true;
            root = root.child(self.conn_filter_layer(cx));
        }
        if self.model.menu.is_some() {
            covered = true;
            root = root.child(self.context_layer(cx));
        }
        // 连接模态盖在最上层：backdrop 吞掉下面控件的点击，Esc 与 Tab 也归它。
        if self.model.conn_modal.open {
            covered = true;
            root = root.child(self.connection_modal_layer(window, cx));
        }
        if let Some(layer) = self.tag_manager_layer(window, cx) {
            covered = true;
            root = root.child(layer);
        }
        if self.model.close_prompt.is_some() {
            covered = true;
            root = root.child(self.close_layer(cx));
        }
        if self.model.transaction_guard.is_some() {
            covered = true;
            root = root.child(self.transaction_guard_layer(cx));
        }
        if self.model.connections.confirm_delete {
            covered = true;
            root = root.child(self.delete_layer(cx));
        }
        if self.model.group_delete.is_some() {
            covered = true;
            root = root.child(self.group_delete_layer(cx));
        }
        if !self.model.open_choices.is_empty() {
            covered = true;
            root = root.child(self.open_choice_layer(cx));
        }
        if self.model.history.confirm_clear {
            covered = true;
            root = root.child(self.clear_history_layer(cx));
        }
        if self.model.history.confirm_delete {
            covered = true;
            root = root.child(self.delete_saved_layer(cx));
        }
        if self.model.ddl_view.as_ref().is_some_and(|view| !view.copy_only) {
            covered = true;
            root = root.child(self.ddl_layer(window, cx));
        }
        if self.model.danger.is_some() {
            covered = true;
            root = root.child(self.danger_layer(window, cx));
        }
        if self.model.password_prompt.is_some() {
            covered = true;
            root = root.child(self.password_layer(window, cx));
        }
        crate::ui::ShellOverlayOpen::set(window, covered, cx);
        root.into_any_element()
    }

    /// 浮层打开时，方向键、回车与 Esc 归浮层。回车经由 `perform` 执行，保证效果完整。
    fn overlay_key(&mut self, event: &gpui_kit::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        // 事务保护层独占键盘：Esc 取消，其他按键不在后面操作（对话框本身不是表单）。
        if self.model.transaction_guard.is_some() {
            if key == "escape" {
                self.perform("tx-guard-cancel", cx);
                cx.notify();
            }
            return;
        }
        // 浮层关闭后键盘要回到打开它的地方，而不是留在已经消失的弹层上。
        let overlay_was_open = self.model.menu.is_some()
            || self.model.shell.palette_open
            || self.model.shell.help_open
            || self.model.tag_manager.is_some();
        // 连接模态打开时，Esc 关闭、Tab 在模态里循环、回车等于当前页脚动作。
        if self.model.conn_modal.open && self.model.menu.is_none() && !self.model.shell.palette_open && self.model.danger.is_none() {
            if key == "escape" {
                // 有未保存的修改时 Esc 先问一句；确认条已经在问时再按 Esc 是“继续编辑”。
                self.perform("conn-escape", cx);
                cx.notify();
                return;
            }
            // Cmd/Ctrl+Enter 在任何字段里都是“保存并连接”。全局的“执行”绑定在模态打开时也转到这里，
            // 这一处兼顾按键没有先被动作绑定接走的情况。
            if key == "enter" && (event.keystroke.modifiers.platform || event.keystroke.modifiers.control) {
                self.perform("conn-save-connect", cx);
                cx.notify();
                return;
            }
            // Tab/Shift+Tab 由面板上的 GPUI focus trap 处理（Root 的 Tab 动作在容器内成环），
            // 这里不再自绘序列——那会被 Root 的绑定抢先吃掉。
            // URL 快速填充框里按回车直接解析连接串，不必再伸手点“解析”。
            if key == "enter" && self.fields.is_focused(DraftField::Url, window, cx) {
                self.perform("conn-apply-url", cx);
                cx.notify();
                return;
            }
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
        // 密码输入框：Esc 取消，回车用输入的密码连接。
        if self.model.password_prompt.is_some() && self.model.menu.is_none() && !self.model.conn_modal.open {
            match key {
                "escape" => {
                    self.perform("password-cancel", cx);
                    self.restore_focus(window, cx);
                    cx.notify();
                    return;
                }
                "enter" => {
                    self.perform("password-submit", cx);
                    if self.model.password_prompt.is_none() {
                        self.restore_focus(window, cx);
                    }
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }
        // 打开方式询问：Esc 跳过这个文件。
        if key == "escape" && !self.model.open_choices.is_empty() && self.model.menu.is_none() {
            self.perform("open-choice-cancel", cx);
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
            if self.model.shell.page == AppPage::Structure
                && self.model.tool == WorkTool::Structure
                && self.structure.update(cx, |pane, cx| {
                    let closed = pane.handle_escape();
                    if closed {
                        cx.notify();
                    }
                    closed
                })
            {
                return;
            }
            // 查询结果全屏时 Esc 退出全屏（正在原位编辑单元格时 Esc 仍归那一格）。
            if self.active_query_context() && self.query.update(cx, |pane, cx| pane.handle_escape(cx)) {
                cx.notify();
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
        // 确认层/对话框开着时，页面级键一律让路：不能落到侧栏对象树、连接列表或历史列表上，
        // 那样"确认"或"打开"的就会是背景里的另一个对象。Esc 交给下面的浮层收尾处理。
        let modal_blocks_page = self.model.history.confirm_open()
            || self.model.connections.confirm_delete
            || self.model.group_delete.is_some()
            || !self.model.open_choices.is_empty()
            || self.model.group_prompt.is_some()
            || self.model.tag_manager.is_some()
            || self.model.shell.help_open
            || self.model.danger.is_some()
            || self.model.password_prompt.is_some()
            || self.model.ddl_view.as_ref().is_some_and(|view| !view.copy_only);
        if self.model.menu.is_none()
            && self.model.close_prompt.is_none()
            && !self.model.shell.palette_open
            && modal_blocks_page
            && key != "escape"
        {
            return;
        }
        // 侧栏有焦点时方向键属于对象树：上下移动、左右展开/折叠、回车打开。
        if self.model.menu.is_none()
            && self.model.close_prompt.is_none()
            && !self.model.shell.palette_open
            && !modal_blocks_page
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
        // 连接页的方向键属于连接启动台列表：上下/Home/End 在筛选后的可见连接里移动，回车打开/连接。
        if self.model.menu.is_none()
            && self.model.close_prompt.is_none()
            && !self.model.shell.palette_open
            && !modal_blocks_page
            && self.model.shell.page == AppPage::Connections
            && self.model.shell.focus != FocusTarget::Sidebar
        {
            let filter_focused = self.fields.is_focused(DraftField::Filter, window, cx);
            let launcher_btn_focused = self.conn_launcher_focus.is_focused(window);
            let nav = match key {
                "up" => Some(super::menus::NavKey::ArrowUp),
                "down" => Some(super::menus::NavKey::ArrowDown),
                "home" if !filter_focused => Some(super::menus::NavKey::Home),
                "end" if !filter_focused => Some(super::menus::NavKey::End),
                "enter" if !launcher_btn_focused => Some(super::menus::NavKey::Enter),
                _ => None,
            };
            if let Some(nav) = nav {
                if let Some(command) = self.model.connections_key(nav) {
                    self.perform(&command, cx);
                }
                cx.notify();
                return;
            }
        }
        // 历史页的方向键属于列表：上下/Home/End 在筛选后的可见行里选，
        // 回车只载入编辑器（不执行），Delete 删除当前条目（与条目右键同一条命令）。
        // 有键位绑定时 Delete 作为动作到 perform，没有时在这里直接落到同一条命令。
        if self.model.menu.is_none()
            && self.model.close_prompt.is_none()
            && !self.model.shell.palette_open
            && !modal_blocks_page
            && self.model.shell.page == AppPage::History
            && self.model.shell.focus != FocusTarget::Sidebar
        {
            let rename_focused = self.fields.is_focused(DraftField::RenameName, window, cx);
            let save_focused = self.fields.is_focused(DraftField::SaveName, window, cx);
            let search_focused = self.fields.is_focused(DraftField::HistorySearch, window, cx);
            let any_input = rename_focused || save_focused || search_focused;
            if rename_focused || save_focused {
                return;
            }
            // R13：列表里 ⌘C 复制选中条目的 SQL（与右键「复制」同一条命令）；输入框里的 ⌘C 留给输入框。
            let mods = event.keystroke.modifiers;
            if key == "c" && mods.secondary(cx) && !mods.shift && !mods.alt && !any_input {
                self.perform("history-copy", cx);
                cx.notify();
                return;
            }
            if mods.shift && !mods.secondary(cx) && !mods.alt && !any_input && matches!(key, "up" | "down") && !self.model.history.confirm_open() {
                self.perform(if key == "down" { "history-extend-next" } else { "history-extend-prev" }, cx);
                self.model.history_reveal = true;
                cx.notify();
                return;
            }
            if key == "delete" && !any_input {
                if let Some(command) = self.model.history.delete_command() {
                    self.perform(command, cx);
                    cx.notify();
                    return;
                }
            }
            let nav = match key {
                "up" => Some(super::menus::NavKey::ArrowUp),
                "down" => Some(super::menus::NavKey::ArrowDown),
                "home" if !any_input => Some(super::menus::NavKey::Home),
                "end" if !any_input => Some(super::menus::NavKey::End),
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
        if self.model.menu.is_none()
            && self.model.close_prompt.is_none()
            && !self.model.shell.palette_open
            && !self.model.shell.help_open
            && self.model.tag_manager.is_none()
        {
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
        if was_open
            && self.model.menu.is_none()
            && !self.model.shell.palette_open
            && !self.model.shell.help_open
            && self.model.tag_manager.is_none()
        {
            self.restore_focus(window, cx);
        }
    }

    fn restore_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.model.shell.focus {
            FocusTarget::Sidebar if self.model.shell.sidebar_open => window.focus(&self.nav_focus, cx),
            // 查询页的结果网格属于 QueryPane。公开焦点入口是编辑器；表格页才回到共享网格。
            FocusTarget::Grid if self.model.tool == super::model::WorkTool::Query => self.query.update(cx, |pane, _| pane.request_focus()),
            FocusTarget::Grid => self.table.update(cx, |pane, _| pane.request_focus()),
            // 查询编辑器只画在工作区；在别的页面（连接页、历史页）关掉浮层时把焦点交回工作台根，
            // 否则焦点落在已经不在界面上的浮层输入框里，Esc、方向键都没人接。
            FocusTarget::Editor if self.model.shell.page != AppPage::Workspace => window.focus(&self.focus, cx),
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
            .min_w_0()
            .pr(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(controls(
                kit::icon_button("top-sidebar", IconName::PanelLeft, self.t("shell.command.toggleSidebar"), act("sidebar", cx))
                    .selected(self.model.shell.sidebar_open)
                    .into_any_element(),
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
                    .w(px(if narrow { 176. } else { 360. }))
                    .h(px(m.control_h))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(Radius::CONTROL))
                    .border_1()
                    .border_color(p.line)
                    .bg(p.sunken)
                    .hover(|style| style.bg(p.surface).border_color(p.line_strong))
                    .child(kit::icon(IconName::Search, 13., p.text_faint))
                    .child(div().flex_1().truncate().text_size(px(TypeScale::SM)).text_color(p.text_faint).child(self.t("ui.search.placeholder")))
                    .child(kit::kbd(kit::shortcut("Ctrl+K"), &p))
                    .on_click(act("command-palette", cx))
                    .into_any_element(),
            ))
            .child(div().flex_1())
            .child(
                // 唯一的“打开文件”入口：.sql 进编辑器，SQLite 文件建连接，认不出时询问。
                controls(kit::icon_button("top-open-file", IconName::FolderOpen, self.t("shell.command.openFile"), act("open-file", cx)).into_any_element())
                    .when(!narrow, |this| {
                        this.child(
                            kit::icon_button("top-exchange", IconName::ArrowDownUp, self.t("shell.command.openExchange"), act("open-exchange", cx))
                                .selected(self.model.exchange_open || self.model.exchange_running),
                        )
                    })
                    .child(kit::icon_button(
                        "top-theme",
                        if dark { IconName::Sun } else { IconName::Moon },
                        // 名称固定、不随当前主题变：悬停提示在点击后的那一帧之前就已生成，
                        // “切换到浅色主题”这类随状态变的文案会在切换后还挂着旧说法。图标本身表明去向。
                        self.t("shell.command.toggleTheme"),
                        act(theme_next, cx),
                    ))
                    .child(if narrow {
                        kit::icon_button("top-new-query", IconName::Plus, self.t("shell.command.newQuery"), act("new-query", cx)).primary().into_any_element()
                    } else {
                        kit::subtle_button("top-new-query", Some(IconName::Plus), self.t("shell.command.newQuery"), act("new-query", cx)).into_any_element()
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
        let rail = crate::app::model::RAIL_TOP;
        for id in rail {
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
        let (filter, filter_state) =
            self.field_input(DraftField::ObjectFilter, &self.model.text("ui.explorer.filterObjects"), false, window, cx);
        // Esc 清空筛选框：与框内 × 一样以空值送达，走唯一的清除入口；框已空时 Esc 照常向外传递。
        filter_state.update(cx, |state, _| state.set_clean_on_escape(true));
        let group_prompt = self.group_prompt_panel(window, cx);
        // 对象索引来自模型里的同一份快照；连接列表保持完整，连接筛选只属于连接页。
        // 可见行先拍成扁平序列（分组标题 → 连接 → 树行），渲染只创建视口窗口里的行。
        let mut list = div()
            .id("explorer-list")
            .test_support()
            .track_focus(&self.nav_focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .scroll_area_y()
            .scroll_handle(&self.explorer_scroll)
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
                    .justify_center()
                    .gap(px(6.))
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
                    .child(kit::secondary_button(
                        "explorer-open-file",
                        Some(IconName::FolderOpen),
                        self.t("shell.command.openFile"),
                        act("open-file", cx),
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
                    .min_w_0()
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
                                    .child(
                                        kit::icon_button("explorer-collapse", IconName::ChevronsUp, self.t("ui.explorer.collapseAll"), act("collapse-tree", cx))
                                            .xsmall()
                                            .disabled(!self.model.nav.has_expanded()),
                                    )
                                    .child(
                                        kit::icon_button("explorer-refresh", IconName::RefreshCw, self.t("query.tree.refresh"), act("refresh-tree", cx))
                                            .xsmall()
                                            .disabled(!self.model.has_connected()),
                                    )
                                    // 分组入口就在树旁边：新建组、右键组改名/删除、拖连接归组。
                                    .child(kit::icon_button("explorer-group-add", IconName::Folder, self.t("ui.group.new"), act("group-new", cx)).xsmall())
                                    .child(kit::icon_button("explorer-add", IconName::Plus, self.t("common.newConnection"), act("conn-new-sidebar", cx)).xsmall()),
                            ),
                    )
                    // 没有连接时没有可筛的对象，筛选框不出现。
                    .when(!self.model.connections.rows.is_empty(), |this| {
                        this.child(div().px(px(10.)).pb(px(8.)).child(filter.prefix(kit::input_prefix(IconName::Search, 13., p.text_faint)).cleanable(true)))
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
            ExplorerItem::ConnectFailed { connection_id, reason } => self.connect_failed_row(connection_id, reason, p, cx),
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
                            .min_w_0()
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
                let has_object_filter = !self.model.object_filter.trim().is_empty();
                div()
                    .id("explorer-search-empty")
                    .test_support()
                    .flex_none()
                    .h(px(m.tree_row_h + 4.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .text_size(px(TypeScale::SM))
                    .text_color(p.text_faint)
                    .child(div().flex_1().min_w_0().truncate().child(self.t("ui.explorer.noMatch")))
                    .when(has_object_filter, |this| {
                        this.child(
                            kit::tool_button(
                                "explorer-clear-filter",
                                Some(IconName::X),
                                self.t("connections.filter.clear"),
                                act("explorer-clear-filter", cx),
                            )
                            .xsmall(),
                        )
                    })
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
            .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(SharedString::from(name.clone())))
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
    pub(super) fn group_prompt_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(prompt) = self.model.group_prompt.clone() else {
            self.group_prompt_focused = false;
            return None;
        };
        let p = Palette::of(cx);
        let title = if prompt.id.is_some() { "ui.group.rename" } else { "ui.group.new" };
        let (input, state) = self.field_input(DraftField::GroupName, &self.model.text("ui.group.namePlaceholder"), false, window, cx);
        // 面板一出现就把键盘交给名称输入框：点“新建分组”后直接打字，回车保存，Esc 取消。
        if !self.group_prompt_focused {
            self.group_prompt_focused = true;
            let handle = state.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        Some(
            div()
                .id("group-prompt")
                .test_support()
                .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| match event.keystroke.key.as_str() {
                    "enter" => {
                        cx.stop_propagation();
                        this.perform("group-save", cx);
                    }
                    "escape" => {
                        cx.stop_propagation();
                        this.perform("group-prompt-cancel", cx);
                    }
                    _ => {}
                }))
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
        let query_id = id.clone();
        let group = SharedString::from(format!("side-conn-group-{id}"));
        let supports_query = row.saved.engine.workbench().has_query_editor();
        kit::row_states(div().id(SharedString::from(format!("side-{id}"))), p)
            .test_support()
            .group(group.clone())
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
            .when(selected, |this| this.bg(p.selection))
            .when(!selected, |this| this.hover(|style| style.bg(p.hover)))
            .child(
                div()
                    .id(SharedString::from(format!("nav-chevron-{id}")))
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(self.t(if expanded { "catalog.tree.collapse" } else { "catalog.tree.expand" }))
                    .w(px(15.))
                    .flex()
                    .items_center()
                    .justify_center()
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
            .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(name))
            .when(status == LinkStatus::Connecting, |this| this.child(self.spinners.element(format!("conn-{}", row.saved.id), Size::XSmall, Some(p.text_muted), cx)))
            .when(supports_query, |this| {
                this.child(explorer_hover_icon(
                    SharedString::from(format!("side-query-{id}")),
                    group.clone(),
                    IconName::SquareTerminal,
                    self.t("shell.command.newQuery"),
                    self.model.menu.is_none(),
                    p,
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.model.connections.selected = Some(query_id.clone());
                        this.perform("conn-new-query", cx);
                    }),
                ))
            })
            .when_some(browser_command(row), |this, (command, key, element_id)| {
                let label = format!("{} {}", self.model.text(key), row.saved.name);
                let icon = if row.saved.engine.workbench() == WorkbenchKind::Redis { IconName::Key } else { IconName::Braces };
                this.child(kit::icon_button(SharedString::from(element_id), icon, SharedString::from(label), act(command, cx)).xsmall())
            })
            .child({
                // 失败时悬停圆点也能读到原因，不只是“连接失败”四个字。
                let state_text = match self.model.connections.failures.get(&id).filter(|_| status == LinkStatus::Failed) {
                    Some(reason) => SharedString::from(format!("{}: {reason}", self.t(state_key))),
                    None => self.t(state_key),
                };
                div()
                    .id(SharedString::from(format!("side-state-{id}")))
                    .aria_label(state_text.clone())
                    .tooltip_text(state_text)
                    .child(kit::status_dot(dot))
            })
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.menu_at = event.position;
                this.model.connections.selected = Some(menu_id.clone());
                this.model.set_explorer_current(super::explorer::ExplorerRow::Connection(menu_id.clone()));
                this.perform("menu-explorer-connection", cx);
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
                    .min_w_0()
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

    /// 连接没连上：原因直接写在这条连接下面（完整原文在悬停提示里），旁边是重试和编辑连接。
    /// 不用再去状态栏里找一闪而过的提示，也不用猜是密码、地址还是证书的问题。
    fn connect_failed_row(&self, id: &str, reason: &str, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let full = SharedString::from(format!("{}: {reason}", self.t("connections.status.error")));
        div()
            .id(SharedString::from(format!("nav-conn-failed-{id}")))
            .test_support()
            .flex_none()
            .pl(px(42.))
            .pr(px(4.))
            .py(px(3.))
            .flex()
            .items_center()
            .gap(px(4.))
            .child(
                div()
                    .id(SharedString::from(format!("nav-conn-failed-text-{id}")))
                    .test_support()
                    .aria_label(full.clone())
                    .tooltip_text(full.clone())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(TypeScale::XS))
                    .text_color(p.danger)
                    // 行内只写原因首行；处理建议在悬停提示里。
                    .child(SharedString::from(reason.lines().next().unwrap_or(reason).to_owned())),
            )
            .child(
                kit::icon_button(
                    SharedString::from(format!("nav-connect-retry-{id}")),
                    IconName::RefreshCw,
                    self.t("common.retry"),
                    act(format!("nav-connect-retry:{id}"), cx),
                )
                .xsmall(),
            )
            .child(
                kit::icon_button(
                    SharedString::from(format!("nav-connect-edit-{id}")),
                    IconName::Pencil,
                    self.t("ui.explorer.editConnection"),
                    act(format!("nav-connect-edit:{id}"), cx),
                )
                .xsmall(),
            )
            .into_any_element()
    }

    fn click_connection(&mut self, id: &str, double: bool, cx: &mut Context<Self>) {
        self.model.connections.selected = Some(id.to_owned());
        self.model.shell.focus = FocusTarget::Sidebar;
        let status = self.model.connections.status.get(id).copied().unwrap_or(LinkStatus::Disconnected);
        // 关系型连接单击打开（或就地替换）概览预览标签，双击固定；未连接的先按需连接再读。
        self.model.overview_click_connection(id, double);
        match status {
            LinkStatus::Connected => {
                // 单击选中这一行并打开概览；双击在树上展开/折叠，Redis/MongoDB 连接双击直接开浏览器。
                // chevron 单击是展开/折叠的另一条鼠标路径，和双击、键盘右/左键走同一份状态。
                if double {
                    let specialized = self
                        .model
                        .connections
                        .rows
                        .iter()
                        .any(|row| row.saved.id == id && !row.saved.engine.is_relational());
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
            SharedString::from(localized_nav_tooltip(&row.path, &row.name, label.as_ref(), row.label_key.is_some()))
        };
        let is_group = matches!(row.kind, NavKind::Group | NavKind::ColumnGroup | NavKind::IndexGroup);
        // 已读完、确实一个对象都没有的种类分组：不画展开箭头，名称与数量弱化，看起来不像“展开了却丢了内容”。
        let empty_group = row.kind == NavKind::Group && !row.unloaded && !row.loading && row.error.is_none() && row.total == 0 && row.loaded == 0 && !row.has_more;
        let body = match row.highlight {
            Some((start, end)) if end > start && end <= label.len() => highlight_label(&id, &label, start, end, p),
            _ => div().flex_1().min_w_0().truncate().text_size(px(TypeScale::SM)).when(empty_group, |this| this.text_color(p.text_faint)).child(label.clone()).into_any_element(),
        };
        let click_id = id.clone();
        let menu_id = id.clone();
        let focus_id = id.clone();
        let toggle_id = id.clone();
        let open_data_id = id.clone();
        let open_struct_id = id.clone();
        let has_error = row.error.is_some();
        let quick_table = row.kind == NavKind::Object
            && matches!(row.object_kind, Some(ObjectKind::Table | ObjectKind::View))
            && !row.stale
            && !has_error;
        let group = SharedString::from(format!("nav-row-group-{id}"));
        let guides = (0..depth).map(|level| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(13. + level as f32 * 12.))
                .w(px(1.))
                .bg(p.line_soft)
                .into_any_element()
        });
        kit::row_states(div().id(SharedString::from(format!("nav-{id}"))), p)
            .test_support()
            .group(group.clone())
            .role(gpui_kit::Role::TreeItem)
            .aria_label(label.clone())
            .aria_selected(row.focused || row.selected)
            .aria_expanded(row.expanded)
            // 右键菜单打开时不再挂悬停提示：提示浮层会盖住菜单的第一项。
            .when(self.model.menu.is_none(), |this| this.tooltip_text(tooltip))
            .relative()
            .flex_none()
            .h(px(m.tree_row_h))
            .pl(px(indent))
            .pr(px(4.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(Radius::CONTROL))
            .when(row.focused || row.selected, |this| this.bg(p.selection))
            .when(!row.focused && !row.selected, |this| this.hover(|style| style.bg(p.hover)))
            .children(guides)
            .child(
                div()
                    .id(SharedString::from(format!("nav-chevron-{id}")))
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(self.t(if row.expanded { "catalog.tree.collapse" } else { "catalog.tree.expand" }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .flex_none()
                    .w(px(16.))
                    .when(row.expandable && !empty_group, |this| {
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
            .child(
                // 名称区占满行内剩余宽度；悬停快捷按钮浮在名称区末尾，不常驻占宽，
                // 也不盖住行尾的命中数、加载、错误与重试：悬停时只遮住名称最后几个字。
                div()
                    .id(SharedString::from(format!("nav-name-{id}")))
                    .test_support()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(body)
                    .when(quick_table, |this| {
                        let tip = self.model.menu.is_none();
                        this.child(
                            div()
                                .id(SharedString::from(format!("nav-quick-{id}")))
                                .test_support()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .right_0()
                                .pl(px(6.))
                                .flex()
                                .items_center()
                                .gap(px(1.))
                                .invisible()
                                .group_hover(group.clone(), |style| style.visible())
                                // 底色与行当前底色一致：选中行是选中底，其余是悬停底，不出现色块。
                                .bg(if row.focused || row.selected { p.selection } else { p.hover })
                                .rounded(px(Radius::CONTROL))
                                .child(explorer_hover_icon(
                                    SharedString::from(format!("nav-quick-data-{id}")),
                                    group.clone(),
                                    IconName::Table,
                                    self.t("catalog.tree.openData"),
                                    tip,
                                    p,
                                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.perform(&format!("nav-open-data:{open_data_id}"), cx);
                                    }),
                                ))
                                .child(explorer_hover_icon(
                                    SharedString::from(format!("nav-quick-struct-{id}")),
                                    group.clone(),
                                    IconName::Columns3,
                                    self.t("catalog.tree.viewStructure"),
                                    tip,
                                    p,
                                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.perform(&format!("nav-open-structure:{open_struct_id}"), cx);
                                    }),
                                )),
                        )
                    }),
            )
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
            .when(row.search_hits > 0 && !(is_group && row.expanded), |this| {
                this.child(
                    div()
                        .id(SharedString::from(format!("nav-hits-{id}")))
                        .test_support()
                        .flex_none()
                        .px(px(5.))
                        .rounded(px(Radius::TAG))
                        .bg(p.accent_soft)
                        .text_size(px(TypeScale::XS))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(p.accent_text)
                        .when(self.model.menu.is_none(), |this| {
                            this.tooltip_text(SharedString::from(i18n::fill(&self.model.text("ui.nav.searchHits"), &[("n", &row.search_hits.to_string())])))
                        })
                        .child(SharedString::from(row.search_hits.to_string())),
                )
            })
            .when(is_group && !row.unloaded && row.error.is_none() && row.search_hits == 0, |this| {
                this.child(
                    div()
                        .flex_none()
                        .px(px(5.))
                        .rounded(px(Radius::TAG))
                        .when(!empty_group, |this| this.bg(p.hover))
                        .text_size(px(TypeScale::XS))
                        .text_color(if empty_group { p.text_faint } else { p.text_muted })
                        .child(SharedString::from(if row.total_unknown { format!("{}+", row.loaded) } else { row.total.to_string() })),
                )
            })
            .when(row.stale, |this| {
                this.child(div().text_size(px(TypeScale::XS)).text_color(p.warn).child(self.t("catalog.tree.stale")))
            })
            .when(row.loading, |this| this.child(self.spinners.element(format!("nav-{}", row.id), Size::XSmall, Some(p.text_muted), cx)))
            .when_some(row.error.clone(), |this, error| {
                let error = self.model.text(&error);
                let full = format!("{}: {error}", self.t("catalog.tree.nodeError"));
                this.child(
                    div()
                        .id(SharedString::from(format!("nav-err-{id}")))
                        .test_support()
                        .aria_label(SharedString::from(full.clone()))
                        .when(self.model.menu.is_none(), |this| this.tooltip_text(SharedString::from(full.clone())))
                        // 错误与名称分享剩余宽度（名称占三分之二），窄侧栏里对象名不会被错误挤没；
                        // 完整原因在悬停提示和无障碍名称里。
                        .flex_basis(px(0.))
                        .flex_grow(0.5)
                        .flex_shrink(1.)
                        .min_w_0()
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
            // 续读只在展开的节点上出现，并写明还剩多少；同时提供“全部”一键读完，不必反复点 100 条。
            .when(row.has_more && (row.expanded || is_group), |this| {
                let remaining = row.total.saturating_sub(row.loaded);
                let label = if remaining > 0 && !row.total_unknown {
                    i18n::fill(&self.model.text("ui.nav.moreCount"), &[("count", &remaining.to_string())])
                } else {
                    self.model.text("catalog.tree.loadMore")
                };
                this.child(
                    kit::tool_button(
                        SharedString::from(format!("nav-more-{focus_id}")),
                        Some(IconName::ChevronsDown),
                        SharedString::from(label),
                        act(format!("nav-more:{focus_id}"), cx),
                    )
                    .xsmall()
                    .tooltip(self.t("catalog.tree.loadMore")),
                )
                .child(
                    kit::tool_button(
                        SharedString::from(format!("nav-all-{focus_id}")),
                        None,
                        self.t("ui.nav.loadAll"),
                        act(format!("nav-all:{focus_id}"), cx),
                    )
                    .xsmall()
                    .tooltip(self.t("ui.nav.loadAllTip")),
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
                if event.modifiers().secondary(cx) {
                    // Ctrl/⌘ 点击只切换多选（只有表可以多选），不展开、不打开概览。
                    this.perform(&format!("nav-select-toggle:{click_id}"), cx);
                    cx.notify();
                    return;
                }
                // 普通点击清空多选。
                this.perform("nav-select-clear", cx);
                this.perform(&format!("nav-focus:{click_id}"), cx);
                if is_group && !empty_group {
                    // 纯分组目录行（表 / 视图 / 列 / 索引）与用户连接组一样单击即展开/折叠；
                    // 连点第二下忽略，避免双击展开又立即收起。
                    if event.click_count() == 1 {
                        this.perform(&format!("nav-toggle:{click_id}"), cx);
                    }
                } else if event.click_count() >= 2 {
                    // 双击容器在树上展开/折叠，对象打开自己的数据或文档页；库/模式的概览预览转为固定标签。
                    this.model.overview_click_node(&click_id, true);
                    if let Some(command) = this.model.nav_toggle_command(&click_id) {
                        this.perform(&command, cx);
                    }
                } else {
                    // 单击库/模式：主区打开（或就地替换）概览预览标签。
                    this.model.overview_click_node(&click_id, false);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    // ------------------------------------------------------------ 工作区

    /// 共享事务控件：查询、网格与结构工作区共用同一行，状态只读连接服务的共享快照。
    ///
    /// 任一入口（开始、提交、回滚、保存点、隔离/模式）都作用于 R2 的同一个用户事务；命令结果由
    /// 视图执行并推进快照，其他标签在下一帧或轮询兜底时看到同一状态。
    #[inline(never)]
    fn transaction_bar(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !matches!(self.model.shell.page, AppPage::Workspace | AppPage::Structure) {
            return None;
        }
        let target = self.model.transaction_target()?;
        let snapshot = self.services.connections.transaction_snapshot(&target.connection_id)?;
        let capabilities = self
            .services
            .connections
            .transaction_coordinator(&target.connection_id)
            .map(|coordinator| coordinator.capabilities())
            .unwrap_or(TransactionCapabilities::UNSUPPORTED);
        if !capabilities.explicit_control {
            return None;
        }
        self.model.transactions.align(&target.connection_id);
        let ui = self.model.transactions.clone();
        let actions = TransactionActions::of(&snapshot, capabilities);
        let state = snapshot.state;
        let locale = self.model.locale();
        let p = Palette::of(cx);
        let metrics = self.metrics();
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let busy = ui.busy || self.transaction_busy();
        let (state_fg, state_bg) = match state {
            crate::transaction::TransactionState::Idle => (p.text_muted, p.surface),
            crate::transaction::TransactionState::Active => (p.accent_text, p.accent_soft),
            crate::transaction::TransactionState::Aborted => (p.warn, p.warn_soft),
            crate::transaction::TransactionState::Unknown => (p.danger, p.danger_soft),
        };
        let owner = if target.connection_name.trim().is_empty() { target.connection_id.clone() } else { target.connection_name.clone() };
        let mut row = div()
            .id("transaction-bar")
            .test_support()
            .debug_selector(|| "transaction-bar".to_owned())
            .flex_none()
            .min_h(px(metrics.toolbar_h()))
            .px(px(Space::LG))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(Space::SM))
            .bg(p.raised)
            .border_b_1()
            .border_color(p.line)
            .child(kit::pill(t(transaction::state_key(state)), state_fg, state_bg))
            .child(div().text_size(px(TypeScale::XS)).text_color(p.text_muted).whitespace_nowrap().child(SharedString::from(owner)))
            .child(kit::vline(&p));

        if actions.begin {
            row = row.child(
                kit::primary_button(transaction::BEGIN, Some(IconName::Play), t("transaction.action.begin"), act(transaction::BEGIN, cx))
                    .disabled(busy),
            );
        }
        if actions.commit {
            row = row.child(
                kit::primary_button(transaction::COMMIT, Some(IconName::Check), t("transaction.action.commit"), act(transaction::COMMIT, cx))
                    .disabled(busy),
            );
        }
        if actions.rollback {
            row = row.child(
                kit::danger_button(transaction::ROLLBACK, Some(IconName::RotateCcw), t("transaction.action.rollback"), act(transaction::ROLLBACK, cx))
                    .disabled(busy),
            );
        }
        if actions.savepoints || actions.savepoint_recovery {
            let open = ui.menu == Some(TransactionMenu::Savepoints);
            let button = kit::tool_button(transaction::SAVEPOINT_MENU, Some(IconName::Bookmark), t("transaction.action.savepoint"), act(transaction::SAVEPOINT_MENU, cx))
                .disabled(busy);
            row = row.child(if open { button.selected(true) } else { button });
        }
        if actions.resolve {
            row = row
                .child(
                    kit::secondary_button(transaction::RESOLVE_COMMITTED, Some(IconName::Check), t("transaction.resolve.committed"), act(transaction::RESOLVE_COMMITTED, cx))
                        .disabled(busy),
                )
                .child(
                    kit::secondary_button(transaction::RESOLVE_ROLLED_BACK, Some(IconName::RotateCcw), t("transaction.resolve.rolledBack"), act(transaction::RESOLVE_ROLLED_BACK, cx))
                        .disabled(busy),
                );
        }
        if actions.isolation {
            let open = ui.menu == Some(TransactionMenu::Isolation);
            let button = kit::tool_button(transaction::ISOLATION_MENU, Some(IconName::Layers), t("transaction.action.isolation"), act(transaction::ISOLATION_MENU, cx))
                .disabled(busy);
            row = row.child(if open { button.selected(true) } else { button });
        }
        if actions.mode {
            let open = ui.menu == Some(TransactionMenu::Mode);
            let button = kit::tool_button(transaction::MODE_MENU, Some(IconName::SlidersHorizontal), t("transaction.action.mode"), act(transaction::MODE_MENU, cx))
                .disabled(busy);
            row = row.child(if open { button.selected(true) } else { button });
        }

        // 已有保存点：每个都可就地部分回滚；支持释放的方言再给一个释放按钮。
        for name in &snapshot.savepoints {
            let rollback = transaction::TransactionCommand::savepoint_rollback_id(name.as_str());
            row = row.child(
                kit::tool_button(rollback.clone(), Some(IconName::CornerDownLeft), SharedString::from(name.as_str()), act(rollback, cx))
                    .tooltip(t("transaction.savepoint.rollback"))
                    .disabled(busy || !(state == crate::transaction::TransactionState::Active || actions.savepoint_recovery)),
            );
            if capabilities.release_savepoint {
                let release = transaction::TransactionCommand::savepoint_release_id(name.as_str());
                row = row.child(
                    kit::icon_button(release.clone(), IconName::X, t("transaction.savepoint.release"), act(release, cx))
                        .xsmall()
                        .disabled(busy || state != crate::transaction::TransactionState::Active),
                );
            }
        }

        row = row.child(div().flex_1());
        if let Some(isolation) = &snapshot.settings.isolation {
            row = row.child(kit::muted(t(&transaction::isolation_label_key(isolation)), &p));
        }
        if let Some(mode) = &snapshot.settings.mode {
            row = row.child(kit::muted(t(&transaction::mode_label_key(mode)), &p));
        }
        if ui.busy {
            row = row.child(kit::muted(t("transaction.busy"), &p));
        } else if let Some(key) = ui.message_key {
            let text = if ui.message_detail.trim().is_empty() {
                self.model.text(key)
            } else {
                format!("{}：{}", self.model.text(key), ui.message_detail.trim())
            };
            row = row.child(div().min_w_0().max_w(px(ControlSize::PICKER_MAX_W)).truncate().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(SharedString::from(text)));
        } else if let Some(hint) = transaction::state_hint_key(state) {
            row = row.child(div().min_w_0().max_w(px(ControlSize::PICKER_MAX_W)).truncate().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(t(hint)));
        }

        let mut column = div().id("transaction-frame").flex().flex_col().child(row);
        if let Some(menu) = ui.menu {
            let options = match menu {
                TransactionMenu::Savepoints => self.transaction_savepoint_menu(&snapshot, actions, busy, &p, cx),
                TransactionMenu::Isolation => self.transaction_setting_menu(true, &snapshot, &capabilities, busy, &p, cx),
                TransactionMenu::Mode => self.transaction_setting_menu(false, &snapshot, &capabilities, busy, &p, cx),
            };
            column = column.child(options);
        }
        Some(column.into_any_element())
    }

    fn transaction_savepoint_menu(
        &self,
        snapshot: &crate::transaction::TransactionSnapshot,
        actions: TransactionActions,
        busy: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let locale = self.model.locale();
        let mut row = div()
            .id("transaction-savepoint-menu")
            .test_support()
            .flex_none()
            .px(px(Space::LG))
            .py(px(Space::XS))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(Space::SM))
            .bg(p.surface)
            .border_b_1()
            .border_color(p.line);
        if actions.savepoints {
            let name = transaction::suggested_savepoint(snapshot);
            let id = transaction::TransactionCommand::savepoint_create_id(&name);
            let label = i18n::fill(&i18n::text(locale, "transaction.savepoint.createNamed"), &[("name", &name)]);
            row = row.child(kit::secondary_button(id.clone(), Some(IconName::BookmarkPlus), SharedString::from(label), act(id, cx)).disabled(busy));
        } else if actions.savepoint_recovery {
            row = row.child(kit::muted(i18n::text(locale, "transaction.savepoint.recoveryOnly"), p));
        }
        if snapshot.savepoints.is_empty() {
            row = row.child(kit::muted(i18n::text(locale, "transaction.savepoint.empty"), p));
        } else {
            for name in &snapshot.savepoints {
                let rollback = transaction::TransactionCommand::savepoint_rollback_id(name.as_str());
                row = row.child(
                    kit::tool_button(rollback.clone(), Some(IconName::CornerDownLeft), SharedString::from(name.as_str()), act(rollback, cx))
                        .tooltip(i18n::text(locale, "transaction.savepoint.rollback"))
                        .disabled(busy),
                );
            }
        }
        row.into_any_element()
    }

    fn transaction_setting_menu(
        &self,
        isolation: bool,
        snapshot: &crate::transaction::TransactionSnapshot,
        capabilities: &TransactionCapabilities,
        busy: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let locale = self.model.locale();
        let support = if isolation { capabilities.isolation } else { capabilities.mode };
        let available = support.change != crate::transaction::SettingChange::Unsupported;
        let current = if isolation { snapshot.settings.isolation.as_deref() } else { snapshot.settings.mode.as_deref() };
        let locked = support.change == crate::transaction::SettingChange::WhileIdle
            && snapshot.state != crate::transaction::TransactionState::Idle;
        let mut row = div()
            .id("transaction-setting-menu")
            .test_support()
            .flex_none()
            .px(px(Space::LG))
            .py(px(Space::XS))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(Space::SM))
            .bg(p.surface)
            .border_b_1()
            .border_color(p.line);
        let mut choices: Vec<(Option<&str>, String, bool)> = vec![(None, "transaction.menu.default".to_owned(), current.is_none())];
        for item in support.options {
            let key = if isolation { transaction::isolation_label_key(item.id) } else { transaction::mode_label_key(item.id) };
            choices.push((Some(item.id), key, current == Some(item.id)));
        }
        for (value, key, selected) in choices {
            let id = if isolation { transaction::TransactionCommand::isolation_id(value) } else { transaction::TransactionCommand::mode_id(value) };
            let label = SharedString::from(i18n::text(locale, &key));
            let button = kit::tool_button(id.clone(), None, label, act(id, cx)).disabled(busy || locked || !available);
            row = row.child(if selected { button.selected(true) } else { button });
        }
        if locked {
            row = row.child(kit::muted(i18n::text(locale, "transaction.settings.locked"), p));
        }
        row.into_any_element()
    }

    /// 断开/退出/安装更新前的事务保护层：逐连接提供取消、提交后继续、回滚后继续。
    #[inline(never)]
    fn transaction_guard_layer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let Some(guard) = self.model.transaction_guard.clone() else { return div().into_any_element() };
        let name = if guard.current.connection_name.trim().is_empty() { guard.current.connection_id.clone() } else { guard.current.connection_name.clone() };
        let body = i18n::fill(&self.model.text("transaction.guard.body"), &[("name", &name)]);
        let state = self.model.text(transaction::state_key(guard.current.state));
        let remaining = guard.remaining();
        let detail = if remaining > 0 {
            format!("{body}\n{}\n{}", state, i18n::fill(&self.model.text("transaction.guard.multiple"), &[("count", &remaining.to_string())]))
        } else {
            format!("{body}\n{state}")
        };
        let cancel = act(transaction::GUARD_CANCEL, cx);
        let commit = act(transaction::GUARD_COMMIT, cx);
        let rollback = act(transaction::GUARD_ROLLBACK, cx);
        let mut buttons = div().flex().justify_end().gap(px(Space::MD)).child(kit::secondary_button(transaction::GUARD_CANCEL, None, self.t("transaction.guard.cancel"), cancel));
        if guard.allows(GuardDecision::Rollback) {
            buttons = buttons.child(
                kit::danger_button(transaction::GUARD_ROLLBACK, Some(IconName::RotateCcw), self.t("transaction.guard.rollback"), rollback)
                    .disabled(guard.busy),
            );
        }
        if guard.allows(GuardDecision::Commit) {
            buttons = buttons.child(
                kit::primary_button(transaction::GUARD_COMMIT, Some(IconName::Check), self.t("transaction.guard.commit"), commit)
                    .disabled(guard.busy),
            );
        }
        let mut dialog = dialog_card(
            "transaction-guard",
            IconName::TriangleAlert,
            p.warn,
            self.t(guard.cause.title_key()),
            SharedString::from(detail),
            &p,
        )
        .child(buttons);
        if let Some(error) = &guard.error {
            dialog = dialog.child(banner(IconName::CircleX, SharedString::from(error.clone()), p.danger, p.danger_soft));
        }
        if guard.busy {
            dialog = dialog.child(self.t("transaction.busy"));
        }
        backdrop("transaction-guard-backdrop", 0.32, cx)
            .flex()
            .items_center()
            .justify_center()
            .px(px(Space::XL + Space::XS))
            .child(div().id("transaction-guard-dialog").test_support().w_full().flex().justify_center().child(dialog))
            .into_any_element()
    }

    fn workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let page = self.model.shell.page;
        let tool = self.model.tool;
        let table_view = page == AppPage::Workspace && tool == WorkTool::Table;
        let redis_view = page == AppPage::Workspace && tool == WorkTool::Redis;
        let mongo_view = page == AppPage::Workspace && tool == WorkTool::Mongo;
        let overview_view = page == AppPage::Workspace && tool == WorkTool::Overview;
        let tool_view = table_view || redis_view || mongo_view || overview_view || matches!(page, AppPage::Structure | AppPage::Compare | AppPage::Diagram);
        let welcome = self.model.show_welcome() && !tool_view;
        // 结构页的字段表按内容区真实宽度排版。这个宽度只有布局知道（视口减活动栏、
        // 停靠侧栏和根内边距），不能由面板拿视口宽冒充：900 开着侧栏只剩 614。
        if page == AppPage::Structure {
            let content_width = self.model.layout().content_width as f32;
            self.structure.update(cx, |pane, cx| {
                if pane.set_content_width(content_width) {
                    cx.notify();
                }
            });
        }
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
        } else if overview_view {
            self.overview_page(window, cx)
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
                        // 表格优先（表格在结果区里面，两种请求会同时记下），其次是结果区，最后才是编辑器。
                        let (grid, results) = this.query.update(cx, |pane, _| (pane.take_grid_menu_request(), pane.take_results_menu_request()));
                        let menu = if grid {
                            "menu-grid"
                        } else if results {
                            "menu-results"
                        } else {
                            "menu-editor"
                        };
                        this.perform(menu, cx);
                    });
                })
                .child(self.query.clone())
                .into_any_element()
        };
        // 页签条是缓存视图（见 `TabStripHost`）：光标闪烁等只标脏工作台的重绘不再重排几百个页签。
        let tabs_h = self.metrics().tabs_h;
        let strip = (!welcome || !self.model.tabs.is_empty()).then(|| {
            gpui_kit::AnyView::from(self.tab_strip_host.clone())
                .cached(gpui_kit::StyleRefinement::default().flex_none().w_full().h(px(tabs_h)))
        });
        let _ = window;
        let transaction = self.transaction_bar(cx);
        div()
            .flex()
            .flex_col()
            .size_full()
            .children(strip)
            .children(transaction)
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .into_any_element()
    }

    /// 就地重命名中的查询标签：一个输入框，回车确认、Esc 取消；切到别的标签时按当前输入确认。
    fn tab_rename_chip(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let m = self.metrics();
        let (input, state) = self.field_input(DraftField::TabTitle, &self.model.text("shell.tabs.rename"), false, window, cx);
        if !self.tab_rename_focused {
            self.tab_rename_focused = true;
            self.fields.focus(DraftField::TabTitle, window, cx);
            state.update(cx, |state, cx| state.select_all(window, cx));
            // 失焦（点到编辑器、别的面板或窗口外）按当前输入确认，与切到别的标签一致；
            // 回车、Esc 已经结束重命名时这里什么都不做。
            let handle = state.read(cx).focus_handle(cx);
            self.tab_rename_blur = Some(cx.on_blur(&handle, window, |this, _, cx| {
                this.tab_rename_blur = None;
                if this.model.tab_rename.is_some() {
                    this.perform("tab-rename-confirm", cx);
                }
            }));
        }
        div()
            .id("tab-rename")
            .test_support()
            .flex_none()
            .w(px(ControlSize::PICKER_MIN_W - 60.))
            .h(px(m.tabs_h))
            .px(px(Space::XS))
            .flex()
            .items_center()
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                let command = match event.keystroke.key.as_str() {
                    "enter" => "tab-rename-confirm",
                    "escape" => "tab-rename-cancel",
                    _ => return,
                };
                cx.stop_propagation();
                this.perform(command, cx);
            }))
            .child(input)
            .into_any_element()
    }

    pub(super) fn tab_strip(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(test)]
        self.strip_count.set(self.strip_count.get() + 1);
        let p = Palette::of(cx);
        let m = self.metrics();
        // 页签列表独立滚动，新建入口固定在右侧；当前标签可通过横向滚动或 Ctrl+Tab 到达，
        // 并且切过去之后列表自己把当前项滚进视口，不用手动横向拖。
        // 新建按钮紧跟最后一个标签（浏览器式），标签多到放不下时列表自己横向滚动。
        let mut items: Vec<AnyElement> = Vec::new();
        let work_page = matches!(self.model.shell.page, AppPage::Workspace | AppPage::Structure);
        // 当前页签或页签数量一变就挂一个“把它滚进视口”的请求；请求一直挂到确认可见为止，
        // 因为页签位置要下一帧才量得出来（首帧、刚新建时都还没有几何）。确认可见后就不再动，
        // 用户手动横向滚开也不会被每帧拉回。
        if work_page {
            if let Some(active) = self.model.active_tab.clone() {
                let index = self.model.tabs.iter().position(|tab| tab.id == active);
                let key = Some((active, self.model.tabs.len()));
                if self.tab_scroll_seen != key {
                    self.tab_scroll_seen = key;
                    self.tab_scroll_pending = index.map(tabs::Reveal::new);
                }
            } else {
                self.tab_scroll_seen = None;
                self.tab_scroll_pending = None;
            }
        }
        // 确认走共享的 reveal 协议：可见就结束，确认不了（零宽视口、列表还没挂载）
        // 最多再请求 `REVEAL_MAX_FRAMES` 帧就放弃，不会每帧都要一帧重绘。
        let visible = self
            .tab_scroll_pending
            .is_some_and(|reveal| tabs::tab_visible(&self.tab_scroll, reveal.index));
        let reveal = tabs::reveal_step(&mut self.tab_scroll_pending, visible);
        if reveal.is_some() {
            // 这一帧才开始滚，页签位置要等画完才量得出来：再要一帧确认它真的进来了。
            // 页签条是缓存视图，在 prepaint 阶段才构建；这时的通知不产生效果，
            // 所以延到这一轮结束再通知页签条自己，下一帧它才会重建并确认。
            let host = self.tab_strip_host.downgrade();
            cx.defer(move |cx| {
                let _ = host.update(cx, |_, cx| cx.notify());
            });
        }
        // 未保存标记的文字版本进 tooltip：状态不只靠一个颜色点表达（不改 i18n 目录）。
        let dirty_text = self.t("ui.grid.pending");
        let pinned_text = self.t("shell.tabs.pinned");
        let renaming = self.model.tab_rename.as_ref().map(|(id, _)| id.clone());
        if renaming.is_none() {
            self.tab_rename_focused = false;
            self.tab_rename_blur = None;
        }
        for tab in self.model.tabs.clone() {
            if renaming.as_deref() == Some(tab.id.as_str()) {
                items.push(self.tab_rename_chip(window, cx));
                continue;
            }
            // 只有工作页才把标签显示为选中；设置、对比、关系图页不伪装成某个工作项。
            let active = work_page && self.model.active_tab.as_deref() == Some(tab.id.as_str());
            let dirty = tab.uncommitted_cells;
            let title = SharedString::from(tab.title.clone());
            let (is_prod, context_hint) = tab
                .connection_id
                .as_deref()
                .map(|conn_id| {
                    let row = self.model.connections.rows.iter().find(|r| r.saved.id == conn_id);
                    let env = row.map(|r| r.saved.env.as_str()).unwrap_or("");
                    let prod = super::danger::env_is_prod(env, Some(&self.model.env_tags));
                    let conn_name = tab
                        .connection_name
                        .clone()
                        .or_else(|| row.map(|r| r.saved.name.clone()))
                        .unwrap_or_default();
                    let db = tab.database.clone().unwrap_or_default();
                    let mut ctx = if db.is_empty() {
                        conn_name
                    } else if conn_name.is_empty() {
                        db
                    } else {
                        format!("{conn_name} / {db}")
                    };
                    if env_shown(env) {
                        let env_txt = self.env_text(env);
                        if !ctx.is_empty() {
                            ctx = format!("{ctx} [{env_txt}]");
                        }
                    }
                    (prod, (!ctx.is_empty()).then(|| SharedString::from(ctx)))
                })
                .unwrap_or((false, None));
            let id = tab.id.clone();
            let close_id = tab.id.clone();
            let menu_id = tab.id.clone();
            let middle_id = tab.id.clone();
            let preview = self.model.is_preview_tab(&tab.id);
            let close_label = self.t("common.close");
            // 运行中的转圈是独立小实体：动画帧不通知工作台，不触发工作台的整理。
            let spinner = tab.running.then(|| self.spinners.view(format!("tab-{id}"), Size::XSmall, Some(p.accent), cx));
            let chip = tabs::tab(
                tabs::TabSpec::new(SharedString::from(format!("tab-{id}")), item_icon(tab.kind), title)
                    .active(active)
                    .running(tab.running)
                    .spinner(spinner)
                    .pinned(tab.pinned.then(|| pinned_text.clone()))
                    .prod(is_prod)
                    .context(context_hint)
                    .preview(preview)
                    .dirty(dirty, Some(dirty_text.clone())),
                &p,
            )
            .child(tabs::close_action(
                SharedString::from(format!("tab-close-{id}")),
                close_label,
                active,
                &p,
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.model.request_close_tab(&close_id);
                    this.persist_if_needed(cx);
                    cx.notify();
                }),
            ))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                // 右键先真正激活该工作项，菜单动作与随后的视图目标才指向同一项。
                if this.model.tab_menu_for(&menu_id) {
                    this.menu_at = event.position;
                    this.persist_if_needed(cx);
                }
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Middle, cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                this.model.request_close_tab(&middle_id);
                this.persist_if_needed(cx);
                cx.notify();
            }))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                let _ = this.model.activate_item(&id);
                // 双击预览标签把它转为固定标签；双击查询标签就地重命名（R14，不触发任何破坏性操作）。
                if event.click_count() >= 2 {
                    this.model.pin_overview();
                    if this.model.active_tab().is_some_and(|tab| tab.id == id && tab.kind.is_query()) {
                        this.perform("tab-rename", cx);
                        return;
                    }
                }
                if this.model.tool == WorkTool::Query {
                    this.query.update(cx, |pane, _| pane.request_focus());
                }
                this.persist_if_needed(cx);
                cx.notify();
            }));
            items.push(chip.into_any_element());
        }
        // 新建入口与页签同一垂直中心、命中范围不小于控件档，不再用被压小的图标按钮。
        let new_action = kit::icon_action(
            "tab-new",
            IconName::Plus,
            self.t("shell.command.newQuery"),
            kit::HitArea::CONTROL,
            p.text_muted,
            &p,
            false,
            false,
            act("new-query", cx),
        )
        .into_any_element();
        // 标签列表：标签多到放不下时也一直在右侧，可按标题、连接、库搜索后切过去。
        let anchor = self.tab_picker_anchor.clone();
        let list_action = div()
            .relative()
            .flex_none()
            .child(
                kit::icon_action(
                    "tab-picker",
                    IconName::ChevronDown,
                    self.t("shell.tabs.list"),
                    kit::HitArea::CONTROL,
                    p.text_muted,
                    &p,
                    self.model.tabs.is_empty(),
                    self.tab_picker.is_some(),
                    cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_tab_picker(window, cx)),
                ),
            )
            .child(
                gpui_kit::canvas(move |area, _, _| anchor.set(Some(area)), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
            .into_any_element();
        let strip = tabs::tab_strip_with_double_click(
            "tabs",
            self.t("shell.tabs.tablist"),
            "tab-list",
            items,
            vec![list_action, new_action],
            &self.tab_scroll,
            reveal,
            &m,
            &p,
            Some(kit::click(act("new-query", cx))),
        );
        match self.render_tab_picker(window, cx) {
            Some(overlay) => div().size_full().child(strip).child(overlay).into_any_element(),
            None => strip,
        }
    }

    /// 标签列表里的条目：标题；说明是连接 / 库，也参与搜索。固定标签在说明里注明。
    pub(super) fn tab_picker_items(&self) -> Vec<crate::ui::picker::PickerItem> {
        let pinned = self.t("shell.tabs.pinned");
        self.model
            .tabs
            .iter()
            .map(|tab| {
                let mut detail: Vec<String> = Vec::new();
                if let Some(name) = tab.connection_name.as_ref().filter(|name| !name.is_empty()) {
                    detail.push(name.clone());
                }
                if let Some(db) = tab.database.as_ref().filter(|db| !db.is_empty()) {
                    detail.push(db.clone());
                }
                let mut text = detail.join(" / ");
                if tab.pinned {
                    text = if text.is_empty() { pinned.to_string() } else { format!("{text} · {pinned}") };
                }
                let item = crate::ui::picker::PickerItem::new(tab.id.clone(), tab.title.clone());
                if text.is_empty() { item } else { item.detail(text) }
            })
            .collect()
    }

    /// 打开或关闭标签列表；打开时搜索框拿到键盘焦点，当前标签预先高亮。
    pub(super) fn toggle_tab_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_picker.is_some() {
            self.close_tab_picker(cx);
            return;
        }
        let items = self.tab_picker_items();
        if items.is_empty() {
            return;
        }
        let chosen: Vec<SharedString> = self.model.active_tab.clone().map(SharedString::from).into_iter().collect();
        let locale = self.model.locale();
        let picker = cx.new(|cx| crate::ui::picker::Picker::new(items, crate::ui::picker::PickerMode::Single, chosen, locale, window, cx));
        let subscription = cx.subscribe_in(&picker, window, |this, _, event: &crate::ui::picker::PickerEvent, _, cx| match event {
            crate::ui::picker::PickerEvent::Chosen(id) => {
                let id = id.to_string();
                this.close_tab_picker(cx);
                this.pick_tab(&id, cx);
            }
            crate::ui::picker::PickerEvent::Dismissed => this.close_tab_picker(cx),
            crate::ui::picker::PickerEvent::Toggled(_) => {}
        });
        picker.update(cx, |picker, cx| picker.focus_search(window, cx));
        self.tab_picker = Some((picker, subscription));
        cx.notify();
    }

    pub(super) fn close_tab_picker(&mut self, cx: &mut Context<Self>) {
        if self.tab_picker.take().is_some() {
            cx.notify();
        }
    }

    /// 标签列表里选中一项：切到这个标签，查询标签把焦点交回编辑器。
    pub(super) fn pick_tab(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.model.activate_item(id) {
            return;
        }
        if self.model.tool == WorkTool::Query {
            self.query.update(cx, |pane, _| pane.request_focus());
        }
        self.persist_if_needed(cx);
        cx.notify();
    }

    fn render_tab_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.tab_picker.as_ref()?.0.clone();
        let anchor = self.tab_picker_anchor.get().unwrap_or_default();
        let viewport = window.viewport_size();
        // 遮罩铺满窗口、画在弹层下面：点弹层以外的地方只关闭它，不会顺手点到页签。
        let backdrop = gpui_kit::deferred(
            gpui_kit::anchored().position(gpui_kit::Point::default()).child(
                div()
                    .id("tab-picker-backdrop")
                    .w(viewport.width)
                    .h(viewport.height)
                    .occlude()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, _, cx| this.close_tab_picker(cx)))
                    .on_mouse_down(MouseButton::Right, cx.listener(|this, _: &MouseDownEvent, _, cx| this.close_tab_picker(cx))),
            ),
        )
        .with_priority(2);
        let popover = crate::ui::picker::picker_popover(&picker, anchor, window, cx);
        Some(div().child(backdrop).child(popover).into_any_element())
    }

    // ------------------------------------------------------------ 状态栏

    fn status_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        // 目标名可能很长；只压缩这一段，状态提示与语言开关保持在位。
        let target_max = (self.model.viewport_width as f32 * 0.42).max(120.);
        // 目标：活动工作项自己的连接；没有工作项时（欢迎页、历史、设置）落到对象树里选中的连接，
        // 免得树上明明选着一条已连接的库，状态栏却说“未选择连接”。
        let tab_target = self.model.active_tab().filter(|tab| tab.connection_id.is_some()).map(|tab| {
            (
                tab.connection_id.clone().unwrap_or_default(),
                tab.engine.clone().unwrap_or_default(),
                tab.connection_name.clone().unwrap_or_default(),
                tab.database.clone().unwrap_or_default(),
            )
        });
        let tree_target = || {
            self.model.nav.focus_node().filter(|node| !node.connection_id.is_empty()).map(|node| {
                (node.connection_id.clone(), node.engine.as_str().to_owned(), node.connection_name.clone(), node.database.clone())
            })
        };
        let status_target = if self.model.active_tab().is_some() { tab_target } else { tab_target.or_else(tree_target) };
        let target: AnyElement = match status_target {
            Some((connection_id, engine, name, database)) => {
                let env = self
                    .model
                    .connections
                    .rows
                    .iter()
                    .find(|row| row.saved.id == connection_id)
                    .map(|row| row.saved.env.clone())
                    .unwrap_or_default();
                // 绑定的连接已被删除：照样显示原名，但用警示色说明它已不存在，而不是看起来一切就绪。
                let missing = self.model.connections.loaded && !self.model.connections.rows.iter().any(|row| row.saved.id == connection_id);
                let label = SharedString::from(if database.is_empty() { name } else { format!("{name} / {database}") });
                kit::interactive_states(div().id("status-target"), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(if missing { SharedString::from(format!("{label} · {}", self.t("ui.query.targetMissing"))) } else { label.clone() })
                    .tooltip_text(if missing { self.t("ui.query.targetMissing") } else { self.t("ui.status.switchTargetHint") })
                    .when(missing, |this| this.text_color(p.warn))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w_0()
                    .max_w(px(target_max))
                    .rounded(px(Radius::CONTROL))
                    .px(px(4.))
                    .hover(|style| style.bg(p.hover).text_color(p.text))
                    .child(kit::engine_badge(&engine, 14.))
                    .child(div().min_w_0().truncate().child(label))
                    .when(missing, |this| this.child(kit::icon(IconName::TriangleAlert, 12., p.warn)))
                    .children(self.env_pill(&env, &p))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.perform("status-target", cx);
                    this.restore_focus(window, cx);
                    this.open_query_target_picker(cx);
                }))
                    .into_any_element()
            }
            _ => kit::interactive_states(div().id("status-target"), &p, false)
                .test_support()
                .role(gpui_kit::Role::Button)
                .aria_label(self.t("ui.status.noTarget"))
                .tooltip_text(self.t("ui.status.switchTargetHint"))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(Radius::CONTROL))
                .px(px(4.))
                .hover(|style| style.bg(p.hover).text_color(p.text))
                .child(kit::status_dot(p.line_strong))
                .child(self.t("ui.status.noTarget"))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.perform("status-target", cx);
                    this.restore_focus(window, cx);
                }))
                .into_any_element(),
        };
        // 提示有时效：换了一句就重新计时；超过 NOTICE_TTL 不再显示，状态栏回到“就绪”。
        let seen = (self.model.notice_key, self.model.notice_detail.clone());
        if seen != self.notice_seen {
            self.notice_seen = seen;
            self.notice_since = std::time::Instant::now();
            if self.model.notice_key.is_some() && !cfg!(test) {
                self.notice_timer = Some(cx.spawn(async move |view, cx| {
                    cx.background_executor().timer(NOTICE_TTL + std::time::Duration::from_millis(50)).await;
                    let _ = view.update(cx, |_, cx| cx.notify());
                }));
            }
        }
        let expired = !cfg!(test) && self.notice_since.elapsed() > NOTICE_TTL;
        let notice_key = if expired { None } else { self.model.notice_key };
        let notice = match notice_key {
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
            .child(div().flex_1().min_w_0().truncate().child(SharedString::from(notice)))
            .when(running, |this| {
                this.child(
                    kit::interactive_states(div().id("status-jobs"), &p, false)
                        .test_support()
                        .role(gpui_kit::Role::Button)
                        .aria_label(self.t("query.status.running"))
                        .tooltip_text(self.t("ui.status.jobsHint"))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .rounded(px(Radius::CONTROL))
                        .px(px(4.))
                        .hover(|style| style.bg(p.hover).text_color(p.text))
                        .child(self.spinners.element("status-running", Size::XSmall, Some(p.accent), cx))
                        .child(self.t("query.status.running"))
                        .on_click(act("status-jobs", cx)),
                )
            })
            // 有新版本、正在下载或待重启：状态栏给一个常驻入口，点开设置里的更新卡片。
            .when(self.model.update.announces(), |this| {
                use super::update::UpdateStage;
                let version = self.model.update.offer.as_ref().map(|offer| format!("v{}", offer.version)).unwrap_or_default();
                let label = match self.model.update.stage {
                    UpdateStage::Ready => self.model.text("update.chipReady"),
                    UpdateStage::Downloading { .. } | UpdateStage::Installing => match self.model.update.percent() {
                        Some(percent) => format!("{} {percent}%", self.model.text("update.chipDownloading")),
                        None => self.model.text("update.chipDownloading"),
                    },
                    _ => i18n::fill(&self.model.text("update.chipAvailable"), &[("version", &version)]),
                };
                this.child(
                    kit::interactive_states(div().id("status-update"), &p, false)
                        .test_support()
                        .role(gpui_kit::Role::Button)
                        .aria_label(SharedString::from(label.clone()))
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .rounded(px(Radius::CONTROL))
                        .px(px(6.))
                        .bg(p.accent_soft)
                        .text_color(p.accent_text)
                        .font_weight(FontWeight::MEDIUM)
                        .hover(|style| style.bg(p.active))
                        .child(kit::icon(IconName::Download, 12., p.accent_text))
                        .child(SharedString::from(label))
                        .on_click(act("update-show", cx)),
                )
            })
            // 设置写盘失败是全局事实：不只在设置页可见，状态栏常驻入口把它带回设置页。
            .when(self.model.settings_save.failed(), |this| {
                this.child(
                    kit::interactive_states(div().id("status-settings-save"), &p, false)
                        .test_support()
                        .role(gpui_kit::Role::Button)
                        .aria_label(self.t("ui.settings.saveFailed"))
                        .tooltip_text(self.t("ui.settings.saveFailedHint"))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .rounded(px(Radius::CONTROL))
                        .px(px(6.))
                        .text_color(p.danger)
                        .hover(|style| style.bg(p.danger_soft))
                        .child(kit::icon(IconName::CircleAlert, 12., p.danger))
                        .child(self.t("ui.settings.saveFailed"))
                        .on_click(act("settings", cx)),
                )
            })
            .when(self.model.tabs.len() > 1 && self.model.viewport_width >= 760, |this| {
                let total = self.model.tabs.len();
                let current = self
                    .model
                    .active_tab
                    .as_deref()
                    .and_then(|id| self.model.tabs.iter().position(|tab| tab.id == id))
                    .map(|idx| idx + 1)
                    .unwrap_or(1);
                this.child(
                    div()
                        .id("status-tab-count")
                        .flex_none()
                        .text_size(px(TypeScale::XS))
                        .text_color(p.text_faint)
                        .child(SharedString::from(format!("{current} / {total}"))),
                )
            })
            .child(
                kit::interactive_states(div().id("status-help"), &p, false)
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(self.t("shell.help.title"))
                    .tooltip_text(self.t("ui.status.shortcutsHint"))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(20.))
                    .rounded(px(Radius::CONTROL))
                    .hover(|style| style.bg(p.hover).text_color(p.text))
                    .child(kit::icon(IconName::Keyboard, 12., p.text_faint))
                    .on_click(act("help", cx)),
            )

            .into_any_element()
    }

    // ------------------------------------------------------------ 欢迎页

    fn welcome_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let content_w = self.model.layout().content_width as f32;
        let compact_cards = content_w < 720.;
        let recent_runs: Vec<_> = self.model.welcome.recent(&self.model.history.runs).to_vec();
        let quick_connections: Vec<_> = {
            let recent = self.model.connections.launcher_recent(6);
            if recent.is_empty() {
                self.model.connections.rows.iter().take(6).cloned().collect()
            } else {
                recent.into_iter().cloned().collect()
            }
        };
        let _ = window;

        let quick_card = |id: &'static str, icon: IconName, heading: SharedString, desc: SharedString, chord: Option<&'static str>, command: &'static str, cx: &mut Context<Self>| {
            kit::interactive_states(div().id(id), &p, false)
                .test_support()
                .role(gpui_kit::Role::Button)
                .aria_label(heading.clone())
                .flex_1()
                .min_w(px(190.))
                .p(px(m.card_pad() + 2.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .rounded(px(Radius::PANEL))
                .border_1()
                .border_color(p.line)
                .bg(if p.dark { p.raised } else { p.surface })
                .hover(|style| style.border_color(p.accent).bg(p.hover))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            div()
                                .size(px(32.))
                                .rounded(px(Radius::CONTROL))
                                .bg(p.accent_soft)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(kit::icon(icon, 16., p.accent_text)),
                        )
                        .when_some(chord, |this, chord| this.child(kit::kbd(kit::shortcut(chord), &p))),
                )
                .child(div().text_size(px(TypeScale::MD)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(heading))
                .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(desc))
                .on_click(act(command, cx))
                .into_any_element()
        };

        let cards_row = div()
            .flex()
            .when(compact_cards, |this| this.flex_col())
            .when(!compact_cards, |this| this.flex_row().flex_wrap())
            .gap(px(12.))
            .child(quick_card(
                "welcome-card-new-conn",
                IconName::Plug,
                self.t("common.newConnection"),
                self.t("ui.welcome.newConnDesc"),
                None,
                "conn-new-sidebar",
                cx,
            ))
            .child(quick_card(
                "welcome-card-query",
                IconName::SquareTerminal,
                self.t("shell.command.newQuery"),
                self.t("ui.welcome.newQueryDesc"),
                Some("Ctrl+T"),
                "new-query",
                cx,
            ))
            .child(quick_card(
                "welcome-card-open-file",
                IconName::FolderOpen,
                self.t("shell.command.openFile"),
                self.t("ui.welcome.openFileDesc"),
                crate::app::keymap::command_chord("open-file"),
                "open-file",
                cx,
            ));

        let retry_banner = self.model.connect_retry.clone().map(|retry| {
            let saved = self.model.connections.rows.iter().find(|row| row.saved.id == retry.connection_id).map(|row| row.saved.name.clone());
            // 连接已经删掉时不再显示一串 id，也不给没有意义的“重试”，只留关闭。
            let exists = saved.is_some();
            let name = saved.unwrap_or_else(|| self.model.text("connections.retry.missing"));
            let reason = self.model.connections.failures.get(&retry.connection_id).cloned().unwrap_or_default();
            let head = reason.lines().next().unwrap_or_default().to_owned();
            let label = if head.is_empty() {
                format!("{} · {name}", self.model.text("connections.status.error"))
            } else {
                format!("{} · {name}: {head}", self.model.text("connections.status.error"))
            };
            kit::banner(
                kit::BannerTone::Danger,
                IconName::CircleAlert,
                SharedString::from(label),
                Some(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .when(exists, |this| {
                            this.child(kit::secondary_button("welcome-retry-connect", Some(IconName::RefreshCw), self.t("common.retry"), act("conn-retry", cx)).xsmall())
                        })
                        .child(kit::icon_button("welcome-retry-dismiss", IconName::X, self.t("common.close"), act("conn-retry-dismiss", cx)).xsmall())
                        .into_any_element(),
                ),
                &p,
            )
            .id("welcome-connect-retry")
            .into_any_element()
        });

        let connections_section = (!quick_connections.is_empty()).then(|| {
            let mut conn_rows = div().flex().flex_col().gap(px(2.));
            for row in &quick_connections {
                let id = row.saved.id.clone();
                let open_id = id.clone();
                let menu_id = id.clone();
                let status = self.model.connections.row_status(&id);
                let (dot, state_key) = status_parts(status, &p);
                let detail = match row.saved.file_path.as_deref() {
                    Some(path) if !path.is_empty() => short_path(path),
                    _ => connection_detail(row),
                };
                conn_rows = conn_rows.child(
                    list_row(SharedString::from(format!("welcome-conn-{id}")), &p, m.row_pad_y)
                        .test_support()
                        .tooltip_text(SharedString::from(format!("{} · {}", row.saved.name, self.model.text(state_key))))
                        .child(kit::engine_badge(row.saved.engine.as_str(), 22.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(1.))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .child(div().truncate().text_size(px(TypeScale::MD)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(SharedString::from(row.saved.name.clone())))
                                        .children(self.env_pill(&row.saved.env, &p)),
                                )
                                .when(!detail.is_empty(), |this| {
                                    this.child(div().truncate().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(detail)))
                                }),
                        )
                        .child(kit::status_dot(dot))
                        // 右键与连接管理页同一份菜单（连接/断开、新建查询、编辑、复制、测试、移动分组、删除）。
                        .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.menu_at = event.position;
                            this.model.connections.selected = Some(menu_id.clone());
                            this.perform("menu-connection", cx);
                        }))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.model.connections.selected = Some(open_id.clone());
                            if this.model.connections.row_status(&open_id) == LinkStatus::Connected {
                                this.perform("conn-new-query", cx);
                            } else {
                                this.model.expand_nav_connection(&open_id);
                                this.model.queue_connect(super::conn_modal::ConnectIntent::new(
                                    open_id.clone(),
                                    super::conn_modal::ConnectAction::NewQuery,
                                    None,
                                ));
                                cx.notify();
                            }
                        })),
                );
            }
            kit::card(&p)
                .flex_1()
                .min_w(px(260.))
                .p(px(m.card_pad()))
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(kit::eyebrow(self.t("ui.welcome.myConnections"), &p))
                        .child(
                            kit::tool_button("welcome-manage-conns", Some(IconName::ArrowRight), self.t("ui.welcome.manageConnections"), act("connections", cx))
                                .xsmall(),
                        ),
                )
                .child(conn_rows)
                .into_any_element()
        });

        let history_section = (!recent_runs.is_empty()).then(|| {
            let mut history_rows = div().flex().flex_col().gap(px(2.));
            for entry in recent_runs.iter() {
                let id = entry.id.clone();
                let menu_id = id.clone();
                history_rows = history_rows.child(
                    list_row(SharedString::from(format!("welcome-run-{id}")), &p, m.row_pad_y)
                        .test_support()
                        .child(kit::icon(IconName::Clock, 14., p.text_faint))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(px(TypeScale::SM))
                                .child(SharedString::from(one_line(&entry.sql))),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("welcome-run-client-{id}")))
                                .flex_none()
                                .max_w(px(140.))
                                .truncate()
                                .tooltip_text(SharedString::from(entry.connection_name.clone()))
                                .text_size(px(TypeScale::XS))
                                .text_color(p.text_faint)
                                .child(SharedString::from(entry.connection_name.clone())),
                        )
                        .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.menu_at = event.position;
                            if this.model.open_welcome_run_menu(&menu_id) {
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.model.history.selected_run = Some(id.clone());
                            this.model.history.section = HistorySection::Runs;
                            this.perform("history-load", cx);
                        })),
                );
            }
            kit::card(&p)
                .flex_1()
                .min_w(px(260.))
                .p(px(m.card_pad()))
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(kit::eyebrow(self.t("shell.welcome.recent"), &p))
                        .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(recent_runs.len().to_string()))),
                )
                .child(div().id("welcome-recent-runs").max_h(px(240.)).scroll_area_y().child(history_rows))
                .into_any_element()
        });

        let has_panels = connections_section.is_some() || history_section.is_some();

        div()
            .id("welcome-page")
            .role(gpui_kit::Role::Group)
            .aria_label(self.t("shell.welcome.title"))
            .test_support()
            .size_full()
            .scroll_area_y()
            .flex()
            .flex_col()
            .items_center()
            .px(px(m.page_pad))
            .py(px(m.page_pad_y + 10.))
            .child(
                div()
                    .w_full()
                    .max_w(px(840.))
                    .flex()
                    .flex_col()
                    .gap(px(m.section_gap() + 4.))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(14.))
                            .child(brand_mark(40., &p))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(160.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(kit::title("RillSQL", TypeScale::LG, &p))
                                    .child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(self.t("ui.welcome.subtitle"))),
                            )
                            .child(
                                kit::interactive_states(div().id("welcome-history"), &p, false)
                                    .test_support()
                                    .role(gpui_kit::Role::Button)
                                    .aria_label(self.t("ui.welcome.allHistory"))
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .h(px(m.control_h))
                                    .px(px(10.))
                                    .rounded(px(Radius::CONTROL))
                                    .border_1()
                                    .border_color(p.line)
                                    .bg(if p.dark { p.raised } else { p.surface })
                                    .hover(|style| style.bg(p.hover).border_color(p.line_strong))
                                    .text_size(px(TypeScale::SM))
                                    .text_color(p.text_muted)
                                    .child(kit::icon(IconName::Clock, 13., p.text_faint))
                                    .child(self.t("ui.welcome.allHistory"))
                                    .on_click(act("history", cx)),
                            ),
                    )
                    .children(retry_banner)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(kit::eyebrow(self.t("ui.welcome.quickStart"), &p))
                            .child(cards_row),
                    )
                    .when(has_panels, |this| {
                        this.child(
                            div()
                                .flex()
                                .when(compact_cards, |row| row.flex_col())
                                .when(!compact_cards, |row| row.flex_row().flex_wrap().items_start())
                                .gap(px(12.))
                                .children(connections_section)
                                .children(history_section),
                        )
                    })
                    .child(
                        div()
                            .pt(px(4.))
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text_faint)
                            .child(self.t("ui.welcome.enginesHint")),
                    ),
            )
            .into_any_element()
    }


    // ------------------------------------------------------------ 启动台

    /// 连接页与欢迎区共用的启动台：最近连接、全部连接、紧凑行与全部入口。
    /// 新建/编辑不再在页面里开第二块编辑区，而是打开同一个全局模态。
    /// 启动台里的分组标题：折叠箭头、文件夹、组名、数量；支持拖入归组、一键新建组内连接与更多菜单。
    pub(super) fn launcher_group_row(&self, id: &str, name: &str, members: usize, expanded: bool, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let m = self.metrics();
        let menu_id = id.to_owned();
        let more_menu_id = id.to_owned();
        let click_id = id.to_owned();
        let add_id = id.to_owned();
        let drop_id = id.to_owned();
        let drop_soft = p.accent_soft;
        let entity = cx.entity().downgrade();
        let hover_group = SharedString::from(format!("launcher-group-hover-{id}"));
        let quick_actions = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.))
            .opacity(0.)
            .group_hover(hover_group.clone(), |style| style.opacity(1.))
            .child(
                kit::icon_button(
                    SharedString::from(format!("launcher-group-new-conn-{id}")),
                    IconName::Plus,
                    self.t("conn.page.newInGroup"),
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.perform(&format!("conn-new-in-group:{add_id}"), cx);
                    }),
                )
                .xsmall(),
            )
            .child(
                kit::icon_button(
                    SharedString::from(format!("launcher-group-more-{id}")),
                    IconName::Ellipsis,
                    self.t("conn.page.more"),
                    cx.listener(move |this, event: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.menu_at = event.position();
                        this.model.open_group_menu(&more_menu_id);
                        cx.notify();
                    }),
                )
                .xsmall(),
            );
        kit::row_states(div().id(SharedString::from(format!("launcher-group-{id}"))), p)
            .test_support()
            .group(hover_group)
            .role(gpui_kit::Role::Button)
            .aria_label(SharedString::from(name.to_owned()))
            .aria_expanded(expanded)
            .mt(px(6.))
            .h(px(m.row_h + 2.))
            .px(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(Radius::CONTROL))
            .hover(|style| style.bg(p.hover))
            .drag_over::<super::explorer::ConnectionDrag>(move |style, _, _, _| style.bg(drop_soft))
            .on_drop::<super::explorer::ConnectionDrag>(move |drag, _, cx| {
                let connection = drag.connection.clone();
                let group = drop_id.clone();
                if let Some(entity) = entity.upgrade() {
                    entity.update(cx, |this, cx| {
                        this.model.assign_connection_group(&connection, Some(&group));
                        cx.notify();
                    });
                }
            })
            .child(kit::icon(if expanded { IconName::ChevronDown } else { IconName::ChevronRight }, 13., p.text_faint))
            .child(kit::icon(if expanded { IconName::FolderOpen } else { IconName::Folder }, 14., p.text_muted))
            .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).text_size(px(TypeScale::SM)).child(SharedString::from(name.to_owned())))
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
            .child(div().flex_1())
            .child(quick_actions)
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.menu_at = event.position;
                this.model.open_group_menu(&menu_id);
                cx.notify();
            }))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.perform(&format!("group-toggle:{click_id}"), cx);
                cx.notify();
            }))
            .into_any_element()
    }

    /// 启动台的入口按钮。带焦点句柄的那些在模态关闭后接回焦点。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn launcher_action(
        &mut self,
        id: &'static str,
        icon: IconName,
        label: SharedString,
        handle: Option<&FocusHandle>,
        command: &'static str,
        primary: bool,
        _p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut button = if primary {
            kit::primary_button(id, Some(icon), label, act(command, cx))
        } else {
            kit::secondary_button(id, Some(icon), label, act(command, cx))
        };
        if let Some(handle) = handle {
            button = button.focus_handle(handle);
        }
        button.into_any_element()
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
        let file_based = crate::adapters::adapter(engine).meta().file_based();
        let narrow = self.model.viewport_width <= 720;
        let view_w = self.model.viewport_width.max(240) as f32;
        let view_h = self.model.viewport_height.max(240) as f32;
        let panel_w = if narrow { (view_w - 32.).max(240.) } else { 640_f32.min(view_w - 64.) };
        let panel_top = (view_h * 0.08).clamp(18., 72.);
        let panel_h = (view_h - panel_top - 32.).max(220.);
        let pad = if narrow { 14. } else { m.page_pad };
        let focus_first = std::mem::take(&mut self.model.conn_modal.focus_first_pending);
        let active_tab = self.model.conn_modal.effective_tab(engine);
        let url_visible = self.model.conn_modal.url_visible(&self.model.connections);

        // 1. 页头：引擎徽标 + 实时标题与连接地址预览 + 关闭图标（首个 Tab 停靠点）。
        let header = stack_frame(|| {
            let title = if existing {
                let trimmed = draft.name.trim();
                if trimmed.is_empty() {
                    default_connection_name(&draft)
                        .map(SharedString::from)
                        .unwrap_or_else(|| self.t("connections.editor.newTitle"))
                } else {
                    SharedString::from(draft.name.clone())
                }
            } else {
                self.t("connections.editor.newTitle")
            };
            let target = if file_based {
                draft.file_path.trim().to_owned()
            } else if draft.host.trim().is_empty() {
                String::new()
            } else {
                let user = draft.username.trim();
                let port = draft.port.trim();
                let mut text = if user.is_empty() { draft.host.trim().to_owned() } else { format!("{user}@{}", draft.host.trim()) };
                if !port.is_empty() {
                    text.push(':');
                    text.push_str(port);
                }
                if !draft.database.trim().is_empty() {
                    text.push('/');
                    text.push_str(draft.database.trim());
                }
                if draft.ssh_on && !draft.ssh_host.trim().is_empty() {
                    text.push_str(&format!(" · SSH {}", draft.ssh_host.trim()));
                }
                if draft.tls_mode != TlsMode::Off {
                    text.push_str(" · TLS");
                }
                text
            };
            let subtitle = if target.is_empty() {
                SharedString::from(kit::engine_label(engine.as_str()))
            } else {
                SharedString::from(format!("{} · {target}", kit::engine_label(engine.as_str())))
            };
            let close = self.modal_icon_button(
                conn_modal::CLOSE,
                IconName::X,
                self.t("common.close"),
                conn_modal::ModalAction::Command("conn-cancel"),
                &p,
                cx,
            );

            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(pad))
                .py(px(12.))
                .border_b_1()
                .border_color(p.line)
                .child(kit::engine_badge(engine.as_str(), 30.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .min_w_0()
                                .child(
                                    div()
                                        .id("conn-form-title")
                                        .truncate()
                                        .tooltip_text(title.clone())
                                        .text_size(px(TypeScale::LG))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(title),
                                )
                                .children(self.env_pill(&draft.env, &p)),
                        )
                        .child(
                            div()
                                .id("conn-form-target")
                                .truncate()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(px(TypeScale::XS))
                                .text_color(p.text_muted)
                                .child(subtitle),
                        ),
                )
                .child(close)
                .into_any_element()
        });

        // 2. 紧凑引擎选择条 + 分栏切换（常规 / SSH 隧道 / SSL·TLS）+ 可选 URL 快速解析开关。
        let top_controls = stack_frame(|| {
            let mut engine_row = div().flex().flex_wrap().items_center().gap(px(5.));
            for candidate in engines() {
                engine_row = engine_row.child(self.modal_engine_item(candidate, candidate == engine, &p, cx));
            }

            let mut subbar = div().flex().flex_wrap().items_center().gap(px(8.));
            if !file_based {
                let mut tabs = segment_track(&p);
                for (tab, key, enabled_dot) in [
                    (conn_modal::ModalTab::General, "ui.conn.tab.general", false),
                    (conn_modal::ModalTab::Ssh, "ui.conn.tab.ssh", draft.ssh_on),
                    (conn_modal::ModalTab::Tls, "ui.conn.tab.tls", draft.tls_mode != TlsMode::Off),
                ] {
                    tabs = tabs.child(self.modal_tab_item(tab, self.t(key), active_tab == tab, enabled_dot, &p, cx));
                }
                subbar = subbar.child(tabs);
            } else {
                subbar = subbar.child(
                    div()
                        .text_size(px(TypeScale::XS))
                        .text_color(p.text_muted)
                        .child(self.t("ui.conn.block.fileHint")),
                );
            }
            subbar = subbar.child(div().flex_1()).child(self.modal_url_toggle(url_visible, &p, cx));

            div()
                .flex_none()
                .px(px(pad))
                .py(px(10.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .bg(p.chrome)
                .border_b_1()
                .border_color(p.line_soft)
                .child(engine_row)
                .child(subbar)
                .into_any_element()
        });

        // 3. 可折叠连接串解析栏：仅在需要时展开，平时不抢占首屏空间。
        let mut primary_focus: Option<Entity<InputState>> = None;
        let url_drawer = if url_visible {
            Some(stack_frame(|| {
                let (url_hint, url_masked) = self.field_hint(DraftField::Url);
                let (url_input, url_state) = self.field_input(DraftField::Url, &url_hint, url_masked, window, cx);
                if self.model.conn_modal.url_open {
                    primary_focus = Some(url_state);
                }
                let apply_url = self.modal_button(
                    conn_modal::APPLY_URL,
                    Some(IconName::WandSparkles),
                    self.t("ui.conn.parse"),
                    false,
                    conn_modal::ModalAction::Command("conn-apply-url"),
                    &p,
                    cx,
                );
                let url_issue = self.model.connections.issues.iter().find(|issue| issue.field == "url");
                let applied = self.model.conn_modal.url_applied && url_issue.is_none();
                let issue_row = url_issue.map(|issue| {
                    let msg = SharedString::from(field_issue_text(issue, &|key| self.model.text(key)));
                    div()
                        .id("conn-issue-url")
                        .test_support()
                        .aria_label(msg.clone())
                        .text_size(px(TypeScale::XS))
                        .text_color(p.danger)
                        .child(msg)
                        .into_any_element()
                });
                div()
                    .flex_none()
                    .px(px(pad))
                    .py(px(8.))
                    .bg(p.sunken)
                    .border_b_1()
                    .border_color(p.line_soft)
                    .flex()
                    .flex_col()
                    .gap(px(5.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().min_w(px(160.)).child(url_input))
                            .child(apply_url),
                    )
                    .children(issue_row)
                    .when(applied, |this| {
                        this.child(
                            div()
                                .id("conn-url-applied")
                                .test_support()
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .text_size(px(TypeScale::XS))
                                .text_color(p.ok)
                                .child(kit::icon(IconName::CircleCheck, 12., p.ok))
                                .child(self.t("ui.conn.urlApplied")),
                        )
                    })
                    .into_any_element()
            }))
        } else {
            None
        };

        // 4. 主体内容：按当前引擎与激活分栏平铺渲染，一屏内即可完成填写。
        let envs = conn_modal::env_ids(&self.model.env_tags);
        let tab_body = if file_based {
            stack_frame(|| {
                let (file_hint, file_masked) = self.field_hint(DraftField::File);
                let (file, file_state) = self.field_input(DraftField::File, &file_hint, file_masked, window, cx);
                if primary_focus.is_none() {
                    primary_focus = Some(file_state);
                }
                let browse = self.modal_button(
                    conn_modal::BROWSE_FILE,
                    Some(IconName::FolderOpen),
                    self.t("ui.conn.browse"),
                    false,
                    conn_modal::ModalAction::DraftFile,
                    &p,
                    cx,
                );
                let file_issue = self.model.connections.issues.iter().find(|issue| issue.field == "file_path").map(|issue| {
                    let msg = SharedString::from(field_issue_text(issue, &|key| self.model.text(key)));
                    div()
                        .id("conn-issue-file_path")
                        .test_support()
                        .aria_label(msg.clone())
                        .text_size(px(TypeScale::XS))
                        .text_color(p.danger)
                        .child(msg)
                        .into_any_element()
                });
                let identity = self.modal_identity_section(&draft, &envs, &p, window, cx);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .flex()
                                    .items_end()
                                    .gap(px(8.))
                                    .child(div().flex_1().min_w(px(180.)).child(kit::form_row(self.t("ui.conn.file"), file, &p)))
                                    .child(browse),
                            )
                            .children(file_issue),
                    )
                    .child(identity)
                    .into_any_element()
            })
        } else {
            match active_tab {
                conn_modal::ModalTab::General => stack_frame(|| {
                    let (host_hint, host_masked) = self.field_hint(DraftField::Host);
                    let (host, host_state) = self.field_input(DraftField::Host, &host_hint, host_masked, window, cx);
                    if primary_focus.is_none() {
                        primary_focus = Some(host_state);
                    }
                    let (port_hint, port_masked) = self.field_hint(DraftField::Port);
                    let (port, _) = self.field_input(DraftField::Port, &port_hint, port_masked, window, cx);

                    let save_detail = if draft.save_password {
                        "ui.conn.savePasswordOn"
                    } else if self.editing_password_saved() {
                        "ui.conn.savePasswordDrop"
                    } else {
                        "ui.conn.savePasswordOff"
                    };

                    let mut section = div().flex().flex_col().gap(px(12.));
                    // 库索引是一个短数字：和端口排在同一行，用户名退到密码之后（可选的 ACL 用户）。
                    if crate::adapters::adapter(engine).meta().database_is_index() {
                        let (db_hint, db_masked) = self.field_hint(DraftField::Database);
                        let (database, _) = self.field_input(DraftField::Database, &db_hint, db_masked, window, cx);
                        let (password_hint, password_masked) = self.field_hint(DraftField::Password);
                        let (password, _) = self.field_input(DraftField::Password, &password_hint, password_masked, window, cx);
                        let (user_hint, user_masked) = self.field_hint(DraftField::Username);
                        let (user, _) = self.field_input(DraftField::Username, &user_hint, user_masked, window, cx);

                        section = section
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(px(10.))
                                    .child(div().flex_1().min_w(px(180.)).child(self.field_row(self.t("ui.conn.host"), "host", host, &p)))
                                    .child(div().w(px(96.)).child(self.field_row(self.t("ui.conn.port"), "port", port, &p)))
                                    .child(div().w(px(124.)).child(self.field_row(self.t("ui.conn.redisDbLabel"), "database", database, &p))),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(px(10.))
                                    .child(div().flex_1().min_w(px(180.)).child(self.field_row(self.t("ui.conn.password"), "password", password.mask_toggle(), &p)))
                                    .child(div().w(px(184.)).child(self.field_row(self.t("ui.conn.redisAclUser"), "username", user, &p))),
                            );
                    } else {
                        let (user_hint, user_masked) = self.field_hint(DraftField::Username);
                        let (user, _) = self.field_input(DraftField::Username, &user_hint, user_masked, window, cx);
                        let (password_hint, password_masked) = self.field_hint(DraftField::Password);
                        let (password, _) = self.field_input(DraftField::Password, &password_hint, password_masked, window, cx);
                        let (db_hint, db_masked) = self.field_hint(DraftField::Database);
                        let (database, _) = self.field_input(DraftField::Database, &db_hint, db_masked, window, cx);

                        let mut db_row = div()
                            .flex()
                            .flex_wrap()
                            .gap(px(10.))
                            .child(div().flex_1().min_w(px(180.)).child(self.field_row(self.t("ui.conn.defaultDatabase"), "database", database, &p)));
                        if crate::adapters::adapter(engine).meta().uses_replica_set() {
                            let (replica_hint, replica_masked) = self.field_hint(DraftField::Replica);
                            let (replica, _) = self.field_input(DraftField::Replica, &replica_hint, replica_masked, window, cx);
                            db_row = db_row.child(div().w(px(164.)).child(self.field_row(self.t("connections.editor.replicaSet"), "replica", replica, &p)));
                        }

                        section = section
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(px(10.))
                                    .child(div().flex_1().min_w(px(180.)).child(self.field_row(self.t("ui.conn.host"), "host", host, &p)))
                                    .child(div().w(px(108.)).child(self.field_row(self.t("ui.conn.port"), "port", port, &p))),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(px(10.))
                                    .child(div().flex_1().min_w(px(160.)).child(self.field_row(self.t("ui.conn.user"), "username", user, &p)))
                                    .child(div().flex_1().min_w(px(160.)).child(self.field_row(self.t("ui.conn.password"), "password", password.mask_toggle(), &p))),
                            )
                            .child(db_row);
                    }

                    if let Some(issue) = self.model.connections.issues.iter().find(|issue| issue.field == "credentials").cloned() {
                        section = section.child(credentials_row(&issue, &self.model.text("connections.status.error"), &p));
                    }
                    section = section.child(self.modal_switch(
                        conn_modal::SAVE_PASSWORD,
                        "conn-save-password-toggle",
                        draft.save_password,
                        self.t("ui.conn.savePassword"),
                        self.t(save_detail),
                        &p,
                        cx,
                    ));
                    section = section.child(self.modal_identity_section(&draft, &envs, &p, window, cx));
                    section.into_any_element()
                }),
                conn_modal::ModalTab::Ssh => stack_frame(|| {
                    let mut security = div().flex().flex_col().gap(px(12.));
                    let ssh_on = draft.ssh_on;
                    let ssh_handle = self.modal_control(cx, conn_modal::SSH_TOGGLE, conn_modal::ModalAction::Command("conn-ssh-toggle"));
                    security = security.child(
                        kit::interactive_states(div().id(conn_modal::SSH_TOGGLE), &p, false)
                            .test_support()
                            .role(gpui_kit::Role::Switch)
                            .aria_label(self.t("connections.editor.ssh.title"))
                            .aria_selected(ssh_on)
                            .track_focus(&ssh_handle)
                            .p(px(10.))
                            .rounded(px(Radius::PANEL))
                            .border_1()
                            .border_color(if ssh_on { p.accent.opacity(0.45) } else { p.line_soft })
                            .bg(if ssh_on { p.accent_soft } else { p.sunken })
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(switch_visual(ssh_on, &p))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(div().text_size(px(TypeScale::SM)).font_weight(FontWeight::MEDIUM).child(self.t("connections.editor.ssh.title")))
                                    .child(div().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(self.t("ui.conn.sshHint"))),
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.activate_modal_control(conn_modal::SSH_TOGGLE, cx);
                            })),
                    );
                    if ssh_on {
                        let (ssh_host_hint, ssh_host_masked) = self.field_hint(DraftField::SshHost);
                        let (ssh_host, ssh_host_state) = self.field_input(DraftField::SshHost, &ssh_host_hint, ssh_host_masked, window, cx);
                        if primary_focus.is_none() {
                            primary_focus = Some(ssh_host_state);
                        }
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
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(px(10.))
                                    .child(div().flex_1().min_w(px(180.)).child(self.field_row(self.t("ui.conn.sshHost"), "ssh_host", ssh_host, &p)))
                                    .child(div().w(px(96.)).child(self.field_row(self.t("connections.editor.ssh.port"), "ssh_port", ssh_port, &p))),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_end()
                                    .gap(px(10.))
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
                                    .gap(px(10.))
                                    .child(div().flex_1().min_w(px(180.)).child(self.path_row(
                                        self.t("connections.editor.ssh.keyFile"),
                                        "ssh_key",
                                        conn_modal::BROWSE_SSH,
                                        self.t("connections.editor.ssh.browseKey"),
                                        conn_modal::ModalAction::Path(DraftField::SshKey),
                                        ssh_key,
                                        &p,
                                        cx,
                                    )))
                                    .child(div().w(px(190.)).child(self.field_row(self.t("ui.conn.passphrase"), "passphrase", passphrase.mask_toggle(), &p))),
                            );
                        } else {
                            let (ssh_password_hint, ssh_password_masked) = self.field_hint(DraftField::SshPassword);
                            let (ssh_password, _) = self.field_input(DraftField::SshPassword, &ssh_password_hint, ssh_password_masked, window, cx);
                            ssh = ssh.child(self.field_row(self.t("ui.conn.sshPassword"), "ssh_password", ssh_password.mask_toggle(), &p));
                        }
                        security = security.child(ssh);
                    }
                    security.into_any_element()
                }),
                conn_modal::ModalTab::Tls => stack_frame(|| {
                    let mut security = div().flex().flex_col().gap(px(12.));
                    let mut tls_items = segment_track(&p);
                    for (mode, key) in [
                        (TlsMode::Off, "connections.page.tlsOff"),
                        (TlsMode::Require, "ui.conn.tlsRequire"),
                        (TlsMode::Verify, "ui.conn.tlsVerify"),
                    ] {
                        tls_items = tls_items.child(self.modal_segment_item(
                            conn_modal::tls_control_id(mode),
                            self.t(key),
                            draft.tls_mode == mode,
                            conn_modal::ModalAction::Tls(mode),
                            &p,
                            cx,
                        ));
                    }
                    let tls_hint = match draft.tls_mode {
                        TlsMode::Off => "ui.conn.tlsHintOff",
                        TlsMode::Require => "ui.conn.tlsHintRequire",
                        TlsMode::Verify => "ui.conn.tlsHintVerify",
                    };
                    security = security.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(5.))
                            .child(kit::form_row(self.t("connections.editor.tls.title"), tls_items, &p))
                            .child(div().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(self.t(tls_hint))),
                    );
                    if draft.tls_mode == TlsMode::Verify {
                        let mut ca_items = segment_track(&p);
                        for (mode, key) in [
                            (CaMode::System, "connections.editor.tls.caSystem"),
                            (CaMode::File, "connections.editor.tls.caFile"),
                        ] {
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
                            let (ca, ca_state) = self.field_input(DraftField::CaFile, &ca_hint, ca_masked, window, cx);
                            if primary_focus.is_none() {
                                primary_focus = Some(ca_state);
                            }
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
                                .gap(px(10.))
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
                    security.into_any_element()
                }),
            }
        };

        // 5. 结论条与放弃修改确认条（固定在页脚正上方）。
        let status = stack_frame(|| self.modal_status_strip(pad, &p, cx));

        // 6. 固定页脚：顺序与 stops_for 一致（测试 → 取消 → 保存 → 保存并连接）。
        let footer = stack_frame(|| {
            let running = self.model.connections.test_running();
            let saving = self.model.conn_modal.saving;
            let connect_after = self.model.conn_modal.save_and_connect;
            let test = self.modal_button_with(
                "conn-test",
                Some(IconName::Zap),
                if running { self.t("connections.editor.test.running") } else { self.t("connections.editor.test.button") },
                ModalTone::Plain,
                if saving { ModalBusy::Blocked } else { ModalBusy::Idle },
                None,
                conn_modal::ModalAction::Command("conn-test"),
                &p,
                cx,
            );
            let cancel = self.modal_button(
                "conn-cancel",
                None,
                self.t("common.cancel"),
                false,
                conn_modal::ModalAction::Command("conn-cancel"),
                &p,
                cx,
            );
            let save = self.modal_button_with(
                "conn-save",
                None,
                if saving && !connect_after { self.t("ui.conn.saving") } else { self.t("common.save") },
                ModalTone::Plain,
                match (saving, connect_after) {
                    (false, _) => ModalBusy::Idle,
                    (true, false) => ModalBusy::Working,
                    (true, true) => ModalBusy::Blocked,
                },
                None,
                conn_modal::ModalAction::Command("conn-save"),
                &p,
                cx,
            );
            let chord = if cfg!(target_os = "macos") { "⌘↵" } else { "Ctrl+Enter" };
            let save_connect = self.modal_button_with(
                "conn-save-connect",
                None,
                if saving && connect_after { self.t("ui.conn.saving") } else { self.t("connections.editor.saveConnect") },
                ModalTone::Primary,
                match (saving, connect_after) {
                    (false, _) => ModalBusy::Idle,
                    (true, true) => ModalBusy::Working,
                    (true, false) => ModalBusy::Blocked,
                },
                Some(SharedString::from(i18n::fill(&self.model.text("ui.conn.saveConnectTip"), &[("chord", chord)]))),
                conn_modal::ModalAction::Command("conn-save-connect"),
                &p,
                cx,
            );
            div()
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
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(test)
                        .when(running, |this| {
                            this.child(
                                div()
                                    .id("conn-test-running")
                                    .test_support()
                                    .aria_label(self.t("connections.editor.test.running"))
                                    .child(self.spinners.element("conn-test-running", Size::XSmall, Some(p.accent), cx)),
                            )
                        }),
                )
                .child(div().flex_1())
                .child(cancel)
                .child(save)
                .child(save_connect)
                .into_any_element()
        });

        let shell = stack_frame(|| {
            let body = div()
                .id("conn-modal-scroll")
                .flex_1()
                .min_h_0()
                .scroll_area_y()
                .child(div().px(px(pad)).py(px(14.)).child(tab_body));

            let trap = self.modal_focus.clone();
            let content = div()
                .id("conn-form")
                .test_support()
                .flex()
                .flex_col()
                .size_full()
                .overflow_hidden()
                .child(header)
                .child(top_controls)
                .children(url_drawer)
                .child(body)
                .children(status)
                .child(footer);
            let panel = div()
                .id("conn-modal-panel")
                .role(gpui_kit::Role::Group)
                .aria_label(self.t("connections.editor.newTitle"))
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
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

            backdrop("conn-modal-backdrop", 0.32, cx)
                .flex()
                .items_start()
                .justify_center()
                .px(px(20.))
                .pt(px(panel_top))
                .pb(px(20.))
                .child(panel)
                .into_any_element()
        });

        if focus_first {
            match self.model.connections.issues.iter().find_map(|issue| issue_field(&issue.field)) {
                Some(field) if self.fields.contains(field) => self.fields.focus(field, window, cx),
                _ if !draft.name.trim().is_empty() && self.fields.contains(DraftField::Name) => self.fields.focus(DraftField::Name, window, cx),
                _ => {
                    if let Some(first_field) = primary_focus {
                        let handle = first_field.read(cx).focus_handle(cx);
                        window.focus(&handle, cx);
                    }
                }
            }
        }
        if std::mem::take(&mut self.model.conn_modal.focus_discard_pending) {
            if let Some(handle) = self.modal_controls.get(conn_modal::DISCARD_KEEP).cloned() {
                window.focus(&handle, cx);
            }
        }
        shell
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
                // 换类型不清空表单：主机、用户、TLS、SSH 等能沿用的都留着。
                self.model.connections.change_engine(engine);
                self.model.conn_touched();
            }
            conn_modal::ModalAction::Tab(tab) => {
                self.model.conn_modal.tab = tab;
                self.model.conn_modal.discard_armed = false;
            }
            conn_modal::ModalAction::ToggleUrl => {
                self.model.conn_modal.url_open = !self.model.conn_modal.url_open;
                if self.model.conn_modal.url_open {
                    self.model.conn_modal.focus_first_pending = true;
                }
            }
            conn_modal::ModalAction::Env(env) => {
                if let Some(editor) = self.model.connections.editor.as_mut() {
                    editor.env = env;
                }
                // 环境只是标签，不影响连不连得上：测试结论留着，只收起放弃确认。
                self.model.conn_modal.discard_armed = false;
            }
            conn_modal::ModalAction::Group(group) => {
                self.model.conn_modal.draft_group = group;
                self.model.conn_modal.discard_armed = false;
            }
            conn_modal::ModalAction::Tls(mode) => {
                if let Some(editor) = self.model.connections.editor.as_mut() {
                    editor.tls_mode = mode;
                }
                self.model.conn_touched();
            }
            conn_modal::ModalAction::Ca(mode) => {
                if let Some(editor) = self.model.connections.editor.as_mut() {
                    editor.ca_mode = mode;
                }
                self.model.conn_touched();
            }
            conn_modal::ModalAction::SshAuth(kind) => {
                if let Some(editor) = self.model.connections.editor.as_mut() {
                    editor.ssh_auth = kind;
                }
                self.model.conn_touched();
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

    /// 引擎徽标：紧凑单行排布，7 种引擎在一行内完整可见，每个都是 Tab 停靠点。
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
            .h(px(self.metrics().control_h - 2.))
            .px(px(7.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(Radius::CONTROL))
            .border_1()
            .when(selected, |this| {
                this.border_color(p.accent)
                    .bg(p.accent_soft)
                    .text_color(p.accent_text)
                    .font_weight(FontWeight::MEDIUM)
            })
            .when(!selected, |this| {
                this.border_color(p.line)
                    .bg(if p.dark { p.raised } else { p.surface })
                    .hover(|style| style.bg(p.hover).border_color(p.line_strong))
            })
            .child(kit::engine_badge(name, 15.))
            .child(div().text_size(px(TypeScale::XS)).child(kit::engine_label(name)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                let id = conn_modal::engine_control_id(engine);
                this.activate_modal_control(&id, cx);
            }))
            .into_any_element()
    }

    /// 连接模态的配置分栏项（常规 / SSH 隧道 / SSL·TLS），已开启的隧道或加密带状态圆点。
    fn modal_tab_item(
        &mut self,
        tab: conn_modal::ModalTab,
        label: SharedString,
        selected: bool,
        enabled_dot: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = tab.control_id();
        let handle = self.modal_control(cx, id, conn_modal::ModalAction::Tab(tab));
        let height = self.metrics().control_h - 4.;
        kit::interactive_states(div().id(id), p, false)
            .test_support()
            .role(gpui_kit::Role::Tab)
            .aria_label(label.clone())
            .aria_selected(selected)
            .track_focus(&handle)
            .h(px(height))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(Radius::CONTROL))
            .text_size(px(TypeScale::SM))
            .when(selected, |this| {
                this.bg(if p.dark { p.raised } else { p.surface })
                    .text_color(p.text)
                    .font_weight(FontWeight::MEDIUM)
                    .shadow_sm()
            })
            .when(!selected, |this| this.text_color(p.text_muted).hover(|style| style.bg(p.hover).text_color(p.text)))
            .child(label)
            .when(enabled_dot, |this| this.child(kit::status_dot(p.ok)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.activate_modal_control(id, cx);
            }))
            .into_any_element()
    }

    /// 展开/收起连接串快速填充栏的紧凑按钮。
    fn modal_url_toggle(&mut self, active: bool, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = conn_modal::URL_TOGGLE;
        let handle = self.modal_control(cx, id, conn_modal::ModalAction::ToggleUrl);
        let label = self.t("ui.conn.importUrl");
        kit::interactive_states(div().id(id), p, false)
            .test_support()
            .role(gpui_kit::Role::Button)
            .aria_label(label.clone())
            .aria_expanded(active)
            .track_focus(&handle)
            .h(px(self.metrics().control_h - 4.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(Radius::CONTROL))
            .border_1()
            .text_size(px(TypeScale::XS))
            .when(active, |this| {
                this.border_color(p.accent)
                    .bg(p.accent_soft)
                    .text_color(p.accent_text)
                    .font_weight(FontWeight::MEDIUM)
            })
            .when(!active, |this| {
                this.border_color(p.line_soft)
                    .text_color(p.text_muted)
                    .hover(|style| style.bg(p.hover).text_color(p.text).border_color(p.line))
            })
            .child(kit::icon(IconName::WandSparkles, 12., if active { p.accent_text } else { p.text_muted }))
            .child(label)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.activate_modal_control(conn_modal::URL_TOGGLE, cx);
            }))
            .into_any_element()
    }

    /// 名称、环境标签与所属分组：联网与文件型连接共用同一套身份组织区。
    fn modal_identity_section(
        &mut self,
        draft: &crate::connections::model::ConnectionDraft,
        envs: &[String],
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (name_hint, name_masked) = self.field_hint(DraftField::Name);
        let (name, _) = self.field_input(DraftField::Name, &name_hint, name_masked, window, cx);
        let active_env = env_key(&draft.env);
        let mut env_items = segment_track(p);
        for env in envs {
            let dot = env_shown(env).then(|| {
                let tone = env_tone(env, &self.model.env_tags);
                kit::env_colors(tone, p).0
            });
            env_items = env_items.child(self.modal_segment_item_with_dot(
                conn_modal::env_control_id(env),
                self.env_text(env),
                dot,
                active_env == env.as_str(),
                conn_modal::ModalAction::Env(env.clone()),
                p,
                cx,
            ));
        }

        let mut block = div()
            .pt(px(12.))
            .border_t_1()
            .border_color(p.line_soft)
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap(px(10.))
                    .child(div().flex_1().min_w(px(180.)).child(self.field_row(self.t("ui.conn.name"), "name", name, p)))
                    .child(kit::form_row(self.t("conn.page.tag"), env_items, p)),
            );

        if !self.model.groups.is_empty() {
            let current_group = self.model.conn_modal.draft_group.clone();
            let groups = self.model.groups.clone();
            let mut group_items = segment_track(p);
            group_items = group_items.child(self.modal_segment_item(
                conn_modal::group_control_id(None),
                self.t("ui.conn.groupNone"),
                current_group.is_none(),
                conn_modal::ModalAction::Group(None),
                p,
                cx,
            ));
            for group in groups {
                let selected = current_group.as_deref() == Some(group.id.as_str());
                group_items = group_items.child(self.modal_segment_item(
                    conn_modal::group_control_id(Some(&group.id)),
                    SharedString::from(group.name),
                    selected,
                    conn_modal::ModalAction::Group(Some(group.id)),
                    p,
                    cx,
                ));
            }
            block = block.child(kit::form_row(self.t("ui.conn.group"), group_items, p));
        }
        block.into_any_element()
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
        self.modal_segment_item_with_dot(id, label, None, selected, action, p, cx)
    }

    #[allow(clippy::too_many_arguments)]
    fn modal_segment_item_with_dot(
        &mut self,
        id: impl Into<String>,
        label: SharedString,
        dot: Option<gpui_kit::Hsla>,
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
            .px(px(9.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(Radius::CONTROL))
            .text_size(px(TypeScale::SM))
            .when(selected, |this| {
                this.bg(if p.dark { p.raised } else { p.surface })
                    .text_color(p.text)
                    .font_weight(FontWeight::MEDIUM)
                    .shadow_sm()
            })
            .when(!selected, |this| this.text_color(p.text_muted).hover(|style| style.bg(p.hover).text_color(p.text)))
            .when_some(dot, |this, color| this.child(kit::status_dot(color)))
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
        let tone = if primary { ModalTone::Primary } else { ModalTone::Plain };
        self.modal_button_with(id, icon, label, tone, ModalBusy::Idle, None, action, p, cx)
    }

    /// 同 `modal_button`，另外能表达危险动作、“正在进行”（转圈代替图标）和“暂不可用”（变淡、点按不生效）。
    /// 不可用时仍留在 Tab 序列里：序列不随异步状态跳动，焦点也不会突然丢失。
    #[allow(clippy::too_many_arguments)]
    fn modal_button_with(
        &mut self,
        id: &str,
        icon: Option<IconName>,
        label: SharedString,
        tone: ModalTone,
        busy: ModalBusy,
        tip: Option<SharedString>,
        action: conn_modal::ModalAction,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let handle = self.modal_control(cx, id, action);
        let click_id = id.to_owned();
        let element_id = SharedString::from(id);
        let height = self.metrics().control_h;
        let inert = busy != ModalBusy::Idle;
        let mut button = kit::interactive_states(div().id(element_id), p, false)
            .test_support()
            .role(gpui_kit::Role::Button)
            .aria_label(label.clone())
            .track_focus(&handle)
            .h(px(height))
            .px(px(12.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .rounded(px(Radius::CONTROL))
            .border_1()
            .text_size(px(TypeScale::SM))
            .when(inert, |this| this.opacity(0.6))
            .when_some(tip, |this, tip| this.tooltip_text(tip));
        button = match tone {
            ModalTone::Primary => button
                .bg(p.accent)
                .text_color(p.on_accent)
                .font_weight(FontWeight::MEDIUM)
                .border_color(p.accent)
                .when(!inert, |this| this.hover(|style| style.bg(p.accent_hover).border_color(p.accent_hover)).active(|style| style.bg(p.accent_active))),
            ModalTone::Plain => button
                .bg(if p.dark { p.raised } else { p.surface })
                .border_color(p.line)
                .text_color(p.text)
                .when(!inert, |this| this.hover(|style| style.bg(p.hover).border_color(p.line_strong)).active(|style| style.bg(p.active))),
            ModalTone::Danger => button
                .bg(if p.dark { p.raised } else { p.surface })
                .border_color(p.danger.opacity(0.45))
                .text_color(p.danger)
                .font_weight(FontWeight::MEDIUM)
                .when(!inert, |this| this.hover(|style| style.bg(p.danger_soft).border_color(p.danger))),
        };
        let icon_color = match tone {
            ModalTone::Primary => p.on_accent,
            ModalTone::Plain => p.text_muted,
            ModalTone::Danger => p.danger,
        };
        if busy == ModalBusy::Working {
            button = button.child(self.spinners.element(format!("modal-{id}"), Size::XSmall, Some(icon_color), cx));
        } else if let Some(icon) = icon {
            button = button.child(kit::icon(icon, 13., icon_color));
        }
        button
            .child(label)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if !inert {
                    this.activate_modal_control(&click_id, cx);
                }
            }))
            .into_any_element()
    }

    /// 页脚上方的结论条：放弃修改的确认、测试结论、保存失败的原因、没有对应字段的问题。
    /// 它不在滚动区里，所以无论表单滚到哪，按下页脚按钮后的结果都在按钮正上方。
    fn modal_status_strip(&mut self, pad: f32, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let mut rows: Vec<AnyElement> = Vec::new();
        if self.model.conn_modal.discard_armed {
            let title = self.t("ui.conn.discard.title");
            let keep = self.modal_button_with(
                conn_modal::DISCARD_KEEP,
                None,
                self.t("ui.conn.discard.keep"),
                ModalTone::Plain,
                ModalBusy::Idle,
                None,
                conn_modal::ModalAction::Command("conn-discard-keep"),
                p,
                cx,
            );
            let discard = self.modal_button_with(
                conn_modal::DISCARD_CONFIRM,
                None,
                self.t("ui.conn.discard.confirm"),
                ModalTone::Danger,
                ModalBusy::Idle,
                None,
                conn_modal::ModalAction::Command("conn-discard"),
                p,
                cx,
            );
            // 正在问要不要放弃时只问这一件事，别的结论先让开。
            rows.push(
                div()
                    .id("conn-discard-prompt")
                    .test_support()
                    .role(gpui_kit::Role::Group)
                    .aria_label(title.clone())
                    .px(px(12.))
                    .py(px(8.))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(Radius::PANEL))
                    .bg(p.warn_soft)
                    .border_1()
                    .border_color(p.warn.opacity(0.30))
                    .child(kit::icon(IconName::TriangleAlert, 15., p.warn))
                    .child(div().flex_1().min_w(px(120.)).text_size(px(TypeScale::SM)).text_color(p.text).child(title))
                    .child(keep)
                    .child(discard)
                    .into_any_element(),
            );
        } else {
            for issue in self.model.connections.issues.iter().filter(|issue| !field_issue_placed(&issue.field)) {
                rows.push(banner(IconName::CircleAlert, self.t(issue.key), p.danger, p.danger_soft));
            }
            // “正在测试”由测试按钮自己表达，不再占一条。
            if let Some(key) = self.model.connections.message_key.filter(|key| *key != "connections.editor.test.running") {
                let tested = matches!(key, "connections.editor.test.ok" | "connections.editor.test.wait" | "connections.editor.test.failed");
                match self.model.connections.test_report.as_ref().filter(|_| tested) {
                    Some(report) => rows.push(test_verdict(report, &self.model.text(key), self.model.locale(), p)),
                    None => {
                        let ok = key == "connections.status.connected" || key == "connections.editor.test.ok";
                        let detail = self.model.connections.message_detail.clone();
                        let text = if detail.is_empty() { self.model.text(key) } else { format!("{} · {}", self.model.text(key), detail) };
                        rows.push(if ok {
                            banner(IconName::CircleCheck, SharedString::from(text), p.ok, p.ok_soft)
                        } else {
                            banner(IconName::CircleAlert, SharedString::from(text), p.danger, p.danger_soft)
                        });
                    }
                }
            }
        }
        if rows.is_empty() {
            return None;
        }
        Some(
            div()
                .id("conn-modal-status")
                .test_support()
                .flex_none()
                .px(px(pad))
                .pt(px(10.))
                .pb(px(10.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .border_t_1()
                .border_color(p.line_soft)
                .children(rows)
                .into_any_element(),
        )
    }

    /// 模态里的开关：开关、名称和一句“现在这样意味着什么”。整行可点，也是 Tab 停靠点。
    #[allow(clippy::too_many_arguments)]
    fn modal_switch(
        &mut self,
        id: &'static str,
        command: &'static str,
        on: bool,
        label: SharedString,
        detail: SharedString,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let handle = self.modal_control(cx, id, conn_modal::ModalAction::Command(command));
        kit::interactive_states(div().id(id), p, false)
            .test_support()
            .role(gpui_kit::Role::Switch)
            .aria_label(label.clone())
            .aria_description(detail.clone())
            .aria_selected(on)
            .track_focus(&handle)
            .w_full()
            .px(px(10.))
            .py(px(8.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(10.))
            .gap_y(px(2.))
            .rounded(px(Radius::CONTROL))
            .border_1()
            .border_color(if on { p.accent.opacity(0.28) } else { p.line_soft })
            .bg(if on { p.accent_soft.opacity(0.45) } else { p.sunken })
            .hover(|style| style.border_color(if on { p.accent.opacity(0.48) } else { p.line }))
            .child(switch_visual(on, p))
            .child(div().flex_none().text_size(px(TypeScale::SM)).font_weight(FontWeight::MEDIUM).child(label))
            .child(div().min_w_0().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(detail))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.activate_modal_control(id, cx);
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
        let mut row = div().flex().flex_col().gap(px(4.)).min_w_0().child(kit::form_row(label, control, p));
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
                    .min_w_0()
                    .child(div().flex_1().min_w_0().child(kit::form_row(label, input, p)))
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

    /// 正在编辑的已保存连接，钥匙串里有没有它的密码。
    fn editing_password_saved(&self) -> bool {
        let Some(id) = self.model.connections.editor.as_ref().and_then(|draft| draft.id.as_deref()) else {
            return false;
        };
        self.model.connections.rows.iter().any(|row| row.saved.id == id && row.saved.password_saved)
    }

    /// 每个草稿字段的占位符与掩码：渲染与 Tab 聚焦读同一份，避免两处写不同文案。
    fn field_hint(&self, field: DraftField) -> (String, bool) {
        let existing = self.model.connections.editor.as_ref().is_some_and(|draft| draft.id.is_some());
        let engine = self.model.connections.editor.as_ref().map(|draft| draft.engine);
        let meta = engine.map(|engine| crate::adapters::adapter(engine).meta());
        let username_hint = meta.map_or(crate::adapters::FieldHint::None, |meta| meta.username_hint());
        let key = match field {
            DraftField::Url => "ui.conn.urlHint",
            DraftField::Name => "ui.conn.namePlaceholder",
            DraftField::File => "connections.validate.chooseFile",
            DraftField::Database => meta.map_or("ui.conn.optional", |meta| meta.database_hint_key()),
            DraftField::CaFile => "",
            DraftField::ClientCert | DraftField::ClientKey => "",
            DraftField::Passphrase => "ui.conn.optional",
            _ => "",
        };
        let placeholder = match field {
            DraftField::Host => "127.0.0.1".to_owned(),
            // 端口清空后仍看得到这种数据库的默认端口。
            DraftField::Port => engine.map(|engine| crate::app::views::connections::default_port(engine).to_owned()).unwrap_or_default(),
            DraftField::Username => match username_hint {
                crate::adapters::FieldHint::Text(text) => text.to_owned(),
                crate::adapters::FieldHint::Key(key) => self.model.text(key),
                crate::adapters::FieldHint::None => String::new(),
            },
            DraftField::Replica => "rs0".to_owned(),
            DraftField::SshHost => "bastion.example.com".to_owned(),
            DraftField::SshPort => "22".to_owned(),
            DraftField::SshKey => "~/.ssh/id_ed25519".to_owned(),
            DraftField::CaFile => "ca.pem".to_owned(),
            // “留空则沿用已保存的密码”只在真的存了密码时成立；没存时留空意味着连接时再问。
            DraftField::Password if existing && self.editing_password_saved() => self.model.text("ui.conn.passwordKeep"),
            DraftField::Password if existing => self.model.text("ui.conn.passwordAskLater"),
            // 名称留空时保存会用主机名（SQLite 用文件名）：占位里直接说出来，用户知道不填也行、会叫什么。
            DraftField::Name => match self.model.connections.editor.as_ref().and_then(default_connection_name) {
                Some(name) => i18n::fill(&self.model.text("ui.conn.nameDefault"), &[("name", &name)]),
                None => self.model.text(key),
            },
            // 示例跟着当前引擎走：选了 SQLite 不再提示一个 postgres 连接串。
            DraftField::Url => format!("{} {}", self.model.text(key), crate::adapters::adapter(engine.unwrap_or(crate::adapters::registry::DEFAULT_ENGINE)).meta().url_example()),
            _ if key.is_empty() => String::new(),
            _ => self.model.text(key),
        };
        let masked = matches!(
            field,
            DraftField::Password | DraftField::SshPassword | DraftField::Passphrase
        );
        (placeholder, masked)
    }

    /// 一个筛选按钮：展开时用强调底，点击同时记住锚点位置供浮层定位（下拉箭头在右侧）。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn filter_button(
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
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(Radius::CONTROL))
            .border_1()
            .text_size(px(TypeScale::SM))
            .when(open, |this| this.border_color(p.accent).bg(p.accent_soft).text_color(p.accent_text))
            .when(!open, |this| {
                this.border_color(p.line)
                    .bg(if p.dark { p.raised } else { p.surface })
                    .text_color(p.text)
                    .hover(|style| style.bg(p.hover).border_color(p.line_strong))
            })
            .child(kit::icon(icon, 13., if open { p.accent_text } else { p.text_muted }))
            .child(label)
            .child(kit::icon(IconName::ChevronDown, 12., if open { p.accent_text } else { p.text_faint }))
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
            .scroll_area_y()
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
        let issue_name = match field {
            DraftField::CaFile => "ca_file",
            DraftField::ClientCert => "client_cert",
            DraftField::ClientKey => "client_key",
            DraftField::SshKey => "ssh_key",
            _ => "file_path",
        };
        cx.spawn(async move |this, cx| {
            let outcome = match receiver.await {
                Ok(Ok(Some(paths))) => Ok(paths.into_iter().next()),
                Ok(Ok(None)) => Ok(None),
                Ok(Err(err)) => Err(err.to_string()),
                Err(err) => Err(err.to_string()),
            };
            match outcome {
                Ok(Some(path)) => {
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
                }
                Ok(None) => {}
                Err(raw) => {
                    cx.update(|cx| {
                        let _ = this.update(cx, |view, cx| {
                            view.model.connections.clear_issue(issue_name);
                            let issue = if super::view::is_portal_dialog_error(&raw) {
                                crate::app::views::connections::FieldIssue::new(issue_name, "ui.filePicker.portalUnavailable")
                            } else {
                                crate::app::views::connections::FieldIssue::with_detail(issue_name, "connections.status.error", raw)
                            };
                            view.model.connections.issues.push(issue);
                            view.model.conn_modal.focus_first_pending = true;
                            cx.notify();
                        });
                    });
                }
            }
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
            let outcome = match receiver.await {
                Ok(Ok(Some(paths))) => Ok(paths.into_iter().next()),
                Ok(Ok(None)) => Ok(None),
                Ok(Err(err)) => Err(err.to_string()),
                Err(err) => Err(err.to_string()),
            };
            match outcome {
                Ok(Some(path)) => {
                    let accepted = crate::app::accept_open_path(&path);
                    cx.update(|cx| {
                        let _ = this.update(cx, |view, cx| {
                            match accepted {
                                Ok(path) => {
                                    if let Some(editor) = view.model.connections.editor.as_mut() {
                                        if editor.name.trim().is_empty() {
                                            editor.name = path.file_stem().and_then(|name| name.to_str()).unwrap_or(editor.engine.as_str()).to_owned();
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
                }
                Ok(None) => {}
                Err(raw) => {
                    cx.update(|cx| {
                        let _ = this.update(cx, |view, cx| {
                            view.model.connections.clear_issue("file_path");
                            let issue = if super::view::is_portal_dialog_error(&raw) {
                                crate::app::views::connections::FieldIssue::new("file_path", "ui.sqlite.portalFallback")
                            } else {
                                crate::app::views::connections::FieldIssue::with_detail("file_path", "connections.validate.chooseFile", raw)
                            };
                            view.model.connections.issues.push(issue);
                            view.model.conn_modal.focus_first_pending = true;
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
        let (search, save_name, rename_input, tabs, status_seg) = stack_frame(|| {
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
        (search, save_name, rename_input, tabs, status_seg)
        });
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
            .min_w_0()
            .min_h_0()
            .scroll_area_y()
            .scroll_handle(&self.history_scroll)
            .px(px(m.page_pad))
            .pb(px(m.page_pad_y));
        let mut child_count = 0usize;
        let mut selected_child: Option<usize> = None;
        let phase = self.model.history.phase_for(section);
        // R13：多选中的其他行与当前行同样高亮。
        let marked_ids: std::collections::HashSet<String> = self.model.history.marked_ids().into_iter().collect();
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
            child_count += 1;
        }
        if section == HistorySection::Runs {
            list = stack_frame(|| {
            let mut list = list;
            let mut runs = self.model.history.iter_filtered_runs().take(200).peekable();
            if runs.peek().is_none() {
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
                child_count += 1;
            } else if self.model.history.loading {
                // 刷新时旧行继续显示，只在列表顶部说明还在读，不整块换成骨架。
                list = list.child(div().id("history-loading").test_support().child(crate::ui::loading::inline_loading(self.spinners.element("history-loading", Size::Small, Some(p.accent), cx), self.t("history.page.loading"), &m, &p)));
                child_count += 1;
            }
            for entry in runs {
                let id = entry.id.clone();
                let selected = self.model.history.selected_run.as_deref() == Some(id.as_str());
                let marked = selected || marked_ids.contains(&id);
                if selected {
                    selected_child = Some(child_count);
                }
                let ok = entry.status == "ok" || entry.status == "success" || entry.status == "done";
                let elapsed = entry.elapsed_ms.map(|ms| if ms >= 1000 { format!("{:.1} s", ms as f64 / 1000.) } else { format!("{ms} ms") }).unwrap_or_default();
                let when = entry.started_at.replace('T', " ").chars().take(19).collect::<String>();
                let select = id.clone();
                let menu = id.clone();
                let load = id.clone();
                let delete = id.clone();
                let row = stack_frame(|| {
                    kit::row_states(div().id(SharedString::from(format!("run-{id}"))), &p)
                        .test_support()
                        .group("run-row")
                        .role(gpui_kit::Role::ListItem)
                        .aria_label(SharedString::from(entry.sql.clone()))
                        .aria_selected(selected)
                        .p(px(m.row_pad_y + 5.))
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .rounded(px(Radius::PANEL))
                        .border_1()
                        .when(marked, |this| this.border_color(p.accent).bg(p.accent_soft))
                        .when(!marked, |this| this.border_color(p.line).hover(|style| style.border_color(p.line_strong)))
                        .child(
                            div()
                                .min_w_0()
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
                            // R13：Shift 扩展、⌘/Ctrl 切换多选；普通单击只选这一条。
                            let mods = event.modifiers();
                            let prefix = match RowPick::from_modifiers(mods.shift, mods.secondary(cx)) {
                                RowPick::Replace => "history-pick",
                                RowPick::Extend => "history-pick-extend",
                                RowPick::Toggle => "history-pick-toggle",
                            };
                            this.perform(&format!("{prefix}:{select}"), cx);
                            if event.click_count() >= 2 && !mods.modified() {
                                this.perform("history-load", cx);
                            }
                            cx.notify();
                        }))
                        .into_any_element()
                });
                list = list.child(row);
                child_count += 1;
            }
            list
            });
        } else {
            list = stack_frame(|| {
            let mut list = list;
            let mut saved = self.model.history.iter_filtered_saved().take(200).peekable();
            if saved.peek().is_none() {
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
                child_count += 1;
            } else if self.model.history.loading {
                list = list.child(div().id("history-loading").test_support().child(crate::ui::loading::inline_loading(self.spinners.element("history-loading-saved", Size::Small, Some(p.accent), cx), self.t("history.page.loadingSaved"), &m, &p)));
                child_count += 1;
            }
            for item in saved {
                let id = item.id.clone();
                let selected = self.model.history.selected_saved.as_deref() == Some(id.as_str());
                let marked = selected || marked_ids.contains(&id);
                if selected {
                    selected_child = Some(child_count);
                }
                let select = id.clone();
                let copy = id.clone();
                let load = id.clone();
                let rename = id.clone();
                let delete = id.clone();
                let row = stack_frame(|| {
                    kit::row_states(div().id(SharedString::from(format!("saved-{id}"))), &p)
                        .test_support()
                        .group("saved-row")
                        .role(gpui_kit::Role::ListItem)
                        .aria_label(SharedString::from(item.name.clone()))
                        .aria_selected(selected)
                        .p(px(m.row_pad_y + 5.))
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .rounded(px(Radius::PANEL))
                        .border_1()
                        .when(marked, |this| this.border_color(p.accent).bg(p.accent_soft))
                        .when(!marked, |this| this.border_color(p.line).hover(|style| style.border_color(p.line_strong)))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(kit::icon(IconName::Bookmark, 14., p.accent))
                                .child(div().id(SharedString::from(format!("saved-name-{id}"))).flex_1().min_w_0().truncate().tooltip_text(SharedString::from(item.name.clone())).font_weight(FontWeight::MEDIUM).child(SharedString::from(item.name.clone())))
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
                        .child(div().min_w_0().font_family(mono.clone()).text_size(px(TypeScale::SM)).text_color(p.text_muted).max_h(px(36.)).truncate().child(SharedString::from(clip(&item.sql, 200))))
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
                            let mods = event.modifiers();
                            let prefix = match RowPick::from_modifiers(mods.shift, mods.secondary(cx)) {
                                RowPick::Replace => "history-pick",
                                RowPick::Extend => "history-pick-extend",
                                RowPick::Toggle => "history-pick-toggle",
                            };
                            this.perform(&format!("{prefix}:{select}"), cx);
                            if event.click_count() >= 2 && !mods.modified() {
                                this.perform("history-load", cx);
                            }
                            cx.notify();
                        }))
                        .into_any_element()
                });
                list = list.child(row);
                child_count += 1;
            }
            list
            });
        }
        if std::mem::take(&mut self.model.history_reveal) {
            if let Some(child_ix) = selected_child {
                self.history_scroll.scroll_to_item(child_ix);
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
        let page = stack_frame(|| {
        let toolbar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(10.))
            .child(tabs)
            .child(div().w(px(260.)).child(search.prefix(kit::input_prefix(IconName::Search, 13., p.text_faint)).cleanable(true)))
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
                this.child(
                    div()
                        .w(px(220.))
                        .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                            if event.keystroke.key == "enter" {
                                cx.stop_propagation();
                                this.perform("history-save-named", cx);
                            }
                        }))
                        .child(save_name),
                )
                .child(kit::primary_button("history-save-named", Some(IconName::Save), self.t("history.savedPanel.save"), act("history-save-named", cx)))
            });
        let rename_bar = if section == HistorySection::Saved && self.model.history.rename_open {
            if !self.history_rename_focused {
                self.history_rename_focused = true;
                self.fields.focus(DraftField::RenameName, window, cx);
            }
            div()
                .id("history-rename-bar")
                .test_support()
                .px(px(m.page_pad))
                .pb(px(10.))
                .flex()
                .items_center()
                .gap(px(8.))
                .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| match event.keystroke.key.as_str() {
                    "enter" => {
                        cx.stop_propagation();
                        this.perform("history-rename-confirm", cx);
                    }
                    "escape" => {
                        cx.stop_propagation();
                        this.perform("history-rename-cancel", cx);
                        this.restore_focus(window, cx);
                    }
                    _ => {}
                }))
                .child(div().w(px(260.)).child(rename_input))
                .child(kit::primary_button("history-rename-confirm", Some(IconName::Check), self.t("history.page.renameSave"), act("history-rename-confirm", cx)))
                .child(kit::secondary_button("history-rename-cancel", Some(IconName::X), self.t("common.cancel"), act("history-rename-cancel", cx)))
                .into_any_element()
        } else {
            self.history_rename_focused = false;
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
            let mut column = div().flex().flex_1().min_w_0().min_h_0().flex_col();
            if has_preview {
                // 与列表分享剩余高度，最高 300；空预览不占位，矮窗口列表不会被压没。
                column = column.child(
                    div()
                        .flex_1()
                        .max_h(px(300.))
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .child(preview),
                );
            }
            column.child(list)
        } else {
            div().flex().flex_1().min_w_0().min_h_0().child(list).child(preview)
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
                    .pt(px(m.page_pad_y))
                    .pb(px(12.))
                    .border_b_1()
                    .border_color(p.line)
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(8.))
                            .child(kit::title(self.t("history.page.titleRuns"), TypeScale::LG, &p))
                            .when(!self.model.tabs.is_empty(), |this| {
                                this.child(kit::tool_button("history-back", Some(IconName::ArrowLeft), self.t("ui.page.backWorkspace"), act("back-workspace", cx)))
                            }),
                    )
                    .child(toolbar),
            )
            .child(rename_bar)
            .child(body)
            .into_any_element()
        });
        page
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
            .min_w_0()
            .min_h_0()
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
                    .min_w_0()
                    .min_h_0()
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
                            .min_h_0()
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

    fn settings_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
        let stepper = |this: &Self, id: &'static str, value: String, at_min: bool, at_max: bool, cx: &mut Context<Self>| {
            kit::stepper(
                SharedString::from(format!("{id}-down")),
                SharedString::from(format!("{id}-up")),
                this.t("ui.settings.smaller"),
                this.t("ui.settings.increase"),
                SharedString::from(value),
                at_min,
                at_max,
                m.control_h,
                &Palette::of(cx),
                act(format!("{id}-down"), cx),
                act(format!("{id}-up"), cx),
            )
        };
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
            SettingsSection::Appearance => stack_frame(|| {
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
                let font = kit::stepper(
                    "font-down",
                    "font-up",
                    self.t("ui.settings.smaller"),
                    self.t("ui.settings.larger"),
                    SharedString::from(format!("{} px", s.editor_font)),
                    s.editor_font <= FONT_RANGE.0,
                    s.editor_font >= FONT_RANGE.1,
                    m.control_h,
                    &p,
                    act("font-down", cx),
                    act("font-up", cx),
                );
                // 五种配色直接摆出来一次选中，不用左右箭头逐个翻、也看不到还有哪些可选。
                let editor_theme_ids: Vec<String> = crate::app::views::settings::EDITOR_THEMES.iter().map(|name| format!("editor-theme-set-{name}")).collect();
                let editor_theme_items: Vec<(&str, &str, bool)> = crate::app::views::settings::EDITOR_THEMES
                    .iter()
                    .zip(editor_theme_ids.iter())
                    .map(|(name, id)| (id.as_str(), crate::app::views::settings::editor_theme_key(name), s.editor_theme == *name))
                    .collect();
                let editor_theme = seg(self, &editor_theme_items, cx);
                div()
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
                        (self.t("shell.settings.editorTheme"), None, editor_theme.into_any_element()),
                    ],
                    &p,
                )).into_any_element()
            }),
            SettingsSection::Data => stack_frame(|| {
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
                let empty_seg = seg(
                    self,
                    &[("empty-hint", "ui.settings.emptyHint", s.empty_display != "blank"), ("empty-blank", "ui.settings.emptyBlank", s.empty_display == "blank")],
                    cx,
                );
                // 复制 NULL 也写成空文本时，粘贴回来或在别处看到的 NULL 与空字符串分不出来：就地说明。
                let null_hint = (s.null_copy == "empty").then(|| self.t("ui.settings.nullEmptyClash"));
                let page_size = stepper(
                    self,
                    "page-size",
                    s.page_size.to_string(),
                    s.page_size <= PAGE_SIZE_RANGE.0,
                    s.page_size >= PAGE_SIZE_RANGE.1,
                    cx,
                );
                let preview = stepper(
                    self,
                    "preview",
                    format!("{} KB", s.preview_kb),
                    s.preview_kb <= PREVIEW_RANGE.0,
                    s.preview_kb >= PREVIEW_RANGE.1,
                    cx,
                );
                let object_page_step = stepper(
                    self,
                    "object-page",
                    s.object_page.to_string(),
                    s.object_page <= OBJECT_PAGE_RANGE.0,
                    s.object_page >= OBJECT_PAGE_RANGE.1,
                    cx,
                );
                let mut object_page = div().flex().items_center().gap(px(6.));
                for preset in [200_i64, 500, 1000, 2000] {
                    let cmd = format!("object-page-{preset}");
                    let selected = s.object_page == preset;
                    object_page = object_page.child(
                        kit::filter_chip(
                            SharedString::from(format!("settings-object-page-{preset}")),
                            SharedString::from(preset.to_string()),
                            None,
                            None,
                            None,
                            selected,
                            &p,
                            act(cmd, cx),
                        ),
                    );
                }
                object_page = object_page.child(object_page_step);
                let naming_rows = self.index_naming_rows(&s.index_naming, &p, window, cx);
                div()
                .flex()
                .flex_col()
                .gap(px(m.section_gap()))
                .child(group("settings-group-index-naming", self.t("ui.settings.indexNaming"), naming_rows, &p))
                .child(group(
                    "settings-group-data",
                    self.t("ui.settings.data"),
                    vec![
                        (self.t("shell.settings.rowsPerPage"), None, page_size.into_any_element()),
                        (self.t("shell.settings.previewLimit"), None, preview.into_any_element()),
                        (self.t("shell.settings.objectPage"), None, object_page.into_any_element()),
                        (self.t("shell.settings.nullCopy"), null_hint, null_seg),
                        (self.t("shell.settings.emptyDisplay"), None, empty_seg),
                        (self.t("shell.settings.timeout"), None, timeout_seg),
                    ],
                    &p,
                )).into_any_element()
            }),
            SettingsSection::Local => stack_frame(|| {
                let updates = kit::interactive_states(div().id("toggle-updates"), &p, false)
                    .role(gpui_kit::Role::Switch)
                    .aria_label(self.t("shell.update.autoCheck"))
                    .aria_selected(s.auto_check_updates)
                    .child(switch_visual(s.auto_check_updates, &p))
                    .on_click(act("toggle-updates", cx));
                div()
                .flex()
                .flex_col()
                .gap(px(m.section_gap()))
                .child(self.update_card(updates.into_any_element(), &p, &m, cx))
                .child(
                    div()
                        .id("settings-local-hint")
                        .text_size(px(TypeScale::SM))
                        .text_color(p.text_muted)
                        .child(self.t("ui.settings.localHint")),
                )
                .into_any_element()
            }),
        };
        let form = div()
            .id("settings-form")
            .test_support()
            .role(gpui_kit::Role::Group)
            .aria_label(self.t("shell.settings.title"))
            .flex_1()
            .min_w_0()
            .min_h_0()
            .scroll_area_y()
            .child(
                div()
                    .max_w(px(760.))
                    .min_w_0()
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
                        .when(!selected, |this| this.text_color(p.text_muted).hover(|style| style.bg(p.hover).text_color(p.text)))
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
            .min_w_0()
            .min_h_0()
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
                    .pb(px(12.))
                    .border_b_1()
                    .border_color(p.line)
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(kit::title(self.t("shell.settings.title"), TypeScale::LG, &p))
                    .when(!self.model.tabs.is_empty(), |this| {
                        this.child(kit::tool_button("settings-back", Some(IconName::ArrowLeft), self.t("ui.page.backWorkspace"), act("back-workspace", cx)))
                    }),
            )
            .child(body)
            .into_any_element()
    }

    /// 软件更新卡片：当前版本、检查状态、主操作；有新版本时给下载进度与更新说明；
    /// 最后一行是“启动时自动检查”的开关。状态文字与按钮只按 `UpdateState` 画，不另记一份。
    fn update_card(&self, auto_switch: AnyElement, p: &Palette, m: &kit::Metrics, cx: &mut Context<Self>) -> AnyElement {
        use super::update::UpdateStage;
        let update = &self.model.update;
        let route = &self.model.update_route;
        let offer = update.offer.as_ref();
        let version = offer.map(|offer| format!("v{}", offer.version)).unwrap_or_default();
        let fill = |key: &str, pairs: &[(&str, &str)]| SharedString::from(i18n::fill(&self.model.text(key), pairs));
        let installable = offer.is_some_and(|offer| offer.installable(route));
        let (status, tone): (SharedString, gpui_kit::Hsla) = match &update.stage {
            UpdateStage::Idle => (self.t("update.idle"), p.text_muted),
            UpdateStage::Checking { .. } => (self.t("update.checking"), p.text_muted),
            UpdateStage::Latest { checked_at } => {
                let checked = fill("update.checkedAt", &[("time", checked_at)]);
                (SharedString::from(format!("{} · {checked}", self.model.text("update.latest"))), p.ok)
            }
            UpdateStage::Available => (fill("update.available", &[("version", &version)]), p.accent_text),
            UpdateStage::Downloading { .. } => (fill("update.downloading", &[("version", &version)]), p.accent_text),
            UpdateStage::Installing => (self.t("update.installing"), p.accent_text),
            UpdateStage::Ready => (fill("update.ready", &[("version", &version)]), p.ok),
            UpdateStage::Failed { message } => (fill("update.failed", &[("reason", message)]), p.danger),
        };
        // 辅助说明：只能手动下载时说明原因；Windows 安装会关闭应用，提前说清楚。
        let hint: Option<SharedString> = match &update.stage {
            UpdateStage::Available | UpdateStage::Failed { .. } if offer.is_some() && !installable => Some(self.t("update.manualOnly")),
            UpdateStage::Available if installable && cfg!(target_os = "windows") => Some(self.t("update.windowsHint")),
            _ => None,
        };
        let check = |label: SharedString, loading: bool, cx: &mut Context<Self>| {
            kit::secondary_button("update-check", Some(IconName::RefreshCw), label, act("check-updates", cx)).loading(loading).disabled(loading).into_any_element()
        };
        let open_page = |primary: bool, cx: &mut Context<Self>| {
            let label = self.t("update.openPage");
            if primary {
                kit::primary_button("update-open-page", Some(IconName::ExternalLink), label, act("update-open-page", cx)).into_any_element()
            } else {
                kit::tool_button("update-open-page", Some(IconName::ExternalLink), label, act("update-open-page", cx)).into_any_element()
            }
        };
        let mut actions = div().flex().flex_none().flex_wrap().items_center().gap(px(6.));
        actions = match &update.stage {
            UpdateStage::Checking { .. } => actions.child(check(self.t("update.checking"), true, cx)),
            UpdateStage::Available | UpdateStage::Failed { .. } if installable => {
                let key = if matches!(update.stage, UpdateStage::Failed { .. }) { "update.retryInstall" } else { "update.install" };
                actions
                    .child(kit::primary_button("update-install", Some(IconName::Download), self.t(key), act("update-install", cx)))
                    .child(open_page(false, cx))
            }
            UpdateStage::Available | UpdateStage::Failed { .. } if offer.is_some() => actions.child(open_page(true, cx)).child(check(self.t("update.check"), false, cx)),
            UpdateStage::Downloading { .. } | UpdateStage::Installing => actions,
            UpdateStage::Ready => actions.child(kit::primary_button("update-restart", Some(IconName::RefreshCw), self.t("update.restart"), act("update-restart", cx))),
            _ => actions.child(check(self.t("update.check"), false, cx)),
        };

        let header = div()
            .px(px(16.))
            .py(px(m.row_pad_y + 8.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(12.))
            .child(brand_mark(36., p))
            .child(
                div()
                    .flex_1()
                    .min_w(px(200.))
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("RillSQL"))
                            .child(kit::pill(SharedString::from(format!("v{}", crate::release::PRODUCT_VERSION)), p.text_muted, p.hover)),
                    )
                    .child(
                        div()
                            .id("update-status")
                            .test_support()
                            .role(if matches!(update.stage, UpdateStage::Failed { .. }) { gpui_kit::Role::Alert } else { gpui_kit::Role::Status })
                            .text_size(px(TypeScale::SM))
                            .text_color(tone)
                            .child(status),
                    )
                    .when_some(hint, |this, hint| this.child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(hint))),
            )
            .child(actions);

        let progress = match update.stage {
            UpdateStage::Downloading { done, total } => {
                let percent = update.percent();
                let label = match total {
                    Some(total) => format!("{} / {}", super::update::megabytes(done), super::update::megabytes(total)),
                    None => super::update::megabytes(done),
                };
                Some(
                    div()
                        .id("update-progress")
                        .test_support()
                        .role(gpui_kit::Role::ProgressIndicator)
                        .px(px(16.))
                        .pb(px(12.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(
                            div()
                                .flex_1()
                                .h(px(6.))
                                .rounded(px(Radius::TAG))
                                .bg(p.hover)
                                .overflow_hidden()
                                // 总长未知时画一段固定宽度，表示仍在进行。
                                .child(div().h_full().rounded(px(Radius::TAG)).bg(p.accent).w(gpui_kit::relative(percent.map(|value| value as f32 / 100.).unwrap_or(0.3)))),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(TypeScale::XS))
                                .text_color(p.text_muted)
                                .child(SharedString::from(match percent {
                                    Some(percent) => format!("{label}  {percent}%"),
                                    None => label,
                                })),
                        ),
                )
            }
            _ => None,
        };

        // 更新说明：有新版本、下载中或待重启时显示；纯文本行，标题加粗，长说明在框内滚动。
        let show_notes = matches!(update.stage, UpdateStage::Available | UpdateStage::Downloading { .. } | UpdateStage::Installing | UpdateStage::Ready | UpdateStage::Failed { .. });
        let notes = offer.filter(|offer| show_notes && !offer.notes.is_empty()).map(|offer| {
            let mut body = div().id("update-notes-body").flex().flex_col().gap(px(2.)).max_h(px(220.)).scroll_area_y();
            for (heading, line) in super::update::notes_lines(&offer.notes) {
                body = body.child(if line.is_empty() {
                    div().h(px(6.))
                } else if heading {
                    div().pt(px(4.)).text_size(px(TypeScale::SM)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(SharedString::from(line))
                } else {
                    div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(SharedString::from(line))
                });
            }
            let date = offer.pub_date.as_deref().and_then(|date| date.get(..10)).unwrap_or_default().to_owned();
            div()
                .id("update-notes")
                .test_support()
                .mx(px(16.))
                .mb(px(14.))
                .p(px(12.))
                .rounded(px(Radius::PANEL))
                .bg(p.sunken)
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(kit::icon(IconName::Sparkles, 13., p.accent_text))
                        .child(div().text_size(px(TypeScale::SM)).font_weight(FontWeight::SEMIBOLD).child(fill("update.notes", &[("version", &version)])))
                        .child(div().flex_1())
                        .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(date))),
                )
                .child(body)
        });

        let auto = div()
            .px(px(16.))
            .py(px(m.row_pad_y + 5.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(16.))
            .border_t_1()
            .border_color(p.line_soft)
            .child(
                div()
                    .flex_1()
                    .min_w(px(160.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().font_weight(FontWeight::MEDIUM).child(self.t("shell.update.autoCheck")))
                    .child(div().text_size(px(TypeScale::SM)).text_color(p.text_faint).child(self.t("shell.update.autoCheckHint"))),
            )
            .child(auto_switch);

        div()
            .id("settings-group-update")
            .test_support()
            .role(gpui_kit::Role::Group)
            .aria_label(self.t("update.title"))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(kit::eyebrow(self.t("update.title"), p))
            .child(kit::card(p).flex().flex_col().child(header).children(progress).children(notes).child(auto))
            .into_any_element()
    }

    /// 设置写盘的可见结果：失败给原因与就地重试，不把错误吞在后台任务里。
    /// 设置页“数据”分区的索引命名模板：普通、唯一、连接符三个输入框，下方实时预览示例名字。
    fn index_naming_rows(
        &mut self,
        naming: &crate::ddl::naming::IndexNaming,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<(SharedString, Option<SharedString>, AnyElement)> {
        let input_w = kit::ControlSize::PICKER_MIN_W;
        let normal = self.input_for(DraftField::IndexNamingNormal, crate::ddl::naming::DEFAULT_NORMAL, false, window, cx);
        let unique = self.input_for(DraftField::IndexNamingUnique, crate::ddl::naming::DEFAULT_UNIQUE, false, window, cx);
        let joiner = self.input_for(DraftField::IndexNamingJoiner, crate::ddl::naming::DEFAULT_JOINER, false, window, cx);
        let sample = ["user_id", "created_at"];
        let preview = |unique: bool| naming.render("orders", unique, &sample);
        let preview_line = div()
            .id("settings-index-naming-preview")
            .test_support()
            .flex()
            .flex_col()
            .gap(px(kit::Space::XS))
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(px(TypeScale::SM))
            .text_color(p.text_muted)
            .child(SharedString::from(preview(false)))
            .child(SharedString::from(preview(true)));
        vec![
            (self.t("ui.settings.indexNormal"), Some(self.t("ui.settings.indexPlaceholders")), div().w(px(input_w)).child(normal).into_any_element()),
            (self.t("ui.settings.indexUnique"), None, div().w(px(input_w)).child(unique).into_any_element()),
            (self.t("ui.settings.indexJoiner"), None, div().w(px(input_w)).child(joiner).into_any_element()),
            (self.t("ui.settings.indexPreview"), Some(self.t("ui.settings.indexPreviewHint")), preview_line.into_any_element()),
        ]
    }

    fn settings_save_row(&self, p: &Palette, m: &kit::Metrics, cx: &mut Context<Self>) -> AnyElement {
        match self.model.settings_save.clone() {
            SaveState::Idle => div().into_any_element(),
            SaveState::Saving => div()
                .id("settings-save-state")
                .test_support()
                .child(crate::ui::loading::inline_loading(self.spinners.element("settings-saving", Size::Small, Some(p.accent), cx), self.t("ui.settings.saving"), m, p))
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
        let max_panel_h = (self.model.viewport_height.max(480) as f32 - 104.).clamp(360., 560.);
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
            .max_h(px(max_panel_h))
            .flex()
            .flex_col()
            .bg(p.raised)
            .border_1()
            .border_color(p.line)
            .rounded(px(Radius::PANEL))
            .shadow_lg()
            .overflow_hidden()
            .capture_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                if this.model.shell.palette_open {
                    let query_empty = this.model.palette_query.is_empty();
                    let intercept = matches!(key, "up" | "down" | "enter" | "escape")
                        || (query_empty && matches!(key, "home" | "end"));
                    if intercept {
                        cx.stop_propagation();
                        this.overlay_key(event, window, cx);
                    }
                } else if this.model.shell.help_open && key == "escape" {
                    cx.stop_propagation();
                    this.overlay_key(event, window, cx);
                }
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        if help {
            panel = stack_frame(|| {
            // 固定高度：左侧分组导航与右侧列表各自滚动，长列表能完整滚到底。
            let mut panel = panel.h(px(max_panel_h));
            // 示例按键按本机平台写（macOS ⌘/，其他 Ctrl+/），与面板里列出的按键一致。
            let placeholder = i18n::fill(&self.model.text("ui.help.search"), &[("chord", &crate::app::keymap::display_chord("Mod+/"))]);
            let input = self.input_for(DraftField::HelpQuery, &placeholder, false, window, cx);
            if !self.help_focused {
                self.help_focused = true;
                self.fields.focus(DraftField::HelpQuery, window, cx);
            }
            let searching = !self.model.help_query.trim().is_empty();
            let sections = self.model.help_sections();
            // 分组导航：搜索时不限定分组，导航只做跳转提示，不高亮。
            let selected = if searching { None } else { Some(self.model.help_group) };
            let mut nav = div()
                .id("help-groups")
                .test_support()
                .role(gpui_kit::Role::List)
                .aria_label(self.t("ui.help.groups"))
                .flex_none()
                .w(px(132.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .p(px(8.))
                .border_r_1()
                .border_color(p.line)
                .scroll_area_y();
            let nav_item = |id: String, label: SharedString, count: usize, on: bool, command: String, cx: &mut Context<Self>| {
                div()
                    .id(SharedString::from(id))
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(label.clone())
                    .aria_selected(on)
                    .px(px(8.))
                    .h(px(m.menu_row_h))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.))
                    .rounded(px(Radius::CONTROL))
                    .cursor_pointer()
                    .text_size(px(TypeScale::SM))
                    .text_color(if on { p.accent_text } else { p.text })
                    .when(on, |this| this.bg(p.accent_soft))
                    .when(!on, |this| this.hover(|style| style.bg(p.hover)))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .child(div().text_color(p.text_faint).font_features(kit::tabular_figures()).child(SharedString::from(count.to_string())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.model.help_query.clear();
                        this.perform(&command, cx);
                        this.fields.focus(DraftField::HelpQuery, window, cx);
                        cx.notify();
                    }))
            };
            nav = nav.child(nav_item(
                "help-group-all".into(),
                self.t("ui.help.all"),
                crate::app::keymap::SHORTCUTS.len(),
                selected == Some(None),
                "help-group-all".into(),
                cx,
            ));
            for group in crate::app::keymap::GROUPS {
                let count = crate::app::keymap::SHORTCUTS.iter().filter(|shortcut| shortcut.group == group).count();
                nav = nav.child(nav_item(
                    format!("help-group-{group}"),
                    self.t(&format!("ui.help.{group}")),
                    count,
                    selected == Some(Some(group)),
                    format!("help-group:{group}"),
                    cx,
                ));
            }
            let mut list = div()
                .id("help-list")
                .test_support()
                .role(gpui_kit::Role::List)
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .scroll_area_y()
                .p(px(8.));
            if sections.is_empty() {
                list = list.child(
                    div()
                        .id("help-empty")
                        .test_support()
                        .p(px(16.))
                        .text_size(px(TypeScale::SM))
                        .text_color(p.text_faint)
                        .child(self.t("ui.help.noMatch")),
                );
            }
            let show_headings = sections.len() > 1 || searching || self.model.help_group.is_none();
            for (group, items) in sections {
                if show_headings {
                    list = list.child(div().px(px(8.)).pt(px(10.)).pb(px(4.)).child(kit::eyebrow(self.t(&format!("ui.help.{group}")), &p)));
                }
                for shortcut in items {
                    let label = self.t(shortcut.label_key);
                    let query = self.model.help_query.as_str();
                    let mac = cfg!(target_os = "macos");
                    let shown = crate::app::keymap::display_chord_for(shortcut.chord_for(mac), mac);
                    let label_hit = crate::app::keymap::label_match(label.as_ref(), query);
                    let key_hit = crate::app::keymap::chord_match(shortcut, query, mac);
                    let name = match label_hit {
                        Some((start, end)) => help_hit_text(format!("help-match-{}", shortcut.id), label.as_ref(), start, end, &p)
                            .flex_1()
                            .min_w_0()
                            .into_any_element(),
                        None => div().flex_1().min_w_0().child(label.clone()).into_any_element(),
                    };
                    let key = match key_hit {
                        Some((start, end)) => kit::kbd_element(help_hit_text(format!("help-key-match-{}", shortcut.id), &shown, start, end, &p), &p),
                        None => kit::kbd(shown, &p),
                    };
                    list = list.child(
                        div()
                            .id(SharedString::from(format!("help-row-{}", shortcut.id)))
                            .test_support()
                            .flex_none()
                            .px(px(8.))
                            .min_h(px(m.menu_row_h))
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .child(name)
                            .child(key),
                    );
                }
            }
            panel = panel
                .child(
                    div()
                        .flex_none()
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
                .child(
                    div()
                        .flex_none()
                        .px(px(12.))
                        .py(px(8.))
                        .border_b_1()
                        .border_color(p.line)
                        .child(input.prefix(kit::input_prefix(IconName::Search, 13., p.text_faint)).cleanable(true)),
                )
                .child(div().flex_1().min_h_0().flex().flex_row().child(nav).child(list));
            panel
            });
        } else {
            panel = stack_frame(|| {
            let mut panel = panel;
            let input = self.input_for(DraftField::PaletteQuery, &self.model.text("ui.palette.placeholder"), false, window, cx);
            if !self.palette_focused {
                self.palette_focused = true;
                self.fields.focus(DraftField::PaletteQuery, window, cx);
            }
            let active_scope = self.model.effective_palette_scope();
            let mut scopes = div()
                .id("palette-scopes")
                .test_support()
                .pt(px(8.))
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(4.));
            for scope in PaletteScope::ALL {
                let on = scope == active_scope;
                let cmd = scope.command();
                let label = self.t(scope.label_key());
                scopes = scopes.child(kit::filter_chip(
                    SharedString::from(cmd),
                    label,
                    None,
                    None,
                    None,
                    on,
                    &p,
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.perform(cmd, cx);
                        this.fields.focus(DraftField::PaletteQuery, window, cx);
                    }),
                ));
            }
            let entries = self.model.palette_entries();
            let list_max_h = (max_panel_h - 124.).clamp(180., 420.);
            let mut list = div()
                .id("palette-list")
                .test_support()
                .flex()
                .flex_col()
                .scroll_area_y()
                .scroll_handle(&self.palette_scroll)
                .p(px(6.))
                .max_h(px(list_max_h));
            if entries.is_empty() {
                let error = self.model.palette_error().to_owned();
                let empty_key = self.model.palette_empty_key();
                let filtered = !self.model.palette_query.trim().is_empty() || active_scope != PaletteScope::All;
                let new_query_btn = kit::secondary_button(
                    "palette-new-query",
                    Some(IconName::SquareTerminal),
                    self.t("shell.command.newQuery"),
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.model.shell.close_overlay();
                        this.perform("new-query", cx);
                    }),
                )
                .small();
                let empty: AnyElement = if empty_key == "ui.palette.error" && !error.is_empty() {
                    let actions = div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            kit::primary_button(
                                "palette-tree-retry",
                                Some(IconName::RefreshCw),
                                self.t("common.retry"),
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.model.shell.close_overlay();
                                    this.perform("refresh-tree", cx);
                                }),
                            )
                            .small(),
                        )
                        .child(new_query_btn);
                    div()
                        .id("palette-error")
                        .test_support()
                        .aria_label(SharedString::from(error.clone()))
                        .p(px(8.))
                        .child(kit::error_state_with_action(self.t(empty_key), SharedString::from(error), actions, &p))
                        .into_any_element()
                } else if filtered || empty_key == "ui.palette.noMatch" || empty_key == "ui.explorer.noMatch" {
                    let actions = div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            kit::secondary_button(
                                "palette-clear",
                                Some(IconName::X),
                                self.t("ui.palette.clearSearch"),
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.perform("palette-clear", cx);
                                    this.fields.focus(DraftField::PaletteQuery, window, cx);
                                }),
                            )
                            .small(),
                        )
                        .child(new_query_btn);
                    div()
                        .p(px(8.))
                        .child(kit::no_match_state_with_action(IconName::Search, self.t(empty_key), self.t("ui.palette.noMatchHint"), actions, &p))
                        .into_any_element()
                } else {
                    div()
                        .p(px(8.))
                        .child(kit::empty_state_with_action(IconName::Compass, self.t(empty_key), self.t("ui.palette.emptyHint"), new_query_btn, &p))
                        .into_any_element()
                };
                list = list.child(empty);
            }
            // 大目录只构建可见行，Home/End 与鼠标滚动仍能访问全部结果。
            let entries = if entries.len() > 200 {
                let mut rows = Vec::with_capacity(entries.len() + 6);
                let mut last = "";
                let mut selected_row = 0;
                for (index, entry) in entries.iter().enumerate() {
                    if entry.group != last {
                        rows.push((None, entry.group));
                        last = entry.group;
                    }
                    if index == self.model.palette_index { selected_row = rows.len(); }
                    rows.push((Some(index), entry.group));
                }
                let seen = (self.model.palette_index, self.model.palette_query.clone(), active_scope);
                if self.palette_scroll_seen.as_ref() != Some(&seen) {
                    self.palette_virtual_scroll.scroll_to_item(selected_row, gpui_kit::ScrollStrategy::Nearest);
                    self.palette_scroll_seen = Some(seen);
                }
                let row_h = m.row_h + 4.;
                let owner = cx.entity().downgrade();
                let virtual_list = gpui_kit::uniform_list("palette-virtual-list", rows.len(), move |range, _, cx| {
                    owner.update(cx, |this, cx| {
                        range.map(|index| match rows[index] {
                            (Some(entry_index), _) => this.palette_row(entry_index, entries[entry_index].clone(), cx),
                            (None, group) => div().h(px(row_h)).flex().items_center().px(px(10.))
                                .text_size(px(TypeScale::XS)).text_color(Palette::of(cx).text_faint)
                                .child(this.t(super::model::palette_kind_key(group))).into_any_element(),
                        }).collect::<Vec<_>>()
                    }).unwrap_or_default()
                }).track_scroll(&self.palette_virtual_scroll).h(px(list_max_h - 12.));
                // 虚拟列表自己滚动：在同大小的 relative 父层上叠可见滚动条（ADR-8）。
                list = list.child(
                    div()
                        .relative()
                        .child(virtual_list)
                        .child(kit::scrollbar_overlay("palette-virtual-bar", &self.palette_virtual_scroll, kit::ScrollbarAxis::Vertical)),
                );
                Vec::new()
            } else { entries };
            let mut last_group = "";
            let mut child_count = 0usize;
            let mut selected_child: Option<usize> = None;
            for (index, entry) in entries.into_iter().enumerate() {
                let selected = index == self.model.palette_index;
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
                    child_count += 1;
                }
                if selected {
                    selected_child = Some(if index == 0 { 0 } else { child_count });
                }
                let row = self.palette_row(index, entry, cx);
                list = list.child(row);
                child_count += 1;
            }
            let seen_key = (self.model.palette_index, self.model.palette_query.clone(), active_scope);
            if self.palette_scroll_seen.as_ref() != Some(&seen_key) {
                self.palette_scroll_seen = Some(seen_key);
                if let Some(child_ix) = selected_child {
                    self.palette_scroll.scroll_to_item(child_ix);
                }
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
                        .child(
                            input
                                .appearance(false)
                                .prefix(kit::input_prefix(IconName::Search, 15., p.text_faint))
                                .cleanable(true),
                        )
                        .child(scopes),
                )
                .child(list.flex_1().min_h_0())
                .child(footer);
            panel
            });
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
    fn palette_row(&self, index: usize, entry: super::model::PaletteEntry, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = self.metrics();
        let selected = index == self.model.palette_index;
        let id = entry.id.clone();
        // 快捷键来自菜单同源（COMMANDS → SHORTCUTS），类型标注来自条目种类。
        let chord = entry.shortcut;
        let type_label = self.t(super::model::palette_kind_key(entry.group));
        let show_kind = entry.group != "command";
        let detail = entry.detail.clone();
        let icon = palette_icon(&entry.id);
        stack_frame(|| {
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
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().flex_1().min_w_0().truncate().text_size(px(TypeScale::MD)).child(SharedString::from(entry.label)))
                        // 上下文（库.模式 · 连接 / 引擎 · 库）：选中后去哪一目了然。
                        .when(!detail.is_empty(), |this| {
                            let full_detail = SharedString::from(detail.clone());
                            this.child(
                                div()
                                    .id(SharedString::from(format!("pal-detail-{index}")))
                                    .test_support()
                                    .tooltip_text(full_detail.clone())
                                    .flex_none()
                                    .max_w(px(if show_kind { 190. } else { 256. }))
                                    .truncate()
                                    .text_size(px(TypeScale::XS))
                                    .text_color(if selected { p.accent_text } else { p.text_faint })
                                    .child(full_detail),
                            )
                        })
                        // 类型标注：表 / 视图 / 连接 / 库 / 保存的查询。
                        // 也是无障碍名称的一部分：读屏能听到这一行是什么类型。
                        // 命令占了面板的大半，每行都写一遍“命令”只是噪音；它们由分组标题和快捷键认出。
                        .when(show_kind, |this| this.child(
                            div()
                                .id(SharedString::from(format!("pal-kind-{index}")))
                                .test_support()
                                .flex_none()
                                .text_size(px(TypeScale::XS))
                                .text_color(if selected { p.accent_text } else { p.text_faint })
                                .aria_label(type_label.clone())
                                .child(type_label),
                        )),
                )
                .when_some(chord, |this, chord| this.child(kit::kbd(kit::shortcut(&chord.replace("Mod+", "Ctrl+")), &p)))
                .when(selected && chord.is_none(), |this| this.child(kit::icon(IconName::CornerDownLeft, 12., p.accent_text)))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.model.shell.close_overlay();
                    this.perform(&id, cx);
                    if !this.model.shell.palette_open && !this.model.shell.help_open {
                        this.restore_focus(window, cx);
                    }
                }))
                .into_any_element()
        })
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
        let mut layer = menu_backdrop(cx).child(list);
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
    #[allow(clippy::too_many_arguments)]
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
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            // 菜单自己身上的右键不算“点在外面”，不关菜单。
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());
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
            // 禁用原因：会话给出的具体文案优先，其次是固定的说明键。
            let disabled_reason: Option<SharedString> = item.reason.clone().map(SharedString::from).or_else(|| item.reason_key.map(|key| self.t(key)));
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
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .when_some(shortcut, |this, shortcut| this.child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(shortcut)))
                    .when(has_children, |this| this.child(kit::icon(IconName::ChevronRight, 12., p.text_faint)))
                    .when_some(disabled_reason.filter(|_| !enabled), |this, reason| this.tooltip_text(reason))
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
            if window_scope && this.allow_close(cx) {
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
        // 标题点名要删的是哪一条；正在用它的标签数一并说明，删除后这些标签要重新选执行目标。
        let selected = self.model.connections.selected.clone().unwrap_or_default();
        let name = self.model.connections.rows.iter().find(|row| row.saved.id == selected).map(|row| row.saved.name.clone());
        let title = match name {
            Some(name) => SharedString::from(i18n::fill(&self.model.text("connections.deleteDialog.titleNamed"), &[("name", &name)])),
            None => self.t("connections.deleteDialog.title"),
        };
        let bound = self.model.tabs.iter().filter(|tab| tab.connection_id.as_deref() == Some(selected.as_str())).count();
        let body = if bound > 0 {
            SharedString::from(format!(
                "{}
{}",
                self.model.text("connections.deleteDialog.willKeepDb"),
                i18n::fill(&self.model.text("connections.deleteDialog.boundTabs"), &[("count", &bound.to_string())])
            ))
        } else {
            self.t("connections.deleteDialog.willKeepDb")
        };
        let dialog = dialog_card("delete-connection", IconName::Trash, p.danger, title, body, &p).child(
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

    /// “打开文件”认不出类型时询问打开方式：SQL 文本进编辑器，或按数据库文件建连接。
    fn open_choice_layer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let name = self
            .model
            .open_choices
            .front()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let body = i18n::fill(&self.model.text("ui.openFile.askBody"), &[("name", &name)]);
        let dialog = dialog_card("open-choice", IconName::FolderOpen, p.accent, self.t("ui.openFile.askTitle"), SharedString::from(body), &p).child(
            div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap(px(8.))
                .child(kit::secondary_button("cmd-open-choice-cancel", None, self.t("common.cancel"), act("open-choice-cancel", cx)))
                .child(kit::secondary_button("cmd-open-as-database", Some(IconName::Database), self.t("ui.openFile.asDatabase"), act("open-as-database", cx)))
                .child(kit::primary_button("cmd-open-as-sql", Some(IconName::FileText), self.t("ui.openFile.asSql"), act("open-as-sql", cx))),
        );
        backdrop("open-choice-backdrop", 0.32, cx).flex().items_center().justify_center().px(px(20.)).child(dialog).into_any_element()
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
        let viewer_h = (self.model.viewport_height.max(520) as f32 - 220.).clamp(220., 520.);
        let body: AnyElement = if view.loading {
            crate::ui::loading::loading_state(self.spinners.element("ddl-loading", Size::Small, Some(p.accent), cx), self.t("catalog.ddl.loading"), &p)
                .py(px(28.))
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
                .h(px(viewer_h))
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
            let detail = if error.trim().is_empty() { text.clone() } else { error };
            div()
                .id("ddl-viewer-error")
                .test_support()
                .aria_label(SharedString::from(text))
                .child(kit::error_state_with_action(
                    self.t("catalog.ddl.failed"),
                    SharedString::from(detail),
                    kit::secondary_button("cmd-ddl-retry", Some(IconName::RefreshCw), self.t("common.retry"), act("ddl-retry", cx)),
                    &p,
                ))
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
            .min_w_0()
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
                    .child(div().flex_1().min_w_0().truncate().child(kit::title(title, TypeScale::LG, &p)))
                    .child(kit::icon_button("ddl-header-close", IconName::X, self.t("common.close"), act("ddl-close", cx))),
            )
            .when_some(rebuilt, |this, rebuilt| this.child(rebuilt))
            .child(body)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(px(8.))
                    .child(kit::secondary_button("cmd-ddl-close", None, self.t("common.close"), act("ddl-close", cx)))
                    .when(has_text, |this| {
                        this.child(kit::secondary_button(
                            "cmd-ddl-open-query",
                            Some(IconName::SquarePen),
                            self.t("catalog.ddl.openInQuery"),
                            act("ddl-open-query", cx),
                        ))
                        .child(kit::primary_button("cmd-ddl-copy", Some(IconName::Copy), self.t("catalog.tree.copyDdl"), act("ddl-copy", cx)))
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
        let facts = stack_frame(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .text_size(px(TypeScale::SM))
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(div().w(px(84.)).flex_none().text_color(p.text_muted).child(self.t("catalog.danger.target")))
                        .child(div().id("danger-target").min_w_0().truncate().child(SharedString::from(name.clone()))),
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
                )
                .into_any_element()
        });
        let m = self.metrics();
        let mut content = div().flex().flex_col().gap(px(10.)).child(facts);
        if prompt.kind == super::danger::DangerKind::Rename {
            let input = self.input_for(DraftField::DangerNewName, &self.model.text("catalog.danger.newName"), false, window, cx);
            let running = prompt.running;
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .on_key_down(cx.listener(move |this, event: &gpui_kit::KeyDownEvent, _, cx| {
                        if event.keystroke.key == "enter" && !running {
                            cx.stop_propagation();
                            this.perform("danger-preview", cx);
                        }
                    }))
                    .child(div().flex_1().min_w_0().child(input))
                    .child(kit::secondary_button("cmd-danger-preview", Some(IconName::Eye), self.t("catalog.danger.preview"), act("danger-preview", cx)).disabled(prompt.running)),
            );
        }
        if prompt.loading {
            content = content.child(crate::ui::loading::inline_loading(self.spinners.element("danger-loading", Size::Small, Some(p.accent), cx), self.t("catalog.danger.loading"), &m, &p));
        } else if prompt.plan.is_none() && prompt.error.is_none() && prompt.kind == super::danger::DangerKind::Rename {
            content = content.child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(self.t("catalog.danger.renameHint")));
        }
        // 计划文字与错误由执行层按中文生成；英文界面按句型表逐条换成英文（数据库原文不动）。
        let locale = if self.model.shell.locale == crate::app::Locale::En { i18n::Locale::En } else { i18n::Locale::ZhCn };
        if let Some(plan) = prompt.plan.as_ref() {
            let plan_body = stack_frame(|| {
                let mut column = div().flex().flex_col().gap(px(10.));
                if !plan.sql.is_empty() {
                    let mono = cx.theme().mono_font_family.clone();
                    column = column.child(kit::eyebrow(self.t("catalog.danger.sql"), &p)).child(
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
                column = column.child(risks);
                if !plan.blocked.is_empty() {
                    let blocked: Vec<String> = plan.blocked.iter().map(|text| crate::ddl::english::localize(text, locale)).collect();
                    let blocked = blocked.join(" ");
                    column = column.child(
                        div()
                            .id("danger-blocked")
                            .test_support()
                            .aria_label(SharedString::from(blocked.clone()))
                            .child(banner(IconName::Ban, SharedString::from(blocked), p.danger, p.danger_soft)),
                    );
                }
                column.into_any_element()
            });
            content = content.child(plan_body);
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
        if self.danger_focused_epoch != Some(prompt.epoch) {
            self.danger_focused_epoch = Some(prompt.epoch);
            if prompt.kind == super::danger::DangerKind::Rename {
                self.fields.focus(DraftField::DangerNewName, window, cx);
            } else if prompt.prod {
                self.fields.focus(DraftField::DangerConfirm, window, cx);
            }
        }
        if prompt.running {
            content = content.child(crate::ui::loading::inline_loading(self.spinners.element("danger-running", Size::Small, Some(p.accent), cx), self.t("catalog.danger.running"), &m, &p));
        }
        let ready = prompt.execute_ready();
        let label = if prompt.armed { self.t("catalog.danger.confirmAgain") } else { self.t("catalog.danger.execute") };
        stack_frame(|| {
            let dialog = dialog_card("danger-confirm", IconName::TriangleAlert, p.danger, title, SharedString::from(name), &p)
                .test_support()
                .max_w(px(560.))
                .child(div().id("danger-body").max_h(px(420.)).scroll_area_y().child(content))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(kit::secondary_button("cmd-danger-cancel", None, self.t("common.cancel"), act("danger-cancel", cx)))
                        .child(kit::danger_button("cmd-danger-execute", Some(IconName::TriangleAlert), label, act("danger-execute", cx)).disabled(!ready)),
                );
            backdrop("danger-backdrop", 0.32, cx).flex().items_center().justify_center().px(px(20.)).child(dialog).into_any_element()
        })
    }

    /// 连接缺密码或密码被拒：就地要密码并重连，挂起的动作随后重放。
    fn password_layer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let Some(prompt) = self.model.password_prompt.clone() else {
            return div().into_any_element();
        };
        let row = self.model.connections.rows.iter().find(|row| row.saved.id == prompt.id).cloned();
        let name = row.as_ref().map(|row| row.saved.name.clone()).unwrap_or_default();
        let detail = row.as_ref().map(connection_detail).unwrap_or_default();
        let title = SharedString::from(i18n::fill(&self.model.text("ui.passwordPrompt.title"), &[("name", &name)]));
        let (input, _) = self.field_input(DraftField::PromptPassword, &self.model.text("ui.conn.password"), true, window, cx);
        if self.password_focused_epoch != Some(prompt.epoch) {
            self.password_focused_epoch = Some(prompt.epoch);
            self.fields.focus(DraftField::PromptPassword, window, cx);
        }
        let note: AnyElement = if prompt.rejected {
            div()
                .id("password-rejected")
                .test_support()
                .aria_label(self.t("ui.passwordPrompt.rejected"))
                .child(banner(IconName::TriangleAlert, self.t("ui.passwordPrompt.rejected"), p.danger, p.danger_soft))
                .into_any_element()
        } else {
            let key = if prompt.save_password { "ui.conn.savePasswordOn" } else { "ui.passwordPrompt.missing" };
            div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(self.t(key)).into_any_element()
        };
        let save_on = prompt.save_password;
        let save_toggle = kit::interactive_states(div().id("cmd-password-save"), &p, false)
            .test_support()
            .role(gpui_kit::Role::Switch)
            .aria_label(self.t("ui.passwordPrompt.save"))
            .aria_selected(save_on)
            .self_start()
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(Radius::CONTROL))
            .child(switch_visual(save_on, &p))
            .child(div().text_size(px(TypeScale::SM)).text_color(p.text).child(self.t("ui.passwordPrompt.save")))
            .on_click(act("password-toggle-save", cx));
        let ready = !prompt.text.is_empty();
        let dialog = dialog_card("password-prompt", IconName::KeyRound, p.accent, title, SharedString::from(detail), &p)
            .test_support()
            .child(div().flex().flex_col().gap(px(10.)).child(input.mask_toggle()).child(save_toggle).child(note))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(kit::tool_button("cmd-password-edit", Some(IconName::Pencil), self.t("ui.passwordPrompt.edit"), act("password-edit", cx)))
                    .child(div().flex_1())
                    .child(kit::secondary_button("cmd-password-cancel", None, self.t("common.cancel"), act("password-cancel", cx)))
                    .child(kit::primary_button("cmd-password-submit", Some(IconName::Plug), self.t("ui.passwordPrompt.connect"), act("password-submit", cx)).disabled(!ready)),
            );
        backdrop("password-backdrop", 0.32, cx).flex().items_center().justify_center().px(px(20.)).child(dialog).into_any_element()
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
            .child(div().id("exchange-scroll").flex_1().min_h_0().scroll_area_y().child(self.exchange.clone()));
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

/// 对象树行尾的悬停快捷动作图标：平时透明、行悬停时浮现，不抢树自身的 Tab 停靠点。
/// 右键菜单开着时（`tip == false`）不挂提示，免得提示浮层盖住菜单。
fn explorer_hover_icon(
    id: SharedString,
    group: SharedString,
    icon: IconName,
    label: SharedString,
    tip: bool,
    p: &Palette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .when(tip, |this| this.tooltip_text(label))
        .flex_none()
        .size(px(18.))
        .rounded(px(Radius::CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .opacity(0.)
        .group_hover(group, |style| style.opacity(1.))
        .hover(|style| style.bg(p.active))
        .child(kit::icon(icon, 12., p.text_muted))
        .on_click(on_click)
        .into_any_element()
}

/// 搜索命中的一段：前缀与后缀保持普通文本，中间用当前主题的选中底色强调。
/// 长名称时前缀与后缀各自省略，命中段保持完整可见：命中在长名称末尾也不会被裁掉。
fn highlight_label(id: &str, label: &SharedString, start: usize, end: usize, p: &Palette) -> AnyElement {
    let text = label.as_ref();
    let head = if start > 0 { SharedString::from(text[..start].to_owned()) } else { SharedString::from("") };
    let hit = SharedString::from(text[start..end].to_owned());
    let tail = if end < text.len() { SharedString::from(text[end..].to_owned()) } else { SharedString::from("") };
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .overflow_hidden()
        .text_size(px(TypeScale::SM))
        .child(div().min_w_0().flex_shrink(1.).truncate().child(head))
        .child(
            div()
                .id(SharedString::from(format!("nav-match-{id}")))
                .test_support()
                .flex_none()
                .bg(p.selection)
                .rounded(px(Radius::TAG))
                .text_color(p.accent_text)
                .font_weight(FontWeight::SEMIBOLD)
                .child(hit),
        )
        .child(div().flex_1().min_w_0().truncate().child(tail))
        .into_any_element()
}

/// 快捷键面板搜索命中的一段：动作名或组合键里命中的字用选中底色与粗体强调，其余保持普通文字。
/// 命中段的无障碍名就是命中的文字。
fn help_hit_text(id: String, text: &str, start: usize, end: usize, p: &Palette) -> gpui_kit::Div {
    let (start, end) = if start < end && end <= text.len() && text.is_char_boundary(start) && text.is_char_boundary(end) { (start, end) } else { (0, text.len()) };
    let hit = SharedString::from(text[start..end].to_owned());
    div()
        .flex()
        .items_center()
        .overflow_hidden()
        .when(start > 0, |this| this.child(SharedString::from(text[..start].to_owned())))
        .child(
            div()
                .id(SharedString::from(id))
                .test_support()
                .aria_label(hit.clone())
                .flex_none()
                .bg(p.selection)
                .rounded(px(Radius::TAG))
                .text_color(p.accent_text)
                .font_weight(FontWeight::SEMIBOLD)
                .child(hit),
        )
        .when(end < text.len(), |this| this.child(SharedString::from(text[end..].to_owned())))
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

/// 状态栏提示的显示时长。
const NOTICE_TTL: std::time::Duration = std::time::Duration::from_secs(8);

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
        WorkItemKind::Overview => IconName::LayoutDashboard,
    }
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
pub(super) fn section_label(text: SharedString, p: &Palette) -> AnyElement {
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
pub(super) fn status_parts(status: LinkStatus, p: &Palette) -> (gpui_kit::Hsla, &'static str) {
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
        .child(div().flex_1().min_w_0().text_size(px(TypeScale::XS)).text_color(p.danger).child(SharedString::from(text)))
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

/// 测试连接的结论：一句话说结果，失败时写出卡在哪一步、服务端原话和处理建议，
/// 下面一行列出走过的每个阶段——经跳板机、TLS 的连接一眼看出是哪一段不通。
pub(super) fn test_verdict(report: &crate::connections::stages::TestReport, headline: &str, locale: i18n::Locale, p: &Palette) -> AnyElement {
    use crate::connections::stages::{
        localize_report_hint, localize_report_summary, localize_stage_label, localize_stage_message, StageStatus, TestStatus,
    };
    let (fg, bg, mark) = match report.status {
        TestStatus::Ok => (p.ok, p.ok_soft, IconName::CircleCheck),
        TestStatus::Wait => (p.warn, p.warn_soft, IconName::TriangleAlert),
        TestStatus::Err => (p.danger, p.danger_soft, IconName::CircleAlert),
    };
    let blocked = report.stages.iter().find(|stage| matches!(stage.status, StageStatus::Err | StageStatus::Wait));
    let blocked_label = blocked.map(|stage| localize_stage_label(stage, locale));
    let title = match (report.status, blocked_label.as_deref()) {
        (TestStatus::Ok, _) => match report.summary.as_deref().map(|s| localize_report_summary(s, locale)).filter(|summary| !summary.is_empty()) {
            Some(summary) => format!("{headline} · {summary}"),
            None => headline.to_owned(),
        },
        (TestStatus::Err, Some(label)) if !label.trim().is_empty() => format!("{headline} · {}", label.trim()),
        _ => headline.to_owned(),
    };
    let detail = blocked.map(|stage| localize_stage_message(stage, locale)).filter(|message| !message.is_empty());
    let hint = localize_report_hint(report, locale).filter(|hint| Some(hint.as_str()) != detail.as_deref());
    let mut stages = div().flex().flex_wrap().gap_x(px(12.)).gap_y(px(2.));
    let mut shown = 0;
    for stage in &report.stages {
        let (icon, color) = match stage.status {
            StageStatus::Ok => (IconName::Check, p.ok),
            StageStatus::Err => (IconName::X, p.danger),
            StageStatus::Wait => (IconName::TriangleAlert, p.warn),
            // 前一步失败后没执行的阶段不列：它们没有结论。
            StageStatus::Skipped => continue,
        };
        shown += 1;
        stages = stages.child(
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(px(TypeScale::XS))
                .text_color(p.text_muted)
                .child(kit::icon(icon, 11., color))
                .child(SharedString::from(localize_stage_label(stage, locale))),
        );
    }
    let spoken = [Some(title.clone()), detail.clone(), hint.clone()].into_iter().flatten().collect::<Vec<_>>().join("\n");
    div()
        .id("conn-test-verdict")
        .test_support()
        .role(gpui_kit::Role::Group)
        .aria_label(SharedString::from(spoken))
        .px(px(12.))
        .py(px(8.))
        .flex()
        .items_start()
        .gap(px(8.))
        .rounded(px(Radius::PANEL))
        .bg(bg)
        .border_1()
        .border_color(fg.opacity(0.26))
        .child(div().flex_none().pt(px(1.)).child(kit::icon(mark, 15., fg)))
        .child(
            div()
                .id("conn-test-verdict-text")
                .flex_1()
                .min_w_0()
                // 驱动的报错可能很长：结论条自己滚，不把表单主体挤没。
                .max_h(px(132.))
                .scroll_area_y()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().text_size(px(TypeScale::SM)).font_weight(FontWeight::MEDIUM).text_color(fg).child(SharedString::from(title)))
                .when_some(detail, |this, detail| this.child(div().text_size(px(TypeScale::SM)).text_color(p.text).child(SharedString::from(detail))))
                .when_some(hint, |this, hint| this.child(div().text_size(px(TypeScale::XS)).text_color(p.text_muted).child(SharedString::from(hint))))
                .when(shown > 1, |this| this.child(stages)),
        )
        .into_any_element()
}

fn banner(icon: IconName, text: SharedString, fg: gpui_kit::Hsla, bg: gpui_kit::Hsla) -> AnyElement {
    div()
        .px(px(12.))
        .py(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(Radius::PANEL))
        .bg(bg)
        .border_1()
        .border_color(fg.opacity(0.26))
        .text_color(fg)
        .text_size(px(TypeScale::SM))
        .child(kit::icon(icon, 15., fg))
        .child(div().flex_1().min_w_0().child(text))
        .into_any_element()
}

/// 模态按钮的语气：普通、主操作、危险（放弃修改）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ModalTone {
    Plain,
    Primary,
    Danger,
}

/// 模态按钮此刻能不能点：Idle 正常；Working 是它自己的请求在路上（转圈）；Blocked 是别的请求在路上。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ModalBusy {
    Idle,
    Working,
    Blocked,
}

fn switch_visual(on: bool, p: &Palette) -> AnyElement {
    kit::switch_visual(on, p)
}

fn dialog_card(id: &'static str, icon: IconName, tone: gpui_kit::Hsla, title: SharedString, body: SharedString, p: &Palette) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .role(gpui_kit::Role::Dialog)
        .aria_label(title.clone())
        .occlude()
        .w_full()
        .max_w(px(440.))
        .min_w_0()
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
                .child(
                    div()
                        .flex_none()
                        .size(px(36.))
                        .rounded(px(Radius::PANEL))
                        .bg(tone.opacity(0.14))
                        .border_1()
                        .border_color(tone.opacity(0.24))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(kit::icon(icon, 17., tone)),
                )
                .child(div().flex().flex_col().gap(px(6.)).min_w_0().child(kit::title(title, TypeScale::LG, p)).child(div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(body))),
        )
}

/// 全窗口遮罩；点空白处关闭当前浮层。
/// 模态里分段选择的槽：浅底圆角，选中项浮起（与 kit::segmented_sized 同一外观）。
/// 选项多时整槽换行，不被裁切。
fn segment_track(p: &Palette) -> gpui_kit::Div {
    div()
        .flex()
        .flex_wrap()
        .flex_none()
        .self_start()
        .max_w_full()
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(Radius::CONTROL))
        .bg(p.sunken)
        .border_1()
        .border_color(p.line_soft)
}

/// 右键菜单的遮罩：左键、右键点在菜单外，或在任何位置滚动，菜单都关闭。
/// 右键只关菜单，不在遮罩下面再弹一个新菜单；滚动不会滚动菜单下面的页面。
fn menu_backdrop(cx: &mut Context<Workbench>) -> impl IntoElement + gpui_kit::ParentElement {
    let close = |entity: Entity<Workbench>| {
        move |window: &mut Window, cx: &mut App| {
            entity.update(cx, |this, cx| {
                if this.model.close_menu() {
                    this.restore_focus(window, cx);
                }
                cx.notify();
            });
        }
    };
    let left = close(cx.entity());
    let right = close(cx.entity());
    let wheel = close(cx.entity());
    div()
        .id("context-backdrop")
        .test_support()
        .absolute()
        .inset_0()
        .occlude()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            left(window, cx);
        })
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            cx.stop_propagation();
            right(window, cx);
        })
        .on_scroll(move |_, window, cx| {
            cx.stop_propagation();
            wheel(window, cx);
        })
}

pub(super) fn backdrop(id: &'static str, dim: f32, cx: &mut Context<Workbench>) -> gpui_kit::Stateful<gpui_kit::Div> {
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
                // 若模态仍开着（例如脏表单刚弹出“放弃修改？”确认），键盘必须留在模态内，不能还焦给底页。
                if !this.model.has_blocking_dialog() {
                    this.restore_focus(window, cx);
                }
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
        "open-admin" => IconName::Gauge,
        "open-exchange" => IconName::ArrowDownUp,
        "new-connection" => IconName::Plus,
        "open-file" => IconName::FolderOpen,
        "history" => IconName::Clock,
        "save-query" => IconName::Save,
        "connections" => IconName::Plug,
        "settings" => IconName::Settings,
        "sidebar" => IconName::PanelLeft,
        "theme-light" => IconName::Sun,
        "theme-dark" => IconName::Moon,
        "theme-system" => IconName::Monitor,
        "help" => IconName::Keyboard,
        "check-updates-show" => IconName::Download,
        _ => IconName::ChevronRight,
    }
}

/// 专项工作台（键值、文档）连接对应的浏览器命令、文案键和元素 id；关系型连接没有。
pub(super) fn browser_command(row: &crate::connections::ConnectionRecord) -> Option<(String, &'static str, String)> {
    let name = row.saved.name.replace('|', " ");
    match row.saved.engine.workbench() {
        WorkbenchKind::Redis => {
            let default_database = crate::adapters::adapter(row.saved.engine).meta().default_database();
            let database = row.saved.database.clone().unwrap_or_else(|| default_database.to_owned());
            Some((format!("open-redis:{}|{name}|{database}", row.saved.id), "redis.open", format!("side-open-redis-{}", row.saved.id)))
        }
        WorkbenchKind::Mongo => Some((format!("open-mongo:{}|{name}", row.saved.id), "mongo.open", format!("side-open-mongo-{}", row.saved.id))),
        WorkbenchKind::Relational => None,
    }
}

/// 列表里显示的文件路径：长路径只留最后两段（目录/文件名），完整路径放在提示里。
pub(super) fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|part| !part.is_empty()).collect();
    if parts.len() <= 2 || path.chars().count() <= 40 {
        return path.to_owned();
    }
    format!("…/{}/{}", parts[parts.len() - 2], parts[parts.len() - 1])
}

pub(super) fn connection_detail(row: &crate::connections::ConnectionRecord) -> String {
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

/// 树节点的悬停全路径。库、模式、对象节点的路径本身已以自己的名字结尾，
/// 再拼一次名字就成了 “127.0.0.1 / rillsql / rillsql”。
pub(super) fn nav_row_tooltip(path: &str, name: &str) -> String {
    let path = path.trim_end();
    let own = path == name || path.ends_with(&format!(" / {name}")) || path.ends_with(&format!(".{name}"));
    if own || name.is_empty() { path.to_owned() } else { format!("{path} / {name}") }
}

/// 本地化分组节点（表 / 视图 / 列 / 索引）的路径末尾原本带内部英文代号（如 `/ view`），
/// 悬停提示换成当前语言标签，不再露出内部标识符。
pub(super) fn localized_nav_tooltip(path: &str, name: &str, label: &str, localized: bool) -> String {
    if !localized || label.is_empty() {
        return nav_row_tooltip(path, name);
    }
    let path = path.trim_end();
    if !name.is_empty() {
        if path == name {
            return label.to_owned();
        }
        let suffix = format!(" / {name}");
        if let Some(prefix) = path.strip_suffix(&suffix) {
            return nav_row_tooltip(prefix, label);
        }
    }
    nav_row_tooltip(path, label)
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
/// 连接的环境 id 归一：空串与 none 都算“无标签”。
pub(super) fn env_key(env: &str) -> &str {
    let env = env.trim();
    if env.is_empty() { "none" } else { env }
}

pub(super) fn env_label(text: impl Fn(&str) -> String, env: &str, tags: &[EnvTag]) -> String {
    let trimmed = env.trim();
    if trimmed.is_empty() || trimmed == crate::connections::model::ENV_NONE {
        return text("ui.conn.env.none");
    }
    if let Some(tag) = tags.iter().find(|tag| tag.id == trimmed) {
        let label = tag.label.trim();
        if !label.is_empty() {
            return label.to_owned();
        }
    }
    let key = format!("ui.conn.env.{trimmed}");
    let value = text(&key);
    if value == key { trimmed.to_owned() } else { value }
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

/// 名称留空时保存所用的名字：与连接配置保存时的取名规则一致（文件库取文件名，其余取主机）。
fn default_connection_name(draft: &crate::connections::model::ConnectionDraft) -> Option<String> {
    let name = if crate::adapters::adapter(draft.engine).meta().file_based() {
        let path = draft.file_path.trim();
        std::path::Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path).to_owned()
    } else {
        draft.host.trim().to_owned()
    };
    (!name.is_empty()).then_some(name)
}

/// 连接表单里字段问题的名字对应到哪个输入框：打开表单时键盘落在第一个出问题的字段上。
pub(super) fn issue_field(name: &str) -> Option<DraftField> {
    Some(match name {
        // “credentials” 是用户名与密码一起被拒：先改的通常是密码。
        "password" | "credentials" => DraftField::Password,
        "host" => DraftField::Host,
        "port" => DraftField::Port,
        "username" => DraftField::Username,
        "database" => DraftField::Database,
        "name" => DraftField::Name,
        "passphrase" => DraftField::Passphrase,
        "replica" => DraftField::Replica,
        "file_path" => DraftField::File,
        "ca_file" => DraftField::CaFile,
        "ssh_host" => DraftField::SshHost,
        "ssh_port" => DraftField::SshPort,
        "ssh_user" => DraftField::SshUser,
        "ssh_password" => DraftField::SshPassword,
        _ => return None,
    })
}
