//! 执行计划画布：把 `Plan.roots` 排成自上而下的整齐树，节点是卡片，父子之间是贝塞尔连线。
//!
//! 布局是纯函数（`layout`）：子树宽度自底向上累加，子节点按顺序水平排开，父节点在子节点块的正上方居中，
//! 同一棵子树占用的水平区间互不相交，所以节点框不会重叠；多个根节点并排放置。
//!
//! 视图变换以视口中心为锚点：屏幕位置 = 视口中心 + 平移 + (内容坐标 − 内容中心) × 缩放。
//! 平移为 0 时内容居中，单节点计划因此天然在画布中央。视口尺寸在 `canvas` 预绘制阶段记下，
//! “适配”与选中节点滚入视口都按最近一次的视口计算。
//!
//! 选中与列表视图共享 `QueryPane::plan_selected`；画布容器持有同一个计划焦点，方向键导航照常可用。

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    canvas, div, px, relative, AnyElement, Bounds, Entity, FocusHandle, FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _,
    PathBuilder, Pixels, ScrollDelta, ScrollEvent, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
};

use crate::i18n::{self, Locale};
use crate::query::plan::{NodeKind, OpKind, Plan, PlanNode};
use crate::ui::kit::{self, Palette, Radius, Space, TypeScale};

use super::plan_view;
use super::QueryPane;

/// 节点槽位的未缩放尺寸与间距：表、操作与子查询卡片占满槽位，JOIN 与虚拟根是槽位中央的小胶囊。
pub const NODE_W: f32 = 236.;
pub const NODE_H: f32 = 104.;
pub const PILL_W: f32 = 176.;
pub const PILL_H: f32 = 52.;
pub const GAP_X: f32 = 28.;
pub const GAP_Y: f32 = 52.;
/// 虚拟根胶囊与下面一层根节点之间的竖向间距。
pub const ROOT_GAP: f32 = 32.;
/// 内容四周的留白。
pub const MARGIN: f32 = 24.;
pub const ZOOM_MIN: f32 = 0.4;
pub const ZOOM_MAX: f32 = 2.0;
/// 多个根节点时加在顶部的虚拟“查询”根的下标（不在 `Plan::lines()` 里）。
pub const VIRTUAL_ROOT: usize = usize::MAX;
/// 按钮缩放的步进倍数。
const ZOOM_STEP: f32 = 1.2;
/// ⌘/Ctrl + 滚轮：每像素滚动量对应的缩放指数。
const WHEEL_ZOOM_RATE: f32 = 0.002;
/// 按行滚动（鼠标滚轮）时一行折算的像素。
const WHEEL_LINE_PX: f32 = 40.;
/// 拖拽超过这个距离才算平移（否则视为点击）。
const DRAG_SLOP: f32 = 3.;
/// 适配视口时四周保留的距离。
const FIT_PAD: f32 = 24.;
/// 缩放低于该值时卡片只保留标题两行，避免文字挤成一团。
const COMPACT_ZOOM: f32 = 0.6;
const HEAT_BAR_H: f32 = 3.;
const CARD_PAD: f32 = 8.;
const EDGE_W: f32 = 1.4;
const EDGE_HOT_W: f32 = 2.2;
/// 选中卡片外发光的模糊半径与扩散（未缩放）。
const GLOW_BLUR: f32 = 10.;
const GLOW_SPREAD: f32 = 1.;

/// 一张卡片在内容坐标系中的位置（左上角）与尺寸。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeBox {
    /// 前序下标，与 `Plan::lines()` 一致；虚拟根为 [`VIRTUAL_ROOT`]。
    pub index: usize,
    /// 计划树里的深度（与列表视图一致）；虚拟根为 0，真实节点在有虚拟根时画在下一层。
    pub depth: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl NodeBox {
    pub fn center_x(&self) -> f32 {
        self.x + self.w / 2.
    }

    pub fn center_y(&self) -> f32 {
        self.y + self.h / 2.
    }
}

/// 父子连线（前序下标；父节点可以是 [`VIRTUAL_ROOT`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    pub parent: usize,
    pub child: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// 真实节点，按前序下标排列。
    pub nodes: Vec<NodeBox>,
    /// 多个根节点时的虚拟“查询”根：保证各根之间也有连线。
    pub virtual_root: Option<NodeBox>,
    pub edges: Vec<Edge>,
    pub width: f32,
    pub height: f32,
}

impl Layout {
    /// 按下标取卡片（含虚拟根）。
    pub fn node(&self, index: usize) -> Option<&NodeBox> {
        if index == VIRTUAL_ROOT {
            self.virtual_root.as_ref()
        } else {
            self.nodes.get(index)
        }
    }
}

/// 卡片尺寸：JOIN 是紧凑胶囊，其余占满槽位。
pub fn card_size(node: &PlanNode) -> (f32, f32) {
    match node.node_kind() {
        NodeKind::Join => (PILL_W, PILL_H),
        _ => (NODE_W, NODE_H),
    }
}

