//! 结果区的“消息”页：最近一次执行里每条语句的状态、行数/影响行数、耗时和错误，按执行顺序排列。
//! 点成功的一条切到它的结果；点失败的一条跳到编辑器里的错误位置。

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Entity, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, div, px,
};

use crate::i18n::{self, Locale};
use crate::query::desk::LogEntry;
use crate::ui::kit::{self, Metrics, Palette, TypeScale};

use super::{elapsed_label, QueryPane};

/// 一条记录的状态文案。
pub fn status_text(locale: Locale, entry: &LogEntry) -> String {
    let key = match entry.status.as_str() {
        "ok" => "query.log.ok",
        "error" | "failed" => "query.log.error",
        "skipped" => "query.log.skipped",
        "cancelled" => "query.status.cancelled",
        "running" => "query.status.running",
        _ => "query.log.unknown",
    };
    i18n::text(locale, key)
}

/// 一条记录在消息页里的完整说明：状态、第几条、行数或影响行数、耗时和错误。
/// 也用作这一行的可读名称，测试和读屏按它读出整行。
pub fn entry_text(locale: Locale, entry: &LogEntry) -> String {
    let mut parts = vec![status_text(locale, entry)];
    parts.push(match entry.statement {
        Some(index) => format!("{} {}", i18n::text(locale, "query.results.statement"), index + 1),
        None => i18n::text(locale, "query.log.run"),
    });
    if let Some(rows) = entry.rows {
        parts.push(i18n::fill(&i18n::text(locale, "query.log.rows"), &[("count", &rows.to_string())]));
    } else if let Some(affected) = entry.affected {
        parts.push(i18n::fill(&i18n::text(locale, "query.log.affected"), &[("count", &affected.to_string())]));
    }
    if let Some(ms) = entry.elapsed_ms {
        parts.push(elapsed_label(ms));
    }
    // 没执行到的语句只说状态：执行层写的说明与状态词重复，而且不随界面语言变化。
    if !entry.message.is_empty() && entry.status != "skipped" {
        parts.push(entry.message.clone());
    }
    parts.join(" · ")
}

pub struct LogPanel<'a> {
    pub entries: &'a [LogEntry],
    pub batch: u64,
    pub locale: Locale,
    pub metrics: Metrics,
    pub mono: SharedString,
}

impl LogPanel<'_> {
    pub fn render(self, p: &Palette, entity: &Entity<QueryPane>) -> AnyElement {
        let locale = self.locale;
        let heading = if self.batch == 0 {
            i18n::text(locale, "query.log.tab")
        } else {
            i18n::fill(
                &i18n::text(locale, "query.log.heading"),
                &[("batch", &self.batch.to_string()), ("count", &self.entries.len().to_string())],
            )
        };
        let mut list = div()
            .id("query-log")
            .test_support()
            .role(gpui_kit::Role::List)
            .aria_label(SharedString::from(heading.clone()))
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .bg(p.surface)
            .child(div().flex_none().px(px(12.)).pt(px(8.)).pb(px(4.)).child(kit::eyebrow(SharedString::from(heading), p)));
        if self.entries.is_empty() {
            list = list.child(
                div()
                    .id("query-log-empty")
                    .test_support()
                    .px(px(12.))
                    .py(px(8.))
                    .text_size(px(TypeScale::SM))
                    .text_color(p.text_muted)
                    .child(SharedString::from(i18n::text(locale, "query.log.empty"))),
            );
        }
        for (index, entry) in self.entries.iter().enumerate() {
            let failed = entry.failed();
            let color = match entry.status.as_str() {
                "ok" => p.ok,
                "error" | "failed" => p.danger,
                "cancelled" => p.warn,
                _ => p.text_faint,
            };
            let label = entry_text(locale, entry);
            let clickable = entry.statement.is_some();
            let command = format!("log:{index}");
            let open = entity.clone();
            let counts = entry
                .rows
                .map(|rows| i18n::fill(&i18n::text(locale, "query.log.rows"), &[("count", &rows.to_string())]))
                .or_else(|| entry.affected.map(|affected| i18n::fill(&i18n::text(locale, "query.log.affected"), &[("count", &affected.to_string())])))
                .unwrap_or_default();
            let title = match entry.statement {
                Some(statement) => format!("{} {}", i18n::text(locale, "query.results.statement"), statement + 1),
                None => i18n::text(locale, "query.log.run"),
            };
            let sql = entry.sql.split_whitespace().collect::<Vec<_>>().join(" ");
            list = list.child(
                div()
                    .id(SharedString::from(format!("query-log-{index}")))
                    .test_support()
                    .role(gpui_kit::Role::Button)
                    .aria_label(SharedString::from(label))
                    .flex_none()
                    .min_h(px(self.metrics.row_h))
                    .px(px(12.))
                    .py(px(self.metrics.row_pad_y.min(6.)))
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(p.line)
                    .text_size(px(TypeScale::SM))
                    .when(clickable, |this| {
                        kit::interactive(this, p, false).on_click(move |_, _, cx| {
                            let command = command.clone();
                            open.update(cx, |pane, cx| pane.command(&command, cx));
                        })
                    })
                    .child(div().flex_none().pt(px(5.)).child(kit::status_dot(color)))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(div().flex_none().text_color(p.text).child(SharedString::from(title)))
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("query-log-status-{index}")))
                                            .test_support()
                                            .flex_none()
                                            .text_color(color)
                                            .child(SharedString::from(status_text(locale, entry))),
                                    )
                                    .when(!counts.is_empty(), |this| this.child(div().flex_none().text_color(p.text_muted).child(SharedString::from(counts))))
                                    .when_some(entry.elapsed_ms, |this, ms| this.child(div().flex_none().text_color(p.text_faint).child(SharedString::from(elapsed_label(ms)))))
                                    .child(div().flex_1().min_w(px(0.)).truncate().font_family(self.mono.clone()).text_color(p.text_faint).child(SharedString::from(sql))),
                            )
                            .when(!entry.message.is_empty() && entry.status != "skipped", |this| {
                                this.child(
                                    div()
                                        .text_color(if failed { p.danger } else { p.text_muted })
                                        .child(SharedString::from(entry.message.clone())),
                                )
                            }),
                    ),
            );
        }
        list.into_any_element()
    }
}
