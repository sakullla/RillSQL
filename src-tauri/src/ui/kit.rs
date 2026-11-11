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
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window,
    div, hsla, px, rgb,
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
            // 次级文字必须比 faint 更强一档，避免 token 层级倒置。
            text_muted: hex(0x4f5a68),
            // 弱文用于 11–12px 的提示与分区标题；保持在白底和 chrome 底上都过 4.5:1。
            // 选中/悬停底（active、accent_soft、hover）上也要过 4.5:1。
            text_faint: hex(0x5b6573),
            accent: hex(0x4f5bd5),
            accent_hover: hex(0x4350c4),
            accent_active: hex(0x3a45ad),
            accent_soft: hex(0xeceefc),
            accent_text: hex(0x3a45ad),
            selection: alpha(0x4f5bd5, 0.14),
            zebra: hex(0xfafbfc),
            // 状态色同时作为小号文字使用，压深到 soft 底也过 4.5:1。
            ok: hex(0x0f7a4d),
            warn: hex(0x8f5e14),
            danger: hex(0xb42f2f),
            info: hex(0x2568b0),
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
            // 深色主题的选中/悬停底同样按 4.5:1 校准。
            text_faint: hex(0x98a0ac),
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
    theme.font_size = px(TypeScale::MD);
    theme.mono_font_size = px(TypeScale::MD);
    // 组件库按钮、输入框用 radius（控件 4px），对话框、通知等浮层用 radius_lg（面板 6px）。
    theme.radius = px(Radius::CONTROL);
    theme.radius_lg = px(Radius::PANEL);
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

// ---------------------------------------------------------------- 字号与圆角

/// 字号刻度：规范 S4.1 的五档 `11 / 12 / 13 / 15 / 18` px。
/// 业务视图只用这里的档位，不写裸字号；`style_literals_follow_design_tokens` 扫描源码守住它。
pub struct TypeScale;

impl TypeScale {
    /// 分区标签、键帽、徽标、次要元信息。
    pub const XS: f32 = 11.;
    /// 默认 UI 字号：列表、表单、按钮、正文。
    pub const SM: f32 = 12.;
    /// 强调正文、对话框小标题、空态标题、代码预览。
    pub const MD: f32 = 13.;
    /// 面板与页面标题。
    pub const LG: f32 = 15.;
    /// 欢迎区品牌字、统计大数字。
    pub const XL: f32 = 18.;
}

/// 圆角：规范 S4.2 的控件 4px、面板/浮层 6px。取消胶囊与大圆角卡片。
pub struct Radius;

impl Radius {
    /// 按钮、列表行、标签、菜单项、分段项、输入类控件。
    pub const CONTROL: f32 = 4.;
    /// 面板、卡片、提示条、浮层、对话框、代码块。
    pub const PANEL: f32 = 6.;
}

/// 焦点环宽度：规范 S5.3 的 2px。
pub const FOCUS_RING_W: f32 = 2.;

// ---------------------------------------------------------------- 四态

/// 自定义可点元素的四态样式。数值只由色板与是否禁用决定，
/// `interactive` 照此绘制，测试也直接断言这一份值。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InteractionStyle {
    pub hover_bg: Option<Hsla>,
    pub active_bg: Option<Hsla>,
    /// 键盘聚焦时的焦点环颜色；禁用时为 None（不可聚焦）。
    pub ring: Option<Hsla>,
    pub ring_width: f32,
    pub opacity: f32,
    pub focusable: bool,
}

pub fn interaction_style(p: &Palette, disabled: bool) -> InteractionStyle {
    if disabled {
        InteractionStyle { hover_bg: None, active_bg: None, ring: None, ring_width: 0., opacity: 0.55, focusable: false }
    } else {
        InteractionStyle { hover_bg: Some(p.hover), active_bg: Some(p.active), ring: Some(p.accent), ring_width: FOCUS_RING_W, opacity: 1., focusable: true }
    }
}