/// 确定性整齐树布局：同一份计划总得到同一组坐标。
pub fn layout(plan: &Plan) -> Layout {
    // 先按前序算每个节点的子树宽度（自底向上），再自顶向下放置。
    fn widths(node: &PlanNode, out: &mut Vec<f32>) -> f32 {
        let at = out.len();
        out.push(0.);
        let mut kids = 0.;
        for (n, child) in node.children.iter().enumerate() {
            if n > 0 {
                kids += GAP_X;
            }
            kids += widths(child, out);
        }
        let width = if node.children.is_empty() { NODE_W } else { f32::max(NODE_W, kids) };
        out[at] = width;
        width
    }
    fn subtree_len(node: &PlanNode) -> usize {
        1 + node.children.iter().map(subtree_len).sum::<usize>()
    }
    /// 卡片在槽位里水平、垂直居中。
    fn card(index: usize, depth: usize, top: f32, slot_x: f32, (w, h): (f32, f32)) -> NodeBox {
        NodeBox { index, depth, x: slot_x + (NODE_W - w) / 2., y: top + (NODE_H - h) / 2., w, h }
    }
    struct Ctx<'a> {
        widths: &'a [f32],
        /// 虚拟根占用的顶部高度（没有虚拟根时为 0）。
        band: f32,
        out: Layout,
        max_depth: usize,
    }
    /// 放置一棵子树，返回根卡片的槽位左边。
    fn place(node: &PlanNode, index: usize, left: f32, depth: usize, parent: Option<usize>, ctx: &mut Ctx<'_>) -> f32 {
        let width = ctx.widths[index];
        let top = MARGIN + ctx.band + depth as f32 * (NODE_H + GAP_Y);
        ctx.max_depth = ctx.max_depth.max(depth);
        let slot = ctx.out.nodes.len();
        ctx.out.nodes.push(card(index, depth, top, left + (width - NODE_W) / 2., card_size(node)));
        if let Some(parent) = parent {
            ctx.out.edges.push(Edge { parent, child: index });
        }
        let mut kids = 0.;
        for (n, child_width) in child_starts(node, index).iter().map(|start| ctx.widths[*start]).enumerate() {
            kids += child_width + if n > 0 { GAP_X } else { 0. };
        }
        let mut cursor = left + (width - kids) / 2.;
        let mut span: Option<(f32, f32)> = None;
        for (child, start) in node.children.iter().zip(child_starts(node, index)) {
            let child_slot = place(child, start, cursor, depth + 1, Some(index), ctx);
            span = Some(span.map_or((child_slot, child_slot), |(first, _)| (first, child_slot)));
            cursor += ctx.widths[start] + GAP_X;
        }
        // 父节点放在首尾子节点的正中上方；子节点都在本子树区间内，父节点因此也不越界。
        let mut slot_x = left + (width - NODE_W) / 2.;
        if let Some((first, last)) = span {
            slot_x = (first + last) / 2.;
            ctx.out.nodes[slot] = card(index, depth, top, slot_x, card_size(node));
        }
        slot_x
    }
    /// 子节点在前序中的下标：第一个子节点紧跟父节点，之后每个跳过前一个子树的大小。
    fn child_starts(node: &PlanNode, index: usize) -> Vec<usize> {
        let mut at = index + 1;
        node.children
            .iter()
            .map(|child| {
                let start = at;
                at += subtree_len(child);
                start
            })
            .collect()
    }

    let mut out = Layout::default();
    if plan.roots.is_empty() {
        return out;
    }
    let mut all_widths = Vec::new();
    for root in &plan.roots {
        widths(root, &mut all_widths);
    }
    let multi = plan.roots.len() > 1;
    // 虚拟根是一条矮胶囊：只占胶囊高度加较短的间距，不占一整层卡片高度。
    let band = if multi { PILL_H + ROOT_GAP } else { 0. };
    let mut ctx = Ctx { widths: &all_widths, band, out: Layout::default(), max_depth: 0 };
    let mut cursor = MARGIN;
    let mut index = 0;
    let mut root_slots = Vec::new();
    for (n, root) in plan.roots.iter().enumerate() {
        if n > 0 {
            cursor += GAP_X;
        }
        root_slots.push(place(root, index, cursor, 0, multi.then_some(VIRTUAL_ROOT), &mut ctx));
        cursor += all_widths[index];
        index += subtree_len(root);
    }
    out = Layout { width: cursor + MARGIN, ..ctx.out };
    let max_depth = ctx.max_depth;
    if multi {
        // 虚拟根在各根节点块的正上方居中。
        let first = root_slots.first().copied().unwrap_or(MARGIN);
        let last = root_slots.last().copied().unwrap_or(MARGIN);
        let slot_x = (first + last) / 2.;
        out.virtual_root = Some(NodeBox { index: VIRTUAL_ROOT, depth: 0, x: slot_x + (NODE_W - PILL_W) / 2., y: MARGIN, w: PILL_W, h: PILL_H });
    }
    // 节点按前序下标排好，渲染与测试都可以直接用下标取。
    out.nodes.sort_by_key(|node| node.index);
    out.height = MARGIN * 2. + band + (max_depth + 1) as f32 * NODE_H + max_depth as f32 * GAP_Y;
    out
}

/// 拖拽平移的起点。
#[derive(Clone, Copy, Debug)]
struct Drag {
    x: f32,
    y: f32,
    pan_x: f32,
    pan_y: f32,
    moved: bool,
}

/// 画布视图状态（缩放、平移、拖拽），由 `QueryPane` 跨帧持有。
#[derive(Clone, Debug)]
pub struct CanvasState {
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
    drag: Option<Drag>,
    /// 刚结束的一次拖拽移动过：随后的点击不当作选中节点。
    suppress_click: bool,
    /// 新计划进来后等视口尺寸可用时自动适配一次。
    pub fit_pending: bool,
    /// 最近一次绘制时的视口（窗口坐标）。
    viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// 当前内容尺寸（未缩放）。
    content: (f32, f32),
}

impl Default for CanvasState {
    fn default() -> Self {
        Self { zoom: 1., pan_x: 0., pan_y: 0., drag: None, suppress_click: false, fit_pending: true, viewport: Rc::new(Cell::new(None)), content: (0., 0.) }
    }
}

pub fn clamp_zoom(zoom: f32) -> f32 {
    zoom.clamp(ZOOM_MIN, ZOOM_MAX)
}

impl CanvasState {
    /// 换了一份计划：回到默认视图，等视口可用时自动适配。
    pub fn reset(&mut self) {
        self.zoom = 1.;
        self.pan_x = 0.;
        self.pan_y = 0.;
        self.drag = None;
        self.suppress_click = false;
        self.fit_pending = true;
    }

