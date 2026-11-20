//! 共享页签：工作项、查询结果与固定结果共用同一份几何、状态与滚动规则。
//!
//! 业务层仍拥有自己的语义（工作项切换、结果选择、固定与重命名、关闭确认），
//! 但类型图标、标题、选中/运行/未保存、关闭动作、完整标题和横向滚动只在这里实现一次。
//! 页面不再自绘第二套页签样式。

use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyView, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div, px,
};

use super::kit::{self, HitArea, IconSize, Metrics, Palette, Radius, Tip as _, TypeScale};

/// 页签最大宽度：长标题在这里截断，完整标题留在 tooltip 与无障碍名称上。
pub const TAB_MAX_W: f32 = 220.;
/// 页签最小宽度：标题很短（新建的空查询）也保持可点，关闭动作不与标题挤在一起。
pub const TAB_MIN_W: f32 = 96.;
/// 类型图标、标题与未保存标记之间的间距。
pub const TAB_GAP: f32 = 7.;
/// 左侧内边距：图标不贴边。
pub const TAB_PAD_L: f32 = 12.;
/// 右侧内边距：关闭动作与相邻页签之间留出呼吸。
pub const TAB_PAD_R: f32 = 5.;
/// 当前页签顶部的选中指示条高度（线条，不是控件）。
const TAB_MARK_H: f32 = 2.;

/// 一个页签的表现数据。点击后做什么由调用方挂事件，组件不持有业务语义。
#[derive(Clone)]
pub struct TabSpec {
    /// 元素 id，同时是测试与无障碍定位用的名字（例如 `tab-3`）。
    pub id: SharedString,
    pub icon: IconName,
    pub title: SharedString,
    pub active: bool,
    pub running: bool,
    pub dirty: bool,
    /// 是否关联生产环境连接：顶部标记条改用危险色，防止跨环境误操作。
    pub prod: bool,
    /// 连接/库上下文说明（例如 `Atlas / orders · PROD`）：进入 tooltip，不占页签宽度。
    pub context: Option<SharedString>,
    /// 状态短标签（已翻译，例如“未保存”）：进入 tooltip，状态不只靠颜色表达。
    pub status: Option<SharedString>,
    /// 预览标签（单击对象树打开、会被下一次单击就地替换）：标题用斜体区分，双击或操作后转为固定标签。
    pub preview: bool,
    /// 固定标签：标题后画一个小图钉，状态写进 tooltip。
    pub pinned: Option<SharedString>,
    /// 运行中的转圈：调用方持有的隔离实体（`ui::spinner_view`），动画帧只通知它自己。
    /// 没给时运行态只把类型图标改成强调色，不在页签里直接画动画。
    pub spinner: Option<AnyView>,
}

impl TabSpec {
    pub fn new(id: impl Into<SharedString>, icon: IconName, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            icon,
            title: title.into(),
            active: false,
            running: false,
            dirty: false,
            prod: false,
            context: None,
            status: None,
            preview: false,
            pinned: None,
            spinner: None,
        }
    }

    /// 运行中的转圈视图；只在 `running` 时画出来。
    pub fn spinner(mut self, spinner: Option<AnyView>) -> Self {
        self.spinner = spinner;
        self
    }

    /// 固定标签；参数是 tooltip 里的状态文字（已翻译）。
    pub fn pinned(mut self, label: Option<SharedString>) -> Self {
        self.pinned = label;
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn running(mut self, running: bool) -> Self {
        self.running = running;
        self
    }

    /// 生产环境警示与连接上下文提示。
    pub fn prod(mut self, prod: bool) -> Self {
        self.prod = prod;
        self
    }

    pub fn context(mut self, context: Option<SharedString>) -> Self {
        self.context = context;
        self
    }

    pub fn preview(mut self, preview: bool) -> Self {
        self.preview = preview;
        self
    }

    /// 未保存修改：画标记，并把状态写进 tooltip。
    pub fn dirty(mut self, dirty: bool, status: Option<SharedString>) -> Self {
        self.dirty = dirty;
        self.status = status;
        self
    }

    /// 完整标题：截断后仍可获取，连接上下文与未保存状态一并带上。
    fn hint(&self) -> SharedString {
        let mut parts = vec![self.title.to_string()];
        if let Some(ctx) = self.context.as_ref().filter(|ctx| !ctx.is_empty()) {
            parts.push(ctx.to_string());
        }
        if let Some(pinned) = self.pinned.as_ref().filter(|label| !label.is_empty()) {
            parts.push(pinned.to_string());
        }
        if self.dirty {
            if let Some(status) = self.status.as_ref().filter(|s| !s.is_empty()) {
                parts.push(status.to_string());
            }
        }
        SharedString::from(parts.join(" · "))
    }
}

