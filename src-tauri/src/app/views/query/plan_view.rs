//! 执行计划视图：顶部摘要（代价、实际耗时、预估/实际行数、问题数）、问题与建议、
//! 缩进计划树（访问类型徽标、覆盖索引与索引名、热力占比条、实际耗时/行数列）和选中节点的属性详情。
//! 数据来自 `query::plan`，与引擎无关；“原始结果”切回网格看引擎原文。
//!
//! 计划树是一个 Tab 停靠点（焦点在树容器上，节点行不进 Tab 顺序）：树持有焦点时 Up/Down/Home/End
//! 移动选中，点节点把焦点交给树。建议、鼠标和键盘都只改 `QueryPane::plan_selected` 这一份选中。

use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{div, px, relative, AnyElement, Entity, FocusHandle, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _};

use crate::i18n::{self, Locale};
use crate::query::plan::{OpKind, Plan, PlanNode};
use crate::ui::kit::{self, Metrics, Palette, Radius, Space, TypeScale};
use crate::ui::kit::ScrollAreaExt as _;

use super::QueryPane;

/// 诊断建议区最大高度：多条建议时内部滚动，不把下方的执行计划树挤出可视区。
const FINDINGS_MAX_H: f32 = 120.;
const TREE_BASE_PAD: f32 = 12.;
const TREE_INDENT_STEP: f32 = 18.;
const TREE_CONNECTOR_OFFSET: f32 = 12.;
const TREE_BRANCH_W: f32 = 8.;
const TREE_LINE_W: f32 = 1.;
const TREE_ROW_EXTRA_H: f32 = 6.;
const DETAIL_SUBLINE_INDENT: f32 = 20.;
const COL_SHARE_W: f32 = 148.;
const COL_TIME_W: f32 = 84.;
const COL_COST_W: f32 = 72.;
const COL_ACTUAL_ROWS_W: f32 = 82.;
const COL_ROWS_W: f32 = 80.;
const SHARE_BAR_H: f32 = 6.;
const SHARE_PCT_W: f32 = 34.;
const DETAIL_W: f32 = 316.;
const DETAIL_NARROW_MAX_H: f32 = 240.;
const TREE_MIN_OP_W: f32 = 280.;
const TREE_MIN_TOTAL_W: f32 = 460.;

/// 计划头的视图切换：画布（节点卡片与连线）、列表（缩进树）、原始（引擎原文网格）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanMode {
    Canvas,
    List,
    Raw,
}

impl PlanMode {
    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "canvas" => Some(Self::Canvas),
            "list" => Some(Self::List),
            "raw" => Some(Self::Raw),
            _ => None,
        }
    }

    /// 用户没选过时的默认：多节点看画布，单节点看列表。
    pub fn resolve(choice: Option<Self>, nodes: usize) -> Self {
        choice.unwrap_or(if nodes > 1 { Self::Canvas } else { Self::List })
    }
}

pub struct PlanView<'a> {
    pub plan: &'a Plan,
    pub selected: usize,
    pub mode: PlanMode,
    pub locale: Locale,
    pub metrics: Metrics,
    pub mono: SharedString,
    pub narrow: bool,
    /// 计划树容器的焦点（唯一 Tab 停靠点）与滚动句柄，由 `QueryPane` 跨帧持有。
    pub focus: FocusHandle,
    pub scroll: ScrollHandle,
}

/// 计划树里的导航键：只认不带修饰键的 Up/Down/Home/End，其它键（含 Cmd/Ctrl/Alt/Shift 组合）照常冒泡。
pub fn nav_key(event: &gpui_kit::KeyDownEvent) -> Option<&'static str> {
    let modifiers = &event.keystroke.modifiers;
    if modifiers.control || modifiers.alt || modifiers.shift || modifiers.platform {
        return None;
    }
    match event.keystroke.key.as_str() {
        "up" | "arrowup" => Some("up"),
        "down" | "arrowdown" => Some("down"),
        "home" => Some("home"),
        "end" => Some("end"),
        _ => None,
    }
}