    fn viewport_size(&self) -> Option<(f32, f32)> {
        self.viewport.get().map(|bounds| (bounds.size.width.as_f32(), bounds.size.height.as_f32())).filter(|(w, h)| *w > 1. && *h > 1.)
    }

    /// 每帧渲染前同步内容尺寸；视口已知且有待适配时执行适配。
    pub fn sync(&mut self, content_w: f32, content_h: f32) {
        self.content = (content_w, content_h);
        if self.fit_pending {
            if let Some((w, h)) = self.viewport_size() {
                self.fit_to(w, h);
                self.fit_pending = false;
            }
        }
    }

    /// 适配视口：整体缩小到能放下（不放大超过 100%），放不下的高度从顶部开始看。
    pub fn fit_to(&mut self, viewport_w: f32, viewport_h: f32) {
        let (w, h) = self.content;
        if w <= 1. || h <= 1. {
            self.zoom = 1.;
            self.pan_x = 0.;
            self.pan_y = 0.;
            return;
        }
        let zx = (viewport_w - FIT_PAD * 2.).max(1.) / w;
        let zy = (viewport_h - FIT_PAD * 2.).max(1.) / h;
        self.zoom = zx.min(zy).clamp(ZOOM_MIN, 1.);
        self.pan_x = 0.;
        let scaled_h = h * self.zoom;
        self.pan_y = if scaled_h > viewport_h { (scaled_h - viewport_h) / 2. } else { 0. };
    }

    pub fn fit(&mut self) {
        match self.viewport_size() {
            Some((w, h)) => {
                self.fit_to(w, h);
                self.fit_pending = false;
            }
            None => self.fit_pending = true,
        }
    }

    /// 以视口内某点（相对视口中心）为不动点缩放。
    pub fn zoom_at(&mut self, zoom: f32, at_x: f32, at_y: f32) {
        let next = clamp_zoom(zoom);
        let ratio = next / self.zoom;
        self.pan_x = at_x - (at_x - self.pan_x) * ratio;
        self.pan_y = at_y - (at_y - self.pan_y) * ratio;
        self.zoom = next;
    }

    pub fn zoom_in(&mut self) {
        self.zoom_at(self.zoom * ZOOM_STEP, 0., 0.);
    }

    pub fn zoom_out(&mut self) {
        self.zoom_at(self.zoom / ZOOM_STEP, 0., 0.);
    }

    pub fn zoom_reset(&mut self) {
        self.zoom_at(1., 0., 0.);
    }

    /// 滚轮：普通滚动平移，⌘/Ctrl + 滚动以指针为中心缩放。`at` 为指针相对视口中心的位置。
    pub fn wheel(&mut self, dx: f32, dy: f32, zoom_modifier: bool, at: (f32, f32)) {
        if zoom_modifier {
            if dy.abs() > f32::EPSILON {
                self.zoom_at(self.zoom * (dy * WHEEL_ZOOM_RATE).exp(), at.0, at.1);
            }
        } else {
            self.pan_x += dx;
            self.pan_y += dy;
        }
    }

    pub fn begin_drag(&mut self, x: f32, y: f32) {
        self.suppress_click = false;
        self.drag = Some(Drag { x, y, pan_x: self.pan_x, pan_y: self.pan_y, moved: false });
    }

    /// 拖拽中：返回是否改变了平移。
    pub fn move_drag(&mut self, x: f32, y: f32) -> bool {
        let Some(drag) = self.drag.as_mut() else { return false };
        let dx = x - drag.x;
        let dy = y - drag.y;
        if !drag.moved && dx.abs() < DRAG_SLOP && dy.abs() < DRAG_SLOP {
            return false;
        }
        drag.moved = true;
        self.pan_x = drag.pan_x + dx;
        self.pan_y = drag.pan_y + dy;
        true
    }

    pub fn end_drag(&mut self) {
        if let Some(drag) = self.drag.take() {
            self.suppress_click = drag.moved;
        }
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some_and(|drag| drag.moved)
    }

    /// 点击节点前调用：刚拖拽过的这一下不算点击。
    pub fn take_click(&mut self) -> bool {
        let moved = self.dragging() || self.suppress_click;
        self.suppress_click = false;
        !moved
    }

    /// 节点相对视口中心的屏幕偏移（左上角）。
    pub fn node_offset(&self, node: &NodeBox) -> (f32, f32) {
        let (w, h) = self.content;
        (self.pan_x + (node.x - w / 2.) * self.zoom, self.pan_y + (node.y - h / 2.) * self.zoom)
    }

    /// 选中节点不完整在视口内时，把它移到视口中央。视口未知时不动。
    pub fn reveal(&mut self, node: &NodeBox) {
        let Some((vw, vh)) = self.viewport_size() else { return };
        let (x, y) = self.node_offset(node);
        let (w, h) = (node.w * self.zoom, node.h * self.zoom);
        let inside = x >= -vw / 2. && x + w <= vw / 2. && y >= -vh / 2. && y + h <= vh / 2.;
        if !inside {
            let (cw, ch) = self.content;
            self.pan_x = -(node.center_x() - cw / 2.) * self.zoom;
            self.pan_y = -(node.center_y() - ch / 2.) * self.zoom;
        }
    }

    fn viewport_cell(&self) -> Rc<Cell<Option<Bounds<Pixels>>>> {
        Rc::clone(&self.viewport)
    }
}

fn wheel_delta(delta: &ScrollDelta) -> (f32, f32) {
    match delta {
        ScrollDelta::Pixels(point) => (point.x.as_f32(), point.y.as_f32()),
        ScrollDelta::Lines(point) => (point.x * WHEEL_LINE_PX, point.y * WHEEL_LINE_PX),
    }
}

