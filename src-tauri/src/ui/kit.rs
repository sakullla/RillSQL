//! RillSQL 设计底座：品牌主题、色板与通用控件。
//!
//! 所有页面都从这里取颜色和控件，不在业务视图里写死色值。
//! 浅色与深色是两套分别调过的色板，不是机械反色。

use std::collections::HashMap;
use std::hash::Hash;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, Theme, ThemeMode};
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, ElementId, Entity, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
    hsla, px, rgb,
};

/// 一套色板。字段名按用途取，不按色相取。
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// 工作区底色（编辑器、表格、表单）。
    pub surface: Hsla,
    /// 外壳底色（顶栏、侧栏、状态栏、标签条）。
    pub chrome: Hsla,
    /// 比外壳再深/浅一档，用于活动栏。
    pub rail: Hsla,
    /// 浮层与卡片。
    pub raised: Hsla,
    pub hover: Hsla,
    pub active: Hsla,
    pub line: Hsla,
    pub line_soft: Hsla,
    pub line_strong: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub text_faint: Hsla,
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub accent_active: Hsla,
    pub accent_soft: Hsla,
    pub accent_text: Hsla,
    pub selection: Hsla,
    pub zebra: Hsla,
    pub ok: Hsla,
    pub warn: Hsla,
    pub danger: Hsla,
    pub info: Hsla,
    pub ok_soft: Hsla,
    pub warn_soft: Hsla,
    pub danger_soft: Hsla,
    pub dark: bool,
}

fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

fn alpha(value: u32, a: f32) -> Hsla {
    let mut color: Hsla = rgb(value).into();
    color.a = a;
    color
}

impl Palette {
    pub fn light() -> Self {
        Self {
            surface: hex(0xffffff),
            chrome: hex(0xf6f7f9),
            rail: hex(0xeef0f4),
            raised: hex(0xffffff),
            hover: hex(0xeceef3),
            active: hex(0xe2e5ec),
            line: hex(0xe4e7ec),
            line_soft: hex(0xf0f2f5),
            line_strong: hex(0xd0d5dd),
            text: hex(0x16191f),
            text_muted: hex(0x5d6675),
            text_faint: hex(0x8d95a3),
            accent: hex(0x4f5bd5),
            accent_hover: hex(0x4350c4),
            accent_active: hex(0x3a45ad),
            accent_soft: hex(0xeceefc),
            accent_text: hex(0x3a45ad),
            selection: alpha(0x4f5bd5, 0.14),
            zebra: hex(0xfafbfc),
            ok: hex(0x13925d),
            warn: hex(0xb7791f),
            danger: hex(0xd23f3f),
            info: hex(0x2f7dd1),
            ok_soft: hex(0xe6f6ee),
            warn_soft: hex(0xfdf3e1),
            danger_soft: hex(0xfdecec),
            dark: false,
        }
    }

    pub fn dark() -> Self {
        Self {
            surface: hex(0x17181c),
            chrome: hex(0x1d1f24),
            rail: hex(0x15161a),
            raised: hex(0x24262c),
            hover: hex(0x2a2d34),
            active: hex(0x33363e),
            line: hex(0x2c2f36),
            line_soft: hex(0x222429),
            line_strong: hex(0x3b3f48),
            text: hex(0xe9ebef),
            text_muted: hex(0xa1a7b3),
            text_faint: hex(0x6f7682),
            accent: hex(0x7c86f5),
            accent_hover: hex(0x949cf8),
            accent_active: hex(0x6670e6),
            accent_soft: hex(0x262a45),
            accent_text: hex(0xaab1ff),
            selection: alpha(0x7c86f5, 0.22),
            zebra: hex(0x1a1b20),
            ok: hex(0x3ccf8e),
            warn: hex(0xf0b44c),
            danger: hex(0xf06d6d),
            info: hex(0x5eaaf5),
            ok_soft: hex(0x16352a),
            warn_soft: hex(0x3a2f17),
            danger_soft: hex(0x3d1e20),
            dark: true,
        }
    }

    pub fn of(cx: &App) -> Self {
        if cx.theme().is_dark() {
            Self::dark()
        } else {
            Self::light()
        }
    }
}

/// 切换明暗并把品牌色板写进组件库主题。
pub fn apply_theme(dark: bool, cx: &mut App) {
    Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    let p = if dark { Palette::dark() } else { Palette::light() };
    let white = hex(0xffffff);
    let clear = hsla(0., 0., 0., 0.);
    Theme::update(cx, |theme| brand(theme, &p, white, clear));
}