/// 页签主体：高度撑满页签条，图标、标题、未保存标记和关闭动作共用同一条垂直中心。
/// 返回的元素由调用方继续挂 on_click / 右键 / 中键，再追加 `close_action`。
pub fn tab(spec: TabSpec, p: &Palette) -> impl IntoElement + gpui_kit::ParentElement + gpui_kit::StatefulInteractiveElement {
    let title = spec.title.clone();
    let hint = spec.hint();
    let tone = if spec.active { p.text } else { p.text_muted };
    let icon_tone = if spec.prod && spec.active {
        p.danger
    } else if spec.active {
        p.accent_text
    } else {
        p.text_muted
    };
    let weight = if spec.active { FontWeight::MEDIUM } else { FontWeight::NORMAL };
    // 图标与转圈占同一个方形槽位，页签在运行/空闲之间切换时标题不左右跳。
    let slot = |child: AnyElement| {
        div()
            .id(ElementId::Name(SharedString::from(format!("{}-icon", spec.id))))
            .test_support()
            .flex_none()
            .size(px(IconSize::CONTROL))
            .flex()
            .items_center()
            .justify_center()
            .child(child)
            .into_any_element()
    };
    let leading = if let Some(spinner) = spec.spinner.clone().filter(|_| spec.running) {
        slot(spinner.into_any_element())
    } else if spec.running {
        slot(kit::icon(spec.icon, IconSize::CONTROL, p.accent).into_any_element())
    } else {
        slot(kit::icon(spec.icon, IconSize::CONTROL, icon_tone).into_any_element())
    };
    kit::interactive_states(div().id(ElementId::Name(spec.id.clone())), p, false)
        .test_support()
        .role(gpui_kit::Role::Tab)
        .aria_label(spec.title.clone())
        .aria_selected(spec.active)
        .tooltip_text(hint)
        .group("tab")
        .relative()
        .flex_none()
        .h_full()
        .min_w(px(TAB_MIN_W))
        .max_w(px(TAB_MAX_W))
        .pl(px(TAB_PAD_L))
        .pr(px(TAB_PAD_R))
        .flex()
        .items_center()
        .gap(px(TAB_GAP))
        .border_r_1()
        .border_color(if spec.active { p.line } else { p.line_soft })
        .when(spec.active, |this| this.bg(p.surface).text_color(p.text))
        .when(!spec.active, |this| this.text_color(p.text_muted).hover(|style| style.bg(p.hover).text_color(p.text)))
        .when(spec.active || spec.prod, |this| {
            let mark_color = if spec.prod {
                if spec.active { p.danger } else { p.danger.opacity(0.45) }
            } else {
                p.accent
            };
            this.child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(6.))
                    .right(px(6.))
                    .h(px(TAB_MARK_H))
                    .rounded(px(Radius::TAG))
                    .bg(mark_color),
            )
        })
        .child(leading)
        .child(
            kit::control_text(title, TypeScale::SM, weight, tone)
                .id(ElementId::Name(SharedString::from(format!("{}-title", spec.id))))
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .when(spec.preview, |this| this.italic())
                .test_support(),
        )
        .when(spec.pinned.is_some(), |this| {
            this.child(
                div()
                    .id(ElementId::Name(SharedString::from(format!("{}-pinned", spec.id))))
                    .test_support()
                    .flex_none()
                    .child(kit::icon(IconName::Pin, IconSize::INLINE, p.text_faint)),
            )
        })
        .when(spec.dirty, |this| this.child(kit::status_dot(p.warn)))
        .test_support()
}

/// 页签关闭动作：固定 20px 命中范围、12px 图标，与标题同一垂直中心。
/// 关闭确认仍由业务层处理，组件只负责把点击交回去；页签本身不可关闭时不调用它。
pub fn close_action(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    p: &Palette,
    on_click: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
) -> impl IntoElement + gpui_kit::StatefulInteractiveElement {
    kit::icon_action(id, IconName::X, label, HitArea::INLINE, if active { p.text_muted } else { p.text_faint }, p, false, false, on_click)
}

