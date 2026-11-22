//! 执行前风险确认面板：目标（连接 + 库/模式）、原始 SQL，以及每条语句的实际后果。
//!
//! 只读语句不经过这里：面板只列出真正会被拦下的语句，并说明同一批里还有几条直通。
//! 文案按界面语言就地给出（i18n 字典不在本任务范围），consequence 的语义来自 query::risk。

use gpui_kit::{
    div, px, AnyElement, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, TestSupportExt as _,
};

use crate::i18n::Locale;
use crate::query::desk::RiskPrompt;
use crate::query::risk::{Consequence, RiskKind};
use crate::ui::kit::{self, Metrics, Palette, Radius, TypeScale};

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

/// 风险确认面板：确认前一条语句都不发送。
pub fn render(
    prompt: &RiskPrompt,
    locale: Locale,
    p: &Palette,
    metrics: Metrics,
    mono: &SharedString,
    entity: &Entity<QueryPane>,
) -> AnyElement {
    let total = prompt.total();
    let mut body = div().flex().flex_col().gap(px(kit::Space::MD)).min_w_0();
    body = body.child(
        div()
            .flex()
            .items_center()
            .gap(px(kit::Space::SM))
            .text_size(px(TypeScale::SM))
            .font_weight(FontWeight::MEDIUM)
            .text_color(p.danger)
            .child(kit::icon(gpui_kit::assets::IconName::TriangleAlert, 14., p.danger))
            .child(SharedString::from(title(locale)))
            .child(
                div()
                    .text_size(px(TypeScale::XS))
                    .text_color(p.text_faint)
                    .child(SharedString::from(format!("{} / {}", prompt.statements.len(), prompt.total()))),
            ),
    );
    body = body.child(
        div()
            .text_size(px(TypeScale::SM))
            .text_color(p.text_muted)
            .child(SharedString::from(target_text(locale, prompt))),
    );
    for statement in &prompt.statements {
        let mut block = div().flex().flex_col().gap(px(kit::Space::XS)).min_w_0();
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
                .border_1()
                .border_color(p.line)
                .font_family(mono.clone())
                .text_size(px(TypeScale::SM))
                .text_color(p.text)
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
                    .child(kit::status_dot(p.warn))
                    .child(SharedString::from(consequence_text(locale, consequence))),
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
    body = body.child(
        div()
            .flex()
            .items_center()
            .gap(px(kit::Space::MD))
            .min_h(px(metrics.control_h))
            .child(kit::primary_button(
                "query-act-confirm-risks",
                Some(gpui_kit::assets::IconName::Play),
                SharedString::from(confirm_label(locale)),
                click(entity, "confirm-risks"),
            ))
            .child(kit::secondary_button(
                "query-act-cancel-risks",
                None,
                SharedString::from(cancel_label(locale)),
                click(entity, "cancel-risks"),
            )),
    );
    div()
        .id("query-risks")
        .test_support()
        .flex_none()
        .p(px(kit::Space::LG))
        .flex()
        .flex_col()
        .gap(px(kit::Space::MD))
        .bg(p.danger_soft)
        .border_b_1()
        .border_color(p.line)
        .on_key_down({
            let entity = entity.clone();
            move |event: &gpui_kit::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    let entity = entity.clone();
                    entity.update(cx, |pane, cx| pane.command("cancel-risks", cx));
                }
            }
        })
        .child(body)
        .into_any_element()
}
