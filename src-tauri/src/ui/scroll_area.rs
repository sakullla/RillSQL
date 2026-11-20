//! 带可见滚动条的滚动区域（ADR-8 滚动条部分）。
//!
//! 原生 `overflow_*_scroll` 只滚不画条：长列表、表单、预览一眼看不出还能往哪儿滚。
//! `scroll_area_y/x` 把原元素原样留作滚动容器（元素 id、焦点、测试查询都不变），
//! 外面套一层只负责占位的包装，在包装上叠一条组件库的滚动条。
//!
//! 与组件库 `overflow_y_scrollbar` 的区别：那个包装会把原元素改名成内部内容 id，
//! 测试按 id 查不到它；这里元素本身就是滚动视口，不改 id、不多套内容层，
//! 子元素的 flex 布局与原生 `overflow_y_scroll` 时完全一样。

use gpui_kit::component::scroll::{Scrollbar, ScrollbarAxis, ScrollbarHandle};
use gpui_kit::{
    div, relative, AnyElement, App, Element, ElementId, InteractiveElement, Interactivity, IntoElement, Length, Overflow, ParentElement,
    PointRefinement, Refineable as _, RenderOnce, ScrollHandle, SizeRefinement, StatefulInteractiveElement, StyleRefinement, Styled, Window,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::TestSupportExt as _;

/// 一个带可见滚动条的滚动区域。由 [`ScrollAreaExt::scroll_area_y`] / [`ScrollAreaExt::scroll_area_x`] 构造。
///
/// 外层布局（尺寸、最小/最大尺寸、flex 伸缩、外边距、定位）在渲染时从原元素挪到包装上；
/// 原元素填满包装并在滚动轴上 `overflow: scroll`，内边距、边框、底色、子元素都留在原元素上。
#[derive(IntoElement)]
pub struct ScrollArea<E: StatefulInteractiveElement + Styled + ParentElement + Element> {
    element: E,
    axis: ScrollbarAxis,
    handle: Option<ScrollHandle>,
}

impl<E> ScrollArea<E>
where
    E: StatefulInteractiveElement + Styled + ParentElement + Element,
{
    fn new(element: E, axis: ScrollbarAxis) -> Self {
        Self { element, axis, handle: None }
    }

    /// 让滚动容器跟踪调用方自己的滚动句柄（虚拟化、滚到某行等需要读写偏移时）。
    /// 不给时按元素 id 在窗口状态里保存一个句柄，跨帧保持滚动位置。
    pub fn scroll_handle(mut self, handle: &ScrollHandle) -> Self {
        self.handle = Some(handle.clone());
        self
    }
}

/// 给任何带 id 的元素加 `scroll_area_y/x`：替代原生 `overflow_y_scroll/overflow_x_scroll`。
pub trait ScrollAreaExt: StatefulInteractiveElement + Styled + ParentElement + Element + Sized {
    /// 纵向滚动，右侧常驻可见滚动条。
    fn scroll_area_y(self) -> ScrollArea<Self> {
        ScrollArea::new(self, ScrollbarAxis::Vertical)
    }

    /// 横向滚动，底部常驻可见滚动条；鼠标滚轮的纵向增量也能推动它。
    /// 目前唯一的横向原生滚动是页签条（注明的例外），这里先供新横向区域使用并由测试覆盖。
    #[cfg_attr(not(test), allow(dead_code))]
    fn scroll_area_x(self) -> ScrollArea<Self> {
        ScrollArea::new(self, ScrollbarAxis::Horizontal)
    }

    /// 两个方向都能滚，右侧与底部各一条滚动条。
    fn scroll_area_xy(self) -> ScrollArea<Self> {
        ScrollArea::new(self, ScrollbarAxis::Both)
    }
}

impl<E: StatefulInteractiveElement + Styled + ParentElement + Element> ScrollAreaExt for E {}

impl<E> Styled for ScrollArea<E>
where
    E: StatefulInteractiveElement + Styled + ParentElement + Element,
{
    fn style(&mut self) -> &mut StyleRefinement {
        self.element.style()
    }
}

impl<E> ParentElement for ScrollArea<E>
where
    E: StatefulInteractiveElement + Styled + ParentElement + Element,
{
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.element.extend(elements)
    }
}

impl<E> InteractiveElement for ScrollArea<E>
where
    E: StatefulInteractiveElement + Styled + ParentElement + Element,
{
    fn interactivity(&mut self) -> &mut Interactivity {
        self.element.interactivity()
    }

    fn track_focus(mut self, focus: &gpui_kit::FocusHandle) -> Self {
        self.element = self.element.track_focus(focus);
        self
    }
}