/// 给自定义可点元素统一加 hover / active / focus-visible（2px 焦点环）/ disabled 四态。
/// 可用时成为 Tab 停靠点，键盘聚焦才出焦点环（鼠标点击不出）；禁用时去掉 hover/active、
/// 降低不透明度、光标改为 not-allowed，并且不可聚焦。
pub fn interactive<E>(element: E, p: &Palette, disabled: bool) -> E
where
    E: gpui_kit::StatefulInteractiveElement + gpui_kit::Styled,
{
    let style = interaction_style(p, disabled);
    let mut element = interactive_states(element, p, disabled);
    if let Some(bg) = style.hover_bg {
        element = element.hover(move |s| s.bg(bg));
    }
    element
}

/// 同 `interactive`，但 hover 由调用方自己定义（例如分段控件里未选项只提亮文字）。
pub fn interactive_states<E>(element: E, p: &Palette, disabled: bool) -> E
where
    E: gpui_kit::StatefulInteractiveElement + gpui_kit::Styled,
{
    let style = interaction_style(p, disabled);
    let mut element = element.opacity(style.opacity);
    if !style.focusable {
        return element.cursor_not_allowed();
    }
    element = element.cursor_pointer().tab_index(0);
    if let Some(bg) = style.active_bg {
        element = element.active(move |s| s.bg(bg));
    }
    if let Some(ring) = style.ring {
        let width = style.ring_width;
        element = element.focus_visible(move |s| {
            s.shadow(vec![gpui_kit::BoxShadow { color: ring, offset: gpui_kit::point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(width), inset: false }])
        });
    }
    element
}

/// 列表容器管的行（对象树、命令面板、右键菜单、连接/历史列表）：键盘焦点在容器上，
/// 当前行用选中底色表达（规范 S5.2 Listbox 的 activedescendant 做法），行本身不抢焦点。
/// 这里只补按下反馈与光标；hover 仍由调用方按是否选中决定。
pub fn row_states<E>(element: E, p: &Palette) -> E
where
    E: gpui_kit::StatefulInteractiveElement + gpui_kit::Styled,
{
    let active = p.active;
    element.cursor_pointer().active(move |s| s.bg(active))
}

// ---------------------------------------------------------------- 尺寸指标

/// 外壳与页面的尺寸指标。只由密度决定，绘制与模型布局共用这一份值，
/// 不再让页面各自保留一套高度常量。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// 树、列表、标签行高度。
    pub row_h: f32,
    /// 对象树行高度。
    pub tree_row_h: f32,
    /// 上下文菜单 / 浮层行高度。
    pub menu_row_h: f32,
    /// 分段控件与紧凑按钮高度。
    pub control_h: f32,
    pub topbar_h: f32,
    pub tabs_h: f32,
    pub status_h: f32,
    /// 根节点内边距，与 `WorkbenchModel::painted` 共用。
    pub pad: f32,
    /// 页面内容左右内边距。
    pub page_pad: f32,
    /// 页面内容上下内边距。
    pub page_pad_y: f32,
    /// 列表行的上下内边距。
    pub row_pad_y: f32,
}

impl Metrics {
    /// 规范 S2.2 / S9：紧凑密度为 行 26 / 树 23 / 控件 26。
    pub fn compact() -> Self {
        Self {
            row_h: 26.,
            tree_row_h: 23.,
            menu_row_h: 24.,
            control_h: 26.,
            topbar_h: 40.,
            tabs_h: 32.,
            status_h: 24.,
            pad: 4.,
            page_pad: 22.,
            page_pad_y: 20.,
            row_pad_y: 5.,
        }
    }

    pub fn standard() -> Self {
        Self {
            row_h: 32.,
            tree_row_h: 28.,
            menu_row_h: 28.,
            control_h: 24.,
            topbar_h: 44.,
            tabs_h: 36.,
            status_h: 26.,
            pad: 8.,
            page_pad: 28.,
            page_pad_y: 28.,
            row_pad_y: 7.,
        }
    }

