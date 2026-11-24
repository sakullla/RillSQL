//! 执行前风险确认面板：目标（连接 + 库/模式）、原始 SQL，以及每条语句的实际后果。
//!
//! 只读语句不经过这里：面板只列出真正会被拦下的语句，并说明同一批里还有几条直通。
//! 文案按界面语言就地给出（i18n 字典不在本任务范围），consequence 的语义来自 query::risk。

use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, AnyElement, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, TestSupportExt as _,
};

use crate::i18n::Locale;
use crate::query::desk::RiskPrompt;
use crate::query::risk::{Consequence, RiskKind};
use crate::ui::kit::{self, Metrics, Palette, Radius, TypeScale};
use crate::ui::kit::ScrollAreaExt as _;

use super::QueryPane;

fn pick(locale: Locale, zh: &str, en: &str) -> String {
    match locale {
        Locale::ZhCn => zh.to_owned(),
        Locale::En => en.to_owned(),
    }
}

/// 面板标题。
pub fn title(locale: Locale) -> String {
    pick(locale, "执行前确认：这些语句有实际风险", "Confirm before running: these statements have real consequences")
}

/// 后果分类的名字。
pub fn kind_label(locale: Locale, kind: RiskKind) -> String {
    match kind {
        RiskKind::DataLoss => pick(locale, "数据丢失", "Data loss"),
        RiskKind::ObjectReplace => pick(locale, "对象替换", "Object replaced"),
        RiskKind::MassRewrite => pick(locale, "大量重写", "Mass rewrite"),
        RiskKind::ImplicitCommit => pick(locale, "隐式提交", "Implicit commit"),
        RiskKind::ExternalSchedule => pick(locale, "外部调度状态", "External schedule"),
        RiskKind::ServerState => pick(locale, "服务端状态", "Server state"),
    }
}

/// 一项后果的完整说明（分类 + 受影响对象 + 实际影响）。
pub fn consequence_text(locale: Locale, consequence: &Consequence) -> String {
    let object = consequence.object.as_deref().unwrap_or_default();
    let label = kind_label(locale, consequence.kind);
    let detail = match consequence.kind {
        RiskKind::DataLoss => {
            if object.is_empty() {
                pick(locale, "会删除数据或对象，无法撤销。", "Deletes data or the object itself; this cannot be undone.")
            } else {
                pick(
                    locale,
                    "会删除该对象里的数据或对象本身，无法撤销。",
                    "Deletes the rows inside it, or the object itself; this cannot be undone.",
                )
            }
        }
        RiskKind::ObjectReplace => pick(
            locale,
            "会先删除再重建（或按行替换），原有内容被覆盖；中途失败时原对象可能已经不存在。",
            "Drops and recreates it (or replaces rows), so the previous content is gone; if the second step fails the original may already be lost.",
        ),
        RiskKind::MassRewrite => pick(
            locale,
            "会重写大量数据，期间占用服务器，可能持续很久。",
            "Rewrites a lot of data, holds the server while it runs, and can take a long time.",
        ),
        RiskKind::ImplicitCommit => pick(
            locale,
            "该方言的 DDL 会隐式提交当前事务，前面的语句一旦提交就不能整体回滚。",
            "DDL on this engine implicitly commits the current transaction, so earlier statements can no longer be rolled back.",
        ),
        RiskKind::ExternalSchedule => pick(
            locale,
            "会改动服务器上按时间自动执行的任务或调度器开关，影响之后自动运行的作业。",
            "Changes work the server runs on a schedule (events or the scheduler), affecting jobs that run later.",
        ),
        RiskKind::ServerState => pick(
            locale,
            "会改动服务端或连接状态（权限、服务端变量、会话或索引），不只是数据内容。",
            "Changes server or connection state (privileges, server variables, sessions or indexes), not just data.",
        ),
    };
    if object.is_empty() {
        format!("{label}：{detail}")
    } else {
        format!("{label}（{object}）：{detail}")
    }
}

