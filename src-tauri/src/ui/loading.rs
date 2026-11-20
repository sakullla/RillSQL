//! 加载态的两种排版（整块、行内），转圈由调用方从 `SpinnerPool` 取来传入。
//! 与 `kit::loading_state` / `kit::inline_loading` 排版相同，区别只在转圈：这里用隔离的
//! `SpinnerView`，动画帧只通知那个小实体，不通知画它的工作台或查询面板。

use std::cell::RefCell;
use std::collections::HashMap;

use gpui_kit::component::Size;
use gpui_kit::{div, px, AnyElement, AnyView, App, Entity, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _, TestSupportExt as _};

use crate::ui::kit::{Metrics, Palette, Space, TypeScale};
use crate::ui::spinner_view::{spinner_view, SpinnerView};

/// 池里最多留多少个实体；超过时整池清空重建（只是几个小实体，重建便宜）。
const POOL_MAX: usize = 64;

/// 按稳定键复用的 `ui::spinner_view` 实体。列表行、页签、状态栏这类没有自己实体的位置用它：
/// 同一个键每帧拿到同一个实体，动画帧只通知这个小实体，不通知画它的大视图。
/// 只有在 render 里实际取用（正在加载）时才会被画出来；不画的实体不请求动画帧。
#[derive(Default)]
pub(crate) struct SpinnerPool {
    views: RefCell<HashMap<SharedString, Entity<SpinnerView>>>,
}

impl SpinnerPool {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 取（或新建）这个键的 Spinner，颜色变了才通知它。
    pub(crate) fn view(&self, key: impl Into<SharedString>, size: Size, color: Option<Hsla>, cx: &mut App) -> AnyView {
        let key = key.into();
        let existing = self.views.borrow().get(&key).cloned();
        let entity = match existing {
            Some(entity) => {
                entity.update(cx, |spinner, cx| {
                    spinner.set_color(color, cx);
                    spinner.set_active(true, cx);
                });
                entity
            }
            None => {
                let entity = spinner_view(size, color, cx);
                let mut views = self.views.borrow_mut();
                if views.len() >= POOL_MAX {
                    views.clear();
                }
                views.insert(key, entity.clone());
                entity
            }
        };
        AnyView::from(entity)
    }

    pub(crate) fn element(&self, key: impl Into<SharedString>, size: Size, color: Option<Hsla>, cx: &mut App) -> AnyElement {
        self.view(key, size, color, cx).into_any_element()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.views.borrow().len()
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;

    /// 同一个键每帧拿到同一个实体（动画状态不丢、不新建实体）；池有上限。
    #[gpui_kit::test]
    fn the_pool_reuses_one_entity_per_key(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let pool = SpinnerPool::new();
        assert_eq!(pool.len(), 0);
        let (a, b, c) = cx.update(|cx| {
            let a = pool.view("tab-1", Size::XSmall, None, cx);
            let b = pool.view("tab-1", Size::XSmall, None, cx);
            let c = pool.view("nav-x", Size::XSmall, None, cx);
            (a, b, c)
        });
        assert_eq!(a.entity_id(), b.entity_id(), "同一个键复用同一个实体");
        assert_ne!(a.entity_id(), c.entity_id());
        assert_eq!(pool.len(), 2);
        for ix in 0..POOL_MAX {
            cx.update(|cx| {
                let _ = pool.view(format!("row-{ix}"), Size::XSmall, None, cx);
            });
        }
        assert!(pool.len() <= POOL_MAX, "池有上限：{}", pool.len());
    }
}
