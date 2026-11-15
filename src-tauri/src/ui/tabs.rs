//! 共享页签：工作项、查询结果与固定结果共用同一份几何、状态与滚动规则。
//!
//! 业务层仍拥有自己的语义（工作项切换、结果选择、固定与重命名、关闭确认），
//! 但类型图标、标题、选中/运行/未保存、关闭动作、完整标题和横向滚动只在这里实现一次。
//! 页面不再自绘第二套页签样式。

use gpui_kit::assets::IconName;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div, px,
};

use super::kit::{self, HitArea, IconSize, Metrics, Palette, Tip as _, TypeScale};

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
#[derive(Clone, Debug)]
pub struct TabSpec {
    /// 元素 id，同时是测试与无障碍定位用的名字（例如 `tab-3`）。
    pub id: SharedString,
    pub icon: IconName,
    pub title: SharedString,
    pub active: bool,
    pub running: bool,
    pub dirty: bool,
    /// 状态短标签（已翻译，例如“未保存”）：进入 tooltip，状态不只靠颜色表达。
    pub status: Option<SharedString>,
}

impl TabSpec {
    pub fn new(id: impl Into<SharedString>, icon: IconName, title: impl Into<SharedString>) -> Self {
        Self { id: id.into(), icon, title: title.into(), active: false, running: false, dirty: false, status: None }
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn running(mut self, running: bool) -> Self {
        self.running = running;
        self
    }

    /// 未保存修改：画标记，并把状态写进 tooltip。
    pub fn dirty(mut self, dirty: bool, status: Option<SharedString>) -> Self {
        self.dirty = dirty;
        self.status = status;
        self
    }

    /// 完整标题：截断后仍可获取，状态一并带上。
    fn hint(&self) -> SharedString {
        match (self.dirty, self.status.as_ref()) {
            (true, Some(status)) if !status.is_empty() => SharedString::from(format!("{} · {}", self.title, status)),
            _ => self.title.clone(),
        }
    }
}

/// 页签主体：高度撑满页签条，图标、标题、未保存标记和关闭动作共用同一条垂直中心。
/// 返回的元素由调用方继续挂 on_click / 右键 / 中键，再追加 `close_action`。
pub fn tab(spec: TabSpec, p: &Palette) -> impl IntoElement + gpui_kit::ParentElement + gpui_kit::StatefulInteractiveElement {
    let title = spec.title.clone();
    let hint = spec.hint();
    let tone = if spec.active { p.text } else { p.text_muted };
    let icon_tone = if spec.active { p.accent_text } else { p.text_muted };
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
    let leading = if spec.running {
        slot(Spinner::new().xsmall().color(p.accent).into_any_element())
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
        .border_color(p.line)
        .when(spec.active, |this| this.bg(p.surface).text_color(p.text))
        .when(!spec.active, |this| this.text_color(p.text_muted).hover(|style| style.bg(p.hover).text_color(p.text)))
        .when(spec.active, |this| {
            this.child(div().absolute().top_0().left_0().right_0().h(px(TAB_MARK_H)).bg(p.accent))
        })
        .child(leading)
        .child(
            kit::control_text(title, TypeScale::SM, weight, tone)
                .id(ElementId::Name(SharedString::from(format!("{}-title", spec.id))))
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .test_support(),
        )
        .when(spec.dirty, |this| this.child(kit::status_dot(p.text_muted)))
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
    kit::icon_action(id, IconName::X, label, HitArea::INLINE, if active { p.text_muted } else { p.text_faint }, p, false, on_click)
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

/// 页签条：页签列表自己横向滚动，尾部动作（新建）固定在右侧。
/// 列表 `flex_shrink` + `min_w(0)`，窄窗口里滚动而不是被裁切；尾部动作 `flex_none`，始终留在视口内。
///
/// `scroll` 是调用方持有的滚动句柄（跨帧保存偏移，不能每帧新建），`reveal` 是本帧要把哪个
/// 页签（下标）完整滚进列表视口：当前项由 Ctrl+Tab/新建切过去之后，列表自己滚到它，
/// 不必用户手动横向拖动。调用方用 `tab_visible` 确认它真的进来了才停止给 `Some`，
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
    let mut list = div()
        .id(ElementId::Name(list_id.into()))
        .flex()
        .items_end()
        .h_full()
        .test_support()
        .flex_shrink(1.)
        .min_w(px(0.))
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
    div()
        .id(ElementId::Name(id.into()))
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
        .child(div().flex_1())
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
        /// 上一次已经滚动到的（当前项，页签数量），避免每帧重滚把用户的手动横向滚动拉回。
        revealed: Option<(usize, usize)>,
    }

    impl Render for Probe {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let p = Palette::of(cx);
            let layout = self.layout;
            let reveal = if self.revealed == Some((self.active, self.count)) {
                None
            } else {
                self.revealed = Some((self.active, self.count));
                Some(self.active)
            };
            let items: Vec<AnyElement> = (0..self.count)
                .map(|index| {
                    let active = index == self.active;
                    let spec = TabSpec::new(format!("tab-{index}"), IconName::Code, LONG_TITLE)
                        .active(active)
                        .running(index == 2)
                        .dirty(index == 1, Some(SharedString::from("有未提交的修改")));
                    tab(spec, &p)
                        .child(close_action(
                            format!("tab-close-{index}"),
                            "关闭",
                            active,
                            &p,
                            cx.listener(|this: &mut Self, _, _, cx| {
                                this.closes += 1;
                                cx.notify();
                            }),
                        ))
                        .on_click(cx.listener(|this: &mut Self, _, _, cx| {
                            this.clicks += 1;
                            cx.notify();
                        }))
                        .into_any_element()
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
                cx.listener(|this: &mut Self, _, _, cx| {
                    this.clicks += 100;
                    cx.notify();
                }),
            )
            .into_any_element();
            div()
                .id("tab-probe")
                .test_support()
                .size_full()
                .flex()
                .flex_col()
                .child(tab_strip(
                    "tabs",
                    "打开的标签",
                    "tab-list",
                    items,
                    vec![new_tab],
                    &self.scroll,
                    reveal,
                    &layout,
                    &p,
                ))
        }
    }

    fn open(
        cx: &mut TestAppContext,
        width: f32,
        layout: Metrics,
        dark: bool,
        count: usize,
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
                    cx.new(|_| Probe {
                        layout,
                        count,
                        clicks: 0,
                        closes: 0,
                        active: 0,
                        scroll: gpui_kit::ScrollHandle::new(),
                        revealed: None,
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
    }

    /// 键盘 Tab 依次走到页签、它的关闭动作和尾部新建：三个停靠点互不相同，
    /// 走完一圈回到第一个；三者几何上都留在页签条内，不被裁切也不互相遮挡。
    #[gpui_kit::test]
    fn keyboard_focus_visits_tab_close_and_new_in_order(cx: &mut TestAppContext) {
        let layout = Metrics::standard();
        let (window, _view) = open(cx, 900., layout, false, 1);
        let handles = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                let mut handles = Vec::new();
                for _ in 0..4 {
                    window.focus_next(cx);
                    window.render_frame(cx);
                    handles.push(window.focused(cx).expect("页签条里的控件应当都是 Tab 停靠点"));
                }
                handles
            })
            .unwrap();
        assert_ne!(handles[0], handles[1], "页签与关闭动作是两个停靠点");
        assert_ne!(handles[1], handles[2], "关闭动作与尾部新建是两个停靠点");
        assert_ne!(handles[0], handles[2], "页签与尾部新建是两个停靠点");
        assert_eq!(handles[0], handles[3], "三个停靠点走完回到第一个，焦点不会跑出页签条");
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
