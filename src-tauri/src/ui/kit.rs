//! RillSQL 设计底座：品牌主题、色板与通用控件。
//!
//! 所有页面都从这里取颜色和控件，不在业务视图里写死色值。
//! 浅色与深色是两套分别调过的色板，不是机械反色。

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, Theme, ThemeMode};
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, ElementId, Entity, FontFeatures, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window,
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
    /// 凹陷表面：比工作区和外壳都收进去一档，用来放代码、预览和嵌套分区。
    pub sunken: Hsla,
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
    /// 实心强调底上的前景（主按钮文字与图标）。
    pub on_accent: Hsla,
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
            // 比外壳再沉一档的冷灰，和白底、外壳、浮层都分开。
            sunken: hex(0xe8edf5),
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
            on_accent: hex(0xffffff),
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
            // 比工作区更深的底，避免和外壳、浮层挤在同一档明度上。
            sunken: hex(0x101216),
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
            on_accent: hex(0xffffff),
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
    /// 统计大数字（对比结果计数）。页面主标题不超过 16px（规范 S8.6），欢迎区品牌字用 `LG`。
    pub const XL: f32 = 18.;
}

/// 圆角：规范 S4.2 的控件 4px、面板/浮层 6px、状态标签 3px。取消胶囊与大圆角卡片。
pub struct Radius;

impl Radius {
    /// 状态/环境标签（`kit::pill`、`kit::env_tag`），规范 S4.2 / S5.2 `Tag`。
    pub const TAG: f32 = 3.;
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
    // 可聚焦的 div 被鼠标按下时会把键盘焦点抢到自己身上。这些元素只为键盘 Tab 可达才设 tab_index，
    // 鼠标点击不应让网格、编辑器或对象树丢掉焦点：按下时 prevent_default 抑制这次转移（点击照常触发）。
    element = element
        .cursor_pointer()
        .tab_index(0)
        .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, _| window.prevent_default());
    if let Some(bg) = style.active_bg {
        element = element.active(move |s| s.bg(bg));
    }
    if let Some(ring) = style.ring {
        let width = style.ring_width;
        element = element.focus_visible(move |s| s.shadow(vec![focus_ring(ring, width)]));
    }
    element
}

/// 焦点环：元素内侧一圈 `width` 宽的实线（inset 阴影，无模糊）。
/// 内侧阴影画在背景之上、子元素之下，只着色边缘 `width` 宽的一圈；外侧投影在没有背景的元素上
/// 会把整块区域涂成强调色（投影本身是一块实心矩形，只靠不透明背景遮住中间），所以不用。
pub fn focus_ring(color: Hsla, width: f32) -> gpui_kit::BoxShadow {
    gpui_kit::BoxShadow { color, offset: gpui_kit::point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(width), inset: true }
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

/// 视口窄于这个宽度时，水平页边距收到紧凑档，避免把内容挤出屏幕。
pub const NARROW_PAGE_WIDTH: f32 = 760.;

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
            page_pad: 16.,
            page_pad_y: 14.,
            row_pad_y: 5.,
        }
    }

    /// 规范 S9：标准密度为 行 32 / 树 30 / 控件 30。
    pub fn standard() -> Self {
        Self {
            row_h: 32.,
            tree_row_h: 30.,
            menu_row_h: 28.,
            control_h: 30.,
            topbar_h: 44.,
            tabs_h: 36.,
            status_h: 26.,
            pad: 8.,
            page_pad: 20.,
            page_pad_y: 18.,
            row_pad_y: 7.,
        }
    }

    /// 当前外壳密度。外壳每帧把自己的密度发布到这里，子视图像取色板一样取尺寸，
    /// 不必逐个实体下发；没发布过（独立测试窗口）时取默认的紧凑密度（规范 S9）。
    pub fn of(cx: &App) -> Self {
        cx.try_global::<DensityMetrics>().map(|global| global.0).unwrap_or_else(Self::compact)
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
            control_h: 32.,
            topbar_h: 48.,
            tabs_h: 40.,
            status_h: 28.,
            pad: 12.,
            page_pad: 26.,
            page_pad_y: 24.,
            row_pad_y: 9.,
        }
    }

    /// 分栏内边距：紧凑 10 / 标准 12 / 宽松 16。
    pub fn pane_pad(&self) -> f32 {
        self.by_density(10., 12., 16.)
    }

    /// 区块间距：紧凑 12 / 标准 16 / 宽松 20。
    pub fn section_gap(&self) -> f32 {
        self.by_density(12., 16., 20.)
    }

    /// 卡片内边距：紧凑 12 / 标准 14 / 宽松 18。
    pub fn card_pad(&self) -> f32 {
        self.by_density(12., 14., 18.)
    }

    /// 次级页统一页头高度：紧凑 42 / 标准 48 / 宽松 54。
    pub fn header_h(&self) -> f32 {
        self.by_density(42., 48., 54.)
    }

    /// 水平页边距。视口窄于 [`NARROW_PAGE_WIDTH`] 时钳到紧凑档，不改垂直边距。
    pub fn page_pad_for(&self, viewport_width: f32) -> f32 {
        if viewport_width < NARROW_PAGE_WIDTH {
            self.page_pad.min(Self::compact().page_pad)
        } else {
            self.page_pad
        }
    }

    /// 拷贝一份指标，只把水平页边距按视口钳制。调用方继续读 `page_pad` 即可。
    pub fn for_viewport(self, viewport_width: f32) -> Self {
        let mut metrics = self;
        metrics.page_pad = self.page_pad_for(viewport_width);
        metrics
    }

    fn by_density(&self, compact: f32, standard: f32, relaxed: f32) -> f32 {
        if self.row_h < 32. {
            compact
        } else if self.row_h < 40. {
            standard
        } else {
            relaxed
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
        .rounded(px(Radius::TAG))
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

/// 凹陷表面：圆角面板，底色比工作区、外壳和浮层都收进去一档。
pub fn sunken(p: &Palette) -> gpui_kit::Div {
    div().bg(p.sunken).border_1().border_color(p.line).rounded(px(Radius::PANEL))
}

/// 等宽数字。行号、计数、耗时、分值走这里，避免位数变化时左右跳动。
pub fn metric_value(text: impl Into<SharedString>, p: &Palette) -> AnyElement {
    div()
        .text_size(px(TypeScale::SM))
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.text)
        .font_features(tabular_figures())
        .child(text.into())
        .into_any_element()
}

fn tabular_figures() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

/// 页头上的一个数字指标。标签在上，数字按表格数字对齐。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetricCell {
    pub label: SharedString,
    pub value: SharedString,
}

