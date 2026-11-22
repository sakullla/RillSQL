//! 结果区的“消息”页：最近一次执行里每条语句的状态、行数/影响行数、耗时和错误，按执行顺序排列。
//! 点成功的一条切到它的结果；点失败的一条跳到编辑器里的错误位置。

use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Entity, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, div, px,
};

use crate::i18n::{self, Locale};
use crate::query::desk::LogEntry;
use crate::ui::kit::{self, Metrics, Palette, TypeScale};
use crate::ui::kit::ScrollAreaExt as _;
use crate::ui::kit::Tip as _;

use super::{elapsed_label, rewrite_note, QueryPane};

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
        let count = crate::app::views::grid::group_digits(rows as i64);
        parts.push(i18n::fill(&i18n::text(locale, "query.log.rows"), &[("count", &count)]));
    } else if let Some(affected) = entry.affected {
        let count = crate::app::views::grid::group_digits(affected as i64);
        parts.push(i18n::fill(&i18n::text(locale, "query.log.affected"), &[("count", &count)]));
    }
    if let Some(ms) = entry.elapsed_ms {
        parts.push(elapsed_label(ms));
    }
    // 没执行到的语句只说状态：执行层写的说明与状态词重复，而且不随界面语言变化。
    if !entry.message.is_empty() && entry.status != "skipped" {
        parts.push(entry.message.clone());
    }
    // 失败且被自动改写时，读屏也要听到“语句被执行前改过”。
    if entry.failed() {
        if let Some(rewrite) = entry.rewrite.as_ref() {
            parts.push(rewrite_note(locale, rewrite));
        }
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
            .min_h_0()
            .scroll_area_y()
            .flex()
            .flex_col()
            .bg(p.surface)
            .child(div().flex_none().px(px(kit::Space::LG)).pt(px(kit::Space::MD)).pb(px(kit::Space::XS)).child(kit::eyebrow(SharedString::from(heading), p)));
        if self.entries.is_empty() {
            list = list.child(
                div()
                    .id("query-log-empty")
                    .test_support()
                    .px(px(kit::Space::LG))
                    .py(px(kit::Space::MD))
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
                .map(|rows| {
                    let count = crate::app::views::grid::group_digits(rows as i64);
                    i18n::fill(&i18n::text(locale, "query.log.rows"), &[("count", &count)])
                })
                .or_else(|| {
                    entry.affected.map(|affected| {
                        let count = crate::app::views::grid::group_digits(affected as i64);
                        i18n::fill(&i18n::text(locale, "query.log.affected"), &[("count", &count)])
                    })
                })
                .unwrap_or_default();
            let title = match entry.statement {
                Some(statement) => format!("{} {}", i18n::text(locale, "query.results.statement"), statement + 1),
                None => i18n::text(locale, "query.log.run"),
            };
            let sql = entry.sql.split_whitespace().collect::<Vec<_>>().join(" ");
            let full_sql = entry.sql.trim().to_owned();
            let copy_sql_text = full_sql.clone();
            let copy_msg_text = entry.message.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("query-log-{index}")))
                    .test_support()
                    .role(if clickable { gpui_kit::Role::Button } else { gpui_kit::Role::ListItem })
                    .aria_label(SharedString::from(label))
                    .flex_none()
                    .min_h(px(self.metrics.row_h))
                    .px(px(kit::Space::LG))
                    .py(px(self.metrics.row_pad_y.min(6.)))
                    .flex()
                    .items_start()
                    .gap(px(kit::Space::MD))
                    .border_b_1()
                    .border_color(p.line_soft)
                    .when(failed, |this| this.bg(p.danger_soft.opacity(0.45)))
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
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(kit::Space::XXS))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(kit::Space::MD))
                                    .child(div().flex_none().text_color(p.text).child(SharedString::from(title)))
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("query-log-status-{index}")))
                                            .test_support()
                                            .flex_none()
                                            .text_color(color)
                                            .child(SharedString::from(status_text(locale, entry))),
                                    )
                                    .when(!counts.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .flex_none()
                                                .font_features(kit::tabular_figures())
                                                .text_color(p.text_muted)
                                                .child(SharedString::from(counts)),
                                        )
                                    })
                                    .when_some(entry.elapsed_ms, |this, ms| {
                                        this.child(
                                            div()
                                                .flex_none()
                                                .font_features(kit::tabular_figures())
                                                .text_color(p.text_faint)
                                                .child(SharedString::from(elapsed_label(ms))),
                                        )
                                    })
                                    // 单行截断的 SQL 悬停显示全文（保留原始换行）。
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("query-log-sql-{index}")))
                                            .test_support()
                                            .aria_label(SharedString::from(full_sql.clone()))
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_family(self.mono.clone())
                                            .text_color(p.text_faint)
                                            .when(!full_sql.is_empty(), |this| this.tooltip_text(SharedString::from(full_sql)))
                                            .child(SharedString::from(sql)),
                                    )
                                    .when(!copy_sql_text.is_empty(), |this| {
                                        let text = copy_sql_text.clone();
                                        this.child(
                                            kit::icon_button(
                                                SharedString::from(format!("query-log-copy-sql-{index}")),
                                                IconName::Copy,
                                                SharedString::from(i18n::text(locale, "query.log.copySql")),
                                                move |_, _, cx| {
                                                    cx.stop_propagation();
                                                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.clone()));
                                                },
                                            )
                                            .xsmall(),
                                        )
                                    }),
                            )
                            .when(!entry.message.is_empty() && entry.status != "skipped", |this| {
                                let text = copy_msg_text.clone();
                                this.child(
                                    div()
                                        .flex()
                                        .items_start()
                                        .gap(px(kit::Space::SM))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_color(if failed { p.danger } else { p.text_muted })
                                                .child(SharedString::from(entry.message.clone())),
                                        )
                                        .child(
                                            kit::icon_button(
                                                SharedString::from(format!("query-log-copy-msg-{index}")),
                                                IconName::Copy,
                                                SharedString::from(i18n::text(locale, "query.log.copyMessage")),
                                                move |_, _, cx| {
                                                    cx.stop_propagation();
                                                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.clone()));
                                                },
                                            )
                                            .xsmall(),
                                        ),
                                )
                            })
                            // 失败且被自动改写时才补说明和实际语句：手写 LIMIT 的报错原文不动，
                            // 成功语句也不为一条本来就有的读取上限加噪声。
                            .when_some(
                                failed.then(|| entry.rewrite.clone()).flatten().zip(entry.sent_sql.clone()),
                                |this, (rewrite, sent)| {
                                    let note = rewrite_note(locale, &rewrite);
                                    let sent_copy = sent.clone();
                                    this.child(
                                        div()
                                            .id(SharedString::from(format!("query-log-rewrite-{index}")))
                                            .test_support()
                                            .aria_label(SharedString::from(note.clone()))
                                            .text_color(p.warn)
                                            .child(SharedString::from(note)),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_start()
                                            .gap(px(kit::Space::SM))
                                            .child(
                                                div()
                                                    .id(SharedString::from(format!("query-log-sent-{index}")))
                                                    .test_support()
                                                    .aria_label(SharedString::from(sent.clone()))
                                                    .flex_1()
                                                    .min_w_0()
                                                    .font_family(self.mono.clone())
                                                    .text_color(p.text_muted)
                                                    .child(SharedString::from(sent)),
                                            )
                                            .child(
                                                kit::icon_button(
                                                    SharedString::from(format!("query-log-copy-sent-{index}")),
                                                    IconName::Copy,
                                                    SharedString::from(i18n::text(locale, "query.log.copySql")),
                                                    move |_, _, cx| {
                                                        cx.stop_propagation();
                                                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(sent_copy.clone()));
                                                    },
                                                )
                                                .xsmall(),
                                            ),
                                    )
                                },
                            ),
                    ),
            );
        }
        list.into_any_element()
    }
}