/// 目标：连接、引擎、库/模式与（如果记过）环境。
pub fn target_text(locale: Locale, prompt: &RiskPrompt) -> String {
    let engine = crate::adapters::query::label_named(&prompt.engine);
    let prefix = pick(locale, "目标", "Target");
    let mut parts = vec![prompt.connection_name.clone()];
    if !engine.is_empty() {
        parts.push(engine.to_owned());
    }
    let scope = if prompt.schema.is_empty() {
        prompt.database.clone()
    } else {
        format!("{}.{}", prompt.database, prompt.schema)
    };
    parts.push(scope);
    if !prompt.environment.is_empty() {
        parts.push(prompt.environment.clone());
    }
    format!("{prefix}：{}", parts.join(" · "))
}

/// 这条语句的标题：第几条 / 共几条。
pub fn statement_title(locale: Locale, index: usize, total: usize) -> String {
    pick(
        locale,
        &format!("语句 {}（这批共 {total} 条）", index + 1),
        &format!("Statement {} (of {total} in this batch)", index + 1),
    )
}

/// 同一批里直通执行的语句说明。
pub fn passthrough_text(locale: Locale, passthrough: usize) -> String {
    pick(
        locale,
        &format!("其余 {passthrough} 条语句是只读或没有实际后果，确认后一起执行，不会单独拦截。"),
        &format!("The other {passthrough} statement(s) are read-only or harmless; they run with the batch and are not blocked."),
    )
}

/// 结果区标题旁的说明：几条语句需要确认。
pub fn note(locale: Locale, prompt: &RiskPrompt) -> String {
    pick(
        locale,
        &format!("执行前确认：{} 条语句有实际风险", prompt.statements.len()),
        &format!("Confirm before running: {} statement(s) have real consequences", prompt.statements.len()),
    )
}

/// 确认与取消按钮的文字。
pub fn confirm_label(locale: Locale) -> String {
    pick(locale, "确认执行", "Run anyway")
}

pub fn cancel_label(locale: Locale) -> String {
    pick(locale, "取消", "Cancel")
}

/// 整段说明：目标、每条 SQL 与后果拼成一行，供读屏与测试断言用。
#[allow(dead_code)] // 由视图测试与读屏文案使用，主渲染路径按块排版。
pub fn summary_text(locale: Locale, prompt: &RiskPrompt) -> String {
    let mut lines = vec![title(locale), target_text(locale, prompt)];
    for statement in &prompt.statements {
        lines.push(statement_title(locale, statement.index, prompt.total()));
        lines.push(statement.sql.clone());
        for consequence in &statement.consequences {
            lines.push(consequence_text(locale, consequence));
        }
    }
    if prompt.passthrough > 0 {
        lines.push(passthrough_text(locale, prompt.passthrough));
    }
    lines.extend(prompt.notices.iter().cloned());
    lines.join(" · ")
}

fn click(entity: &Entity<QueryPane>, id: &'static str) -> impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static {
    let entity = entity.clone();
    move |_, _, cx| entity.update(cx, |pane, cx| pane.command(id, cx))
}

/// 单条语句文本在确认框里的最大高度；更长的语句截断，完整文本在编辑器里。
const STATEMENT_MAX_H: f32 = 160.;

/// 目标是不是生产环境：模态框头部用危险色强调。
pub fn production(prompt: &RiskPrompt) -> bool {
    matches!(prompt.environment.to_ascii_lowercase().as_str(), "prod" | "production")
}

/// 工具栏运行按钮在等待确认期间的文字。
pub fn waiting_label(locale: Locale) -> String {
    pick(locale, "等待确认…", "Awaiting confirmation…")
}

/// 取消确认后的提示：明确告诉用户这批语句一条都没有执行。
pub fn cancelled_note(locale: Locale) -> String {
    pick(locale, "已取消：未执行任何语句", "Cancelled: no statements were run")
}

/// 生产环境的强调说明。
pub fn production_text(locale: Locale) -> String {
    pick(locale, "生产环境：确认后会直接作用于线上数据。", "Production: confirming runs these statements against live data.")
}