/// 页签是否已经完整落在列表视口内。用上一帧记下的列表区域、页签位置和滚动偏移判断，
/// 位置还没量出来（首帧、刚新建）时按“还看不见”处理，让调用方再请求一次滚动。
pub fn tab_visible(scroll: &gpui_kit::ScrollHandle, index: usize) -> bool {
    let viewport = scroll.bounds();
    if f32::from(viewport.size.width) <= 0.5 {
        return false;
    }
    let Some(item) = scroll.bounds_for_item(index) else { return false };
    let offset = scroll.offset();
    let left = item.origin.x + offset.x;
    if item.size.width > viewport.size.width {
        // 页签比视口还宽：对齐左缘就是能看到的最大程度。
        return f32::from(left - viewport.origin.x) <= 0.5;
    }
    let right = left + item.size.width;
    f32::from(left - viewport.origin.x) >= -0.5
        && f32::from(right - (viewport.origin.x + viewport.size.width)) <= 0.5
}

/// 一次“把当前页签滚进视口”的请求。`frames` 是已经为它请求过的帧数，
/// 用于给同一个请求设上限；当前页签或页签数量一变就重新从 0 开始。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reveal {
    pub index: usize,
    pub frames: u8,
}

impl Reveal {
    pub fn new(index: usize) -> Self {
        Self { index, frames: 0 }
    }
}

/// 同一个请求最多再请求几帧。首帧只是发起滚动，第二帧才量得出页签是否真的进来了；
/// 超过这个帧数仍在视口外，就说明几何确认不了（零宽视口、列表还没挂载、被别的布局压成
/// 零高），此时必须放弃确认并清掉请求，否则每帧都会再要一次重绘。
pub const REVEAL_MAX_FRAMES: u8 = 2;

/// 推进一帧的滚动确认：
/// - 已经完整可见 → 结束请求，之后不再动滚动位置（用户手动横向滚开不会被拉回）；
/// - 还在视口外且没超过 `REVEAL_MAX_FRAMES` → 再请求一帧，返回要滚的下标；
/// - 已经请求够帧数 → 放弃确认并清掉请求，宁可停在当前位置也不再每帧重绘。
pub fn reveal_step(pending: &mut Option<Reveal>, visible: bool) -> Option<usize> {
    let reveal = (*pending)?;
    if visible {
        *pending = None;
        return None;
    }
    if reveal.frames >= REVEAL_MAX_FRAMES {
        *pending = None;
        return None;
    }
    *pending = Some(Reveal { frames: reveal.frames + 1, ..reveal });
    Some(reveal.index)
}

/// 页签条：页签列表自己横向滚动，尾部动作（新建）固定在右侧。
/// 列表 `flex_shrink` + `min_w(0)`，窄窗口里滚动而不是被裁切；尾部动作 `flex_none`，始终留在视口内。
///
/// `scroll` 是调用方持有的滚动句柄（跨帧保存偏移，不能每帧新建），`reveal` 是本帧要把哪个
/// 页签（下标）完整滚进列表视口：当前项由 Ctrl+Tab/新建切过去之后，列表自己滚到它，
/// 不必用户手动横向拖动。调用方每帧用 `tab_visible` + `reveal_step` 确认它真的进来了
/// 才停止给 `Some`（超过 `REVEAL_MAX_FRAMES` 帧仍确认不了就放弃），
/// 确认之后不再动滚动位置，用户手动横向滚开不会被每帧拉回。
#[allow(clippy::too_many_arguments)]
pub fn tab_strip(
    id: impl Into<SharedString>,
    aria_label: impl Into<SharedString>,
    list_id: impl Into<SharedString>,
    tabs: Vec<AnyElement>,
    actions: Vec<AnyElement>,
    scroll: &gpui_kit::ScrollHandle,
    reveal: Option<usize>,
    layout: &Metrics,
    p: &Palette,
) -> AnyElement {
    tab_strip_with_double_click(id, aria_label, list_id, tabs, actions, scroll, reveal, layout, p, None)
}