/// 次级页面的统一页头。高度取 `header_h`，数字列由 [`metric_value`] 对齐。
/// 返回的是可继续追加操作按钮的容器。
pub fn page_header(title_text: impl Into<SharedString>, metrics: &[MetricCell], layout: &Metrics, p: &Palette) -> gpui_kit::Div {
    let mut row = div()
        .h(px(layout.header_h()))
        .px(px(layout.page_pad))
        .flex()
        .items_center()
        .gap(px(layout.section_gap()))
        .border_b_1()
        .border_color(p.line)
        .bg(p.surface)
        .child(title(title_text, TypeScale::LG, p));
    for cell in metrics {
        row = row.child(
            div()
                .flex()
                .flex_col()
                .items_end()
                .gap(px(1.))
                .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(cell.label.clone()))
                .child(metric_value(cell.value.clone(), p)),
        );
    }
    row
}

/// 拖拽条携带的值。父容器用 `on_drag_move::<ResizeDrag>` 跟随宽度。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResizeDrag {
    pub key: SharedString,
}

struct ResizeGhost;

impl Render for ResizeGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size(px(0.))
    }
}

/// 约 6px 的纵向拖拽条：列调整光标，悬停或拖拽时画强调色细线，双击调用复位。
/// 拖拽已经在这条上开始；宽度更新挂在父容器的 `on_drag_move::<ResizeDrag>`。
pub fn resize_handle(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    dragging: bool,
    p: &Palette,
    on_reset: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let key = id.into();
    let group = key.clone();
    let accent = p.accent;
    let line = if dragging { p.accent } else { p.line };
    div()
        .group(group.clone())
        .id(ElementId::Name(key.clone()))
        .test_support()
        .flex_none()
        .w(px(RESIZE_HANDLE_W))
        .h_full()
        .cursor_col_resize()
        .flex()
        .items_center()
        .justify_center()
        .role(gpui_kit::Role::Button)
        .aria_label(label)
        .child(div().w(px(2.)).h_full().bg(line).group_hover(group, move |style| style.bg(accent)))
        .on_drag(ResizeDrag { key }, |_drag, _offset, _window, cx| cx.new(|_| ResizeGhost))
        .on_click(move |event, window, cx| {
            if click_count(event) >= 2 {
                on_reset(event, window, cx);
            }
        })
}

fn click_count(event: &ClickEvent) -> usize {
    match event {
        ClickEvent::Mouse(mouse) => mouse.down.click_count.max(mouse.up.click_count),
        ClickEvent::Touch(touch) => touch.tap_count,
        ClickEvent::Keyboard(_) => 1,
    }
}

/// 纵向分隔条的命中宽度。
pub const RESIZE_HANDLE_W: f32 = 6.;

/// 三态语义（需求 R4 / 规范 S8.5 / S12.4）：同一份排版，只换语义、图标色调与文案。
/// 页面按场景选语义，不再各自发明另一套三态；三类文案必须不同 ——
/// 无数据说明这里将出现什么，无命中说明怎么调整条件，错误说明问题与纠正动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateKind {
    /// 无数据：这里还没有数据；主操作是添加入口。
    NoData,
    /// 用户操作无命中（筛选、搜索、目录过滤）；主操作是清除条件。
    NoMatch,
    /// 错误：读取或执行失败；主操作是重试。
    Error,
}

impl StateKind {
    /// 图标块的（前景, 底色）。三类靠色调区分，尺寸与位置完全一致。
    pub fn icon_colors(self, p: &Palette) -> (Hsla, Hsla) {
        match self {
            StateKind::NoData => (p.accent_text, p.accent_soft),
            StateKind::NoMatch => (p.text_muted, p.hover),
            StateKind::Error => (p.danger, p.danger_soft),
        }
    }

    fn block_id(self) -> &'static str {
        match self {
            StateKind::NoData => "empty-state",
            StateKind::NoMatch => "no-match-state",
            StateKind::Error => "error-state",
        }
    }

    fn detail_id(self) -> &'static str {
        match self {
            StateKind::Error => "error-state-detail",
            _ => "empty-state-detail",
        }
    }
}

/// 图标块的边长：规范 S8.5 的 16px 图标放在方形浅底里。
const STATE_ICON_BOX: f32 = 30.;
/// 主操作相对说明文字的左缩进。图标块在标题上方（同一列），所以主操作与标题、说明同列贴齐。
pub const STATE_ACTION_INSET: f32 = 0.;
/// 说明文字的最大宽度：窄面板里按面板宽换行，宽页面里不拉成一整行。
pub const STATE_DETAIL_MAX_W: f32 = 360.;