    /// 当前外壳密度。外壳每帧把自己的密度发布到这里，子视图像取色板一样取尺寸，
    /// 不必逐个实体下发；没发布过（独立测试窗口）时取标准密度。
    pub fn of(cx: &App) -> Self {
        cx.try_global::<DensityMetrics>().map(|global| global.0).unwrap_or_else(Self::standard)
    }

    /// 发布密度；变了返回 true，调用方据此通知子视图重绘。
    pub fn publish(self, cx: &mut App) -> bool {
        if cx.try_global::<DensityMetrics>().is_some_and(|global| global.0 == self) {
            return false;
        }
        cx.set_global(DensityMetrics(self));
        true
    }

    /// 页面工具条（查询工具条、Mongo/Redis 顶栏）：标准 42。
    pub fn toolbar_h(&self) -> f32 {
        self.topbar_h - 2.
    }

    /// 区块标题栏（结果头、记录视图头、列表表头）：紧凑 30（规范 S7.2 结果头）、标准 34。
    pub fn bar_h(&self) -> f32 {
        self.tabs_h - 2.
    }

    /// 网格工具条（含查找框）：标准 40。
    pub fn tools_h(&self) -> f32 {
        self.topbar_h - 4.
    }

    /// 细条（选区统计、提示条）：标准 22。
    pub fn strip_h(&self) -> f32 {
        self.status_h - 4.
    }

    pub fn relaxed() -> Self {
        Self {
            row_h: 40.,
            tree_row_h: 36.,
            menu_row_h: 34.,
            control_h: 28.,
            topbar_h: 48.,
            tabs_h: 40.,
            status_h: 28.,
            pad: 12.,
            page_pad: 34.,
            page_pad_y: 36.,
            row_pad_y: 9.,
        }
    }
}

struct DensityMetrics(Metrics);

impl gpui_kit::Global for DensityMetrics {}

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
        .rounded(px(Radius::CONTROL))
        .bg(bg)
        .text_color(fg)
        .text_size(px(TypeScale::XS))
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
        .text_size(px(TypeScale::XS))
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
    div().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(text.into()).into_any_element()
}

/// 白底圆角卡片，带发丝线边框。
pub fn card(p: &Palette) -> gpui_kit::Div {
    div().bg(p.surface).border_1().border_color(p.line).rounded(px(Radius::PANEL))
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
                .rounded(px(Radius::PANEL))
                .bg(p.accent_soft)
                .flex()
                .items_center()
                .justify_center()
                .child(icon(icon_name, 22., p.accent_text)),
        )
        .child(title(heading, TypeScale::MD, p))
        .child(div().max_w(px(360.)).text_center().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(detail.into()))
}

/// 加载态：居中的小号转圈 + 一行说明（规范 S8.5 三种状态之一）。加载优先于空态。
pub fn loading_state(text: impl Into<SharedString>, p: &Palette) -> gpui_kit::Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .p(px(16.))
        .text_size(px(TypeScale::SM))
        .text_color(p.text_muted)
        .child(gpui_kit::component::spinner::Spinner::new().small().color(p.accent))
        .child(text.into())
}

/// 错误态：与空态同一排版，图标块换成危险色；重试按钮由调用方追加。
pub fn error_state(heading: impl Into<SharedString>, detail: impl Into<SharedString>, p: &Palette) -> gpui_kit::Div {
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
                .rounded(px(Radius::PANEL))
                .bg(p.danger_soft)
                .flex()
                .items_center()
                .justify_center()
                .child(icon(IconName::CircleAlert, 22., p.danger)),
        )
        .child(title(heading, TypeScale::MD, p))
        .child(div().max_w(px(360.)).text_center().text_size(px(TypeScale::SM)).text_color(p.text_muted).child(detail.into()))
}