impl<E> StatefulInteractiveElement for ScrollArea<E> where E: StatefulInteractiveElement + Styled + ParentElement + Element {}

/// 从原元素拆出外层布局：包装按它在父容器里占位，原元素改为填满包装。
///
/// 滚动轴上的 `overflow: hidden` 放在包装上：flex 子项只有 overflow 不是 visible 时才会
/// 把自动最小尺寸降到 0，否则内容会把包装撑开、把兄弟元素挤出去，而不是滚动。
pub(crate) fn split_outer_style(style: &mut StyleRefinement, axis: ScrollbarAxis) -> StyleRefinement {
    let outer = StyleRefinement {
        position: style.position.take(),
        inset: std::mem::take(&mut style.inset),
        size: std::mem::take(&mut style.size),
        min_size: std::mem::take(&mut style.min_size),
        max_size: std::mem::take(&mut style.max_size),
        margin: std::mem::take(&mut style.margin),
        flex_grow: style.flex_grow.take(),
        flex_shrink: style.flex_shrink.take(),
        flex_basis: style.flex_basis.take(),
        align_self: style.align_self.take(),
        overflow: PointRefinement {
            x: axis.has_horizontal().then_some(Overflow::Hidden),
            y: axis.has_vertical().then_some(Overflow::Hidden),
        },
        ..Default::default()
    };
    style.size = SizeRefinement { width: Some(Length::Definite(relative(1.))), height: Some(Length::Definite(relative(1.))) };
    // 视口必须能缩到包装大小：滚动容器的自动最小尺寸本就是 0，这里只保证可收缩。
    style.flex_shrink = Some(1.);
    style.min_size = SizeRefinement { width: Some(Length::Definite(px0())), height: Some(Length::Definite(px0())) };
    if axis.has_vertical() {
        style.overflow.y = Some(Overflow::Scroll);
    }
    if axis.has_horizontal() {
        style.overflow.x = Some(Overflow::Scroll);
    }
    outer
}

fn px0() -> gpui_kit::DefiniteLength {
    gpui_kit::px(0.).into()
}

impl<E> RenderOnce for ScrollArea<E>
where
    E: StatefulInteractiveElement + Styled + ParentElement + Element + 'static,
{
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = Element::id(&self.element).unwrap_or_else(|| ElementId::Name("scroll-area".into()));
        let handle = match self.handle.take() {
            Some(handle) => handle,
            None => window.use_keyed_state((id.clone(), "scroll-area-handle"), cx, |_, _| ScrollHandle::default()).read(cx).clone(),
        };
        let outer = split_outer_style(self.element.style(), self.axis);
        let axis = self.axis;
        // 不锁滚动轴：与原生 overflow_*_scroll 一样，横向区域仍可用普通鼠标滚轮推动。
        let viewport = self.element.track_scroll(&handle);
        let mut wrapper = div().relative().flex().map(|this| if axis == ScrollbarAxis::Horizontal { this.flex_row() } else { this.flex_col() });
        wrapper.style().refine(&outer);
        wrapper
            .child(viewport)
            .when(!window.is_inspector_picking(cx), |this| {
                this.child(
                    div()
                        .id((id.clone(), "scroll-area-bar"))
                        .test_support()
                        .absolute()
                        .inset_0()
                        .child(Scrollbar::new(&handle).id((id, "scrollbar")).axis(axis).viewport_from_layout()),
                )
            })
    }
}

