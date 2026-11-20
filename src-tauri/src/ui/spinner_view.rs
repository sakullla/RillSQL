//! Spinner 隔离视图。
//!
//! 组件库的 `Spinner` 用 `with_animation` 转圈，动画每一帧都会通知“正在渲染它的那个视图”。
//! 直接画在工作台或列表里时，每一帧都通知那个大视图（触发它的观察者，并让它缓存的子树失效）。
//! 这里把它包成一个独立的小实体：动画帧只通知这个小视图，用 `.cached()` 挂的兄弟视图不重绘。
//! 注意：gpui 会把被通知视图的祖先一并标脏，祖先视图的 render 仍会执行；空闲时要停用或卸掉 Spinner，
//! 祖先的 render 也应只构建元素。停用（`set_active(false)`）后不再请求动画帧。
#![allow(dead_code)] // 共享底座：本任务只建组件，各页面在后续任务里接入。

use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Sizable as _, Size};
use gpui_kit::{div, App, AppContext as _, Context, Entity, Hsla, IntoElement, ParentElement as _, Render, Styled as _, Window};

use super::kit::Palette;

pub struct SpinnerView {
    size: Size,
    color: Option<Hsla>,
    active: bool,
}

impl SpinnerView {
    pub fn new(size: Size) -> Self {
        Self { size, color: None, active: true }
    }

    pub fn active(&self) -> bool {
        self.active
    }

    /// 停用后渲染为空，不再请求动画帧；值没变时不重绘。
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            cx.notify();
        }
    }

    /// 颜色；None 跟随色板的强调色。
    pub fn set_color(&mut self, color: Option<Hsla>, cx: &mut Context<Self>) {
        if self.color != color {
            self.color = color;
            cx.notify();
        }
    }
}

impl Render for SpinnerView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.active {
            return div();
        }
        let color = self.color.unwrap_or_else(|| Palette::of(cx).accent);
        div().flex_none().child(Spinner::new().with_size(self.size).color(color))
    }
}

/// 新建一个隔离的 Spinner 实体（默认激活）。父视图持有它并在 render 里 `.child(entity.clone())`。
pub fn spinner_view(size: Size, color: Option<Hsla>, cx: &mut App) -> Entity<SpinnerView> {
    cx.new(|_| SpinnerView { size, color, active: true })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{px, size, AnyView, StyleRefinement, TestAppContext};
    use std::cell::Cell;
    use std::rc::Rc;

    /// 代表工作台里的重视图（结果网格、资源树）：被缓存时只有自己被通知才重绘。
    struct Heavy {
        renders: Rc<Cell<usize>>,
    }

    impl Render for Heavy {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.renders.set(self.renders.get() + 1);
            div().size_full()
        }
    }

    struct Host {
        spinner: Entity<SpinnerView>,
        heavy: Entity<Heavy>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(self.spinner.clone())
                .child(AnyView::from(self.heavy.clone()).cached(StyleRefinement::default().size_full()))
        }
    }

    #[gpui_kit::test]
    fn animation_frames_notify_only_the_spinner_view(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let heavy_renders = Rc::new(Cell::new(0));
        let host_notified = Rc::new(Cell::new(0));
        let spinner = cx.update(|cx| spinner_view(Size::XSmall, None, cx));
        let heavy = cx.update(|cx| {
            let renders = heavy_renders.clone();
            cx.new(|_| Heavy { renders })
        });
        let (host_spinner, host_heavy) = (spinner.clone(), heavy.clone());
        let window = cx.open_window(size(px(200.), px(100.)), move |_, _| Host { spinner: host_spinner, heavy: host_heavy });
        cx.run_until_parked();
        let host = window.update(cx, |_, _, cx| cx.entity()).unwrap();
        let notified = host_notified.clone();
        cx.update(|cx| cx.observe(&host, move |_, _| notified.set(notified.get() + 1)).detach());
        let first = heavy_renders.get();
        assert_eq!(first, 1);
        let mut frames = 0;
        for _ in 0..5 {
            frames += window.update(cx, |_, window, cx| window.simulate_next_frame(cx)).unwrap();
            cx.run_until_parked();
        }
        assert!(frames >= 5, "激活的 Spinner 每帧请求下一帧：{frames}");
        assert_eq!(host_notified.get(), 0, "动画帧只通知 Spinner 自己，宿主视图没有收到通知");
        assert_eq!(heavy_renders.get(), first, "缓存的兄弟视图在动画帧里不重绘");
        // 停用后不再请求动画帧。
        cx.update(|cx| spinner.update(cx, |spinner, cx| spinner.set_active(false, cx)));
        cx.run_until_parked();
        let _ = window.update(cx, |_, window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        assert_eq!(window.update(cx, |_, window, cx| window.simulate_next_frame(cx)).unwrap(), 0, "停用后不再请求动画帧");
        assert!(!cx.update(|cx| spinner.read(cx).active()));
    }
}