/// 键盘提示 kbd。
pub fn kbd(text: impl Into<SharedString>, p: &Palette) -> AnyElement {
    div()
        .flex_none()
        .px(px(5.))
        .h(px(18.))
        .rounded(px(Radius::CONTROL))
        .border_1()
        .border_color(p.line_strong)
        .bg(p.surface)
        .text_size(px(TypeScale::XS))
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
    segmented_sized(items, p, Metrics::standard().control_h)
}

/// 同 `segmented`，高度跟随密度。
pub fn segmented_sized(items: Vec<(SharedString, SharedString, bool, Click)>, p: &Palette, height: f32) -> AnyElement {
    let mut row = div().flex().flex_none().p(px(2.)).gap(px(2.)).rounded(px(Radius::CONTROL)).bg(p.hover);
    for (id, label, selected, on_click) in items {
        row = row.child(
            interactive_states(div().id(ElementId::Name(id)), p, false)
                .test_support()
                .role(gpui_kit::Role::Button)
                .aria_label(label.clone())
                .aria_selected(selected)
                .px(px(10.))
                .h(px(height))
                .flex()
                .items_center()
                .rounded(px(Radius::CONTROL))
                .text_size(px(TypeScale::SM))
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
    /// 上次绑定时的占位符；语言切换后要更新已创建输入框的占位符。
    placeholders: HashMap<K, String>,
    subscriptions: Vec<Subscription>,
}

impl<K: Copy + Eq + Hash + 'static> Default for Fields<K> {
    fn default() -> Self {
        Self { inputs: HashMap::new(), last: HashMap::new(), placeholders: HashMap::new(), subscriptions: Vec::new() }
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
            // 语言切换后占位符也要跟随，否则已创建的输入框会保留旧语言文案。
            let desired = spec.placeholder.to_owned();
            let stale = self.placeholders.get(&key) != Some(&desired);
            if stale {
                let placeholder = SharedString::from(desired.clone());
                state.update(cx, |state, cx| state.set_placeholder(placeholder, window, cx));
                self.placeholders.insert(key, desired);
            }
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
        .child(div().text_size(px(TypeScale::SM)).font_weight(FontWeight::MEDIUM).text_color(p.text_muted).child(label.into()))
        .child(control)
        .into_any_element()
}

use gpui_kit::prelude::FluentBuilder as _;

#[cfg(test)]
mod palette_tests {
    use super::*;

    fn luminance(color: Hsla) -> f32 {
        let rgba: gpui_kit::Rgba = color.into();
        let channel = |value: f32| {
            if value <= 0.03928 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(rgba.r) + 0.7152 * channel(rgba.g) + 0.0722 * channel(rgba.b)
    }

    fn contrast(foreground: Hsla, background: Hsla) -> f32 {
        let (a, b) = (luminance(foreground), luminance(background));
        let (light, dark) = if a >= b { (a, b) } else { (b, a) };
        (light + 0.05) / (dark + 0.05)
    }

    /// 正文、提示与状态文字在选中/悬停底上也达到 WCAG AA 的 4.5:1。
    #[test]
    fn every_theme_keeps_faint_text_readable_on_selected_backgrounds() {
        for p in [Palette::light(), Palette::dark()] {
            for background in [p.active, p.accent_soft, p.hover] {
                assert!(
                    contrast(p.text_faint, background) >= 4.5,
                    "faint on selected background: {:.2}",
                    contrast(p.text_faint, background)
                );
            }
            for (color, background) in [(p.ok, p.ok_soft), (p.warn, p.warn_soft), (p.danger, p.danger_soft), (p.info, p.accent_soft)] {
                assert!(
                    contrast(color, background) >= 4.5,
                    "status on soft background: {:.2}",
                    contrast(color, background)
                );
            }
        }
    }

    /// 正文、提示与状态文字在两种主题里都达到 WCAG AA 的 4.5:1。
    #[test]
    fn every_theme_keeps_text_readable() {
        for p in [Palette::light(), Palette::dark()] {
            for (name, fg) in [("text", p.text), ("muted", p.text_muted), ("faint", p.text_faint)] {
                for (surface_name, bg) in [("surface", p.surface), ("chrome", p.chrome), ("raised", p.raised)] {
                    let ratio = contrast(fg, bg);
                    assert!(ratio >= 4.5, "{name} on {surface_name}: {ratio:.2} ({})", p.dark);
                }
            }
            for (name, fg, bg) in [
                ("ok", p.ok, p.ok_soft),
                ("warn", p.warn, p.warn_soft),
                ("danger", p.danger, p.danger_soft),
                ("accent", p.accent_text, p.accent_soft),
            ] {
                let ratio = contrast(fg, bg);
                assert!(ratio >= 4.5, "{name} on soft: {ratio:.2} ({})", p.dark);
            }
            for (name, color) in [("ok", p.ok), ("warn", p.warn), ("danger", p.danger), ("info", p.info)] {
                let ratio = contrast(color, p.surface);
                assert!(ratio >= 4.5, "{name} on surface: {ratio:.2} ({})", p.dark);
            }
        }
    }

    /// 三档密度必须真的拉开行高，否则切换只是文案变化。
    /// 紧凑档按规范取 26/23/26（行/树/控件）；标准与宽松保持既有值，只补进规范。
    /// 控件高度因此不再按密度递增（紧凑 26 > 标准 24），这是 D1 的确认结果。
    #[test]
    fn density_metrics_are_distinct_and_ordered() {
        let (compact, standard, relaxed) = (Metrics::compact(), Metrics::standard(), Metrics::relaxed());
        assert!(compact.row_h < standard.row_h && standard.row_h < relaxed.row_h);
        assert!(compact.tree_row_h < standard.tree_row_h && standard.tree_row_h < relaxed.tree_row_h);
        assert!(standard.control_h < relaxed.control_h);
        assert!(compact.pad < standard.pad && standard.pad < relaxed.pad);
        assert_eq!((compact.row_h, compact.tree_row_h, compact.control_h), (26., 23., 26.), "紧凑密度按规范 S2.2");
        assert_eq!((standard.row_h, standard.tree_row_h, standard.control_h), (32., 28., 24.), "标准密度保持既有值");
        assert_eq!((relaxed.row_h, relaxed.tree_row_h, relaxed.control_h), (40., 36., 28.), "宽松密度保持既有值");
    }

    /// 自定义可点元素的四态：可用时有 hover/active 底色与 2px 焦点环，禁用时全部去掉且不可聚焦。
    /// 焦点环在浅深色的工作区、外壳、浮层、悬停底上都达到非文字元素的 3:1。
    #[test]
    fn interactive_states_have_visible_focus_ring_in_both_themes() {
        for p in [Palette::light(), Palette::dark()] {
            let on = interaction_style(&p, false);
            assert_eq!(on.ring_width, 2., "焦点环 2px");
            assert!(on.focusable && on.opacity == 1.);
            assert_ne!(on.hover_bg, None);
            assert_ne!(on.active_bg, on.hover_bg, "按下与悬停可区分");
            let ring = on.ring.expect("可用时有焦点环");
            for (name, bg) in [("surface", p.surface), ("chrome", p.chrome), ("raised", p.raised), ("rail", p.rail), ("hover", p.hover)] {
                let ratio = contrast(ring, bg);
                assert!(ratio >= 3., "focus ring on {name}: {ratio:.2} (dark={})", p.dark);
            }
            let off = interaction_style(&p, true);
            assert_eq!((off.hover_bg, off.active_bg, off.ring, off.focusable), (None, None, None, false), "禁用去掉反馈且不可聚焦");
            assert!(off.opacity < 1.);
        }
    }
}

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


#[cfg(test)]
mod interactive_tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{Render, TestAppContext};

    struct Probe {
        disabled: bool,
    }

    impl Render for Probe {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let p = Palette::of(cx);
            div().size_full().child(interactive(div().id("probe-item"), &p, self.disabled).w(px(80.)).h(px(24.)).child("probe"))
        }
    }

    fn focus_after_tab(disabled: bool, dark: bool, cx: &mut TestAppContext) -> bool {
        cx.update(|cx| {
            gpui_kit::init(cx);
            apply_theme(dark, cx);
        });
        let (window, _) = cx.update(|cx| gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, |_, cx| cx.new(|_| Probe { disabled })).unwrap());
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.focus_next(cx);
            window.render_frame(cx);
            window.focused(cx).is_some()
        })
        .unwrap()
    }

    /// 键盘 Tab 能停到可用的自定义可点元素上（焦点环随之出现），禁用的不能。
    #[gpui_kit::test]
    fn interactive_element_is_a_tab_stop_unless_disabled_light(cx: &mut TestAppContext) {
        assert!(focus_after_tab(false, false, cx), "浅色：可用元素是 Tab 停靠点");
    }

    #[gpui_kit::test]
    fn interactive_element_is_a_tab_stop_unless_disabled_dark(cx: &mut TestAppContext) {
        assert!(focus_after_tab(false, true, cx), "深色：可用元素是 Tab 停靠点");
    }

    #[gpui_kit::test]
    fn disabled_interactive_element_is_skipped_by_tab(cx: &mut TestAppContext) {
        assert!(!focus_after_tab(true, false, cx), "禁用元素不可聚焦");
    }
}