/// 存储过程调用的说明：CALL / EXEC 仍然需要确认，但说明比“服务端状态”更贴切。
fn procedure_call(sql: &str) -> bool {
    let lead = sql.split_whitespace().next().unwrap_or_default().to_ascii_uppercase();
    matches!(lead.as_str(), "CALL" | "EXEC" | "EXECUTE")
}

/// 一项后果的说明；存储过程调用换成“将执行存储过程，可能修改数据”。
pub fn statement_consequence_text(locale: Locale, sql: &str, consequence: &Consequence) -> String {
    if consequence.kind == RiskKind::ServerState && procedure_call(sql) {
        let label = pick(locale, "存储过程", "Stored procedure");
        let detail = pick(locale, "将执行存储过程，可能修改数据。", "Runs a stored procedure, which may change data.");
        return match consequence.object.as_deref().filter(|object| !object.is_empty()) {
            Some(object) => format!("{label}（{object}）：{detail}"),
            None => format!("{label}：{detail}"),
        };
    }
    consequence_text(locale, consequence)
}

/// 风险确认模态框：头部（标题、目标连接、环境、库）固定；正文逐条列出语句与后果并独立滚动；
/// 底部“确认执行 / 取消”固定可见。确认前一条语句都不发送；只有“取消”按钮与 Esc 会取消。
/// 点遮罩（含被盖住的工具栏）不取消，只把焦点送回确认框并闪一下强调边框（flash）。
#[allow(clippy::too_many_arguments)]
pub fn render(
    prompt: &RiskPrompt,
    locale: Locale,
    p: &Palette,
    metrics: Metrics,
    mono: &SharedString,
    entity: &Entity<QueryPane>,
    focus: &gpui_kit::FocusHandle,
    flash: bool,
) -> AnyElement {
    let total = prompt.total();
    let prod = production(prompt);
    let tone = p.danger;
    let engine = crate::adapters::query::label_named(&prompt.engine);
    let scope = if prompt.schema.is_empty() { prompt.database.clone() } else { format!("{}.{}", prompt.database, prompt.schema) };

    // 头部：标题 + 目标（连接、引擎、环境徽标、库）。
    let mut target = div()
        .id("query-risks-target")
        .test_support()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(kit::Space::SM))
        .text_size(px(TypeScale::SM))
        .text_color(p.text_muted)
        .child(SharedString::from(format!("{}：", pick(locale, "目标", "Target"))))
        .child(div().font_weight(FontWeight::MEDIUM).text_color(p.text).child(SharedString::from(prompt.connection_name.clone())));
    if !engine.is_empty() {
        target = target.child(SharedString::from(engine.to_owned()));
    }
    if !prompt.environment.is_empty() {
        target = target.child(kit::env_tag(&prompt.environment.to_ascii_lowercase(), SharedString::from(prompt.environment.clone()), p));
    }
    if !scope.is_empty() {
        target = target.child(div().font_family(mono.clone()).text_color(p.text).child(SharedString::from(scope)));
    }
    let head = div()
        .id("query-risks-head")
        .test_support()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(kit::Space::SM))
        .px(px(metrics.card_pad()))
        .pt(px(metrics.card_pad()))
        .pb(px(kit::Space::MD))
        .border_b_1()
        .border_color(if prod { p.danger } else { p.line_soft })
        .when(prod, |this| this.bg(p.danger_soft))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(kit::Space::SM))
                .child(kit::icon(gpui_kit::assets::IconName::TriangleAlert, 16., tone))
                .child(div().flex_1().min_w_0().child(kit::title(SharedString::from(title(locale)), TypeScale::LG, p)))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(TypeScale::XS))
                        .text_color(p.text_faint)
                        .child(SharedString::from(format!("{} / {}", prompt.statements.len(), total))),
                ),
        )
        .child(target)
        .when(prod, |this| {
            this.child(
                div()
                    .id("query-risks-production")
                    .test_support()
                    .flex()
                    .items_center()
                    .gap(px(kit::Space::SM))
                    .text_size(px(TypeScale::SM))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.danger)
                    .child(kit::icon(gpui_kit::assets::IconName::CircleAlert, 14., p.danger))
                    .child(SharedString::from(production_text(locale))),
            )
        });

    // 正文：逐条语句与后果，独立滚动。
    let mut body = div()
        .id("query-risks-body")
        .test_support()
        .flex_1()
        .min_h_0()
        .scroll_area_y()
        .px(px(metrics.card_pad()))
        .py(px(kit::Space::MD))
        .flex()
        .flex_col()
        .gap(px(kit::Space::MD));
    for statement in &prompt.statements {
        let mut block = div()
            .flex()
            .flex_col()
            .gap(px(kit::Space::XS))
            .min_w_0()
            .p(px(kit::Space::MD))
            .rounded(px(Radius::CONTROL))
            .bg(p.surface);
        block = block.child(
            div()
                .text_size(px(TypeScale::XS))
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.text)
                .child(SharedString::from(statement_title(locale, statement.index, total))),
        );
        block = block.child(
            div()
                .px(px(kit::Space::MD))
                .py(px(kit::Space::SM))
                .rounded(px(Radius::CONTROL))
                .bg(p.sunken)
                .font_family(mono.clone())
                .text_size(px(TypeScale::SM))
                .text_color(p.text)
                .max_h(px(STATEMENT_MAX_H))
                .overflow_hidden()
                .child(SharedString::from(statement.sql.clone())),
        );
        for consequence in &statement.consequences {
            block = block.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(kit::Space::SM))
                    .text_size(px(TypeScale::XS))
                    .text_color(p.text_muted)
                    .child(div().pt(px(kit::Space::XS)).flex_none().child(kit::status_dot(p.warn)))
                    .child(div().flex_1().min_w_0().child(SharedString::from(statement_consequence_text(locale, &statement.sql, consequence)))),
            );
        }
        body = body.child(block);
    }
    if prompt.passthrough > 0 {
        body = body.child(
            div()
                .text_size(px(TypeScale::XS))
                .text_color(p.text_muted)
                .child(SharedString::from(passthrough_text(locale, prompt.passthrough))),
        );
    }
    for notice in &prompt.notices {
        body = body.child(
            div()
                .text_size(px(TypeScale::XS))
                .text_color(p.text_muted)
                .child(SharedString::from(notice.clone())),
        );
    }

    // 底部按钮固定：语句再多也不会被挤出可见范围。
    let footer = div()
        .id("query-risks-footer")
        .test_support()
        .flex_none()
        .px(px(metrics.card_pad()))
        .py(px(kit::Space::MD))
        .border_t_1()
        .border_color(p.line_soft)
        .child(
            kit::modal_actions()
                .min_h(px(metrics.control_h))
                .child(kit::secondary_button(
                    "query-act-cancel-risks",
                    None,
                    SharedString::from(cancel_label(locale)),
                    click(entity, "cancel-risks"),
                ))
                .child(kit::danger_button(
                    "query-act-confirm-risks",
                    Some(gpui_kit::assets::IconName::Play),
                    SharedString::from(confirm_label(locale)),
                    click(entity, "confirm-risks"),
                )),
        );

    let scrim_entity = entity.clone();
    let key_entity = entity.clone();
    div()
        .id("query-risks-scrim")
        .test_support()
        .absolute()
        .inset_0()
        .occlude()
        .bg(gpui_kit::hsla(0., 0., 0., if p.dark { 0.45 } else { 0.25 }))
        .flex()
        .items_center()
        .justify_center()
        .on_mouse_down(gpui_kit::MouseButton::Left, move |_, _, cx| {
            scrim_entity.update(cx, |pane, cx| pane.command("attend-risks", cx));
        })
        .child(
            kit::modal_surface("query-risks", SharedString::from(title(locale)), kit::ModalWidth::PREVIEW, p)
                .test_support()
                .track_focus(focus)
                .focus_trap("query-risks", focus)
                .when(prod, |this| this.border_2().border_color(p.danger))
                .when(flash, |this| this.border_2().border_color(p.accent))
                .overflow_hidden()
                .on_key_down(move |event: &gpui_kit::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        key_entity.update(cx, |pane, cx| pane.command("cancel-risks", cx));
                    }
                })
                .child(head)
                .child(body)
                .child(footer),
        )
        .into_any_element()
}
