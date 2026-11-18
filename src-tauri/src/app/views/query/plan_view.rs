//! 执行计划视图：顶部摘要（代价、预估行数、问题数）、问题与建议、缩进计划树（代价占比条）
//! 和选中节点的属性详情。数据来自 `query::plan`，与引擎无关；“原始结果”切回网格看引擎原文。

use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{div, px, relative, AnyElement, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _};

use crate::i18n::{self, Locale};
use crate::query::plan::{OpKind, Plan};
use crate::ui::kit::{self, Metrics, Palette, Radius, TypeScale};

use super::QueryPane;

pub struct PlanView<'a> {
    pub plan: &'a Plan,
    pub selected: usize,
    pub raw: bool,
    pub locale: Locale,
    pub metrics: Metrics,
    pub mono: SharedString,
    pub narrow: bool,
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

fn click(entity: &Entity<QueryPane>, id: String) -> impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static {
    let entity = entity.clone();
    move |_, _, cx| entity.update(cx, |pane, cx| pane.command(&id, cx))
}

impl PlanView<'_> {
    /// 计划视图的顶部条：标题与摘要，右侧“计划 / 原始结果”切换。原始结果模式下网格由调用方画在下面。
    pub fn header(&self, p: &Palette, entity: &Entity<QueryPane>) -> AnyElement {
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let findings = self.plan.findings();
        let chip = |label: String, fg, bg| kit::pill(SharedString::from(label), fg, bg);
        let mut summary = div().flex().flex_wrap().items_center().gap(px(6.)).min_w(px(0.));
        if let Some(cost) = self.plan.total_cost() {
            summary = summary.child(chip(format!("{} {}", i18n::text(locale, "query.plan.cost"), cost_text(cost)), p.text_muted, p.hover));
        }
        if let Some(rows) = self.plan.result_rows() {
            summary = summary.child(chip(i18n::fill(&i18n::text(locale, "query.plan.estRows"), &[("rows", &rows_text(rows))]), p.text_muted, p.hover));
        }
        summary = summary.child(if findings.is_empty() {
            chip(i18n::text(locale, "query.plan.noIssues"), p.ok, p.ok_soft)
        } else {
            chip(i18n::fill(&i18n::text(locale, "query.plan.issues"), &[("count", &findings.len().to_string())]), p.warn, p.warn_soft)
        });
        let toggle = kit::segmented_sized(
            vec![
                (SharedString::from("plan-view-tree"), t("query.plan.viewTree"), !self.raw, kit::click(click(entity, "plan-view:tree".into()))),
                (SharedString::from("plan-view-raw"), t("query.plan.viewRaw"), self.raw, kit::click(click(entity, "plan-view:raw".into()))),
            ],
            p,
            self.metrics.control_h - 6.,
        );
        div()
            .id("query-plan-head")
            .flex_none()
            .min_h(px(self.metrics.bar_h()))
            .px(px(10.))
            .py(px(3.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(10.))
            .border_b_1()
            .border_color(p.line)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
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
    pub fn body(&self, p: &Palette, entity: &Entity<QueryPane>) -> AnyElement {
        let locale = self.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let lines = self.plan.lines();
        let selected = self.selected.min(lines.len().saturating_sub(1));
        let max_cost = self.plan.max_cost().filter(|cost| *cost > 0.);
        // 只有一个节点时代价占比永远是 100%，那根条没有信息量，不画。
        let show_share = max_cost.is_some() && lines.len() > 1;

        let findings = self.plan.findings();
        let advice = (!findings.is_empty()).then(|| {
            let mut list = div()
                .id("query-plan-findings")
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(px(8.))
                .py(px(6.))
                .bg(p.warn_soft)
                .border_b_1()
                .border_color(p.line);
            for (at, (issue, object)) in findings.iter().enumerate() {
                let place = object.clone().unwrap_or_default();
                let target = lines
                    .iter()
                    .position(|line| line.node.issues.contains(issue) && line.node.object == *object)
                    .unwrap_or(0);
                list = list.child(
                    kit::interactive_states(div().id(SharedString::from(format!("query-plan-finding-{at}"))), p, false)
                        .test_support()
                        .px(px(6.))
                        .py(px(4.))
                        .rounded(px(Radius::CONTROL))
                        .flex()
                        .items_start()
                        .gap(px(8.))
                        .text_size(px(TypeScale::SM))
                        .hover(|style| style.bg(p.warn.opacity(0.08)))
                        .child(div().pt(px(2.)).child(kit::icon(IconName::Lightbulb, 13., p.warn)))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_wrap()
                                .gap_x(px(6.))
                                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(p.warn).child(t(issue.label_key())))
                                .when(!place.is_empty(), |this| this.child(div().font_family(self.mono.clone()).text_color(p.text).child(SharedString::from(place.clone()))))
                                .child(div().text_color(p.text_muted).child(SharedString::from(i18n::fill(&i18n::text(locale, issue.hint_key()), &[("object", &place)])))),
                        )
                        .on_click(click(entity, format!("plan-select:{target}"))),
                );
            }
            list
        });

        let mut tree = div().id("query-plan-tree").flex_1().min_w(px(0.)).h_full().overflow_y_scroll().flex().flex_col();
        tree = tree.child(
            div()
                .flex_none()
                .h(px(self.metrics.row_h))
                .px(px(12.))
                .flex()
                .items_center()
                .gap(px(10.))
                .border_b_1()
                .border_color(p.line_soft)
                .bg(p.chrome)
                .text_size(px(TypeScale::XS))
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.text_muted)
                .child(div().flex_1().child(t("query.plan.colOperation")))
                .when(show_share, |this| this.child(div().w(px(150.)).flex_none().child(t("query.plan.colShare"))))
                .when(max_cost.is_some(), |this| this.child(div().w(px(72.)).flex_none().flex().justify_end().child(t("query.plan.cost"))))
                .child(div().w(px(80.)).flex_none().flex().justify_end().child(t("query.plan.colRows"))),
        );
        for (index, line) in lines.iter().enumerate() {
            let node = line.node;
            let warn = !node.issues.is_empty();
            let (icon, tone) = match node.kind() {
                OpKind::Scan => (IconName::Rows3, if warn { p.warn } else { p.text_muted }),
                OpKind::Seek => (IconName::Search, p.ok),
                OpKind::Join => (IconName::Merge, p.info),
                OpKind::Sort => (IconName::ArrowDownWideNarrow, if warn { p.warn } else { p.text_muted }),
                OpKind::Aggregate => (IconName::Sigma, p.info),
                OpKind::Other => (IconName::Circle, p.text_faint),
            };
            let share = match (node.cost, max_cost) {
                (Some(cost), Some(max)) => (cost / max).clamp(0., 1.) as f32,
                _ => 0.,
            };
            let is_selected = index == selected;
            let indent = 12. + line.depth as f32 * 18.;
            let mut row = kit::row_states(div().id(SharedString::from(format!("query-plan-node-{index}"))), p)
                .flex_none()
                .min_h(px(self.metrics.row_h + 4.))
                .py(px(4.))
                .pl(px(indent))
                .pr(px(12.))
                .flex()
                .items_center()
                .gap(px(10.))
                .border_b_1()
                .border_color(p.line_soft)
                .when(is_selected, |this| this.bg(p.selection))
                .when(!is_selected, |this| this.hover(|style| style.bg(p.hover)))
                .on_click(click(entity, format!("plan-select:{index}")));
            if line.depth > 0 {
                row = row.relative().child(div().absolute().top_0().bottom_0().left(px(indent - 12.)).w(px(1.)).bg(p.line));
            }
            let mut head = div()
                .flex()
                .items_center()
                .gap(px(6.))
                .min_w(px(0.))
                .child(kit::icon(icon, 14., tone))
                .child(div().flex_none().text_size(px(TypeScale::SM)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(SharedString::from(node.op.clone())));
            if let Some(object) = &node.object {
                head = head.child(div().min_w(px(0.)).truncate().font_family(self.mono.clone()).text_size(px(TypeScale::SM)).text_color(p.accent_text).child(SharedString::from(object.clone())));
            }
            for issue in &node.issues {
                head = head.child(kit::pill(SharedString::from(i18n::text(locale, issue.label_key())), p.warn, p.warn_soft));
            }
            // 条件、用到的索引等补充说明放第二行，不再和操作名挤在一行里被截掉。
            let label = div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(head)
                .when(!node.detail.is_empty(), |this| {
                    this.child(
                        div()
                            .pl(px(20.))
                            .min_w(px(0.))
                            .truncate()
                            .font_family(self.mono.clone())
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text_faint)
                            .child(SharedString::from(node.detail.clone())),
                    )
                });
            row = row.child(label);
            if show_share {
                let percent = (share * 100.).round() as i64;
                row = row.child(
                    div()
                        .w(px(150.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex_1()
                                .h(px(6.))
                                .rounded(px(Radius::TAG))
                                .bg(p.hover)
                                .overflow_hidden()
                                .child(div().h_full().w(relative(share.max(0.02))).rounded(px(Radius::TAG)).bg(if share > 0.6 { p.warn } else { p.accent })),
                        )
                        .child(div().w(px(34.)).flex_none().flex().justify_end().text_size(px(TypeScale::XS)).font_features(kit::tabular_figures()).text_color(p.text_muted).child(SharedString::from(format!("{percent}%")))),
                );
            }
            if max_cost.is_some() {
                row = row.child(div().w(px(72.)).flex_none().flex().justify_end().text_size(px(TypeScale::XS)).font_features(kit::tabular_figures()).text_color(p.text_muted).child(SharedString::from(node.cost.map(cost_text).unwrap_or_else(|| "—".into()))));
            }
            row = row.child(div().w(px(80.)).flex_none().flex().justify_end().text_size(px(TypeScale::XS)).font_features(kit::tabular_figures()).text_color(p.text_muted).child(SharedString::from(node.rows.map(rows_text).unwrap_or_else(|| "—".into()))));
            tree = tree.child(row);
        }

        // 详情：先给关键数字，再列引擎原始属性；问题只用标签点出，完整建议在顶部建议条里。
        let node = lines.get(selected).map(|line| line.node);
        let mut detail = div()
            .id("query-plan-detail")
            .flex_none()
            .overflow_y_scroll()
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .bg(p.chrome)
            .when(self.narrow, |this| this.w_full().max_h(px(220.)).border_t_1())
            .when(!self.narrow, |this| this.w(px(300.)).h_full().border_l_1())
            .border_color(p.line);
        if let Some(node) = node {
            let metric = |label: SharedString, value: String| {
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(Radius::CONTROL))
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.line_soft)
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(label))
                    .child(div().text_size(px(TypeScale::MD)).font_weight(FontWeight::SEMIBOLD).font_features(kit::tabular_figures()).text_color(p.text).child(SharedString::from(value)))
            };
            let total = self.plan.total_cost().filter(|total| *total > 0.);
            let share = match (node.cost, total) {
                (Some(cost), Some(total)) if lines.len() > 1 => Some(format!("{}%", ((cost / total) * 100.).round().clamp(0., 100.) as i64)),
                _ => None,
            };
            detail = detail
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(div().text_size(px(TypeScale::MD)).font_weight(FontWeight::SEMIBOLD).child(SharedString::from(node.op.clone())))
                        .when_some(node.object.clone(), |this, object| this.child(div().font_family(self.mono.clone()).text_size(px(TypeScale::SM)).text_color(p.accent_text).child(SharedString::from(object))))
                        .when(!node.issues.is_empty(), |this| {
                            let mut pills = div().flex().flex_wrap().gap(px(4.));
                            for issue in &node.issues {
                                pills = pills.child(kit::pill(SharedString::from(i18n::text(locale, issue.label_key())), p.warn, p.warn_soft));
                            }
                            this.child(pills)
                        }),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .when_some(node.cost, |this, cost| this.child(metric(t("query.plan.cost"), cost_text(cost))))
                        .when_some(share, |this, share| this.child(metric(t("query.plan.colShare"), share)))
                        .when_some(node.rows, |this, rows| this.child(metric(t("query.plan.colRows"), rows_text(rows)))),
                )
                .when(!node.detail.is_empty(), |this| {
                    this.child(
                        div()
                            .p(px(8.))
                            .rounded(px(Radius::CONTROL))
                            .bg(p.sunken)
                            .font_family(self.mono.clone())
                            .text_size(px(TypeScale::XS))
                            .text_color(p.text)
                            .child(SharedString::from(node.detail.clone())),
                    )
                });
            if !node.props.is_empty() {
                let mut props = div().flex().flex_col().gap(px(6.)).pt(px(8.)).border_t_1().border_color(p.line_soft).child(
                    div().text_size(px(TypeScale::XS)).font_weight(FontWeight::SEMIBOLD).text_color(p.text_muted).child(t("query.plan.rawProps")),
                );
                for (key, value) in &node.props {
                    props = props.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .child(div().text_size(px(TypeScale::XS)).text_color(p.text_faint).child(SharedString::from(key.clone())))
                            .child(div().font_family(self.mono.clone()).text_size(px(TypeScale::XS)).text_color(p.text).child(SharedString::from(value.clone()))),
                    );
                }
                detail = detail.child(props);
            }
        }
        let main = div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .when(self.narrow, |this| this.flex_col())
            .child(tree)
            .child(detail);
        div().id("query-plan").flex_1().min_h(px(0.)).flex().flex_col().children(advice).child(main).into_any_element()
    }
}