#[cfg(test)]
mod token_tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// 允许不走字号/圆角 token 的表达式。每条都写明理由；其余一律必须是 `TypeScale::*` / `Radius::*`。
    const EXCEPTIONS: &[(&str, &str, &str)] = &[
        ("ui/kit.rs", "px((size * 0.42).max(8.))", "引擎徽标里的两字母缩写随徽标尺寸等比缩放，属于图形而非排版"),
        ("ui/kit.rs", "px((size / 4.).max(3.))", "引擎徽标方块的圆角随徽标尺寸等比缩放，属于图形"),
        ("ui/kit.rs", "px(size)", "kit::title 的实现；调用方的 size 参数本身被本测试按 TypeScale 检查"),
        ("ui/editor_proto.rs", "px(self.font_size)", "SQL 编辑器字号来自用户设置（规范 S4.1：编辑器字号由设置控制）"),
        ("app/shell/chrome.rs", "px(size * 0.28)", "品牌标记的圆角随尺寸等比缩放，属于图形"),
        ("app/shell/chrome.rs", "px(2.)", "活动栏激活指示条只有 3px 宽，是线条而非控件"),
    ];

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    /// 取 `open` 之后、与之配对的右括号之前的文本。
    fn balanced(text: &str, open: usize) -> Option<&str> {
        let mut depth = 0usize;
        for (offset, ch) in text[open..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&text[open + 1..open + offset]);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// 按顶层逗号切分参数。
    fn args(text: &str) -> Vec<String> {
        let (mut out, mut depth, mut current) = (Vec::new(), 0i32, String::new());
        for ch in text.chars() {
            match ch {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    out.push(current.trim().to_owned());
                    current.clear();
                    continue;
                }
                _ => {}
            }
            current.push(ch);
        }
        if !current.trim().is_empty() {
            out.push(current.trim().to_owned());
        }
        out
    }

    /// 去掉注释行与测试模块，只看产品代码。
    fn product_source(text: &str) -> String {
        let mut out = String::new();
        for line in text.lines() {
            if line.trim_start().starts_with("mod token_tests") {
                break;
            }
            if line.trim_start().starts_with("//") {
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    /// (文件, 调用, 实参) 中不合规的那些。
    pub(super) fn violations(files: &[(String, String)]) -> Vec<String> {
        let mut bad = Vec::new();
        for (name, text) in files {
            let source = product_source(text);
            for (call, token) in [(".text_size(", "px(TypeScale::"), (".rounded(", "px(Radius::")] {
                for (at, _) in source.match_indices(call) {
                    let Some(arg) = balanced(&source, at + call.len() - 1) else { continue };
                    let arg = arg.trim();
                    let tokenized = arg.starts_with(token) && arg.ends_with(')') && arg[token.len()..arg.len() - 1].chars().all(|c| c.is_ascii_uppercase() || c == '_');
                    let excepted = EXCEPTIONS.iter().any(|(file, expr, _)| file == name && *expr == arg);
                    if !tokenized && !excepted {
                        bad.push(format!("{name}: {call}{arg})"));
                    }
                }
            }
            for call in ["kit::title(", "(title("] {
                for (at, _) in source.match_indices(call) {
                    if call == "(title(" && name != "ui/kit.rs" {
                        continue;
                    }
                    let Some(inner) = balanced(&source, at + call.len() - 1) else { continue };
                    let parts = args(inner);
                    if parts.len() == 3 && !parts[1].starts_with("TypeScale::") {
                        bad.push(format!("{name}: title(.., {}, ..)", parts[1]));
                    }
                }
            }
        }
        bad
    }

    fn sources() -> Vec<(String, String)> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut paths = Vec::new();
        rust_files(&root.join("app"), &mut paths);
        rust_files(&root.join("ui"), &mut paths);
        paths
            .into_iter()
            .map(|path| {
                let name = path.strip_prefix(&root).unwrap().to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
                (name, std::fs::read_to_string(&path).unwrap())
            })
            .collect()
    }

    /// app/ 与 ui/ 的字号只取五档刻度、圆角只取控件 4 / 面板 6；例外逐条列出理由。
    #[test]
    fn style_literals_follow_design_tokens() {
        assert_eq!([TypeScale::XS, TypeScale::SM, TypeScale::MD, TypeScale::LG, TypeScale::XL], [11., 12., 13., 15., 18.], "规范 S4.1 五档字号");
        assert_eq!((Radius::CONTROL, Radius::PANEL), (4., 6.), "规范 S4.2 控件 4 / 面板 6");
        let files = sources();
        assert!(files.len() > 20, "扫描到了 app/ 与 ui/ 的源码");
        let bad = violations(&files);
        assert!(bad.is_empty(), "以下样式没有走设计 token：\n{}", bad.join("\n"));
        for (file, expr, reason) in EXCEPTIONS {
            assert!(!reason.is_empty());
            assert!(files.iter().any(|(name, text)| name == file && text.contains(expr)), "例外 {file} {expr} 已不存在，应从清单删除");
        }
    }

    /// 回退任何一处替换（裸字号、旧圆角、标题裸字号）都会被扫描抓到。
    #[test]
    fn style_scan_rejects_reverted_literals() {
        for (snippet, hint) in [
            ("div().text_size(px(12.5))", "12.5"),
            ("div().text_size(px(12.))", "12."),
            ("div().rounded(px(8.))", "8."),
            ("div().rounded(px(6.))", "6."),
            ("kit::title(text, 16., &p)", "16."),
            ("div().child(title(heading, 14., p))", "14."),
            ("div().text_size(px(TypeScale::SM + 1.))", "+ 1."),
        ] {
            let file = if snippet.contains("(title(") { "ui/kit.rs" } else { "app/views/probe.rs" };
            let bad = violations(&[(file.to_owned(), snippet.to_owned())]);
            assert_eq!(bad.len(), 1, "{snippet} 必须被拒绝");
            assert!(bad[0].contains(hint));
        }
        let ok = violations(&[(
            "app/views/probe.rs".to_owned(),
            "div().text_size(px(TypeScale::SM)).rounded(px(Radius::PANEL)).child(kit::title(t, TypeScale::LG, &p))".to_owned(),
        )]);
        assert!(ok.is_empty(), "{ok:?}");
    }
}