/// 键盘导航后的选中：首尾不越界，空计划没有可选节点。
pub fn step(selected: usize, len: usize, key: &str) -> Option<usize> {
    let last = len.checked_sub(1)?;
    let current = selected.min(last);
    Some(match key {
        "up" => current.saturating_sub(1),
        "down" => (current + 1).min(last),
        "home" => 0,
        "end" => last,
        _ => current,
    })
}

/// 代价显示：大数取整分组，小数保留两位并去掉多余的零。
pub fn cost_text(value: f64) -> String {
    if value >= 1000. {
        crate::app::views::grid::group_digits(value.round() as i64)
    } else {
        let text = format!("{value:.2}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

pub fn rows_text(value: f64) -> String {
    crate::app::views::grid::group_digits(value.round() as i64)
}

/// 实际耗时（毫秒）格式化：小于 1ms 保留三位，其余保留一到两位。
pub fn time_ms_text(ms: f64) -> String {
    if ms >= 1000. {
        format!("{:.2} s", ms / 1000.)
    } else if ms >= 10. {
        let text = format!("{ms:.1}");
        format!("{} ms", text.trim_end_matches('0').trim_end_matches('.'))
    } else if ms >= 1. {
        let text = format!("{ms:.2}");
        format!("{} ms", text.trim_end_matches('0').trim_end_matches('.'))
    } else {
        let text = format!("{ms:.3}");
        format!("{} ms", text.trim_end_matches('0').trim_end_matches('.'))
    }
}

pub fn percent_text(value: f64) -> String {
    let text = format!("{value:.1}");
    format!("{}%", text.trim_end_matches('0').trim_end_matches('.'))
}

/// 节点代价占总代价的比例（0.0..=1.0）。树列与详情面板统一用同一口径；总代价非正数时安全返回 None。
pub fn cost_share(node_cost: Option<f64>, total_cost: Option<f64>) -> Option<f32> {
    let cost = node_cost?;
    let total = total_cost.filter(|total| *total > 0.)?;
    Some((cost / total).clamp(0., 1.) as f32)
}

/// 访问类型语义配色：点查/主键绿色、范围/索引扫描蓝色、全表扫描红色。
pub(super) fn access_type_colors(access: &str, p: &Palette) -> (Hsla, Hsla) {
    match access.to_ascii_lowercase().as_str() {
        "const" | "system" | "eq_ref" | "ref" => (p.ok, p.ok_soft),
        "range" | "index" | "ref_or_null" | "index_merge" | "fulltext" | "unique_subquery" | "index_subquery" => (p.info, p.accent_soft),
        "all" => (p.danger, p.danger_soft),
        _ => (p.text_muted, p.badge),
    }
}

/// 热力占比条配色：>60% 危险红、>30% 告警橙、其余主色蓝。
pub(super) fn heat_bar_color(share: f32, p: &Palette) -> Hsla {
    if share > 0.6 {
        p.danger
    } else if share > 0.3 {
        p.warn
    } else {
        p.accent
    }
}

/// 节点的可读名称（操作 + 对象）：树行与详情面板的无障碍名称用同一份，详情跟着选中节点走。
pub(super) fn node_label(lines: &[crate::query::plan::PlanLine<'_>], index: usize) -> String {
    lines
        .get(index)
        .map(|line| match &line.node.object {
            Some(object) => format!("{} {object}", line.node.op),
            None => line.node.op.clone(),
        })
        .unwrap_or_default()
}

/// 去掉 detail 里已经由 key_used 徽标展示的 `key xxx` 前缀，避免同一行重复显示两次索引名。
fn trimmed_detail(node: &PlanNode) -> String {
    let detail = node.detail.trim();
    if detail.is_empty() {
        return String::new();
    }
    if let Some(key) = &node.key_used {
        let prefix = format!("key {key}");
        if detail == prefix {
            return String::new();
        }
        if let Some(rest) = detail.strip_prefix(&format!("{prefix} · ")) {
            return rest.trim().to_owned();
        }
    }
    detail.to_owned()
}

fn click(entity: &Entity<QueryPane>, id: String) -> impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static {
    let entity = entity.clone();
    move |_, _, cx| entity.update(cx, |pane, cx| pane.command(&id, cx))
}

impl PlanView<'_> {
    /// 计划视图的顶部条：标题与指标摘要胶囊，右侧“计划 / 原始结果”切换。原始结果模式下网格由调用方画在下面。
    pub fn header(&self, p: &Palette, entity: &Entity<QueryPane>) -> AnyElement {
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let findings = self.plan.findings();
        let chip = |label: String, fg, bg| kit::pill(SharedString::from(label), fg, bg);
        let mut summary = div().flex().flex_wrap().items_center().gap(px(Space::SM)).min_w_0();
        if let Some(cost) = self.plan.total_cost() {
            summary = summary.child(chip(format!("{} {}", i18n::text(locale, "query.plan.cost"), cost_text(cost)), p.text_muted, p.badge));
        }
        if let Some(time_ms) = self.plan.total_actual_time_ms() {
            summary = summary.child(chip(format!("{} {}", i18n::text(locale, "query.plan.actualTime"), time_ms_text(time_ms)), p.info, p.accent_soft));
        }
        if let Some(rows) = self.plan.result_rows() {
            summary = summary.child(chip(i18n::fill(&i18n::text(locale, "query.plan.estRows"), &[("rows", &rows_text(rows))]), p.text_muted, p.badge));
        }
        if let Some(actual_rows) = self.plan.result_actual_rows() {
            summary = summary.child(chip(i18n::fill(&i18n::text(locale, "query.plan.actualRowsSummary"), &[("rows", &rows_text(actual_rows))]), p.accent_text, p.accent_soft));
        }
        summary = summary.child(if findings.is_empty() {
            chip(i18n::text(locale, "query.plan.noIssues"), p.ok, p.ok_soft)
        } else {
            chip(i18n::fill(&i18n::text(locale, "query.plan.issues"), &[("count", &findings.len().to_string())]), p.warn, p.warn_soft)
        });
        let segment = |mode: PlanMode, id: &str, key: &str| {
            (SharedString::from(format!("plan-view-{id}")), t(key), self.mode == mode, kit::click(click(entity, format!("plan-view:{id}"))))
        };
        let toggle = kit::segmented_sized(
            vec![
                segment(PlanMode::Canvas, "canvas", "query.plan.viewCanvas"),
                segment(PlanMode::List, "list", "query.plan.viewList"),
                segment(PlanMode::Raw, "raw", "query.plan.viewRaw"),
            ],
            p,
            self.metrics.control_h - Space::SM,
        );
        div()
            .id("query-plan-head")
            .flex_none()
            .min_h(px(self.metrics.bar_h()))
            .px(px(Space::LG))
            .py(px(Space::XS))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(Space::MD))
            .bg(p.surface)
            .border_b_1()
            .border_color(p.line_soft)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(Space::SM))
                    .child(kit::icon(IconName::Route, 14., p.accent_text))
                    .child(div().text_size(px(TypeScale::SM)).font_weight(FontWeight::SEMIBOLD).child(t("query.plan.title"))),
            )
            .child(summary)
            .child(div().flex_1())
            .child(toggle)
            .into_any_element()
    }

    /// 计划主体：问题与建议、计划树、详情面板。
    ///
    /// 同一条建议只完整说一遍（顶部建议条）；树行与详情只用标签点出“这里有问题”，点建议直接跳到对应节点。
    ///
    /// 画布模式由调用方传入画布元素，占据列表树的位置；建议条与详情面板两种模式共用。
    pub fn body(&self, p: &Palette, entity: &Entity<QueryPane>, canvas: Option<AnyElement>) -> AnyElement {
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let lines = self.plan.lines();
        let selected = self.selected.min(lines.len().saturating_sub(1));
        let total_cost = self.plan.total_cost().filter(|cost| *cost > 0.);
        let total_time = self.plan.total_actual_time_ms().filter(|time| *time > 0.);
        let has_cost = self.plan.max_cost().filter(|cost| *cost > 0.).is_some();
        let has_actual_time = self.plan.max_actual_time_ms().is_some();
        let has_actual_rows = lines.iter().any(|line| line.node.actual_rows.is_some());
        // 优先用实际耗时算热力占比（EXPLAIN ANALYZE / MariaDB ANALYZE），否则用预估代价。
        let use_time_share = total_time.is_some();
        let show_share = (use_time_share || total_cost.is_some()) && lines.len() > 1;
        let max_depth = lines.iter().map(|line| line.depth).max().unwrap_or(0);
        let min_tree_w = (max_depth as f32 * TREE_INDENT_STEP
            + TREE_MIN_OP_W
            + if show_share { COL_SHARE_W + Space::MD } else { 0. }
            + if has_actual_time { COL_TIME_W + Space::MD } else { 0. }
            + if has_cost { COL_COST_W + Space::MD } else { 0. }
            + if has_actual_rows { COL_ACTUAL_ROWS_W + Space::MD } else { 0. }
            + COL_ROWS_W)
            .max(TREE_MIN_TOTAL_W);

        let findings = self.plan.findings();
        let advice = (!findings.is_empty()).then(|| {
            let mut list = div()
                .id("query-plan-findings")
                .test_support()
                .flex_none()
                .max_h(px(FINDINGS_MAX_H))
                .scroll_area_y()
                .flex()
                .flex_col()
                .gap(px(Space::XXS))
                .px(px(Space::MD))
                .py(px(Space::SM))
                .bg(p.warn_soft)
                .border_b_1()
                .border_color(p.line_soft);
            for (at, (issue, object)) in findings.iter().enumerate() {
                let place = object.clone().unwrap_or_default();
                let target = lines
                    .iter()
                    .position(|line| line.node.issues.contains(issue) && line.node.object == *object)
                    .unwrap_or(0);
                list = list.child(
                    // 建议行是普通 Tab 停靠点：Enter/Space 与鼠标点击走同一条 plan-select，焦点留在建议上。
                    kit::interactive_states(div().id(SharedString::from(format!("query-plan-finding-{at}"))), p, false)
                        .test_support()
                        .role(gpui_kit::Role::Button)
                        .px(px(Space::SM))
                        .py(px(Space::XS))
                        .rounded(px(Radius::CONTROL))
                        .flex()
                        .items_start()
                        .gap(px(Space::MD))
                        .text_size(px(TypeScale::SM))
                        .hover(|style| style.bg(p.warn.opacity(0.08)))
                        .child(div().pt(px(Space::XXS)).child(kit::icon(IconName::Lightbulb, 13., p.warn)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_wrap()
                                .gap_x(px(Space::SM))
                                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(p.warn).child(t(issue.label_key())))
                                .when(!place.is_empty(), |this| this.child(div().font_family(self.mono.clone()).text_color(p.text).child(SharedString::from(place.clone()))))
                                .child(div().text_color(p.text_muted).child(SharedString::from(i18n::fill(&i18n::text(locale, issue.hint_key()), &[("object", &place)])))),
                        )
                        .on_click(click(entity, format!("plan-select:{target}"))),
                );
            }
            list
        });

        // 滚动区只放列标题和节点行（子元素 0 是列标题，节点 i 是第 i + 1 个），
        // 焦点与描边在外层容器上，描边不挤占滚动区的可视范围。支持纵横双轴滚动以浏览深层缩进。
        let mut tree = div().id("query-plan-tree-scroll").flex_1().min_h_0().scroll_area_xy().scroll_handle(&self.scroll).flex().flex_col();
        let share_col_label = if use_time_share { t("query.plan.colTimeShare") } else { t("query.plan.colShare") };
        tree = tree.child(
            div()
                .flex_none()
                .min_w(px(min_tree_w))
                .h(px(self.metrics.row_h))
                .px(px(Space::LG))
                .flex()
                .items_center()
                .gap(px(Space::MD))
                .border_b_1()
                .border_color(p.line_soft)
                .bg(p.chrome)
                .text_size(px(TypeScale::XS))
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.text_muted)
                .child(div().flex_1().min_w_0().child(t("query.plan.colOperation")))
                .when(show_share, |this| this.child(div().w(px(COL_SHARE_W)).flex_none().child(share_col_label.clone())))
                .when(has_actual_time, |this| this.child(div().w(px(COL_TIME_W)).flex_none().flex().justify_end().child(t("query.plan.actualTime"))))
                .when(has_cost, |this| this.child(div().w(px(COL_COST_W)).flex_none().flex().justify_end().child(t("query.plan.cost"))))
                .when(has_actual_rows, |this| this.child(div().w(px(COL_ACTUAL_ROWS_W)).flex_none().flex().justify_end().child(t("query.plan.actualRows"))))
                .child(div().w(px(COL_ROWS_W)).flex_none().flex().justify_end().child(t("query.plan.colRows"))),
        );

        let list_lines: &[crate::query::plan::PlanLine<'_>] = if canvas.is_some() { &[] } else { &lines };
        for (index, line) in list_lines.iter().enumerate() {
            let node = line.node;
            let warn = !node.issues.is_empty();
            let (icon, tone) = match node.kind() {
                OpKind::Scan => (IconName::Rows3, if warn { p.danger } else { p.text_muted }),
                OpKind::Seek => (IconName::Search, p.ok),
                OpKind::Join => (IconName::Merge, p.info),
                OpKind::Sort => (IconName::ArrowDownWideNarrow, if warn { p.warn } else { p.text_muted }),
                OpKind::Aggregate => (IconName::Sigma, p.info),
                OpKind::Other => (IconName::Circle, p.text_faint),
            };
            let share = if use_time_share {
                cost_share(node.actual_time_ms, total_time).unwrap_or(0.)
            } else {
                cost_share(node.cost, total_cost).unwrap_or(0.)
            };
            let is_selected = index == selected;
            let indent = TREE_BASE_PAD + line.depth as f32 * TREE_INDENT_STEP;
            let focus = self.focus.clone();
            let select = entity.clone();
            let mut row = kit::row_states(div().id(SharedString::from(format!("query-plan-node-{index}"))), p)
                .test_support()
                .role(gpui_kit::Role::TreeItem)
                .aria_label(SharedString::from(node_label(&lines, index)))
                .aria_selected(is_selected)
                .flex_none()
                .min_w(px(min_tree_w))
                .min_h(px(self.metrics.row_h + TREE_ROW_EXTRA_H))
                .py(px(Space::XS))
                .pl(px(indent))
                .pr(px(Space::LG))
                .flex()
                .items_center()
                .gap(px(Space::MD))
                .border_b_1()
                .border_color(p.line_soft)
                .when(is_selected, |this| this.bg(p.selection))
                .when(!is_selected, |this| this.hover(|style| style.bg(p.hover)))
                .on_click(move |_, window, cx| {
                    // 点节点：焦点交给树（之后方向键接着走），选中走与建议、键盘同一条命令。
                    window.focus(&focus, cx);
                    select.update(cx, |pane, cx| pane.command(&format!("plan-select:{index}"), cx));
                });
            if line.depth > 0 {
                row = row
                    .relative()
                    .child(div().absolute().top_0().bottom_0().left(px(indent - TREE_CONNECTOR_OFFSET)).w(px(TREE_LINE_W)).bg(p.line_soft))
                    .child(
                        div()
                            .absolute()
                            .top(relative(0.5))
                            .left(px(indent - TREE_CONNECTOR_OFFSET))
                            .w(px(TREE_BRANCH_W))
                            .h(px(TREE_LINE_W))
                            .bg(p.line_soft),
                    );
            }

            let mut head = div()
                .flex()
                .items_center()
                .gap(px(Space::SM))
                .min_w_0()
                .child(kit::icon(icon, 14., tone))
                .child(div().flex_none().text_size(px(TypeScale::SM)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(SharedString::from(node.op.clone())));
            if let Some(object) = &node.object {
                head = head.child(div().min_w_0().truncate().font_family(self.mono.clone()).text_size(px(TypeScale::SM)).text_color(p.accent_text).child(SharedString::from(object.clone())));
            }
            if let Some(access) = &node.access_type {
                // 若 op 本身与 access_type 不同（例如 TREE 格式 `Table scan` + `ALL`），显示彩色访问类型徽标；
                // 若 op 本身就是 access_type（表格/JSON 的 `eq_ref` / `ALL`），也以彩色徽标强化可读性。
                let (fg, bg) = access_type_colors(access, p);
                head = head.child(kit::pill(SharedString::from(access.clone()), fg, bg));
            }
            if node.covering_index {
                head = head.child(kit::pill(t("query.plan.coveringIndex"), p.ok, p.ok_soft));
            }
            for issue in &node.issues {
                head = head.child(kit::pill(SharedString::from(i18n::text(locale, issue.label_key())), p.warn, p.warn_soft));
            }

            // 第二行：索引徽标、过滤率提示与补充条件。
            let clean_detail = trimmed_detail(node);
            let has_subline = node.key_used.is_some() || node.filtered.is_some() || !clean_detail.is_empty();
            let label = div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(Space::XXS))
                .child(head)
                .when(has_subline, |this| {
                    let mut meta = div().pl(px(DETAIL_SUBLINE_INDENT)).min_w_0().flex().items_center().gap(px(Space::SM));
                    if let Some(key) = &node.key_used {
                        meta = meta.child(
                            div()
                                .flex_none()
                                .px(px(Space::XS))
                                .rounded(px(Radius::TAG))
                                .bg(p.sunken)
                                .font_family(self.mono.clone())
                                .text_size(px(TypeScale::XS))
                                .text_color(p.text_muted)
                                .child(SharedString::from(key.clone())),
                        );
                    }
                    if let Some(filtered) = node.filtered {
                        meta = meta.child(
                            div()
                                .flex_none()
                                .text_size(px(TypeScale::XS))
                                .font_features(kit::tabular_figures())
                                .text_color(p.text_faint)
                                .child(SharedString::from(format!("{} {}", i18n::text(locale, "query.plan.filtered"), percent_text(filtered)))),
                        );
                    }
                    if !clean_detail.is_empty() {
                        meta = meta.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(self.mono.clone())
                                .text_size(px(TypeScale::XS))
                                .text_color(p.text_faint)
                                .child(SharedString::from(clean_detail)),
                        );
                    }
                    this.child(meta)
                });
            row = row.child(label);

            if show_share {
                let percent = (share * 100.).round() as i64;
                let bar_color = heat_bar_color(share, p);
                row = row.child(
                    div()
                        .w(px(COL_SHARE_W))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(Space::SM))
                        .child(
                            div()
                                .flex_1()
                                .h(px(SHARE_BAR_H))
                                .rounded(px(Radius::TAG))
                                .bg(p.sunken)
                                .overflow_hidden()
                                .child(div().h_full().w(relative(share.max(0.02))).rounded(px(Radius::TAG)).bg(bar_color)),
                        )
                        .child(div().w(px(SHARE_PCT_W)).flex_none().flex().justify_end().text_size(px(TypeScale::XS)).font_features(kit::tabular_figures()).text_color(p.text_muted).child(SharedString::from(format!("{percent}%")))),
                );
            }
            if has_actual_time {
                row = row.child(
                    div()
                        .w(px(COL_TIME_W))
                        .flex_none()
                        .flex()
                        .justify_end()
                        .text_size(px(TypeScale::XS))
                        .font_features(kit::tabular_figures())
                        .text_color(if share > 0.6 && use_time_share { p.danger } else { p.text })
                        .child(SharedString::from(node.actual_time_ms.map(time_ms_text).unwrap_or_else(|| "—".into()))),
                );
            }
            if has_cost {
                row = row.child(
                    div()
                        .w(px(COL_COST_W))
                        .flex_none()
                        .flex()
                        .justify_end()
                        .text_size(px(TypeScale::XS))
                        .font_features(kit::tabular_figures())
                        .text_color(p.text_muted)
                        .child(SharedString::from(node.cost.map(cost_text).unwrap_or_else(|| "—".into()))),
                );
            }
            if has_actual_rows {
                row = row.child(
                    div()
                        .w(px(COL_ACTUAL_ROWS_W))
                        .flex_none()
                        .flex()
                        .justify_end()
                        .text_size(px(TypeScale::XS))
                        .font_features(kit::tabular_figures())
                        .text_color(p.text)
                        .child(SharedString::from(node.actual_rows.map(rows_text).unwrap_or_else(|| "—".into()))),
                );
            }
            row = row.child(
                div()
                    .w(px(COL_ROWS_W))
                    .flex_none()
                    .flex()
                    .justify_end()
                    .text_size(px(TypeScale::XS))
                    .font_features(kit::tabular_figures())
                    .text_color(p.text_muted)
                    .child(SharedString::from(node.rows.map(rows_text).unwrap_or_else(|| "—".into()))),
            );
            tree = tree.child(row);
        }

        // 详情面板：节点标题与徽标 -> 2 列指标网格 -> 访问/索引/条件卡片 -> 原始属性清单。
        let node = lines.get(selected).map(|line| line.node);
        let mut detail = div()
            .id("query-plan-detail")
            .test_support()
            .aria_label(SharedString::from(node_label(&lines, selected)))
            .flex_none()
            .scroll_area_y()
            .p(px(Space::LG))
            .flex()
            .flex_col()
            .gap(px(Space::MD))
            .bg(p.chrome)
            .when(self.narrow, |this| this.w_full().max_h(px(DETAIL_NARROW_MAX_H)).border_t_1())
            .when(!self.narrow, |this| this.w(px(DETAIL_W)).h_full().border_l_1())
            .border_color(p.line_soft);
        if let Some(node) = node {
            let metric_card = |label: SharedString, value: String| {
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(Space::MD))
                    .py(px(Space::SM))
                    .rounded(px(Radius::CONTROL))
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.line_soft)
                    .flex()
                    .flex_col()
                    .gap(px(Space::XXS))
                    .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(label))
                    .child(div().text_size(px(TypeScale::MD)).font_weight(FontWeight::SEMIBOLD).font_features(kit::tabular_figures()).text_color(p.text).child(SharedString::from(value)))
            };

            let share = (lines.len() > 1)
                .then(|| if use_time_share { cost_share(node.actual_time_ms, total_time) } else { cost_share(node.cost, total_cost) })
                .flatten()
                .map(|share| format!("{}%", (share * 100.).round() as i64));

            let has_badges = node.access_type.is_some() || node.covering_index || !node.issues.is_empty();
            detail = detail.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(Space::XS))
                    .child(div().text_size(px(TypeScale::MD)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(SharedString::from(node.op.clone())))
                    .when_some(node.object.clone(), |this, object| this.child(div().font_family(self.mono.clone()).text_size(px(TypeScale::SM)).text_color(p.accent_text).child(SharedString::from(object))))
                    .when(has_badges, |this| {
                        let mut pills = div().flex().flex_wrap().gap(px(Space::XS)).pt(px(Space::XXS));
                        if let Some(access) = &node.access_type {
                            let (fg, bg) = access_type_colors(access, p);
                            pills = pills.child(kit::pill(SharedString::from(access.clone()), fg, bg));
                        }
                        if node.covering_index {
                            pills = pills.child(kit::pill(t("query.plan.coveringIndex"), p.ok, p.ok_soft));
                        }
                        for issue in &node.issues {
                            pills = pills.child(kit::pill(SharedString::from(i18n::text(locale, issue.label_key())), p.warn, p.warn_soft));
                        }
                        this.child(pills)
                    }),
            );

            // 2 列指标网格：按优先级收集当前节点具备的数字指标。
            let mut metrics_items: Vec<(SharedString, String)> = Vec::new();
            if let Some(cost) = node.cost {
                metrics_items.push((t("query.plan.cost"), cost_text(cost)));
            }
            if let Some(share_str) = share {
                metrics_items.push((share_col_label, share_str));
            }
            if let Some(time_ms) = node.actual_time_ms {
                metrics_items.push((t("query.plan.actualTime"), time_ms_text(time_ms)));
            }
            if let Some(rows) = node.rows {
                metrics_items.push((t("query.plan.colRows"), rows_text(rows)));
            }
            if let Some(actual_rows) = node.actual_rows {
                metrics_items.push((t("query.plan.actualRows"), rows_text(actual_rows)));
            }
            if let Some(loops) = node.loops {
                metrics_items.push((t("query.plan.loops"), crate::app::views::grid::group_digits(loops as i64)));
            }
            if let Some(filtered) = node.filtered {
                metrics_items.push((t("query.plan.filtered"), percent_text(filtered)));
            }

            if !metrics_items.is_empty() {
                let mut grid = div().flex().flex_col().gap(px(Space::SM));
                for pair in metrics_items.chunks(2) {
                    let mut pair_row = div().flex().gap(px(Space::SM));
                    for (label, value) in pair {
                        pair_row = pair_row.child(metric_card(label.clone(), value.clone()));
                    }
                    if pair.len() == 1 {
                        pair_row = pair_row.child(div().flex_1().min_w_0());
                    }
                    grid = grid.child(pair_row);
                }
                detail = detail.child(grid);
            }

            // 访问 / 索引 / 条件高亮卡片。
            let clean_detail = trimmed_detail(node);
            let show_access_card = node.access_type.is_some() || node.key_used.is_some() || !clean_detail.is_empty();
            if show_access_card {
                let mut card = div()
                    .p(px(Space::MD))
                    .rounded(px(Radius::CONTROL))
                    .bg(p.sunken)
                    .border_1()
                    .border_color(p.line_soft)
                    .flex()
                    .flex_col()
                    .gap(px(Space::SM));
                if let Some(access) = &node.access_type {
                    let (fg, bg) = access_type_colors(access, p);
                    card = card.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(Space::SM))
                            .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(t("query.plan.accessType")))
                            .child(kit::pill(SharedString::from(access.clone()), fg, bg)),
                    );
                }
                if let Some(key) = &node.key_used {
                    card = card.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(Space::SM))
                            .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(t("query.plan.keyUsed")))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(Space::XS))
                                    .min_w_0()
                                    .child(div().truncate().font_family(self.mono.clone()).text_size(px(TypeScale::XS)).text_color(p.text).child(SharedString::from(key.clone())))
                                    .when(node.covering_index, |this| this.child(kit::pill(t("query.plan.coveringIndex"), p.ok, p.ok_soft))),
                            ),
                    );
                }
                if !clean_detail.is_empty() {
                    card = card.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(Space::XXS))
                            .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(t("query.plan.condition")))
                            .child(div().font_family(self.mono.clone()).text_size(px(TypeScale::XS)).text_color(p.text).child(SharedString::from(clean_detail))),
                    );
                }
                detail = detail.child(card);
            }

            if !node.props.is_empty() {
                let mut props = div().flex().flex_col().gap(px(Space::SM)).pt(px(Space::MD)).border_t_1().border_color(p.line_soft).child(
                    div().text_size(px(TypeScale::XS)).font_weight(FontWeight::SEMIBOLD).text_color(p.text_muted).child(t("query.plan.rawProps")),
                );
                for (key, value) in &node.props {
                    props = props.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(Space::XXS))
                            .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(key.clone())))
                            .child(div().font_family(self.mono.clone()).text_size(px(TypeScale::XS)).text_color(p.text).child(SharedString::from(value.clone()))),
                    );
                }
                detail = detail.child(props);
            }
        }
        let ring = p.accent;
        let nav = entity.clone();
        let tree = if let Some(canvas) = canvas {
            canvas
        } else {
            div()
            .id("query-plan-tree")
            .test_support()
            .role(gpui_kit::Role::Tree)
            .aria_label(t("query.plan.title"))
            .track_focus(&self.focus)
            // 焦点只在树容器上：键盘聚焦时容器描边，选中行仍用选中底色。
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .focus_visible(move |style| style.border_color(ring))
            .on_key_down(move |event: &gpui_kit::KeyDownEvent, _, cx| {
                if let Some(key) = nav_key(event) {
                    cx.stop_propagation();
                    nav.update(cx, |pane, cx| pane.command(&format!("plan-nav:{key}"), cx));
                }
            })
            .flex_1()
            .min_w_0()
            .min_h_0()
            .when(!self.narrow, |this| this.h_full())
            .flex()
            .flex_col()
            .child(tree)
            .into_any_element()
        };
        let main = div()
            .flex_1()
            .min_h_0()
            .flex()
            .when(self.narrow, |this| this.flex_col())
            .child(tree)
            .child(detail);
        div().id("query-plan").flex_1().min_h_0().flex().flex_col().children(advice).child(main).into_any_element()
    }
}
