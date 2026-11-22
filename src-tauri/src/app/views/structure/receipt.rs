//! 结构页、管理页、对象编辑器与危险操作层共用的逐步回执。
//!
//! 一处判定“这次执行算不算完成”，一处渲染逐步状态（label、SQL、状态、原始 message）。
//! 只有被拦下为空、至少一条语句、且每一条都 Ok 才算完成；其余结局一律逐条展示，
//! 调用方不再无条件写“完成”，也不再自己拼一套失败判定。

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _, TestSupportExt as _, div,
    px,
};

use crate::ddl::english::localize;
use crate::ddl::{Receipt, StepStatus};
use crate::i18n::Locale;
use crate::ui::kit::{self, Palette, Radius, ScrollAreaExt as _, Space, TypeScale};

/// 一次执行的结局。判定只在这里做，四个调用方都按它决定关不关预览、读不读回、留不留草稿。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReceiptOutcome {
    /// 计划阶段就被拦下，一条语句都没发送。
    Blocked,
    /// 没有被拦下，但也没有任何语句（例如没有改动）。
    NothingRun,
    /// 每一条都 Ok：改动已经在服务器上生效。
    Done,
    /// 有回执但没全部成功：Failed / Skipped / Unknown / RolledBack 任一出现就算部分成功。
    Partial,
}

impl ReceiptOutcome {
    pub(crate) fn of(receipt: &Receipt) -> Self {
        if !receipt.blocked.is_empty() {
            return Self::Blocked;
        }
        if receipt.steps.is_empty() {
            return Self::NothingRun;
        }
        if receipt.steps.iter().all(|step| step.status == StepStatus::Ok) {
            Self::Done
        } else {
            Self::Partial
        }
    }

    /// 唯一的成功：全部语句都执行成功。成功才允许收起预览、显示统一的“已执行”。
    pub(crate) fn is_success(self) -> bool {
        self == Self::Done
    }

    /// 结局自带的提示键。被拦下与没有语句没有专属文案：原始原因直接写进提示。
    pub(crate) fn note_key(self) -> Option<&'static str> {
        match self {
            Self::Done => Some("structure.preview.done"),
            // 同一句“部分完成：下面的逐步回执里标出了没有成功的语句，表单还是你提交的那份”。
            Self::Partial => Some("objectEditor.partial"),
            Self::Blocked | Self::NothingRun => None,
        }
    }

    /// 执行后要不要读回目标：成功与部分成功都可能已经改了服务器上的东西，必须重新读。
    pub(crate) fn rereads_target(self) -> bool {
        matches!(self, Self::Done | Self::Partial)
    }

    /// 用户填的草稿留不留：只有全部成功才收起表单；其余结局都留着让他改了再试。
    pub(crate) fn keeps_draft(self) -> bool {
        !self.is_success()
    }
}

/// 一条逐步回执。label 与 sql 来自预览计划，message 是数据库或执行层给的原文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReceiptRow {
    pub label: String,
    pub sql: String,
    pub status: StepStatus,
    pub message: String,
}

/// 一步的执行状态：固定技术标识，不跟着界面语言变，免得和服务端原文混在一起看不懂。
pub(crate) fn step_text(status: StepStatus, _locale: Locale) -> &'static str {
    match status {
        StepStatus::Ok => "ok",
        StepStatus::Failed => "failed",
        StepStatus::Skipped => "skipped",
        StepStatus::Unknown => "unknown",
        StepStatus::RolledBack => "rolled back",
    }
}

pub(crate) fn rows(receipt: &Receipt) -> Vec<ReceiptRow> {
    receipt
        .steps
        .iter()
        .map(|step| ReceiptRow {
            label: step.label.clone(),
            sql: step.sql.clone(),
            status: step.status,
            message: step.message.clone(),
        })
        .collect()
}

/// 要原样告诉用户的原因：被拦下的原因，或者没成功的每一步的原始 message（没有 message 用 SQL）。
/// 全部成功时为空。
pub(crate) fn problems(receipt: &Receipt) -> Vec<String> {
    if !receipt.blocked.is_empty() {
        return receipt.blocked.clone();
    }
    if receipt.steps.is_empty() {
        return vec!["没有执行任何语句".to_owned()];
    }
    receipt
        .steps
        .iter()
        .filter(|step| step.status != StepStatus::Ok)
        .map(|step| if step.message.is_empty() { step.sql.clone() } else { step.message.clone() })
        .collect()
}

/// 逐步回执面板：被拦下的原因 + 每一步的 label、SQL、状态与原始 message。
///
/// id_prefix 决定测试与无障碍用的元素 id（{prefix}-receipt、{prefix}-step-{label}），
/// 各页面沿用自己原来的前缀，行为不变。
pub(crate) fn panel(id_prefix: &str, receipt: &Receipt, p: &Palette, locale: Locale) -> AnyElement {
    panel_rows(id_prefix, &receipt.blocked, &rows(receipt), p, locale)
}

/// 已经取好原因与逐步行时的同一套渲染：危险操作层从自己的回执读出这些行再交给它，
/// 渲染代码仍然只有这一份。
pub(crate) fn panel_rows(
    id_prefix: &str,
    blocked: &[String],
    rows: &[ReceiptRow],
    p: &Palette,
    locale: Locale,
) -> AnyElement {
    let mut panel = div()
        .id(SharedString::from(format!("{id_prefix}-receipt")))
        .test_support()
        .flex()
        .flex_col()
        .gap(px(Space::XS));
    if !blocked.is_empty() {
        let blocked: Vec<String> = blocked.iter().map(|text| localize(text, locale)).collect();
        panel = panel.child(
            div()
                .id(SharedString::from(format!("{id_prefix}-receipt-blocked")))
                .test_support()
                .p(px(Space::SM))
                .rounded(px(Radius::PANEL))
                .bg(p.danger_soft)
                .text_color(p.danger)
                .text_size(px(TypeScale::SM))
                .child(SharedString::from(blocked.join(" "))),
        );
    }
    for row in rows {
        let ok = row.status == StepStatus::Ok;
        let (fg, bg, dot) = if ok {
            (p.text_muted, p.sunken, p.ok)
        } else {
            (p.danger, p.danger_soft, p.danger)
        };
        let head = format!("{} · {}", localize(&row.label, locale), step_text(row.status, locale));
        panel = panel.child(
            div()
                .id(SharedString::from(format!("{id_prefix}-step-{}", row.label)))
                .test_support()
                .flex()
                .flex_col()
                .gap(px(Space::XXS))
                .px(px(Space::SM))
                .py(px(Space::XS))
                .rounded(px(Radius::PANEL))
                .bg(bg)
                .when(ok, |this| this.border_1().border_color(p.line_soft))
                .text_color(fg)
                .text_size(px(TypeScale::SM))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(Space::XS))
                        .child(kit::status_dot(dot))
                        .child(SharedString::from(head)),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("{id_prefix}-step-{}-sql", row.label)))
                        .max_h(px(Space::XXL * 6.))
                        .scroll_area_y()
                        .font_family(SharedString::from("monospace"))
                        .text_size(px(TypeScale::XS))
                        .text_color(p.text_faint)
                        .child(SharedString::from(row.sql.clone())),
                )
                .when(!row.message.is_empty(), |this| {
                    this.child(
                        div()
                            .id(SharedString::from(format!("{id_prefix}-step-{}-message", row.label)))
                            .child(SharedString::from(localize(&row.message, locale))),
                    )
                }),
        );
    }
    panel.into_any_element()
}
