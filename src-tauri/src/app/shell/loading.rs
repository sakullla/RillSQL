//! 加载态的两种排版（整块、行内），转圈由调用方从 `SpinnerPool` 取来传入。
//! 与 `kit::loading_state` / `kit::inline_loading` 排版相同，区别只在转圈：这里用隔离的
//! `SpinnerView`，动画帧只通知那个小实体，不通知画它的工作台或查询面板。

use gpui_kit::{div, AnyElement, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _, TestSupportExt as _, px};

use crate::ui::kit::{Metrics, Palette, Space, TypeScale};

/// 整块加载态：居中的转圈加说明文字。
pub(crate) fn loading_state(spinner: AnyElement, text: impl Into<SharedString>, p: &Palette) -> impl gpui_kit::ParentElement + gpui_kit::Styled + IntoElement {
    div()
        .id("loading-state")
        .test_support()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(Space::MD))
        .p(px(Space::XL))
        .text_size(px(TypeScale::SM))
        .text_color(p.text_muted)
        .child(spinner)
        .child(text.into())
}

/// 局部加载（行、节点、字段旁）：一行小号转圈，贴左、单行高。
pub(crate) fn inline_loading(spinner: AnyElement, text: impl Into<SharedString>, layout: &Metrics, p: &Palette) -> impl gpui_kit::ParentElement + gpui_kit::Styled + IntoElement {
    div()
        .id("inline-loading")
        .test_support()
        .flex()
        .items_center()
        .gap(px(Space::SM))
        .min_h(px(layout.strip_h()))
        .text_size(px(TypeScale::XS))
        .text_color(p.text_muted)
        .child(spinner)
        .child(text.into())
}