fn op_icon(node: &PlanNode, p: &Palette) -> (IconName, Hsla) {
    let warn = !node.issues.is_empty();
    match node.kind() {
        OpKind::Scan => (IconName::Rows3, if warn { p.danger } else { p.text_muted }),
        OpKind::Seek => (IconName::Search, p.ok),
        OpKind::Join => (IconName::Merge, p.info),
        OpKind::Sort => (IconName::ArrowDownWideNarrow, if warn { p.warn } else { p.text_muted }),
        OpKind::Aggregate => (IconName::Sigma, p.info),
        OpKind::Other => (IconName::Circle, p.text_faint),
    }
}

/// 卡片绘制用的缩放后尺寸。
struct CardStyle {
    zoom: f32,
    text_sm: f32,
    text_xs: f32,
    radius: f32,
    tag_radius: f32,
    pad: f32,
    gap: f32,
    compact: bool,
    mono: SharedString,
}

impl CardStyle {
    fn chip(&self, label: impl Into<SharedString>, fg: Hsla, bg: Hsla) -> gpui_kit::Div {
        let (text_xs, tag_radius) = (self.text_xs, self.tag_radius);
        div()
            .flex_none()
            .px(px(self.gap * 1.5))
            .rounded(px(tag_radius))
            .bg(bg)
            .text_size(px(text_xs))
            .font_weight(FontWeight::MEDIUM)
            .text_color(fg)
            .child(label.into())
    }
}

/// JOIN 节点：紧凑胶囊，连接方式加产出行数。
fn join_pill(node: &PlanNode, style: &CardStyle, locale: Locale, p: &Palette) -> AnyElement {
    let (text_sm, text_xs) = (style.text_sm, style.text_xs);
    let rows = node.actual_rows.or(node.rows).map(|rows| i18n::fill(&i18n::text(locale, "query.plan.estRows"), &[("rows", &plan_view::rows_text(rows))]));
    div()
        .size_full()
        .px(px(style.pad * 1.5))
        .flex()
        .items_center()
        .gap(px(style.gap))
        .child(kit::icon(IconName::Merge, 14. * style.zoom, p.info))
        .child(div().flex_1().min_w_0().truncate().text_size(px(text_sm)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(SharedString::from(node.title())))
        .when_some(rows, |this, rows| {
            this.child(div().flex_none().text_size(px(text_xs)).font_features(kit::tabular_figures()).text_color(p.text_muted).child(SharedString::from(rows)))
        })
        .into_any_element()
}

/// 表、操作与子查询卡片：顶部热度条；左上相对代价、右上行数；标题（表名加别名）与访问类型标签；
/// 索引或条件一行；底部风险标签。
#[allow(clippy::too_many_arguments)]
fn node_card(node: &PlanNode, kind: NodeKind, share: Option<f32>, time_share: bool, style: &CardStyle, locale: Locale, p: &Palette) -> AnyElement {
    let heat = share.map(|share| plan_view::heat_bar_color(share, p));
    let (gap, text_sm, text_xs) = (style.gap, style.text_sm, style.text_xs);
    let (icon, tone) = op_icon(node, p);
    let icon = match kind {
        NodeKind::Table => IconName::Table,
        NodeKind::Subquery => IconName::Braces,
        _ => icon,
    };

    // 角标：左上相对代价（有代价数据时），右上行数。
    let cost_badge = if time_share {
        node.actual_time_ms.map(plan_view::time_ms_text)
    } else {
        node.cost.map(|cost| format!("{} {}", i18n::text(locale, "query.plan.cost"), plan_view::cost_text(cost)))
    }
    .map(|text| match share {
        Some(share) => format!("{text} · {}%", (share * 100.).round() as i32),
        None => text,
    });
    let rows_badge = match (node.actual_rows, node.rows) {
        (Some(actual), _) => Some(i18n::fill(&i18n::text(locale, "query.plan.actualRowsSummary"), &[("rows", &plan_view::rows_text(actual))])),
        (None, Some(rows)) => Some(i18n::fill(&i18n::text(locale, "query.plan.estRows"), &[("rows", &plan_view::rows_text(rows))])),
        _ => None,
    };
    let hot = share.is_some_and(|share| share > 0.6);
    let badges = div()
        .flex()
        .items_center()
        .gap(px(gap))
        .min_w_0()
        .h(px(style.text_xs * 1.4))
        .text_size(px(text_xs))
        .font_features(kit::tabular_figures())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(if hot { p.danger } else { p.text_faint })
                .child(SharedString::from(cost_badge.unwrap_or_default())),
        )
        .when_some(rows_badge, |this, rows| this.child(div().flex_none().text_color(p.text_muted).child(SharedString::from(rows))));

    // 标题：表名（加别名）或操作名；访问类型是带颜色的标签，没有访问类型的表节点用操作名作标签。
    let mut title = div()
        .flex()
        .items_center()
        .gap(px(gap))
        .min_w_0()
        .child(kit::icon(icon, 14. * style.zoom, if kind == NodeKind::Table { p.accent_text } else { tone }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .when(kind == NodeKind::Table, |this| this.font_family(style.mono.clone()))
                .text_size(px(text_sm))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.text)
                .child(SharedString::from(node.title())),
        );
    match (&node.access_type, kind) {
        (Some(access), _) => {
            let (fg, bg) = plan_view::access_type_colors(access, p);
            title = title.child(style.chip(access.clone(), fg, bg));
        }
        (None, NodeKind::Table) => title = title.child(style.chip(node.op.clone(), p.text_muted, p.badge)),
        _ => {}
    }

    let mut inner = div()
        .flex_1()
        .min_h_0()
        .px(px(style.pad))
        .py(px(style.pad * 0.6))
        .flex()
        .flex_col()
        .gap(px(gap))
        .child(badges)
        .child(title);
    if !style.compact {
        // 索引（粗体，与 Workbench 一致）或条件；非表节点的对象也在这一行。
        let line = node
            .key_used
            .clone()
            .map(|key| (true, key))
            .or_else(|| node.object.clone().filter(|_| kind != NodeKind::Table).map(|object| (false, object)))
            .or_else(|| {
                let detail = node.detail.trim();
                (!detail.is_empty()).then(|| (false, detail.to_owned()))
            });
        inner = inner.child(div().flex().items_center().gap(px(gap)).min_w_0().h(px(style.text_xs * 1.4)).when_some(line, |this, (is_key, text)| {
            this.when(is_key, |this| this.child(kit::icon(IconName::Key, 11. * style.zoom, p.text_faint))).child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(style.mono.clone())
                    .text_size(px(text_xs))
                    .when(is_key, |this| this.font_weight(FontWeight::BOLD).text_color(p.text))
                    .when(!is_key, |this| this.text_color(p.text_muted))
                    .child(SharedString::from(text)),
            )
        }));
        // 底部风险标签：全表扫描、临时排序、临时表；覆盖索引是正面标签。
        let mut tags = div().flex().items_center().gap(px(gap)).min_w_0().overflow_hidden();
        for issue in &node.issues {
            tags = tags.child(style.chip(i18n::text(locale, issue.label_key()), p.danger, p.danger_soft));
        }
        if node.covering_index {
            tags = tags.child(style.chip(i18n::text(locale, "query.plan.coveringIndex"), p.ok, p.ok_soft));
        }
        inner = inner.child(tags);
    }
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_none()
                .h(px(HEAT_BAR_H * style.zoom))
                .w_full()
                .bg(p.sunken)
                .when_some(share.zip(heat), |this, (share, color)| this.child(div().h_full().w(relative(share.max(0.02))).bg(color))),
        )
        .child(inner)
        .into_any_element()
}