fn brand(theme: &mut Theme, p: &Palette, white: Hsla, clear: Hsla) {
    let dark = p.dark;
    theme.font_size = px(13.);
    theme.mono_font_size = px(13.);
    theme.radius = px(6.);
    theme.radius_lg = px(10.);
    let c = &mut theme.colors;
    c.background = p.surface;
    c.foreground = p.text;
    c.border = p.line;
    c.muted = p.chrome;
    c.muted_foreground = p.text_muted;
    c.primary = p.accent;
    c.primary_hover = p.accent_hover;
    c.primary_active = p.accent_active;
    c.primary_foreground = white;
    c.button_primary = p.accent;
    c.button_primary_hover = p.accent_hover;
    c.button_primary_active = p.accent_active;
    c.button_primary_foreground = white;
    c.secondary = if dark { p.raised } else { p.surface };
    c.secondary_hover = p.hover;
    c.secondary_active = p.active;
    c.secondary_foreground = p.text;
    c.button = if dark { p.raised } else { p.surface };
    c.button_hover = p.hover;
    c.button_active = p.active;
    c.button_foreground = p.text;
    c.accent = p.hover;
    c.accent_foreground = p.text;
    c.input = p.line_strong;
    c.ring = p.accent;
    c.caret = p.accent;
    c.selection = p.selection;
    c.danger = p.danger;
    c.success = p.ok;
    c.warning = p.warn;
    c.info = p.info;
    c.sidebar = p.chrome;
    c.sidebar_border = p.line;
    c.sidebar_foreground = p.text;
    c.sidebar_accent = p.hover;
    c.sidebar_accent_foreground = p.text;
    c.sidebar_primary = p.accent;
    c.sidebar_primary_foreground = white;
    c.title_bar = p.chrome;
    c.title_bar_border = p.line;
    c.status_bar = p.chrome;
    c.status_bar_border = p.line;
    c.tab_bar = p.chrome;
    c.tab = clear;
    c.tab_active = p.surface;
    c.tab_active_foreground = p.text;
    c.tab_foreground = p.text_muted;
    c.list = p.surface;
    c.list_hover = p.hover;
    c.list_active = p.selection;
    c.list_active_border = p.accent;
    c.list_even = p.zebra;
    c.list_head = p.chrome;
    c.table = p.surface;
    c.table_head = p.chrome;
    c.table_head_foreground = p.text_muted;
    c.table_hover = p.hover;
    c.table_active = p.selection;
    c.table_active_border = p.accent;
    c.table_even = p.zebra;
    c.table_row_border = p.line_soft;
    c.popover = p.raised;
    c.popover_foreground = p.text;
    c.link = p.accent_text;
    c.link_hover = p.accent;
    c.link_active = p.accent_active;
}

// ---------------------------------------------------------------- 引擎与环境

/// 引擎缩写与品牌色。只做识别，不做商标。
pub fn engine_mark(engine: &str) -> (&'static str, u32) {
    match engine {
        "postgres" | "postgresql" => ("PG", 0x336791),
        "mysql" => ("MY", 0x00758f),
        "mariadb" => ("MA", 0xa35a3d),
        "sqlite" => ("SL", 0x0f80cc),
        "sqlserver" | "mssql" => ("MS", 0xcc2927),
        "redis" => ("RD", 0xd8392c),
        "mongodb" | "mongo" => ("MG", 0x13aa52),
        _ => ("DB", 0x6b7280),
    }
}

pub fn engine_label(engine: &str) -> &'static str {
    match engine {
        "postgres" | "postgresql" => "PostgreSQL",
        "mysql" => "MySQL",
        "mariadb" => "MariaDB",
        "sqlite" => "SQLite",
        "sqlserver" | "mssql" => "SQL Server",
        "redis" => "Redis",
        "mongodb" | "mongo" => "MongoDB",
        _ => "Database",
    }
}

/// 圆角方块里的两字母引擎徽标。
pub fn engine_badge(engine: &str, size: f32) -> AnyElement {
    let (abbr, color) = engine_mark(engine);
    div()
        .flex_none()
        .size(px(size))
        .rounded(px((size / 4.).max(3.)))
        .bg(rgb(color))
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(0xffffff))
        .text_size(px((size * 0.42).max(8.)))
        .font_weight(FontWeight::BOLD)
        .child(abbr)
        .into_any_element()
}