/// 给已有滚动句柄的虚拟列表（`uniform_list` 等）叠一条可见滚动条：
/// 放进与列表同大小、`relative` 的父容器里，绝对定位铺满，只在条所在的边缘接收指针。
pub fn scrollbar_overlay<H: ScrollbarHandle + Clone>(id: impl Into<ElementId>, handle: &H, axis: ScrollbarAxis) -> gpui_kit::Div {
    div().absolute().inset_0().child(Scrollbar::new(handle).id(id).axis(axis).viewport_from_layout())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::px;

    #[test]
    fn outer_layout_moves_to_wrapper_and_viewport_fills_it() {
        let mut style = StyleRefinement::default().flex_1().max_h(px(360.)).mx(px(6.)).p(px(4.));
        let outer = split_outer_style(&mut style, ScrollbarAxis::Vertical);
        assert_eq!(outer.flex_grow, Some(1.));
        assert!(outer.max_size.height.is_some(), "最大高度留在包装上，内容超出时滚动而不是被裁掉");
        assert!(outer.margin.left.is_some(), "外边距属于包装");
        assert_eq!(outer.overflow.y, Some(Overflow::Hidden));
        assert_eq!(outer.overflow.x, None);
        assert_eq!(style.max_size.height, None, "视口不再限高，跟随包装");
        assert_eq!(style.size.height, Some(Length::Definite(relative(1.))));
        assert_eq!(style.overflow.y, Some(Overflow::Scroll));
        assert!(style.padding.left.is_some(), "内边距留在滚动视口上");
    }

    #[test]
    fn horizontal_area_scrolls_on_x_only() {
        let mut style = StyleRefinement::default().w_full();
        let outer = split_outer_style(&mut style, ScrollbarAxis::Horizontal);
        assert_eq!(outer.overflow.x, Some(Overflow::Hidden));
        assert_eq!(style.overflow.x, Some(Overflow::Scroll));
        assert_eq!(style.overflow.y, None);
    }

    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{point, AppContext as _, Context, Render, ScrollDelta, TestAppContext};

    struct Probe {
        handle: ScrollHandle,
    }

    impl Render for Probe {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let rows = (0..20usize).map(|ix| div().id(("probe-row", ix)).test_support().h(px(30.)).flex_none().child(format!("row {ix}")));
            div()
                .size_full()
                .flex()
                .flex_col()
                // 高度跟着内容走的列里限高：内容 600px，区域应停在 120px 并可滚动。
                .child(div().id("probe-y").test_support().flex().flex_col().max_h(px(120.)).p(px(4.)).scroll_area_y().children(rows))
                .child(
                    div()
                        .id("probe-x")
                        .test_support()
                        .w(px(200.))
                        .h(px(40.))
                        .flex()
                        .scroll_area_x()
                        .scroll_handle(&self.handle)
                        .child(div().id("probe-wide").test_support().w(px(1000.)).h_full().flex_none()),
                )
        }
    }

    #[gpui_kit::test]
    fn scroll_area_keeps_element_identity_scrolls_and_overlays_a_bar(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, |_, cx| cx.new(|_| Probe { handle: ScrollHandle::new() })).unwrap()
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            // 原元素保留自己的 id：测试与焦点照旧按 id 找到它，而且它就是滚动视口。
            let area = window.find("probe-y").bounds();
            assert_eq!(area.size.height, px(120.), "限高生效，内容超出的部分靠滚动而不是撑开");
            let bar = window.find(ElementId::from((ElementId::from("probe-y"), "scroll-area-bar"))).bounds();
            assert_eq!(bar, area, "滚动条层铺在视口上，条贴着视口右缘");
            let before = window.find(("probe-row", 0usize)).bounds().origin.y;
            window.scroll("probe-y", ScrollDelta::Pixels(point(px(0.), px(-90.))), cx);
            window.render_frame(cx);
            let after = window.find(("probe-row", 0usize)).bounds().origin.y;
            assert!(after < before, "滚轮能滚动区域：{before:?} -> {after:?}");

            let x = window.find("probe-x").bounds();
            assert_eq!(x.size.width, px(200.));
            let bar = window.find(ElementId::from((ElementId::from("probe-x"), "scroll-area-bar"))).bounds();
            assert_eq!(bar, x);
        })
        .unwrap();
        let max = cx.update(|cx| view.read(cx).handle.max_offset());
        assert!(max.x > px(700.), "调用方的句柄跟踪到横向内容：{max:?}");
    }

    /// 仓库里不再有只滚不画条的原生滚动容器（ADR-8）：`overflow_*_scroll` 只允许出现在
    /// 本模块（包装自身）里，或同一行/上一行带 `滚动条例外：` 注释说明原因的地方。
    #[test]
    fn no_native_scroll_container_without_visible_scrollbar() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    out.push(path);
                }
            }
        }
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&root, &mut files);
        let needles = [concat!(".overflow_", "y_scroll()"), concat!(".overflow_", "x_scroll()"), concat!(".overflow_", "scroll()")];
        let mut offenders = Vec::new();
        for file in files {
            if file.ends_with("ui/scroll_area.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            for (ix, line) in lines.iter().enumerate() {
                if !needles.iter().any(|needle| line.contains(needle)) {
                    continue;
                }
                let excused = line.contains("滚动条例外：") || ix.checked_sub(1).is_some_and(|prev| lines[prev].contains("滚动条例外："));
                if !excused {
                    offenders.push(format!("{}:{}", file.strip_prefix(&root).unwrap().display(), ix + 1));
                }
            }
        }
        assert!(offenders.is_empty(), "改用 kit::ScrollAreaExt::scroll_area_y/x，或注明 `滚动条例外：原因`：{offenders:?}");
    }
}