/// 三态共用排版：左对齐成块（图标块 + 标题 + 说明 + 可选主操作），不居中、不保留内容区的表头骨架。
fn state_block(kind: StateKind, icon_name: IconName, heading: impl Into<SharedString>, detail: impl Into<SharedString>, p: &Palette, action: Option<AnyElement>) -> impl gpui_kit::ParentElement + gpui_kit::Styled + IntoElement {
    let (fg, bg) = kind.icon_colors(p);
    let mut block = div()
        .id(kind.block_id())
        .test_support()
        .flex()
        .flex_col()
        .items_start()
        .gap(px(8.))
        .px(px(12.))
        .py(px(14.))
        // 窄面板里也要能收窄：GPUI 文本的最小内容宽是整行宽，不写 min_w(0) 就会撑出面板。
        .min_w(px(0.))
        .max_w_full()
        .child(
            div()
                .flex_none()
                .size(px(STATE_ICON_BOX))
                .rounded(px(Radius::CONTROL))
                .bg(bg)
                .flex()
                .items_center()
                .justify_center()
                .child(icon(icon_name, 16., fg)),
        )
        .child(title(heading, TypeScale::MD, p))
        .child(
            div()
                .id(kind.detail_id())
                .test_support()
                .w_full()
                .max_w(px(STATE_DETAIL_MAX_W))
                .text_size(px(TypeScale::SM))
                .text_color(p.text_muted)
                .child(detail.into()),
        );
    if let Some(action) = action {
        // 行容器按内容宽排列按钮：主操作是一个按钮，不被拉成整行宽的横条。
        block = block.child(div().id("state-action").test_support().w_full().pt(px(2.)).pl(px(STATE_ACTION_INSET)).flex().items_center().gap(px(8.)).child(action));
    }
    block
}

/// 空态（无数据）：左对齐成块，图标、标题、说明；主操作用 `empty_state_with_action`。
pub fn empty_state(icon_name: IconName, heading: impl Into<SharedString>, detail: impl Into<SharedString>, p: &Palette) -> impl gpui_kit::ParentElement + gpui_kit::Styled + IntoElement {
    state_block(StateKind::NoData, icon_name, heading, detail, p, None)
}

/// 无数据空态 + 上下文主操作（新建、添加、打开…）。空态替换内容区，不保留表头骨架。
// 底座入口：本任务只提供语义，调用方在 T2–T15 逐屏接入。
#[allow(dead_code)]
pub fn empty_state_with_action(icon_name: IconName, heading: impl Into<SharedString>, detail: impl Into<SharedString>, action: impl IntoElement, p: &Palette) -> impl gpui_kit::ParentElement + gpui_kit::Styled + IntoElement {
    state_block(StateKind::NoData, icon_name, heading, detail, p, Some(action.into_any_element()))
}

/// 无命中空态（筛选、搜索）：说明怎么调整条件，主操作通常是清除筛选。
// 底座入口：本任务只提供语义，调用方在 T2–T15 逐屏接入。
#[allow(dead_code)]
pub fn no_match_state(icon_name: IconName, heading: impl Into<SharedString>, detail: impl Into<SharedString>, p: &Palette) -> impl gpui_kit::ParentElement + gpui_kit::Styled + IntoElement {
    state_block(StateKind::NoMatch, icon_name, heading, detail, p, None)
}

/// 无命中空态 + 清除条件一类的下一步。
// 底座入口：本任务只提供语义，调用方在 T2–T15 逐屏接入。
#[allow(dead_code)]
pub fn no_match_state_with_action(icon_name: IconName, heading: impl Into<SharedString>, detail: impl Into<SharedString>, action: impl IntoElement, p: &Palette) -> impl gpui_kit::ParentElement + gpui_kit::Styled + IntoElement {
    state_block(StateKind::NoMatch, icon_name, heading, detail, p, Some(action.into_any_element()))
}

/// 错误态：与空态同一排版，图标块换危险色。
pub fn error_state(heading: impl Into<SharedString>, detail: impl Into<SharedString>, p: &Palette) -> impl gpui_kit::ParentElement + gpui_kit::Styled + IntoElement {
    state_block(StateKind::Error, IconName::CircleAlert, heading, detail, p, None)
}

/// 错误态 + 重试一类的纠正动作。
// 底座入口：本任务只提供语义，调用方在 T2–T15 逐屏接入。
#[allow(dead_code)]
pub fn error_state_with_action(heading: impl Into<SharedString>, detail: impl Into<SharedString>, action: impl IntoElement, p: &Palette) -> impl gpui_kit::ParentElement + gpui_kit::Styled + IntoElement {
    state_block(StateKind::Error, IconName::CircleAlert, heading, detail, p, Some(action.into_any_element()))
}