/// 环境色：开发绿、测试黄、生产红、本地灰。
pub fn env_colors(env: &str, p: &Palette) -> (Hsla, Hsla) {
    match env {
        "prod" | "production" => (p.danger, p.danger_soft),
        "test" | "staging" => (p.warn, p.warn_soft),
        "dev" | "development" => (p.ok, p.ok_soft),
        _ => (p.text_muted, p.hover),
    }
}

pub fn env_tag(env: &str, label: impl Into<SharedString>, p: &Palette) -> AnyElement {
    let (fg, bg) = env_colors(env, p);
    pill(label, fg, bg)
}

pub fn pill(label: impl Into<SharedString>, fg: Hsla, bg: Hsla) -> AnyElement {
    div()
        .flex_none()
        .px(px(6.))
        .h(px(18.))
        .rounded(px(4.))
        .bg(bg)
        .text_color(fg)
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .flex()
        .items_center()
        .child(label.into())
        .into_any_element()
}

pub fn status_dot(color: Hsla) -> AnyElement {
    div().flex_none().size(px(7.)).rounded_full().bg(color).into_any_element()
}

// ---------------------------------------------------------------- 文字与布局

pub fn icon(name: IconName, size: f32, color: Hsla) -> Icon {
    Icon::new(name).size(px(size)).text_color(color)
}

/// 分区小标题：11px、加粗、弱色。
pub fn eyebrow(text: impl Into<SharedString>, p: &Palette) -> AnyElement {
    div()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.text_faint)
        .child(text.into())
        .into_any_element()
}

pub fn title(text: impl Into<SharedString>, size: f32, p: &Palette) -> AnyElement {
    div()
        .text_size(px(size))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.text)
        .child(text.into())
        .into_any_element()
}

pub fn muted(text: impl Into<SharedString>, p: &Palette) -> AnyElement {
    div().text_size(px(12.)).text_color(p.text_muted).child(text.into()).into_any_element()
}

/// 白底圆角卡片，带发丝线边框。
pub fn card(p: &Palette) -> gpui_kit::Div {
    div().bg(p.surface).border_1().border_color(p.line).rounded(px(10.))
}

/// 空态：图标、标题、说明。按钮由调用方追加。
pub fn empty_state(icon_name: IconName, heading: impl Into<SharedString>, detail: impl Into<SharedString>, p: &Palette) -> gpui_kit::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(10.))
        .p(px(24.))
        .child(
            div()
                .size(px(44.))
                .rounded(px(12.))
                .bg(p.accent_soft)
                .flex()
                .items_center()
                .justify_center()
                .child(icon(icon_name, 22., p.accent_text)),
        )
        .child(title(heading, 14., p))
        .child(div().max_w(px(360.)).text_center().text_size(px(12.)).text_color(p.text_muted).child(detail.into()))
}

/// 键盘提示 kbd。
pub fn kbd(text: impl Into<SharedString>, p: &Palette) -> AnyElement {
    div()
        .flex_none()
        .px(px(5.))
        .h(px(18.))
        .rounded(px(4.))
        .border_1()
        .border_color(p.line_strong)
        .bg(p.surface)
        .text_size(px(10.5))
        .text_color(p.text_muted)
        .flex()
        .items_center()
        .child(text.into())
        .into_any_element()
}

pub fn shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") {
        key.replace("Ctrl+", "⌘").replace("Shift+", "⇧").replace("Alt+", "⌥")
    } else {
        key.to_owned()
    }
}

pub fn vline(p: &Palette) -> AnyElement {
    div().flex_none().w(px(1.)).h(px(18.)).mx(px(4.)).bg(p.line).into_any_element()
}

// ---------------------------------------------------------------- 按钮

