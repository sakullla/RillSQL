//! 资源管理器的扁平行模型：分组标题、连接行与对象树行合成一条可见行序列，
//! 渲染只创建视口附近的行元素。鼠标、键盘与右键命令都认这里的稳定 id，
//! 不再按下标索引内层 Vec，虚拟化后上下移动也不会错行。

use crate::catalog::hierarchy::NavRow;
use crate::ui::kit::Metrics;

/// 一行资源管理器。分组与连接来自本地配置，Nav 行来自目录树。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplorerItem {
    /// 用户分组标题：空组也在序列里，删除后的连接回顶层由构建这一步保证。
    Group { id: String, collapsed: bool, members: usize },
    /// 一条连接；名字、状态与徽标由渲染阶段按 id 取，不在这里复制。
    Connection { id: String },
    /// 连接级读取失败：原因只写在这一条连接下面。
    ConnectionError { connection_id: String, error: String },
    /// 连接下“没有对象 / 无命中 / 读取中”的说明行。
    Note { connection_id: String, key: &'static str },
    /// 目录树的一行（库、模式、种类分组、对象、列、索引）。
    Nav(NavRow),
    /// 全局目录读取失败，只留一条。
    CatalogError { message: String },
    /// 筛选之后没有任何命中。
    NoMatch,
}

impl ExplorerItem {
    /// 行高：树行与说明行用树高，连接行用列表行高；窗口计算与渲染读同一份。
    pub fn height(&self, m: &Metrics) -> f32 {
        match self {
            ExplorerItem::Connection { .. } | ExplorerItem::CatalogError { .. } => m.row_h,
            _ => m.tree_row_h,
        }
    }

    /// 稳定 id：同一行在任何一帧、任何滚动位置都得到同一个字符串。
    pub fn id(&self) -> String {
        match self {
            ExplorerItem::Group { id, .. } => format!("group:{id}"),
            ExplorerItem::Connection { id } => format!("conn:{id}"),
            ExplorerItem::ConnectionError { connection_id, .. } => format!("conn-error:{connection_id}"),
            ExplorerItem::Note { connection_id, .. } => format!("note:{connection_id}"),
            ExplorerItem::Nav(row) => format!("nav:{}", row.id),
            ExplorerItem::CatalogError { .. } => "catalog-error".to_owned(),
            ExplorerItem::NoMatch => "explorer-no-match".to_owned(),
        }
    }

    /// 键盘能停的行：说明行、错误行与无命中提示不参与移动。
    pub fn focusable(&self) -> bool {
        matches!(self, ExplorerItem::Group { .. } | ExplorerItem::Connection { .. } | ExplorerItem::Nav(_))
    }

    pub fn nav_row(&self) -> Option<&NavRow> {
        match self {
            ExplorerItem::Nav(row) => Some(row),
            _ => None,
        }
    }
}

/// 拖拽中的连接：只带稳定 id，具体落到哪个分组由放手时的那一行决定。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionDrag {
    pub connection: String,
}

/// 键盘当前行。分组、连接与树节点三种身份都来自这里的稳定 id。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplorerRow {
    Group(String),
    Connection(String),
    Node(String),
}

impl ExplorerRow {
    /// 这一行在扁平序列里的稳定 id（与 ExplorerItem::id 同一套前缀）。
    pub fn item_id(&self) -> String {
        match self {
            ExplorerRow::Group(id) => format!("group:{id}"),
            ExplorerRow::Connection(id) => format!("conn:{id}"),
            ExplorerRow::Node(id) => format!("nav:{id}"),
        }
    }

    pub fn of(item: &ExplorerItem) -> Option<Self> {
        match item {
            ExplorerItem::Group { id, .. } => Some(ExplorerRow::Group(id.clone())),
            ExplorerItem::Connection { id } => Some(ExplorerRow::Connection(id.clone())),
            ExplorerItem::Nav(row) => Some(ExplorerRow::Node(row.id.clone())),
            _ => None,
        }
    }
}

/// 键盘可停留的行，按渲染顺序。
pub fn focus_rows(items: &[ExplorerItem]) -> Vec<ExplorerRow> {
    items.iter().filter(|item| item.focusable()).filter_map(ExplorerRow::of).collect()
}

/// 视口窗口：只渲染这一段，上下用空白撑起其余高度，元素数量不随总节点增长。
#[derive(Debug, Clone, PartialEq)]
pub struct ViewportWindow {
    /// 要渲染的行区间（左闭右开）。
    pub range: std::ops::Range<usize>,
    /// 这段行上方的空白高度。
    pub pad_top: f32,
    /// 这段行下方的空白高度。
    pub pad_bottom: f32,
}

/// 视口高度还未知（第一帧）时的兜底：宁可按普通窗口算，也不渲染全部行。
pub const FALLBACK_VIEWPORT_H: f32 = 480.;
/// 视口上下各多渲染几行，滚动手感和滚动条下沿不会露白。
pub const OVERSCAN_ROWS: usize = 4;