/// 首屏加载（整块）：居中的小号转圈 + 一行说明（规范 S8.5 三种状态之一）。加载优先于空态。
/// 能预估内容行结构时优先用 `skeleton_state`（骨架 + 增量内容），只有一次性整块读取才回落到这里。
pub fn loading_state(text: impl Into<SharedString>, p: &Palette) -> impl gpui_kit::ParentElement + gpui_kit::Styled + IntoElement {
    div()
        .id("loading-state")
        .test_support()
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

/// 局部加载（行、节点、字段旁）：一行小号转圈，贴左、单行高，不占整块。
/// 局部刷新不整块换成 `loading_state`，页面也不用自己拼转圈加文字。
// 底座入口：本任务只提供语义，调用方在 T2–T15 逐屏接入。
#[allow(dead_code)]
pub fn inline_loading(text: impl Into<SharedString>, layout: &Metrics, p: &Palette) -> impl gpui_kit::ParentElement + gpui_kit::Styled + IntoElement {
    div()
        .id("inline-loading")
        .test_support()
        .flex()
        .items_center()
        .gap(px(6.))
        .min_h(px(layout.strip_h()))
        .text_size(px(TypeScale::XS))
        .text_color(p.text_muted)
        .child(gpui_kit::component::spinner::Spinner::new().small().color(p.accent))
        .child(text.into())
}

/// 首屏骨架：按当前密度排出的占位行，读到数据后逐行换成真实内容（增量），
/// 不用全屏转圈盖住整个工作台。占位块是静态色块，没有位移动画，减少动效时同样成立。
// 底座入口：本任务只提供语义，调用方在 T2–T15 逐屏接入。
#[allow(dead_code)]
pub fn skeleton_state(rows: usize, layout: &Metrics, p: &Palette) -> impl gpui_kit::ParentElement + gpui_kit::Styled + IntoElement {
    const WIDTHS: [f32; 6] = [0.62, 0.78, 0.45, 0.70, 0.58, 0.86];
    let rows = rows.max(1);
    let bar_h = (layout.row_h - 10.).max(8.);
    let mut block = div()
        .id("skeleton-state")
        .test_support()
        .flex()
        .flex_col()
        .gap(px(6.))
        .px(px(12.))
        .py(px(10.))
        // 窄面板里也要能收窄：占位行按容器宽排，不撑出面板。
        .min_w(px(0.))
        .max_w_full();
    for (index, ratio) in WIDTHS.iter().copied().cycle().take(rows).enumerate() {
        block = block.child(
            div()
                .id(("skeleton-row", index))
                .test_support()
                .w_full()
                .h(px(layout.row_h))
                .flex()
                .items_center()
                .child(div().flex_none().h(px(bar_h)).w(gpui_kit::relative(ratio)).rounded(px(Radius::CONTROL)).bg(p.line_soft)),
        );
    }
    block
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
    // 菜单表里的组合键用平台无关的 Mod+ 记法；显示时换成本平台的修饰键。
    let key = key.replace("Mod+", "Ctrl+");
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

/// 分段选择：一组互斥选项，选中项高亮；高度由调用方按当前密度的控件高传入。
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
                for (surface_name, bg) in [("surface", p.surface), ("chrome", p.chrome), ("raised", p.raised), ("sunken", p.sunken)] {
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
            assert_ne!(p.sunken, p.surface, "凹陷面和工作区不是同一色");
            assert_ne!(p.sunken, p.chrome, "凹陷面和外壳不是同一色");
            assert_ne!(p.sunken, p.raised, "凹陷面和浮层不是同一色");
            assert!(luminance(p.sunken) < luminance(p.surface), "凹陷面比工作区更沉");
        }
    }

    /// 三档密度必须真的拉开行高，否则切换只是文案变化。
    /// 紧凑与标准按规范 S9（紧凑 26/23/26、标准 32/30/30，行/树/控件）；宽松不小于标准。
    #[test]
    fn density_metrics_are_distinct_and_ordered() {
        let (compact, standard, relaxed) = (Metrics::compact(), Metrics::standard(), Metrics::relaxed());
        assert!(compact.row_h < standard.row_h && standard.row_h < relaxed.row_h);
        assert!(compact.tree_row_h < standard.tree_row_h && standard.tree_row_h < relaxed.tree_row_h);
        assert!(compact.control_h < standard.control_h && standard.control_h < relaxed.control_h);
        assert!(compact.pad < standard.pad && standard.pad < relaxed.pad);
        assert_eq!(relaxed.pad, 12., "宽松档保留锁定的 pad");
        assert_eq!((compact.page_pad, compact.page_pad_y), (16., 14.));
        assert_eq!((standard.page_pad, standard.page_pad_y), (20., 18.));
        assert_eq!((relaxed.page_pad, relaxed.page_pad_y), (26., 24.));
        assert!(compact.page_pad < standard.page_pad && standard.page_pad < relaxed.page_pad);
        assert!(compact.page_pad_y < standard.page_pad_y && standard.page_pad_y < relaxed.page_pad_y);
        assert_eq!((compact.pane_pad(), standard.pane_pad(), relaxed.pane_pad()), (10., 12., 16.));
        assert_eq!((compact.section_gap(), standard.section_gap(), relaxed.section_gap()), (12., 16., 20.));
        assert_eq!((compact.card_pad(), standard.card_pad(), relaxed.card_pad()), (12., 14., 18.));
        assert_eq!((compact.header_h(), standard.header_h(), relaxed.header_h()), (42., 48., 54.));
        assert!(compact.pane_pad() < standard.pane_pad() && standard.pane_pad() < relaxed.pane_pad());
        assert!(compact.section_gap() < standard.section_gap() && standard.section_gap() < relaxed.section_gap());
        assert!(compact.card_pad() < standard.card_pad() && standard.card_pad() < relaxed.card_pad());
        assert!(compact.header_h() < standard.header_h() && standard.header_h() < relaxed.header_h());
        assert_eq!(relaxed.page_pad_for(NARROW_PAGE_WIDTH), relaxed.page_pad, "760 不钳制");
        assert_eq!(relaxed.page_pad_for(NARROW_PAGE_WIDTH - 1.), compact.page_pad, "窄于 760 收到紧凑水平边距");
        assert_eq!(standard.for_viewport(400.).page_pad, compact.page_pad);
        assert_eq!(standard.for_viewport(400.).page_pad_y, standard.page_pad_y, "钳制不动垂直边距");
        assert_eq!(compact.for_viewport(320.).page_pad, compact.page_pad);
        assert_eq!((compact.row_h, compact.tree_row_h, compact.control_h), (26., 23., 26.), "紧凑密度按规范 S2.2 / S9");
        assert_eq!((standard.row_h, standard.tree_row_h, standard.control_h), (32., 30., 30.), "标准密度按规范 S9");
        assert_eq!((relaxed.row_h, relaxed.tree_row_h, relaxed.control_h), (40., 36., 32.), "宽松密度不小于标准");
        assert_eq!(crate::app::Shell::default().density, crate::app::Density::Compact, "默认密度为紧凑（规范 S9）");
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
            for (name, bg) in [("surface", p.surface), ("chrome", p.chrome), ("raised", p.raised), ("rail", p.rail), ("hover", p.hover), ("sunken", p.sunken)] {
                let ratio = contrast(ring, bg);
                assert!(ratio >= 3., "focus ring on {name}: {ratio:.2} (dark={})", p.dark);
            }
            let off = interaction_style(&p, true);
            assert_eq!((off.hover_bg, off.active_bg, off.ring, off.focusable), (None, None, None, false), "禁用去掉反馈且不可聚焦");
            assert!(off.opacity < 1.);
        }
    }

    /// 焦点环只描一圈边，不涂满元素：没有背景的活动栏按钮、未选中的标签/分段项/列表行聚焦时，
    /// 中间的图标与文字底色不变。gpui 的外侧投影是一块实心矩形（只靠不透明背景遮住中间），
    /// 内侧阴影则挖掉 `bounds.dilate(-spread)` 的洞，只着色边缘。
    #[test]
    fn focus_ring_outlines_the_element_without_filling_it() {
        for p in [Palette::light(), Palette::dark()] {
            let style = interaction_style(&p, false);
            let ring = focus_ring(style.ring.unwrap(), style.ring_width);
            assert!(ring.inset, "内侧环：不会在无背景元素上涂出整块强调色");
            assert_eq!((ring.blur_radius, ring.spread_radius), (px(0.), px(2.)), "2px 实线，不模糊");
            assert_eq!(ring.offset, gpui_kit::point(px(0.), px(0.)));
            // 活动栏按钮、紧凑分段项、结果标签：环内都留出未着色的内容区。
            for (w, h) in [(40., 40.), (60., 22.), (90., 26.)] {
                let bounds = gpui_kit::Bounds::new(gpui_kit::point(px(0.), px(0.)), gpui_kit::size(px(w), px(h)));
                let hole = bounds.dilate(-ring.spread_radius);
                assert_eq!((hole.size.width, hole.size.height), (px(w - 4.), px(h - 4.)), "中间不着色");
            }
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

    /// 网格/编辑器/树一类的焦点容器持有焦点时，鼠标点一下自定义可点元素（标签、分段项、活动栏按钮）：
    /// 点击照常触发，但键盘焦点留在原容器，既有快捷键不受影响。
    struct Holder {
        grid: gpui_kit::FocusHandle,
        clicks: u32,
    }

    impl Render for Holder {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let p = Palette::of(cx);
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(div().id("holder-grid").track_focus(&self.grid).w(px(200.)).h(px(100.)).child("grid"))
                .child(
                    interactive(div().id("holder-tab"), &p, false)
                        .test_support()
                        .w(px(80.))
                        .h(px(24.))
                        .child("tab")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.clicks += 1;
                            cx.notify();
                        })),
                )
        }
    }

    #[gpui_kit::test]
    fn clicking_an_interactive_element_keeps_focus_where_it_was(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, |_, cx| cx.new(|cx| Holder { grid: cx.focus_handle(), clicks: 0 })).unwrap()
        });
        let grid = cx.update(|cx| view.read(cx).grid.clone());
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.focus(&grid, cx);
            window.render_frame(cx);
            window.click("holder-tab", cx);
            window.render_frame(cx);
            assert!(grid.is_focused(window), "点击后焦点仍在网格上");
        })
        .unwrap();
        assert_eq!(cx.update(|cx| view.read(cx).clicks), 1, "点击照常生效");
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
        ("app/shell/chrome.rs", "px(RAIL_INDICATOR_RADIUS)", "活动栏激活指示条只有 3px 宽，是线条而非控件；只认这一个具名常量"),
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

    /// app/ 与 ui/ 的字号只取五档刻度、圆角只取标签 3 / 控件 4 / 面板 6；例外逐条列出理由。
    #[test]
    fn style_literals_follow_design_tokens() {
        assert_eq!([TypeScale::XS, TypeScale::SM, TypeScale::MD, TypeScale::LG, TypeScale::XL], [11., 12., 13., 15., 18.], "规范 S4.1 五档字号");
        assert_eq!((Radius::TAG, Radius::CONTROL, Radius::PANEL), (3., 4., 6.), "规范 S4.2 标签 3 / 控件 4 / 面板 6");
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

#[cfg(test)]
mod state_tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{Bounds, Point, Render, TestAppContext, WindowBounds, WindowOptions};

    const HEADING: &str = "还没有表";
    const DETAIL: &str = "连接数据库后这里会列出表，也可以用 Ctrl+N 新建查询。";

    struct Probe {
        kind: StateKind,
        action: bool,
        clicks: u32,
    }

    impl Render for Probe {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let p = Palette::of(cx);
            let action = self.action.then(|| {
                primary_button("state-action-button", None, "重试", cx.listener(|this: &mut Self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                    this.clicks += 1;
                    cx.notify();
                }))
                .into_any_element()
            });
            let block = match (self.kind, action) {
                (StateKind::NoData, None) => empty_state(IconName::Database, HEADING, DETAIL, &p).into_any_element(),
                (StateKind::NoData, Some(action)) => empty_state_with_action(IconName::Database, HEADING, DETAIL, action, &p).into_any_element(),
                (StateKind::NoMatch, None) => no_match_state(IconName::Search, HEADING, DETAIL, &p).into_any_element(),
                (StateKind::NoMatch, Some(action)) => no_match_state_with_action(IconName::Search, HEADING, DETAIL, action, &p).into_any_element(),
                (StateKind::Error, None) => error_state(HEADING, DETAIL, &p).into_any_element(),
                (StateKind::Error, Some(action)) => error_state_with_action(HEADING, DETAIL, action, &p).into_any_element(),
            };
            div().id("state-host").test_support().size_full().child(block)
        }
    }

    fn open(cx: &mut TestAppContext, width: f32, height: f32, kind: StateKind, action: bool) -> (gpui_kit::AnyWindowHandle, Entity<Probe>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            apply_theme(false, cx);
        });
        let bounds = Bounds { origin: Point::default(), size: gpui_kit::size(px(width), px(height)) };
        cx.update(|cx| {
            gpui_kit::open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, cx, |_, cx| {
                cx.new(|_| Probe { kind, action, clicks: 0 })
            })
            .expect("state window")
        })
    }

    /// 空/错误态左对齐成块（规范 S8.5、需求 R4）：图标、标题、说明、主操作都贴左侧文字列，
    /// 不是整块居中；说明最宽 360 并在 1360/900/640 都不越出视口。主操作真的可点。
    #[gpui_kit::test]
    fn states_are_left_aligned_with_their_action_at_every_width(cx: &mut TestAppContext) {
        for width in [1360., 900., 640.] {
            let (window, view) = open(cx, width, 900., StateKind::NoData, true);
            let (host, detail, action, button) = cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    (
                        window.find("state-host").bounds(),
                        window.find("empty-state-detail").bounds(),
                        window.find("state-action").bounds(),
                        window.find("state-action-button").bounds(),
                    )
                })
                .unwrap();
            let inset = f32::from(detail.origin.x - host.origin.x);
            assert!((inset - 12.).abs() < 0.5, "{width}: 说明贴左侧内边距，不是居中（实际 {inset}）");
            assert!((f32::from(action.origin.x - detail.origin.x)).abs() < 0.5, "{width}: 主操作与说明同一列");
            let button_inset = f32::from(button.origin.x - detail.origin.x);
            assert!((button_inset - STATE_ACTION_INSET).abs() < 0.5, "{width}: 主操作与标题文字左对齐（实际 {button_inset}）");
            assert!(f32::from(button.size.width) < 200., "{width}: 主操作按内容宽，不被拉成整行（实际 {}）", f32::from(button.size.width));
            assert!(f32::from(detail.size.width) <= STATE_DETAIL_MAX_W + 0.5, "{width}: 说明最宽 {STATE_DETAIL_MAX_W}");
            let right = f32::from(detail.origin.x + detail.size.width);
            assert!(right <= f32::from(host.origin.x + host.size.width) + 0.5, "{width}: 说明右缘 {right} 越出视口");
            cx.update_window(window, |_, window, cx| window.click("state-action-button", cx)).unwrap();
            assert_eq!(cx.update(|cx| view.read(cx).clicks), 1, "{width}: 主操作可点");
        }
    }

    /// 三类语义共用一份排版与几何，只换图标色调：无数据 / 无命中 / 错误不能各画一套。
    #[gpui_kit::test]
    fn empty_no_match_and_error_share_one_layout_with_distinct_tones(cx: &mut TestAppContext) {
        for p in [Palette::light(), Palette::dark()] {
            let data = StateKind::NoData.icon_colors(&p);
            let no_match = StateKind::NoMatch.icon_colors(&p);
            let error = StateKind::Error.icon_colors(&p);
            assert_eq!(data, (p.accent_text, p.accent_soft), "无数据用强调浅底");
            assert_eq!(no_match, (p.text_muted, p.hover), "无命中用中性底，不报警");
            assert_eq!(error, (p.danger, p.danger_soft), "错误用危险色");
            assert_ne!(data.1, no_match.1, "无数据与无命中底色不同");
            assert_ne!(no_match.1, error.1, "无命中与错误底色不同");
        }
        let mut geometry = Vec::new();
        for (kind, block_id, detail_id) in [
            (StateKind::NoData, "empty-state", "empty-state-detail"),
            (StateKind::NoMatch, "no-match-state", "empty-state-detail"),
            (StateKind::Error, "error-state", "error-state-detail"),
        ] {
            let (window, _) = open(cx, 900., 600., kind, false);
            let (block, detail) = cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    (window.find(block_id).bounds(), window.find(detail_id).bounds())
                })
                .unwrap();
            let inset = f32::from(detail.origin.x - block.origin.x);
            assert!((inset - 12.).abs() < 0.5, "{block_id}: 三种语义的说明都在同一列（实际 {inset}）");
            geometry.push((f32::from(detail.origin.y - block.origin.y), f32::from(block.size.height), f32::from(detail.size.width)));
        }
        assert_eq!(geometry[0], geometry[1], "无数据与无命中共用同一几何");
        assert_eq!(geometry[1], geometry[2], "无命中与错误共用同一几何");
    }

    struct LoadingProbe {
        layout: Metrics,
        inline: bool,
    }

    impl Render for LoadingProbe {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let p = Palette::of(cx);
            let body = if self.inline {
                inline_loading("正在读取表…", &self.layout, &p).into_any_element()
            } else {
                loading_state("正在读取表…", &p).size_full().into_any_element()
            };
            div().id("state-host").test_support().size_full().child(body)
        }
    }

    fn open_loading(cx: &mut TestAppContext, layout: Metrics, inline: bool) -> gpui_kit::AnyWindowHandle {
        cx.update(|cx| {
            gpui_kit::init(cx);
            apply_theme(false, cx);
        });
        let bounds = Bounds { origin: Point::default(), size: gpui_kit::size(px(640.), px(400.)) };
        cx.update(|cx| {
            let (window, _) = gpui_kit::open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, cx, |_, cx| {
                cx.new(|_| LoadingProbe { layout, inline })
            })
            .expect("loading window");
            window
        })
    }

    /// 局部加载是一行（行高 = `strip_h`、内容宽、贴左侧），首屏加载才整块；
    /// 两者不能互相冒充（需求 R4：加载优先骨架/增量，局部不整块换转圈）。
    #[gpui_kit::test]
    fn local_loading_stays_one_line_while_first_screen_loading_fills_the_block(cx: &mut TestAppContext) {
        let layout = Metrics::compact();
        let window = open_loading(cx, layout, true);
        let (host, inline) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (window.find("state-host").bounds(), window.find("inline-loading").bounds())
            })
            .unwrap();
        assert!((f32::from(inline.size.height) - layout.strip_h()).abs() < 0.5, "局部加载只有一行高");
        assert!((f32::from(inline.origin.x - host.origin.x)).abs() < 0.5, "局部加载贴左侧，不居中");
        let window = open_loading(cx, layout, false);
        let (host, block) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (window.find("state-host").bounds(), window.find("loading-state").bounds())
            })
            .unwrap();
        assert!((f32::from(block.size.width) - f32::from(host.size.width)).abs() < 0.5, "首屏加载整块占满");
        assert!(f32::from(block.size.height) > layout.strip_h() * 2., "首屏加载是整块，不是一行");
    }

    struct SkeletonProbe {
        layout: Metrics,
    }

    impl Render for SkeletonProbe {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let p = Palette::of(cx);
            div().id("state-host").test_support().size_full().flex().flex_col().child(skeleton_state(5, &self.layout, &p))
        }
    }

    /// 首屏骨架按当前密度排行，行数正好是请求数；它替代整块转圈，不额外叠一层全屏 Spinner。
    #[gpui_kit::test]
    fn skeleton_state_is_density_driven_and_replaces_the_full_screen_spinner(cx: &mut TestAppContext) {
        for (layout, row_h) in [(Metrics::compact(), 26.), (Metrics::standard(), 32.)] {
            cx.update(|cx| {
                gpui_kit::init(cx);
                apply_theme(false, cx);
            });
            let bounds = Bounds { origin: Point::default(), size: gpui_kit::size(px(900.), px(600.)) };
            let window = cx.update(|cx| {
                let (window, _) = gpui_kit::open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, cx, |_, cx| {
                    cx.new(|_| SkeletonProbe { layout })
                })
                .expect("skeleton window");
                window
            });
            let (rows, spinner) = cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    let rows: Vec<f32> = (0usize..5).map(|index| f32::from(window.find(("skeleton-row", index)).bounds().size.height)).collect();
                    (rows, window.try_find("loading-state").is_some())
                })
                .unwrap();
            assert_eq!(rows, vec![row_h; 5], "骨架行高随密度（{row_h}）");
            assert!(!spinner, "骨架不叠一层全屏转圈");
            let extra = cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    window.try_find(("skeleton-row", 5usize)).is_some()
                })
                .unwrap();
            assert!(!extra, "只画请求的行数");
        }
    }

    struct RowProbe;

    impl Render for RowProbe {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let p = Palette::of(cx);
            div().size_full().child(row_states(div().id("managed-row").child("行"), &p))
        }
    }

    /// 反向：容器持有焦点的列表行（`row_states`）不是 Tab 停靠点，Tab 不会跑进行里；
    /// 焦点仍留在树/面板容器上，快捷键照常生效（规范 S12.3）。
    #[gpui_kit::test]
    fn row_states_are_not_tab_stops(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            apply_theme(false, cx);
        });
        let (window, _) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| RowProbe)).expect("row window")
        });
        let focused = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.focus_next(cx);
                window.render_frame(cx);
                window.focused(cx).is_some()
            })
            .unwrap();
        assert!(!focused, "列表行不抢键盘焦点");
    }
}