/// 页签条（支持双击右侧空白区域新建查询）。
#[allow(clippy::too_many_arguments)]
pub fn tab_strip_with_double_click(
    id: impl Into<SharedString>,
    aria_label: impl Into<SharedString>,
    list_id: impl Into<SharedString>,
    tabs: Vec<AnyElement>,
    actions: Vec<AnyElement>,
    scroll: &gpui_kit::ScrollHandle,
    reveal: Option<usize>,
    layout: &Metrics,
    p: &Palette,
    on_empty_double_click: Option<kit::Click>,
) -> AnyElement {
    let mut list = div()
        .id(ElementId::Name(list_id.into()))
        .flex()
        .items_end()
        .h_full()
        .test_support()
        .flex_shrink(1.)
        .min_w(px(0.))
        // 标签条高度只够一行标签，浮动横条会压住标签文字与激活下划线；溢出靠滚轮/触控板横滑，
        // 当前标签总由 reveal 滚进视野。滚动条例外：页签条不画横向滚动条。
        .overflow_x_scroll()
        .track_scroll(scroll);
    if let Some(index) = reveal {
        // 只滚到能看见为止（gpui 的 FirstVisible 策略），不把列表强行拉回最左。
        scroll.scroll_to_item(index);
    }
    for tab in tabs {
        list = list.child(tab);
    }
    let mut trailing = div().h_full().flex_none().flex().items_center().gap(px(2.)).px(px(4.));
    for action in actions {
        trailing = trailing.child(action);
    }
    let strip_id: SharedString = id.into();
    let filler: AnyElement = match on_empty_double_click {
        Some(on_double) => div()
            .id(ElementId::Name(SharedString::from(format!("{strip_id}-empty"))))
            .test_support()
            .flex_1()
            .h_full()
            .on_click(move |event, window, cx| {
                if event.click_count() >= 2 {
                    on_double(event, window, cx);
                }
            })
            .into_any_element(),
        None => div().flex_1().h_full().into_any_element(),
    };
    div()
        .id(ElementId::Name(strip_id))
        .test_support()
        .role(gpui_kit::Role::TabList)
        .aria_label(aria_label.into())
        .flex_none()
        .w_full()
        .min_w(px(0.))
        .h(px(layout.tabs_h))
        .flex()
        .items_end()
        .bg(p.chrome)
        .border_b_1()
        .border_color(p.line)
        .child(list)
        .child(trailing)
        .child(filler)
        .into_any_element()
}
#[cfg(test)]
mod tab_tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext as _, Bounds, Context, Point, Render, TestAppContext, Window, WindowBounds, WindowOptions};

    const LONG_TITLE: &str = "一个非常长的查询标签标题，用来确认截断之后仍然能读到完整名称";

    struct Probe {
        layout: Metrics,
        count: usize,
        clicks: usize,
        closes: usize,
        /// 当前页签下标：改动它会触发一次“把当前项滚进视口”。
        active: usize,
        /// 跨帧持有的滚动句柄，和 Workbench 一样由界面自己保存。
        scroll: gpui_kit::ScrollHandle,
        /// 上一次已经请求过的（当前项，页签数量），与 Workbench 的 `tab_scroll_seen` 同义。
        seen: Option<(usize, usize)>,
        /// 还没确认进入视口的请求，与 Workbench 的 `tab_scroll_pending` 同义。
        pending: Option<Reveal>,
        /// 已经为滚动请求过多少帧：几何不可测时必须停下来，不能每帧都要一帧重绘。
        requests: usize,
        /// 页签条外面套一层零宽容器，模拟列表视口量不出宽度（零宽、未挂载）。
        starved: bool,
        /// 页签、关闭动作、尾部新建各自的焦点句柄：用来确认焦点顺序落在哪个元素上。
        focus: [gpui_kit::FocusHandle; 3],
    }

    impl Render for Probe {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let p = Palette::of(cx);
            let layout = self.layout;
            // 与 Workbench 的页签条完全同一套协议：当前项/数量变了就挂请求，
            // 每帧用上一帧几何确认它是否已经进来，确认后就不再动滚动位置。
            let key = Some((self.active, self.count));
            if self.seen != key {
                self.seen = key;
                self.pending = Some(Reveal::new(self.active));
            }
            let visible = self.pending.is_some_and(|reveal| tab_visible(&self.scroll, reveal.index));
            let reveal = reveal_step(&mut self.pending, visible);
            if reveal.is_some() {
                self.requests += 1;
            }
            let focus = self.focus.clone();
            let items: Vec<AnyElement> = (0..self.count)
                .map(|index| {
                    let active = index == self.active;
                    let spec = TabSpec::new(format!("tab-{index}"), IconName::Code, LONG_TITLE)
                        .active(active)
                        .running(index == 2)
                        .dirty(index == 1, Some(SharedString::from("有未提交的修改")));
                    let close = close_action(
                        format!("tab-close-{index}"),
                        "关闭",
                        active,
                        &p,
                        cx.listener(|this: &mut Self, _, _, cx| {
                            this.closes += 1;
                            cx.notify();
                        }),
                    );
                    // 第一个页签与它的关闭动作挂上测试持有的焦点句柄：
                    // 焦点顺序断言因此能指名到具体元素，而不只是“三个互不相同的句柄”。
                    let close = if index == 0 { close.track_focus(&focus[1]) } else { close };
                    let mut chip = tab(spec, &p)
                        .child(close)
                        .on_click(cx.listener(|this: &mut Self, _, _, cx| {
                            this.clicks += 1;
                            cx.notify();
                        }));
                    if index == 0 {
                        chip = chip.track_focus(&focus[0]);
                    }
                    chip.into_any_element()
                })
                .collect();
            let new_tab = kit::icon_action(
                "tab-new",
                IconName::Plus,
                "新建查询",
                HitArea::CONTROL,
                p.text_muted,
                &p,
                false,
                false,
                cx.listener(|this: &mut Self, _, _, cx| {
                    this.clicks += 100;
                    cx.notify();
                }),
            )
            .track_focus(&self.focus[2])
            .into_any_element();
            let strip = tab_strip(
                "tabs",
                "打开的标签",
                "tab-list",
                items,
                vec![new_tab],
                &self.scroll,
                reveal,
                &layout,
                &p,
            );
            let root = div().id("tab-probe").test_support().size_full().flex().flex_col();
            // starved：外面套一层零宽容器，列表视口量不出宽度，页签几何永远确认不了。
            if self.starved {
                root.child(div().w(px(0.)).overflow_hidden().child(strip))
            } else {
                root.child(strip)
            }
        }
    }

    fn open(
        cx: &mut TestAppContext,
        width: f32,
        layout: Metrics,
        dark: bool,
        count: usize,
    ) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<Probe>) {
        open_with(cx, width, layout, dark, count, false)
    }

    fn open_with(
        cx: &mut TestAppContext,
        width: f32,
        layout: Metrics,
        dark: bool,
        count: usize,
        starved: bool,
    ) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<Probe>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            kit::apply_theme(dark, cx);
        });
        let bounds = Bounds { origin: Point::default(), size: gpui_kit::size(px(width), px(600.)) };
        cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |_, cx| {
                    // 显式跟踪的句柄由自己声明 tab 停靠属性：gpui 只对自动创建的句柄套用
                    // 元素的 tab_index/tab_stop，句柄默认不是停靠点。
                    let focus = [
                        cx.focus_handle().tab_stop(true),
                        cx.focus_handle().tab_stop(true),
                        cx.focus_handle().tab_stop(true),
                    ];
                    cx.new(|_| Probe {
                        layout,
                        count,
                        clicks: 0,
                        closes: 0,
                        active: 0,
                        scroll: gpui_kit::ScrollHandle::new(),
                        seen: None,
                        pending: None,
                        requests: 0,
                        starved,
                        focus,
                    })
                },
            )
            .expect("tabs window")
        })
    }

    fn center_y(bounds: Bounds<gpui_kit::Pixels>) -> f32 {
        f32::from(bounds.origin.y) + f32::from(bounds.size.height) / 2.
    }

    /// 图标槽、标题、关闭动作与尾部新建在每档宽度、双主题、三档密度下共用同一条垂直中心；
    /// 关闭命中 20、新建命中 26，标题行高由字号推出，彼此不重叠也不越出视口。
    #[gpui_kit::test]
    fn tab_contents_share_one_vertical_center_at_every_width_theme_and_density(cx: &mut TestAppContext) {
        for width in [1360., 900., 640.] {
            for dark in [false, true] {
                for layout in [Metrics::compact(), Metrics::standard(), Metrics::relaxed()] {
                    let (window, _) = open(cx, width, layout, dark, 3);
                    let (strip, chip, icon, title, close, new_tab) = cx
                        .update_window(window, |_, window, cx| {
                            window.render_frame(cx);
                            (
                                window.find("tabs").bounds(),
                                window.find("tab-0").bounds(),
                                window.find("tab-0-icon").bounds(),
                                window.find("tab-0-title").bounds(),
                                window.find("tab-close-0").bounds(),
                                window.find("tab-new").bounds(),
                            )
                        })
                        .unwrap();
                    let label = format!("{width} dark={dark} tabs_h={}", layout.tabs_h);
                    assert!((f32::from(strip.size.height) - layout.tabs_h).abs() < 0.5, "{label}: 页签条高度取当前密度");
                    assert!((f32::from(strip.size.width) - width).abs() < 0.5, "{label}: 页签条占满宽度");
                    let center = center_y(chip);
                    for (name, bounds) in [("类型图标", icon), ("标题", title), ("关闭动作", close), ("新建动作", new_tab)] {
                        assert!(
                            (center_y(bounds) - center).abs() < 0.5,
                            "{label}: {name} 与页签同一垂直中心（{} vs {center}）",
                            center_y(bounds)
                        );
                    }
                    assert!((f32::from(close.size.width) - HitArea::INLINE).abs() < 0.5, "{label}: 关闭命中宽度");
                    assert!((f32::from(close.size.height) - HitArea::INLINE).abs() < 0.5, "{label}: 关闭命中高度");
                    assert!((f32::from(new_tab.size.width) - HitArea::CONTROL).abs() < 0.5, "{label}: 新建命中宽度");
                    assert!((f32::from(new_tab.size.height) - HitArea::CONTROL).abs() < 0.5, "{label}: 新建命中高度");
                    assert!(
                        (f32::from(title.size.height) - kit::line_height(TypeScale::SM)).abs() < 0.5,
                        "{label}: 标题行高由字号推出（基线一致）"
                    );
                    assert!(
                        f32::from(icon.origin.x + icon.size.width) <= f32::from(title.origin.x) + 0.5,
                        "{label}: 图标与标题不重叠"
                    );
                    assert!(
                        f32::from(title.origin.x + title.size.width) <= f32::from(close.origin.x) + 0.5,
                        "{label}: 关闭动作不压标题"
                    );
                    assert!(f32::from(chip.size.width) <= TAB_MAX_W + 0.5, "{label}: 长标题按最大宽截断");
                    assert!(f32::from(new_tab.origin.x + new_tab.size.width) <= width + 0.5, "{label}: 新建动作留在视口内");
                }
            }
        }
    }

    /// 页签多到放不下时由列表自己横向滚动：尾部新建留在视口内、不压住页签，
    /// 首个页签仍从列表左缘开始。
    #[gpui_kit::test]
    fn crowded_strip_scrolls_the_list_and_keeps_the_new_action_inside_the_viewport(cx: &mut TestAppContext) {
        let layout = Metrics::compact();
        let (window, _) = open(cx, 640., layout, false, 12);
        let (list, strip, first, new_tab) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (
                    window.find("tab-list").bounds(),
                    window.find("tabs").bounds(),
                    window.find("tab-0").bounds(),
                    window.find("tab-new").bounds(),
                )
            })
            .unwrap();
        assert!(f32::from(list.size.width) < f32::from(strip.size.width), "页签列表让出尾部动作的宽度");
        assert!(
            f32::from(new_tab.origin.x) >= f32::from(list.origin.x + list.size.width) - 0.5,
            "尾部动作在列表右侧，不压在页签上"
        );
        assert!(f32::from(new_tab.origin.x + new_tab.size.width) <= 640.5, "640 下新建动作仍可达");
        assert!((f32::from(first.origin.x) - f32::from(list.origin.x)).abs() < 0.5, "首个页签贴列表左缘");
        assert!(f32::from(first.size.width) <= TAB_MAX_W + 0.5);
    }

    /// 选中态、完整标题、点击分发与键盘可达走同一份组件：
    /// 截断只影响显示宽度，不影响无障碍名称。
    #[gpui_kit::test]
    fn tabs_expose_selection_full_titles_keyboard_reach_and_clicks(cx: &mut TestAppContext) {
        let layout = Metrics::standard();
        let (window, view) = open(cx, 900., layout, true, 2);
        let (selected, label, second_selected) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                let first = window.find("tab-0");
                let second = window.find("tab-1");
                (first.selected(), first.label().map(str::to_owned), second.selected())
            })
            .unwrap();
        assert_eq!(selected, Some(true), "当前页签表达选中");
        assert_eq!(label.as_deref(), Some(LONG_TITLE), "截断后仍能取得完整标题");
        assert_eq!(second_selected, Some(false), "未选中页签不冒充选中");
        // 页签与尾部动作都在键盘停靠序列里：Tab 能离开页签条，不会把焦点困住。
        let reachable = cx
            .update_window(window, |_, window, cx| {
                window.focus_next(cx);
                window.render_frame(cx);
                window.focused(cx).is_some()
            })
            .unwrap();
        assert!(reachable, "页签条里的控件是键盘停靠点");
        cx.update_window(window, |_, window, cx| {
            window.click("tab-1", cx);
            window.click("tab-close-1", cx);
            window.click("tab-new", cx);
        })
        .unwrap();
        let (clicks, closes) = cx.update(|cx| (view.read(cx).clicks, view.read(cx).closes));
        assert!(clicks >= 100, "新建动作照常分发点击（实际 {clicks}）");
        assert_eq!(closes, 1, "关闭动作把点击交回业务层");
    }

    /// 640 宽、页签多到放不下时，Ctrl+Tab 或新建切到的当前项要自己滚进列表视口：
    /// 当前页签完整可见、尾部新建仍在视口内，列表确实横向滚动了。
    #[gpui_kit::test]
    fn the_current_tab_scrolls_into_the_visible_list_at_640(cx: &mut TestAppContext) {
        let layout = Metrics::compact();
        let (window, view) = open(cx, 640., layout, false, 12);
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        let initial = cx.update(|cx| f32::from(view.read(cx).scroll.offset().x));
        assert_eq!(initial, 0., "首个页签本来就在视口里，列表不预先滚动");
        // Ctrl+Tab 到最后一个页签：只有当前项变化才会触发滚动。
        cx.update(|cx| {
            view.update(cx, |probe, cx| {
                probe.active = probe.count - 1;
                cx.notify();
            })
        });
        let (list, active, new_tab) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (window.find("tab-list").bounds(), window.find("tab-11").bounds(), window.find("tab-new").bounds())
            })
            .unwrap();
        let offset = cx.update(|cx| f32::from(view.read(cx).scroll.offset().x));
        assert!(offset < -0.5, "当前项在视口外时列表横向滚动（offset {offset}）");
        assert!(
            f32::from(active.origin.x) >= f32::from(list.origin.x) - 0.5
                && f32::from(active.origin.x + active.size.width) <= f32::from(list.origin.x + list.size.width) + 0.5,
            "当前页签完整落在列表视口内（tab {}..{} / list {}..{}）",
            f32::from(active.origin.x),
            f32::from(active.origin.x + active.size.width),
            f32::from(list.origin.x),
            f32::from(list.origin.x + list.size.width)
        );
        assert!(f32::from(new_tab.origin.x + new_tab.size.width) <= 640.5, "尾部新建仍在视口内");
        // 新建页签（数量也变了）：新的当前项同样进入视口。
        cx.update(|cx| {
            view.update(cx, |probe, cx| {
                probe.count += 1;
                probe.active = probe.count - 1;
                cx.notify();
            })
        });
        let (list, active, new_tab) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (window.find("tab-list").bounds(), window.find("tab-12").bounds(), window.find("tab-new").bounds())
            })
            .unwrap();
        let offset = cx.update(|cx| f32::from(view.read(cx).scroll.offset().x));
        assert!(offset < -0.5, "新建页签后列表继续滚动（offset {offset}）");
        assert!(
            f32::from(active.origin.x) >= f32::from(list.origin.x) - 0.5
                && f32::from(active.origin.x + active.size.width) <= f32::from(list.origin.x + list.size.width) + 0.5,
            "新建后的当前页签完整落在列表视口内"
        );
        assert!(f32::from(new_tab.origin.x + new_tab.size.width) <= 640.5, "新建后尾部动作仍可达");
        // 两帧确认之后请求必须自己结束：否则每一帧都要一帧重绘。
        let (pending, requests) = cx.update(|cx| (view.read(cx).pending, view.read(cx).requests));
        assert!(pending.is_none(), "确认进入视口后清掉滚动请求");
        assert!(requests <= 4, "只在需要时请求滚动（实际 {requests} 帧）");
    }

    /// 请求必须自己停下来：几何永远确认不了（零宽视口、列表还没挂载）时，
    /// 不能每帧都再要一帧重绘。
    #[test]
    fn reveal_requests_stop_even_when_the_geometry_never_confirms() {
        let mut pending = Some(Reveal::new(3));
        let mut frames = 0;
        while reveal_step(&mut pending, false).is_some() {
            frames += 1;
            assert!(frames <= usize::from(REVEAL_MAX_FRAMES) + 1, "确认不了时请求必须有限");
        }
        assert_eq!(frames, usize::from(REVEAL_MAX_FRAMES), "只请求到上限就放弃");
        assert!(pending.is_none(), "放弃之后清掉请求，不再每帧重绘");
        // 已经可见：立刻结束，不再动滚动位置。
        let mut pending = Some(Reveal { index: 1, frames: 0 });
        assert_eq!(reveal_step(&mut pending, true), None);
        assert!(pending.is_none());
        // 一帧之后才确认可见：只请求一次，第二帧结束。
        let mut pending = Some(Reveal::new(2));
        assert_eq!(reveal_step(&mut pending, false), Some(2));
        assert_eq!(reveal_step(&mut pending, true), None);
        assert!(pending.is_none());
        assert_eq!(reveal_step(&mut pending, false), None, "没有请求时不自己发起");
    }

    /// 列表视口量不出宽度时，页签条最多请求几帧就停下：`tab_visible` 一直 false
    /// 也不会变成持续重绘的循环。
    #[gpui_kit::test]
    fn a_zero_width_strip_stops_asking_for_frames(cx: &mut TestAppContext) {
        let layout = Metrics::compact();
        let (window, view) = open_with(cx, 640., layout, false, 3, true);
        let mut tips = Vec::new();
        let list_width = cx
            .update_window(window, |_, window, cx| {
                for _ in 0..6 {
                    window.render_frame(cx);
                    tips.push(view.read(cx).requests);
                }
                f32::from(window.find("tab-list").bounds().size.width)
            })
            .unwrap();
        assert!(list_width <= 0.5, "零宽容器里的列表量不出宽度（实际 {list_width}），落在不可测分支");
        assert_eq!(tips[4], tips[5], "请求已经停下，不会每帧再要一帧");
        assert!(tips[5] <= usize::from(REVEAL_MAX_FRAMES), "请求次数有上限（实际 {}）", tips[5]);
        let pending = cx.update(|cx| view.read(cx).pending);
        assert!(pending.is_none(), "放弃确认后清掉请求");
        let offset = cx.update(|cx| f32::from(view.read(cx).scroll.offset().x));
        assert_eq!(offset, 0., "没有可见几何时不乱滚");
    }

    /// 键盘 Tab 依次走到页签、它的关闭动作和尾部新建：三个停靠点互不相同，
    /// 走完一圈回到第一个；三者几何上都留在页签条内，不被裁切也不互相遮挡。
    #[gpui_kit::test]
    fn keyboard_focus_visits_tab_close_and_new_in_order(cx: &mut TestAppContext) {
        let layout = Metrics::standard();
        let (window, view) = open(cx, 900., layout, false, 1);
        let focus = cx.update(|cx| view.read(cx).focus.clone());
        // 每一步记下焦点句柄，以及三个具名元素各自是否报告持有焦点。
        let steps = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                let mut steps = Vec::new();
                for _ in 0..4 {
                    window.focus_next(cx);
                    window.render_frame(cx);
                    let handle = window.focused(cx).expect("页签条里的控件应当都是 Tab 停靠点");
                    let facts = ["tab-0", "tab-close-0", "tab-new"].map(|id| window.find(id).focused());
                    steps.push((handle, facts));
                }
                steps
            })
            .unwrap();
        // 逐个指名：第一个停靠点就是页签本体，第二个是它的关闭动作，第三个是尾部新建。
        assert_eq!(steps[0].0, focus[0], "第一个停靠点是 tab-0");
        assert_eq!(steps[1].0, focus[1], "第二个停靠点是 tab-close-0");
        assert_eq!(steps[2].0, focus[2], "第三个停靠点是 tab-new");
        assert_eq!(steps[3].0, focus[0], "走完一圈回到 tab-0，焦点不会跑出页签条");
        // 元素自身也报告持有焦点；关闭动作是页签的子元素，所以页签的作用域同时包含它。
        assert_eq!(steps[0].1[0], Some(true), "tab-0 持有焦点");
        assert_eq!(steps[0].1[1], Some(false), "焦点不在关闭动作上");
        assert_eq!(steps[1].1[1], Some(true), "tab-close-0 持有焦点");
        assert_eq!(steps[1].1[2], Some(false), "焦点不在尾部新建上");
        assert_eq!(steps[2].1[2], Some(true), "tab-new 持有焦点");
        assert_eq!(steps[1].1[0], Some(true), "关闭动作在页签作用域内");
        // 焦点环是 2px inset 阴影，headless 读不到像素；可读的焦点事实就是句柄与持有关系。
        assert_eq!(steps[2].1[0], Some(false), "焦点不在页签作用域内");
        let (strip, chip, title, close, new_tab) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (
                    window.find("tabs").bounds(),
                    window.find("tab-0").bounds(),
                    window.find("tab-0-title").bounds(),
                    window.find("tab-close-0").bounds(),
                    window.find("tab-new").bounds(),
                )
            })
            .unwrap();
        let inside = |bounds: Bounds<gpui_kit::Pixels>| {
            f32::from(bounds.origin.x) >= f32::from(strip.origin.x) - 0.5
                && f32::from(bounds.origin.x + bounds.size.width) <= f32::from(strip.origin.x + strip.size.width) + 0.5
        };
        for (name, bounds) in [("页签", chip), ("标题", title), ("关闭动作", close), ("新建动作", new_tab)] {
            assert!(inside(bounds), "{name} 留在页签条内，不被裁切");
        }
        assert!(
            f32::from(title.origin.x + title.size.width) <= f32::from(close.origin.x) + 0.5,
            "关闭动作不压住页签标题"
        );
        assert!(
            f32::from(close.origin.x + close.size.width) <= f32::from(new_tab.origin.x) + 0.5,
            "新建动作不压住关闭动作"
        );
    }
}