/// 一条连线：起点、终点（相对视口中心）与是否高亮。
type Wire = ((f32, f32), (f32, f32), bool);

/// 画布视图需要的只读输入。
pub struct CanvasView<'a> {
    pub plan: &'a Plan,
    pub layout: &'a Layout,
    pub state: &'a CanvasState,
    pub selected: usize,
    pub locale: Locale,
    pub mono: SharedString,
    pub focus: FocusHandle,
}

impl CanvasView<'_> {
    pub fn render(&self, p: &Palette, entity: &Entity<QueryPane>) -> AnyElement {
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let lines = self.plan.lines();
        let zoom = self.state.zoom;
        let total_cost = self.plan.total_cost().filter(|cost| *cost > 0.);
        let total_time = self.plan.total_actual_time_ms().filter(|time| *time > 0.);
        let use_time_share = total_time.is_some();
        let multi = lines.len() > 1;

        // 连线：父卡片底部中点到子卡片顶部中点的竖向贝塞尔；与选中节点相连的边高亮。
        let mut wires: Vec<Wire> = Vec::new();
        for edge in &self.layout.edges {
            let (Some(from), Some(to)) = (self.layout.node(edge.parent), self.layout.node(edge.child)) else { continue };
            let (fx, fy) = self.state.node_offset(from);
            let (tx, ty) = self.state.node_offset(to);
            let hot = edge.parent == self.selected || edge.child == self.selected;
            wires.push(((fx + from.w * zoom / 2., fy + from.h * zoom), (tx + to.w * zoom / 2., ty), hot));
        }
        let (wire, hot_wire) = (p.line_strong, p.accent);
        let viewport = self.state.viewport_cell();
        let fit_pending = self.state.fit_pending;
        let notify_entity = entity.clone();
        let wires_layer = canvas(
            move |bounds, window, _| {
                let previous = viewport.get();
                viewport.set(Some(bounds));
                // 第一次拿到视口尺寸（或尺寸变了）且还在等适配：下一帧按真实尺寸重画。
                if fit_pending && previous.map(|old| old.size) != Some(bounds.size) {
                    let entity = notify_entity.clone();
                    window.on_next_frame(move |_, cx| entity.update(cx, |_, cx| cx.notify()));
                }
            },
            move |bounds, _, window, _| {
                let center = bounds.center();
                for ((sx, sy), (ex, ey), hot) in &wires {
                    let start = center + gpui_kit::point(px(*sx), px(*sy));
                    let end = center + gpui_kit::point(px(*ex), px(*ey));
                    let bend = ((end.y - start.y) / 2.).max(px(GAP_Y * 0.3));
                    let c1 = gpui_kit::point(start.x, start.y + bend);
                    let c2 = gpui_kit::point(end.x, end.y - bend);
                    let mut builder = PathBuilder::stroke(px(if *hot { EDGE_HOT_W } else { EDGE_W }));
                    builder.move_to(start);
                    builder.cubic_bezier_to(end, c1, c2);
                    if let Ok(path) = builder.build() {
                        window.paint_path(path, if *hot { hot_wire } else { wire });
                    }
                }
            },
        )
        .absolute()
        .inset_0();

        // 卡片挂在视口中心的零尺寸锚点上，偏移即 `node_offset`。
        let style = CardStyle {
            zoom,
            text_sm: TypeScale::SM * zoom,
            text_xs: TypeScale::XS * zoom,
            radius: Radius::PANEL * zoom,
            tag_radius: Radius::TAG * zoom,
            pad: CARD_PAD * zoom,
            gap: Space::XS * zoom,
            compact: zoom < COMPACT_ZOOM,
            mono: self.mono.clone(),
        };
        let (text_sm, radius) = (style.text_sm, style.radius);
        let mut anchor = div().absolute().left(relative(0.5)).top(relative(0.5)).size_0();
        if let Some(root) = &self.layout.virtual_root {
            let (x, y) = self.state.node_offset(root);
            anchor = anchor.child(
                div()
                    .id("query-plan-card-root")
                    .test_support()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(root.w * zoom))
                    .h(px(root.h * zoom))
                    .rounded_full()
                    .border_1()
                    .border_color(p.line_strong)
                    .bg(p.raised)
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(style.gap))
                    .child(kit::icon(IconName::Route, 14. * zoom, p.accent_text))
                    .child(div().text_size(px(text_sm)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(t("query.plan.canvas.root"))),
            );
        }
        for (line, node_box) in lines.iter().zip(&self.layout.nodes) {
            let node = line.node;
            let index = node_box.index;
            let (x, y) = self.state.node_offset(node_box);
            let is_selected = index == self.selected;
            let share = if use_time_share { plan_view::cost_share(node.actual_time_ms, total_time) } else { plan_view::cost_share(node.cost, total_cost) };
            let share = share.filter(|_| multi);
            let kind = node.node_kind();
            let body = match kind {
                NodeKind::Join => join_pill(node, &style, locale, p),
                _ => node_card(node, kind, share, use_time_share, &style, locale, p),
            };
            let warn = !node.issues.is_empty();
            let border = if is_selected {
                p.accent
            } else if warn {
                p.danger
            } else {
                p.line_strong
            };
            // 选中态只用强调色边框加外发光，底色不变，文字始终清晰可读。
            let glow = gpui_kit::BoxShadow {
                color: p.accent.opacity(if p.dark { 0.55 } else { 0.4 }),
                offset: Default::default(),
                blur_radius: px(GLOW_BLUR * zoom),
                spread_radius: px(GLOW_SPREAD * zoom),
                inset: false,
            };
            let mut card = div()
                .id(SharedString::from(format!("query-plan-card-{index}")))
                .test_support()
                .role(gpui_kit::Role::TreeItem)
                .aria_label(SharedString::from(plan_view::node_label(&lines, index)))
                .aria_selected(is_selected)
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(node_box.w * zoom))
                .h(px(node_box.h * zoom))
                .border_color(border)
                .when(is_selected, |this| this.border_2().shadow(vec![glow]))
                .when(!is_selected, |this| this.border_1().shadow_sm().hover(|style| style.border_color(p.accent)))
                .when(kind == NodeKind::Subquery && !is_selected, |this| this.border_dashed())
                .bg(if kind == NodeKind::Subquery { p.raised } else { p.surface })
                .when(kind == NodeKind::Join, |this| this.rounded_full())
                .when(kind != NodeKind::Join, |this| this.rounded(px(radius)))
                .overflow_hidden()
                .cursor_pointer()
                .child(body);
            let focus = self.focus.clone();
            let select = entity.clone();
            card = card.on_click(move |_, window, cx| {
                let accept = select.update(cx, |pane, _| pane.plan_canvas.take_click());
                if accept {
                    window.focus(&focus, cx);
                    select.update(cx, |pane, cx| pane.command(&format!("plan-select:{index}"), cx));
                }
            });
            anchor = anchor.child(card);
        }

        // 右上角工具条：适配 / 100% / − / +，以及当前缩放比例。
        let act = |id: &'static str| {
            let entity = entity.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut gpui_kit::Window, cx: &mut gpui_kit::App| entity.update(cx, |pane, cx| pane.command(id, cx))
        };
        let tools = div()
            .id("query-plan-canvas-tools")
            .test_support()
            .absolute()
            .top(px(Space::MD))
            .right(px(Space::MD))
            .flex()
            .items_center()
            .gap(px(Space::XXS))
            .p(px(Space::XXS))
            .rounded(px(Radius::CONTROL))
            .bg(p.surface)
            .border_1()
            .border_color(p.line_soft)
            .shadow_sm()
            // 工具条上的点击不开始平移拖拽。
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(kit::tool_button("plan-canvas-fit", Some(IconName::Maximize), t("query.plan.canvas.fit"), act("plan-canvas:fit")).tooltip(t("query.plan.canvas.fit")))
            .child(kit::tool_button("plan-canvas-reset", None, t("query.plan.canvas.reset"), act("plan-canvas:reset")).tooltip(t("query.plan.canvas.resetTip")))
            .child(kit::icon_button("plan-canvas-zoom-out", IconName::Minus, t("query.plan.canvas.zoomOut"), act("plan-canvas:out")).tooltip(t("query.plan.canvas.zoomOut")))
            .child(
                div()
                    .id("plan-canvas-zoom")
                    .test_support()
                    .min_w(px(Space::XXL * 1.75))
                    .flex()
                    .justify_center()
                    .text_size(px(TypeScale::XS))
                    .font_features(kit::tabular_figures())
                    .text_color(p.text_muted)
                    .child(SharedString::from(format!("{}%", (zoom * 100.).round() as i32))),
            )
            .child(kit::icon_button("plan-canvas-zoom-in", IconName::Plus, t("query.plan.canvas.zoomIn"), act("plan-canvas:in")).tooltip(t("query.plan.canvas.zoomIn")));

        let ring = p.accent;
        let nav = entity.clone();
        let wheel_entity = entity.clone();
        let down_entity = entity.clone();
        let move_entity = entity.clone();
        let up_entity = entity.clone();
        div()
            .id("query-plan-canvas")
            .test_support()
            .role(gpui_kit::Role::Tree)
            .aria_label(t("query.plan.title"))
            .track_focus(&self.focus)
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .focus_visible(move |style| style.border_color(ring))
            .on_key_down(move |event: &gpui_kit::KeyDownEvent, _, cx| {
                if let Some(key) = plan_view::nav_key(event) {
                    cx.stop_propagation();
                    nav.update(cx, |pane, cx| pane.command(&format!("plan-nav:{key}"), cx));
                }
            })
            .on_scroll(move |event: &ScrollEvent, _, cx| {
                let (dx, dy) = wheel_delta(&event.delta);
                let zoom_modifier = event.modifiers.platform || event.modifiers.control;
                wheel_entity.update(cx, |pane, cx| {
                    let at = pane
                        .plan_canvas
                        .viewport
                        .get()
                        .map(|bounds| {
                            let center = bounds.center();
                            ((event.position.x - center.x).as_f32(), (event.position.y - center.y).as_f32())
                        })
                        .unwrap_or((0., 0.));
                    pane.plan_canvas.wheel(dx, dy, zoom_modifier, at);
                    cx.notify();
                });
            })
            .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _, cx| {
                down_entity.update(cx, |pane, _| pane.plan_canvas.begin_drag(event.position.x.as_f32(), event.position.y.as_f32()));
            })
            .on_mouse_move(move |event: &MouseMoveEvent, _, cx| {
                move_entity.update(cx, |pane, cx| {
                    if pane.plan_canvas.move_drag(event.position.x.as_f32(), event.position.y.as_f32()) {
                        cx.notify();
                    }
                });
            })
            .on_mouse_up(MouseButton::Left, move |_: &gpui_kit::MouseUpEvent, _, cx| {
                up_entity.update(cx, |pane, cx| {
                    let was = pane.plan_canvas.dragging();
                    pane.plan_canvas.end_drag();
                    if was {
                        cx.notify();
                    }
                });
            })
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(p.chrome)
            .child(wires_layer)
            .child(anchor)
            .child(tools)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(op: &str, children: Vec<PlanNode>) -> PlanNode {
        PlanNode { op: op.into(), children, ..Default::default() }
    }

    fn plan(roots: Vec<PlanNode>) -> Plan {
        Plan { engine: "mysql".into(), roots }
    }

    trait Edges {
        fn right(&self) -> f32;
        fn bottom(&self) -> f32;
    }

    impl Edges for NodeBox {
        fn right(&self) -> f32 {
            self.x + self.w
        }

        fn bottom(&self) -> f32 {
            self.y + self.h
        }
    }

    fn overlaps(a: &NodeBox, b: &NodeBox) -> bool {
        a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
    }

    fn assert_no_overlap(layout: &Layout) {
        for (i, a) in layout.nodes.iter().enumerate() {
            for b in &layout.nodes[i + 1..] {
                assert!(!overlaps(a, b), "节点 {} 与 {} 重叠：{a:?} {b:?}", a.index, b.index);
            }
        }
    }

    /// 多层多子节点：节点框互不重叠，父节点在子节点块上方水平居中，全部落在内容范围内，结果确定。
    #[test]
    fn layout_is_tidy_deterministic_and_overlap_free() {
        let tree = plan(vec![node(
            "Nested loop",
            vec![
                node("Hash join", vec![node("Scan a", vec![]), node("Hash", vec![node("Scan b", vec![]), node("Filter", vec![node("Scan c", vec![])])])]),
                node("Sort", vec![node("Index lookup d", vec![])]),
                node("Scan e", vec![]),
            ],
        )]);
        let first = layout(&tree);
        assert_eq!(first, layout(&tree), "同一份计划得到同一组坐标");
        assert_eq!(first.nodes.len(), tree.lines().len());
        assert_eq!(first.edges.len(), tree.lines().len() - 1, "每个非根节点一条父子连线");
        for (at, node) in first.nodes.iter().enumerate() {
            assert_eq!(node.index, at, "按前序下标排列");
            assert_eq!(node.depth, tree.lines()[at].depth, "深度与列表视图一致");
            assert!(node.x >= MARGIN - 0.01 && node.right() <= first.width - MARGIN + 0.01, "节点 {at} 在内容宽度内");
            assert!(node.y >= MARGIN - 0.01 && node.bottom() <= first.height - MARGIN + 0.01, "节点 {at} 在内容高度内");
        }
        assert_no_overlap(&first);
        for parent in &first.nodes {
            let kids: Vec<&NodeBox> = first.edges.iter().filter(|edge| edge.parent == parent.index).map(|edge| &first.nodes[edge.child]).collect();
            if kids.is_empty() {
                continue;
            }
            let (first, last) = (kids[0].center_x(), kids[kids.len() - 1].center_x());
            assert!((parent.center_x() - (first + last) / 2.).abs() < 0.01, "父节点 {} 在首尾子节点上方居中", parent.index);
            for kid in &kids {
                assert!(kid.y > parent.bottom(), "子节点 {} 在父节点 {} 下方", kid.index, parent.index);
            }
            // 子节点按前序从左到右排列。
            for pair in kids.windows(2) {
                assert!(pair[0].right() < pair[1].x, "兄弟节点 {} 与 {} 从左到右不重叠", pair[0].index, pair[1].index);
            }
        }
    }

    /// 多个根节点并排放置，顶部加一个虚拟“查询”根连到每个根；不同子树的节点即使在同一层也不重叠。
    #[test]
    fn layout_places_multiple_roots_under_a_virtual_root() {
        let roots = plan(vec![node("A", vec![node("A1", vec![]), node("A2", vec![])]), node("B", vec![node("B1", vec![])]), node("C", vec![])]);
        let layout = layout(&roots);
        assert_no_overlap(&layout);
        let tops: Vec<&NodeBox> = layout.nodes.iter().filter(|node| node.depth == 0).collect();
        assert_eq!(tops.len(), 3);
        assert!(tops[0].right() < tops[1].x && tops[1].right() < tops[2].x, "根节点从左到右");
        assert_eq!(
            layout.edges,
            vec![
                Edge { parent: VIRTUAL_ROOT, child: 0 },
                Edge { parent: 0, child: 1 },
                Edge { parent: 0, child: 2 },
                Edge { parent: VIRTUAL_ROOT, child: 3 },
                Edge { parent: 3, child: 4 },
                Edge { parent: VIRTUAL_ROOT, child: 5 },
            ]
        );
        let root = layout.virtual_root.expect("多个根时有虚拟根");
        assert_eq!(layout.node(VIRTUAL_ROOT), Some(&root));
        assert!(tops.iter().all(|top| top.y > root.y + root.h), "真实的根画在虚拟根下面一层");
        let first = tops[0].center_x();
        let last = tops[2].center_x();
        assert!((root.center_x() - (first + last) / 2.).abs() < 0.01, "虚拟根在各根上方居中");
        assert!(root.x >= MARGIN - 0.01 && root.x + root.w <= layout.width - MARGIN + 0.01);
        for node in &layout.nodes {
            assert!(node.bottom() <= layout.height - MARGIN + 0.01, "节点 {} 在内容高度内", node.index);
        }
    }

    /// 三表 JOIN（左深 Nested loop）：2 个 JOIN 胶囊、3 张表卡片、4 条连线；JOIN 胶囊比卡片小且在槽位居中。
    #[test]
    fn three_table_join_layout_has_edges_and_join_pills() {
        let table = |name: &str| PlanNode { op: "ALL".into(), object: Some(name.into()), category: Some(NodeKind::Table), ..Default::default() };
        let join = |children| PlanNode { op: "Nested loop".into(), children, category: Some(NodeKind::Join), ..Default::default() };
        let tree = plan(vec![join(vec![join(vec![table("orders o"), table("customers c")]), table("order_lines l")])]);
        let layout = layout(&tree);
        assert!(layout.virtual_root.is_none(), "单根不需要虚拟根");
        assert_eq!(layout.nodes.len(), 5);
        assert_eq!(layout.edges.len(), 4);
        assert_no_overlap(&layout);
        for node in &layout.nodes {
            let kind = tree.lines()[node.index].node.node_kind();
            let expected = if kind == NodeKind::Join { (PILL_W, PILL_H) } else { (NODE_W, NODE_H) };
            assert_eq!((node.w, node.h), expected, "节点 {} 的卡片尺寸按类型", node.index);
        }
        for edge in &layout.edges {
            let (parent, child) = (layout.node(edge.parent).unwrap(), layout.node(edge.child).unwrap());
            assert!(child.y > parent.bottom(), "连线从上到下：{edge:?}");
        }
        assert_eq!(tree.lines()[2].node.title(), "orders o", "表节点标题是表名加别名");
    }

    /// 单节点：内容就是一张卡片加留白；默认视图（平移 0）下卡片中心落在视口中心。
    #[test]
    fn single_node_is_centered() {
        let one = plan(vec![node("SCAN t", vec![])]);
        let layout = layout(&one);
        assert_eq!(layout.nodes.len(), 1);
        assert!(layout.edges.is_empty());
        assert!(layout.virtual_root.is_none());
        assert_eq!((layout.width, layout.height), (NODE_W + MARGIN * 2., NODE_H + MARGIN * 2.));
        let mut state = CanvasState::default();
        state.sync(layout.width, layout.height);
        let (x, y) = state.node_offset(&layout.nodes[0]);
        assert!((x + NODE_W / 2.).abs() < 0.01 && (y + NODE_H / 2.).abs() < 0.01, "卡片中心在视口中心：{x} {y}");
        assert_eq!(super::layout(&plan(vec![])), Layout::default(), "空计划没有节点");
    }

    /// 缩放钳制在 0.4–2.0；以指针为中心缩放时指针下的内容点不动；拖拽超过阈值才平移，拖拽后的点击被吞掉。
    #[test]
    fn zoom_pan_and_drag_follow_the_pointer() {
        let mut state = CanvasState::default();
        state.sync(1000., 600.);
        for _ in 0..20 {
            state.zoom_in();
        }
        assert_eq!(state.zoom, ZOOM_MAX);
        for _ in 0..40 {
            state.zoom_out();
        }
        assert_eq!(state.zoom, ZOOM_MIN);
        state.zoom_reset();
        assert_eq!(state.zoom, 1.);

        state.pan_x = 30.;
        state.pan_y = -10.;
        let at = (120., 80.);
        let before = ((at.0 - state.pan_x) / state.zoom, (at.1 - state.pan_y) / state.zoom);
        state.wheel(0., 200., true, at);
        assert!(state.zoom > 1., "⌘/Ctrl + 向上滚动放大");
        let after = ((at.0 - state.pan_x) / state.zoom, (at.1 - state.pan_y) / state.zoom);
        assert!((before.0 - after.0).abs() < 0.01 && (before.1 - after.1).abs() < 0.01, "指针下的内容点不动");
        state.wheel(0., 100_000., true, at);
        assert_eq!(state.zoom, ZOOM_MAX, "滚轮缩放同样钳制");

        let (pan_x, pan_y) = (state.pan_x, state.pan_y);
        state.wheel(15., -25., false, at);
        assert_eq!((state.pan_x, state.pan_y), (pan_x + 15., pan_y - 25.), "普通滚轮平移");

        state.begin_drag(10., 10.);
        assert!(!state.move_drag(11., 12.), "小于阈值不平移");
        state.end_drag();
        assert!(state.take_click(), "没动过的按下松开仍是点击");
        let (pan_x, pan_y) = (state.pan_x, state.pan_y);
        state.begin_drag(10., 10.);
        assert!(state.move_drag(60., 30.));
        assert_eq!((state.pan_x, state.pan_y), (pan_x + 50., pan_y + 20.));
        state.end_drag();
        assert!(!state.take_click(), "拖拽结束后的那一下点击不选中节点");
        assert!(state.take_click(), "之后的点击照常");
    }

    /// 适配：大内容缩小到能放下且不超过 100%，小内容保持 100% 居中；太高时从顶部开始。
    #[test]
    fn fit_shrinks_to_the_viewport_without_enlarging() {
        let mut state = CanvasState::default();
        state.sync(2000., 400.);
        state.fit_to(1048., 600.);
        assert!((state.zoom - 0.5).abs() < 0.01, "{}", state.zoom);
        assert_eq!((state.pan_x, state.pan_y), (0., 0.));
        state.sync(200., 100.);
        state.fit_to(1000., 600.);
        assert_eq!(state.zoom, 1., "小计划不被放大");
        state.sync(300., 10_000.);
        state.fit_to(800., 600.);
        assert_eq!(state.zoom, ZOOM_MIN);
        let top = state.pan_y + (0. - 10_000. / 2.) * state.zoom;
        assert!((top + 300.).abs() < 0.01, "内容顶部对齐视口顶部：{top}");
    }
}