#[cfg(test)]
mod metric_tests {
    use super::*;
    use gpui_kit::TestAppContext;

    /// 派生高度只由密度决定：紧凑/标准/宽松三档严格递增，且不是写死的常量。
    #[test]
    fn derived_heights_follow_density_and_never_coincide() {
        let (compact, standard, relaxed) = (Metrics::compact(), Metrics::standard(), Metrics::relaxed());
        assert_eq!(
            (compact.toolbar_h(), compact.bar_h(), compact.tools_h(), compact.strip_h()),
            (38., 30., 36., 20.),
            "紧凑派生高度（规范 S12.2：bar_h 紧凑 30）"
        );
        assert_eq!((standard.toolbar_h(), standard.bar_h(), standard.tools_h(), standard.strip_h()), (42., 34., 40., 22.));
        assert_eq!((relaxed.toolbar_h(), relaxed.bar_h(), relaxed.tools_h(), relaxed.strip_h()), (46., 38., 44., 24.));
        for (left, right) in [(&compact, &standard), (&standard, &relaxed)] {
            assert!(left.toolbar_h() < right.toolbar_h());
            assert!(left.bar_h() < right.bar_h());
            assert!(left.tools_h() < right.tools_h());
            assert!(left.strip_h() < right.strip_h());
        }
        // 反向：派生高度不是常量，不能三档相同
        assert_ne!(compact.bar_h(), standard.bar_h());
        assert_ne!(standard.strip_h(), relaxed.strip_h());
        assert!(compact.tools_h() > compact.bar_h(), "网格工具条高于结果头");
    }