/// 按每行真实高度累加，给出视口内（含 overscan）的行区间与上下空白。
pub fn window_rows(heights: &[f32], scroll_top: f32, viewport_h: f32, overscan: usize) -> ViewportWindow {
    let total: f32 = heights.iter().sum();
    if heights.is_empty() {
        return ViewportWindow { range: 0..0, pad_top: 0., pad_bottom: 0. };
    }
    let viewport = if viewport_h.is_finite() && viewport_h > 0. { viewport_h } else { FALLBACK_VIEWPORT_H };
    let scroll = if scroll_top.is_finite() { scroll_top.max(0.) } else { 0. };
    let top = scroll.min((total - viewport).max(0.));
    let mut offset = 0.;
    let mut start = 0usize;
    let mut end = heights.len();
    for (index, height) in heights.iter().enumerate() {
        if offset + height > top {
            start = index;
            break;
        }
        offset += height;
    }
    let mut position = offset;
    let limit = top + viewport;
    for (index, height) in heights.iter().enumerate().skip(start) {
        if position >= limit {
            end = index;
            break;
        }
        position += height;
    }
    let start = start.saturating_sub(overscan);
    let end = (end.saturating_add(overscan)).min(heights.len());
    let pad_top: f32 = heights[..start].iter().sum();
    let pad_bottom: f32 = (total - pad_top - heights[start..end].iter().sum::<f32>()).max(0.);
    ViewportWindow { range: start..end, pad_top, pad_bottom }
}

/// 把某一行滚进视口需要的目标偏移（正数，单位像素，行顶对齐到视口顶）。
pub fn scroll_offset_for(heights: &[f32], index: usize, viewport_h: f32) -> f32 {
    let offset: f32 = heights[..index.min(heights.len())].iter().sum();
    let total: f32 = heights.iter().sum();
    let viewport = if viewport_h.is_finite() && viewport_h > 0. { viewport_h } else { FALLBACK_VIEWPORT_H };
    offset.min((total - viewport).max(0.))
}

/// 对象行、连接行与分组标题共用的一个提示串：完整名称永远可读。
pub fn full_label(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix} / {name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(count: usize, height: f32) -> Vec<f32> {
        vec![height; count]
    }

    #[test]
    fn window_covers_viewport_only_and_pads_the_rest() {
        let heights = rows(1000, 23.);
        let window = window_rows(&heights, 2300., 230., 0);
        assert_eq!(window.range, 100..110, "10 行视口只看这 10 行");
        assert_eq!(window.pad_top, 2300.);
        assert!((window.pad_bottom - (23000. - 2300. - 230.)).abs() < 0.5);
    }

    #[test]
    fn element_count_does_not_grow_with_total_rows() {
        for count in [20usize, 200, 5000, 100_000] {
            let heights = rows(count, 23.);
            let window = window_rows(&heights, 0., 230., OVERSCAN_ROWS);
            let created = window.range.len();
            assert!(created <= 10 + 2 * OVERSCAN_ROWS, "{count} 行时只创建 {created} 个元素");
        }
    }

    #[test]
    fn mixed_heights_keep_scroll_coordinates_consistent() {
        // 连接行 32、树行 30 混排：窗口起点的空白等于前面行的真实高度之和。
        let heights = vec![32., 32., 30., 30., 30., 32.];
        let window = window_rows(&heights, 64., 30., 0);
        assert_eq!(window.range, 2..3);
        assert_eq!(window.pad_top, 64.);
        assert_eq!(window.pad_bottom, 92.);
    }

    #[test]
    fn unknown_viewport_and_scroll_never_render_everything() {
        let heights = rows(4000, 30.);
        let window = window_rows(&heights, f32::NAN, -1., 0);
        assert_eq!(window.range.start, 0);
        assert!(window.range.len() <= (FALLBACK_VIEWPORT_H / 30.).ceil() as usize + 1);
        let tall = window_rows(&heights, 5000., 900., 0);
        assert_eq!(tall.range.start, 166, "5000px 处从第 166 行开始");
        assert_eq!(tall.pad_top, 4980.);
        let past_end = window_rows(&heights, 1_000_000., 900., 0);
        assert_eq!(past_end.pad_top, 4000. * 30. - 900., "滚过末尾时按最后一段算");
        assert_eq!(past_end.range.end, 4000);
    }

    #[test]
    fn empty_list_has_no_window() {
        let window = window_rows(&[], 0., 400., OVERSCAN_ROWS);
        assert_eq!(window.range, 0..0);
        assert_eq!((window.pad_top, window.pad_bottom), (0., 0.));
    }

    #[test]
    fn scroll_to_row_offsets_are_clamped_to_content() {
        let heights = rows(10, 20.);
        assert_eq!(scroll_offset_for(&heights, 3, 40.), 60.);
        assert_eq!(scroll_offset_for(&heights, 9, 40.), 160.);
        assert_eq!(scroll_offset_for(&heights, 99, 40.), 160.);
    }

    #[test]
    fn focus_rows_skip_notes_and_errors_and_keep_stable_ids() {
        let items = vec![
            ExplorerItem::Group { id: "g1".into(), collapsed: false, members: 2 },
            ExplorerItem::Connection { id: "c1".into() },
            ExplorerItem::ConnectionError { connection_id: "c1".into(), error: "boom".into() },
            ExplorerItem::Note { connection_id: "c1".into(), key: "ui.explorer.noObjects" },
            ExplorerItem::NoMatch,
        ];
        let rows = focus_rows(&items);
        assert_eq!(rows, vec![ExplorerRow::Group("g1".into()), ExplorerRow::Connection("c1".into())]);
        assert_eq!(items[0].id(), "group:g1");
        assert_eq!(items[1].id(), "conn:c1");
    }

    #[test]
    fn heights_follow_density_metrics() {
        let compact = Metrics::compact();
        let relaxed = Metrics::relaxed();
        let group = ExplorerItem::Group { id: "g".into(), collapsed: false, members: 0 };
        let connection = ExplorerItem::Connection { id: "c".into() };
        assert_eq!(group.height(&compact), compact.tree_row_h);
        assert_eq!(connection.height(&compact), compact.row_h);
        assert!(connection.height(&relaxed) > connection.height(&compact));
    }
}