type Click = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// 图标按钮：幽灵样式、带提示。`label` 同时作为无障碍名称，测试按它找控件。
pub fn icon_button(id: impl Into<ElementId>, name: IconName, label: impl Into<SharedString>, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Button {
    let label = label.into();
    Button::new(id)
        .ghost()
        .small()
        .icon(name)
        .tooltip(label.clone())
        .accessibility_label(label)
        .on_click(on_click)
}

/// 工具条按钮：幽灵样式、图标 + 文字。
pub fn tool_button(id: impl Into<ElementId>, name: Option<IconName>, label: impl Into<SharedString>, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Button {
    let label = label.into();
    let mut button = Button::new(id).ghost().small().label(label.clone()).accessibility_label(label);
    if let Some(name) = name {
        button = button.icon(name);
    }
    button.on_click(on_click)
}

/// 主要动作按钮。
pub fn primary_button(id: impl Into<ElementId>, name: Option<IconName>, label: impl Into<SharedString>, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Button {
    let label = label.into();
    let mut button = Button::new(id).primary().small().label(label.clone()).accessibility_label(label);
    if let Some(name) = name {
        button = button.icon(name);
    }
    button.on_click(on_click)
}

/// 次要动作按钮（描边）。
pub fn secondary_button(id: impl Into<ElementId>, name: Option<IconName>, label: impl Into<SharedString>, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Button {
    let label = label.into();
    let mut button = Button::new(id).outline().small().label(label.clone()).accessibility_label(label);
    if let Some(name) = name {
        button = button.icon(name);
    }
    button.on_click(on_click)
}

pub fn danger_button(id: impl Into<ElementId>, name: Option<IconName>, label: impl Into<SharedString>, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Button {
    let label = label.into();
    let mut button = Button::new(id).danger().small().label(label.clone()).accessibility_label(label);
    if let Some(name) = name {
        button = button.icon(name);
    }
    button.on_click(on_click)
}

/// 分段选择：一组互斥选项，选中项高亮。
pub fn segmented(items: Vec<(SharedString, SharedString, bool, Click)>, p: &Palette) -> AnyElement {
    let mut row = div().flex().flex_none().p(px(2.)).gap(px(2.)).rounded(px(8.)).bg(p.hover);
    for (id, label, selected, on_click) in items {
        row = row.child(
            div()
                .id(ElementId::Name(id))
                .role(gpui_kit::Role::Button)
                .aria_label(label.clone())
                .aria_selected(selected)
                .px(px(10.))
                .h(px(24.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .text_size(px(12.))
                .cursor_pointer()
                .when(selected, |this| this.bg(p.surface).text_color(p.text).font_weight(FontWeight::MEDIUM).shadow_sm())
                .when(!selected, |this| this.text_color(p.text_muted).hover(|style| style.text_color(p.text)))
                .child(label)
                .on_click(move |event, window, cx| on_click(event, window, cx)),
        );
    }
    div().flex().child(row).into_any_element()
}

/// 给可交互元素加一句文字提示。
pub trait Tip: gpui_kit::StatefulInteractiveElement + Sized {
    fn tooltip_text(self, text: impl Into<SharedString>) -> Self {
        let text = text.into();
        self.tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
    }
}

impl<E: gpui_kit::StatefulInteractiveElement> Tip for E {}

pub fn click(handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Click {
    Box::new(handler)
}

// ---------------------------------------------------------------- 文本框

/// 一组真正的文本框。值的唯一来源仍是视图模型：
/// 输入时回调写进模型；模型被别处改了，下次渲染把新值推回文本框。
pub struct Fields<K: Copy + Eq + Hash + 'static> {
    inputs: HashMap<K, Entity<InputState>>,
    /// 上次渲染时模型里的值。只有模型自己变了才回推文本框，
    /// 免得渲染赶在输入事件送达之前，把刚打的字冲掉。
    last: HashMap<K, String>,
    subscriptions: Vec<Subscription>,
}

impl<K: Copy + Eq + Hash + 'static> Default for Fields<K> {
    fn default() -> Self {
        Self { inputs: HashMap::new(), last: HashMap::new(), subscriptions: Vec::new() }
    }
}

/// 模型值变了才写回文本框；`last` 记住上次见到的模型值。
pub fn sync_text(state: &Entity<InputState>, last: &mut String, value: &str, window: &mut Window, cx: &mut App) {
    if last.as_str() == value {
        return;
    }
    *last = value.to_owned();
    if state.read(cx).value().as_ref() != value {
        let value = value.to_owned();
        state.update(cx, |state, cx| state.set_value(value, window, cx));
    }
}

/// 同 `sync_text`，用于多行编辑器。
pub fn sync_editor(state: &Entity<gpui_kit::component::input::EditorState>, last: &mut String, value: &str, window: &mut Window, cx: &mut App) {
    if last.as_str() == value {
        return;
    }
    *last = value.to_owned();
    if state.read(cx).value().as_ref() != value {
        let value = value.to_owned();
        state.update(cx, |state, cx| state.set_value(value, window, cx));
    }
}

pub struct FieldSpec<'a> {
    pub placeholder: &'a str,
    pub masked: bool,
}

impl<K: Copy + Eq + Hash + 'static> Fields<K> {
    /// 取（或建）文本框，并把模型值同步进去。`on_change` 在每次输入后收到新值。
    pub fn bind<V: 'static>(
        &mut self,
        key: K,
        value: &str,
        spec: FieldSpec<'_>,
        window: &mut Window,
        cx: &mut Context<V>,
        on_change: fn(&mut V, K, String, &mut Context<V>),
    ) -> Entity<InputState> {
        self.bind_enter(key, value, spec, window, cx, on_change, None)
    }

    /// 同 `bind`，另外在回车时调用 `on_enter`。
    #[allow(clippy::too_many_arguments)]
    pub fn bind_enter<V: 'static>(
        &mut self,
        key: K,
        value: &str,
        spec: FieldSpec<'_>,
        window: &mut Window,
        cx: &mut Context<V>,
        on_change: fn(&mut V, K, String, &mut Context<V>),
        on_enter: Option<fn(&mut V, K, &mut Context<V>)>,
    ) -> Entity<InputState> {
        if let Some(state) = self.inputs.get(&key).cloned() {
            let last = self.last.entry(key).or_default();
            sync_text(&state, last, value, window, cx);
            return state;
        }
        self.last.insert(key, value.to_owned());
        let placeholder = SharedString::from(spec.placeholder.to_owned());
        let masked = spec.masked;
        let initial = SharedString::from(value.to_owned());
        let state = cx.new(|cx| {
            InputState::new(window, cx).placeholder(placeholder).masked(masked).default_value(initial)
        });
        let subscription = cx.subscribe(&state, move |this: &mut V, state, event: &InputEvent, cx| {
            match event {
                InputEvent::Change => {
                    let text = state.read(cx).value().to_string();
                    on_change(this, key, text, cx);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    if let Some(on_enter) = on_enter {
                        on_enter(this, key, cx);
                        cx.notify();
                    }
                }
                _ => {}
            }
        });
        self.subscriptions.push(subscription);
        self.inputs.insert(key, state.clone());
        state
    }

    pub fn focus(&self, key: K, window: &mut Window, cx: &mut App) {
        if let Some(state) = self.inputs.get(&key) {
            state.update(cx, |state, cx| state.focus(window, cx));
        }
    }
}

/// 文本框外观：小号、带 id。id 让测试与无障碍能点到它。
pub fn text_input(id: impl Into<ElementId>, state: &Entity<InputState>, label: impl Into<SharedString>) -> Input {
    Input::new(state).id(id).aria_label(label)
}

/// 表单行：上方小标签，下方控件。
pub fn form_row(label: impl Into<SharedString>, control: impl IntoElement, p: &Palette) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(5.))
        .min_w(px(0.))
        .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(p.text_muted).child(label.into()))
        .child(control)
        .into_any_element()
}

use gpui_kit::prelude::FluentBuilder as _;

#[cfg(test)]
mod field_tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{Render, TestAppContext};

    struct Probe {
        fields: Fields<u8>,
        value: String,
    }

    impl Render for Probe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let value = self.value.clone();
            let state = self.fields.bind(1u8, &value, FieldSpec { placeholder: "p", masked: false }, window, cx, |this: &mut Self, _, value, _| this.value = value);
            div().size_full().child(text_input("probe", &state, "probe"))
        }
    }

    #[gpui_kit::test]
    fn typed_text_survives_render_and_model_changes_flow_back(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, |_, cx| cx.new(|_| Probe { fields: Fields::default(), value: String::new() })).unwrap()
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("probe", cx);
            window.input("ab", cx);
        })
        .unwrap();
        let value = cx.update(|cx| view.read(cx).value.clone());
        assert_eq!(value, "ab", "渲染不能在输入事件送达前把刚打的字冲掉");
        cx.update(|cx| view.update(cx, |probe, _| probe.value = "从模型改".into()));
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        let shown = cx.update(|cx| view.read(cx).fields.inputs.get(&1).unwrap().read(cx).value().to_string());
        assert_eq!(shown, "从模型改", "模型自己变了要推回文本框");
    }
}