    /// 内边距按 density 三档取值，边界互斥：31.9 仍是紧凑、32 起算标准、40 起算宽松。
    #[test]
    fn pane_padding_switches_on_exact_density_boundaries() {
        let mut metrics = Metrics::compact();
        assert_eq!((metrics.pane_pad(), metrics.section_gap(), metrics.card_pad()), (10., 12., 12.));
        metrics.row_h = 31.9;
        assert_eq!(metrics.pane_pad(), 10., "31.9 仍是紧凑档");
        metrics.row_h = 32.;
        assert_eq!((metrics.pane_pad(), metrics.section_gap(), metrics.card_pad()), (12., 16., 14.), "32 起算标准档");
        metrics.row_h = 39.9;
        assert_eq!(metrics.pane_pad(), 12., "39.9 仍是标准档");
        metrics.row_h = 40.;
        assert_eq!((metrics.pane_pad(), metrics.section_gap(), metrics.card_pad()), (16., 20., 18.), "40 起算宽松档");
    }

    /// 三档宽度只钳水平页边距：1360/900 保留当前密度，640 收到紧凑；垂直边距与行高不动。
    #[test]
    fn viewport_clamping_touches_only_horizontal_padding_at_the_three_widths() {
        for layout in [Metrics::compact(), Metrics::standard(), Metrics::relaxed()] {
            for width in [1360., 900.] {
                let scoped = layout.for_viewport(width);
                assert_eq!(scoped.page_pad, layout.page_pad, "{width}: 宽屏不钳制");
                assert_eq!(scoped.page_pad_y, layout.page_pad_y);
                assert_eq!(scoped.row_h, layout.row_h);
            }
            let narrow = layout.for_viewport(640.);
            assert_eq!(narrow.page_pad, Metrics::compact().page_pad, "640 收到紧凑水平边距");
            assert_eq!(narrow.page_pad_y, layout.page_pad_y, "640 不动垂直边距（反向）");
            assert_eq!(narrow.row_h, layout.row_h, "640 不改密度（反向）");
            assert_eq!(narrow.tree_row_h, layout.tree_row_h);
        }
        assert_eq!(Metrics::relaxed().page_pad_for(NARROW_PAGE_WIDTH), Metrics::relaxed().page_pad, "760 是边界内");
        assert_eq!(Metrics::relaxed().page_pad_for(NARROW_PAGE_WIDTH - 1.), Metrics::compact().page_pad, "边界下一像素就钳制");
        assert_eq!(Metrics::compact().page_pad_for(640.), Metrics::compact().page_pad, "紧凑档钳制后不变");
    }

