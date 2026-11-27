use super::*;

pub(super) struct BatchControl {
    pub ticket: Ticket,
    pub answers: tokio::sync::mpsc::UnboundedSender<BatchAnswer>,
    pub answered: Option<usize>,
}

impl QueryPane {
    pub(super) fn batch_summary(&self) -> Option<String> {
        if self.desk.status() != "error" {
            return None;
        }
        let mut ok = 0;
        let mut failed = 0;
        let mut skipped = 0;
        for result in self
            .desk
            .results()
            .iter()
            .filter(|view| view.result_set.is_none_or(|(at, _)| at == 0))
        {
            match result.status.as_str() {
                "ok" => ok += 1,
                "error" => failed += 1,
                "skipped" => skipped += 1,
                _ => return None,
            }
        }
        (failed > 0 && ok + failed + skipped > 1).then(|| {
            i18n::fill(
                &i18n::text(self.locale, "query.batch.summary"),
                &[
                    ("ok", &ok.to_string()),
                    ("failed", &failed.to_string()),
                    ("skipped", &skipped.to_string()),
                ],
            )
        })
    }

    pub(super) fn batch_pause(&self) -> Option<(Ticket, usize)> {
        if self.desk.status() != AWAITING_DECISION {
            return None;
        }
        let control = self.batch_controls.get(self.desk.active_id()?)?;
        let index = self.desk.results().iter().rposition(statement_failed)?;
        Some((control.ticket.clone(), index))
    }

    pub(super) fn answer_batch(&mut self, ticket: &Ticket, index: usize, answer: BatchDecision, cx: &mut Context<Self>) {
        if !self.batch_pause().is_some_and(|(current, at)| current == *ticket && at == index) {
            return;
        }
        if let Some(control) = self.batch_controls.get_mut(&ticket.tab_id) {
            if control.answered != Some(index) && control.answers.send((index, answer)).is_ok() {
                control.answered = Some(index);
                cx.notify();
            }
        }
    }

    pub(super) fn render_batch_banner(&self, p: &Palette, entity: &Entity<Self>) -> Option<AnyElement> {
        let (ticket, index) = self.batch_pause()?;
        let total = self.desk.results().len();
        let label = i18n::fill(
            &i18n::text(self.locale, "query.batch.failure"),
            &[
                ("index", &(index + 1).to_string()),
                ("total", &total.to_string()),
                ("left", &(total - index - 1).to_string()),
            ],
        );
        let click = |answer| {
            let entity = entity.clone();
            let ticket = ticket.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |pane, cx| pane.answer_batch(&ticket, index, answer, cx));
            }
        };
        let answered = self.batch_controls[&ticket.tab_id].answered == Some(index);
        let t = |key| i18n::text(self.locale, key);
        Some(
            div()
                .id("query-batch-pause")
                .test_support()
                .aria_label(SharedString::from(label.clone()))
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(Space::SM))
                .px(px(Space::LG))
                .py(px(Space::SM))
                .bg(p.warn_soft)
                .border_b_1()
                .border_color(p.line_soft)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(Space::XS))
                        .text_size(px(TypeScale::SM))
                        .text_color(p.text)
                        .child(kit::icon(IconName::TriangleAlert, 14., p.warn))
                        .child(SharedString::from(label)),
                )
                .child(
                    kit::primary_button(
                        "query-batch-continue",
                        Some(IconName::Play),
                        t("query.batch.continue"),
                        click(BatchDecision::Continue),
                    )
                    .small()
                    .disabled(answered),
                )
                .when(total - index > 2, |row| {
                    row.child(
                        kit::secondary_button(
                            "query-batch-all",
                            None,
                            t("query.batch.continueAll"),
                            click(BatchDecision::ContinueAll),
                        )
                        .small()
                        .disabled(answered)
                        .tooltip(t("query.batch.allHint")),
                    )
                })
                .child(
                    kit::secondary_button("query-batch-stop", None, t("query.batch.stop"), click(BatchDecision::Stop))
                        .small()
                        .disabled(answered),
                )
                .into_any_element(),
        )
    }
}