    /// 密度全局量：未发布时回落紧凑，发布同值不触发重绘，改档才返回 true。
    #[gpui_kit::test]
    fn density_global_defaults_to_compact_and_reports_real_changes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            assert_eq!(Metrics::of(cx), Metrics::compact(), "未发布时回落紧凑（规范 S9）");
            assert!(Metrics::standard().publish(cx), "第一次发布有变化");
            assert_eq!(Metrics::of(cx), Metrics::standard());
            assert!(!Metrics::standard().publish(cx), "同值重复发布不触发重绘");
            assert!(Metrics::compact().publish(cx), "切回紧凑也是变化");
            assert_eq!(Metrics::of(cx), Metrics::compact());
        });
    }

    /// 主题色板跟随应用主题；深色不是浅色的机械反色。
    #[gpui_kit::test]
    fn palette_of_tracks_the_theme_and_dark_is_not_an_inversion(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            apply_theme(false, cx);
            assert!(!Palette::of(cx).dark, "浅色主题");
            apply_theme(true, cx);
            assert!(Palette::of(cx).dark, "深色主题");
            apply_theme(false, cx);
            assert!(!Palette::of(cx).dark, "切回浅色");
        });
        let (light, dark) = (Palette::light(), Palette::dark());
        let flip = |color: Hsla| {
            let mut out = color;
            out.h = (out.h + 0.5).fract();
            out.l = 1. - out.l;
            out
        };
        for (name, light_color, dark_color) in [("surface", light.surface, dark.surface), ("text", light.text, dark.text), ("accent", light.accent, dark.accent)] {
            assert_ne!(light_color, dark_color, "{name}: 两套主题取值不同");
            assert_ne!(flip(light_color), dark_color, "{name}: 深色是单独调的，不是反色");
        }
    }
}
