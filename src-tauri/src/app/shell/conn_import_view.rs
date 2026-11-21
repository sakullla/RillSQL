use super::chrome::act;
use super::conn_import::{
    target_label, ImportField, ImportPreview, ImportReport, ImportResult, ImportState,
};
use super::model::DraftField;
use super::view::Workbench;
use crate::adapters::profile_import::{absolute_path, ProfileRow};
use crate::connections::model::{CaMode, ConnectionDraft, SshAuthKind, TlsMode};
use crate::connections::store::{ImportConflict, ImportConflictKind};
use crate::ui::kit::{self, Palette, Radius, ScrollAreaExt as _, Space, TypeScale};
use gpui_kit::assets::IconName;
use gpui_kit::{
    div, px, AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, Window,
};

const UNLOCK_FORM_MAX_WIDTH: f32 = 540.;
const PROFILE_EDITOR_MAX_WIDTH: f32 = 620.;

impl Workbench {
    pub(super) fn bundle_unlock_page(
        &mut self,
        message: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = Palette::of(cx);
        let label = self.model.text("connections.profile.bundlePassphrase");
        let input = self.input_for(DraftField::BundlePassphrase, &label, true, window, cx);
        div()
            .id("connection-bundle-unlock")
            .size_full()
            .p(px(self.metrics().page_pad))
            .flex()
            .flex_col()
            .gap(px(Space::LG))
            .child(kit::title(
                self.t("connections.page.importRillSql"),
                TypeScale::LG,
                &p,
            ))
            .child(self.t(message))
            .child(
                div()
                    .max_w(px(UNLOCK_FORM_MAX_WIDTH))
                    .child(kit::form_row(SharedString::from(label), input, &p)),
            )
            .child(
                div()
                    .flex()
                    .gap(px(Space::MD))
                    .child(kit::secondary_button(
                        "bundle-unlock-cancel",
                        None,
                        self.t("common.cancel"),
                        act("conn-cancel-unlock", cx),
                    ))
                    .child(
                        kit::primary_button(
                            "bundle-unlock-confirm",
                            None,
                            self.t("connections.profile.bundleUnlock"),
                            act("conn-unlock-import", cx),
                        )
                        .disabled(self.model.bundle_passphrase.trim().is_empty()),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn import_profiles_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = Palette::of(cx);
        let (title, body, footer) = match self.model.profile_import.clone() {
            ImportState::Preview(preview) => {
                let body = self.profile_preview(&preview, &p, window, cx);
                let selected = preview.rows.iter().filter(|row| row.checked).count();
                let caption = self
                    .model
                    .text("connections.profile.selected")
                    .replace("{selected}", &selected.to_string())
                    .replace("{total}", &preview.rows.len().to_string());
                let footer = div()
                    .flex()
                    .items_center()
                    .gap(px(Space::MD))
                    .child(
                        div()
                            .flex_1()
                            .text_color(p.text_muted)
                            .child(SharedString::from(caption)),
                    )
                    .child(kit::secondary_button(
                        "profile-import-cancel",
                        None,
                        self.t("common.cancel"),
                        act("profile-import-close", cx),
                    ))
                    .child(kit::primary_button(
                        "profile-import-confirm",
                        None,
                        self.t("connections.profile.confirm"),
                        act("profile-import-confirm", cx),
                    ))
                    .into_any_element();
                let body = div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap(px(Space::MD))
                    .child(
                        div()
                            .text_color(p.text_muted)
                            .child(self.t("connections.profile.credentials")),
                    )
                    .child(body)
                    .into_any_element();
                ("connections.profile.preview", body, Some(footer))
            }
            ImportState::Report(report) => (
                "connections.profile.report",
                self.profile_report(&report, &p),
                Some(
                    div()
                        .flex()
                        .justify_end()
                        .child(kit::primary_button(
                            "profile-import-close",
                            None,
                            self.t("common.close"),
                            act("profile-import-close", cx),
                        ))
                        .into_any_element(),
                ),
            ),
            ImportState::Error(error) => (
                "connections.profile.error",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(Space::SM))
                    .child(div().text_color(p.danger).child(SharedString::from(error)))
                    .child(self.t("connections.profile.noSaved"))
                    .into_any_element(),
                Some(
                    div()
                        .flex()
                        .justify_end()
                        .child(kit::secondary_button(
                            "profile-import-close",
                            None,
                            self.t("common.close"),
                            act("profile-import-close", cx),
                        ))
                        .into_any_element(),
                ),
            ),
            _ => {
                let key = if matches!(self.model.profile_import, ImportState::Saving) {
                    "connections.profile.saving"
                } else {
                    "connections.profile.loading"
                };
                (key, div().into_any_element(), None)
            }
        };
        div()
            .id("profile-import-page")
            .size_full()
            .flex()
            .flex_col()
            .gap(px(Space::LG))
            .p(px(self.metrics().page_pad))
            .overflow_hidden()
            .child(kit::title(self.t(title), TypeScale::LG, &p))
            .child(body)
            .children(footer)
            .into_any_element()
    }

    fn profile_preview(
        &mut self,
        preview: &ImportPreview,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut list = div()
            .id("profile-import-rows")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(Space::MD))
            .scroll_area_y();
        if preview.rows.is_empty() {
            list = list.child(self.t("connections.profile.empty"));
        }
        for (index, row) in preview.rows.iter().enumerate() {
            let name = row
                .draft
                .as_ref()
                .map(|draft| draft.name.trim())
                .filter(|name| !name.is_empty())
                .unwrap_or(&row.source.name);
            let state = if row.conflict.is_some() || row.draft.is_none() {
                "connections.profile.skipped"
            } else if row.issues.is_empty() {
                "connections.profile.eligible"
            } else {
                "connections.profile.attention"
            };
            let edit = preview.editing == Some(index);
            let mut card = div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(Space::SM))
                .p(px(Space::MD))
                .rounded(px(Radius::CONTROL))
                .border_1()
                .border_color(p.line_soft)
                .bg(p.surface)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(Space::MD))
                        .child(
                            kit::secondary_button(
                                SharedString::from(format!("profile-row-toggle-{index}")),
                                row.checked.then_some(IconName::Check),
                                self.t(if row.checked {
                                    "connections.profile.checked"
                                } else {
                                    "connections.profile.unchecked"
                                }),
                                act(format!("profile-import-toggle:{index}"), cx),
                            )
                            .disabled(!row.eligible()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_weight(FontWeight::MEDIUM)
                                .child(SharedString::from(name.to_owned())),
                        )
                        .child(
                            div()
                                .text_size(px(TypeScale::XS))
                                .text_color(p.text_muted)
                                .child(self.t(state)),
                        ),
                );
            if let Some(draft) = &row.draft {
                card = card.child(self.profile_target(draft, p));
                if row.source.blockers.is_empty() {
                    card = card.child(div().flex().child(kit::subtle_button(
                        SharedString::from(format!("profile-row-edit-{index}")),
                        None,
                        self.t(if edit {
                            "connections.profile.doneEditing"
                        } else {
                            "connections.profile.edit"
                        }),
                        act(format!("profile-import-edit:{index}"), cx),
                    )));
                }
                if edit {
                    card =
                        card.child(self.profile_editor(index, &row.source, draft, p, window, cx));
                }
            }
            let conflict = row
                .conflict
                .as_ref()
                .map(|conflict| self.profile_conflict(conflict));
            let messages = if row.draft.is_some() {
                row.issues
                    .iter()
                    .map(|issue| issue.message.clone())
                    .collect::<Vec<_>>()
            } else {
                row.source.blockers.clone()
            };
            for message in messages.into_iter().chain(conflict) {
                card = card.child(
                    div()
                        .text_size(px(TypeScale::SM))
                        .text_color(p.danger)
                        .child(SharedString::from(message)),
                );
            }
            for message in &row.source.notes {
                card = card.child(
                    div()
                        .text_size(px(TypeScale::XS))
                        .text_color(p.text_muted)
                        .child(SharedString::from(message.clone())),
                );
            }
            list = list.child(card);
        }
        list.into_any_element()
    }

    fn profile_target(&self, draft: &ConnectionDraft, p: &Palette) -> AnyElement {
        let target = target_label(draft);
        let base = div().text_size(px(TypeScale::SM)).text_color(p.text_muted);
        let tls = self.t(tls_label(draft.tls_mode));
        let env = self.env_text(&draft.env);
        let access = self
            .model
            .text("connections.profile.access")
            .replace("{tls}", tls.as_ref())
            .replace("{env}", env.as_ref());
        let mut target = base
            .flex()
            .flex_col()
            .gap(px(Space::XXS))
            .child(SharedString::from(target))
            .child(SharedString::from(access));
        if draft.tls_mode != TlsMode::Off && draft.ca_mode == CaMode::File {
            target = target.child(SharedString::from(format!(
                "{} · {}",
                self.t("connections.editor.tls.caFile"),
                draft.ca_file
            )));
        }
        if draft.ssh_on {
            let auth = self.t(if draft.ssh_auth == SshAuthKind::Key {
                "connections.editor.ssh.authKey"
            } else {
                "connections.editor.ssh.authPassword"
            });
            target = target.child(SharedString::from(format!(
                "SSH · {}@{}:{} · {auth}",
                draft.ssh_user, draft.ssh_host, draft.ssh_port
            )));
        }
        target.into_any_element()
    }

    fn profile_editor(
        &mut self,
        index: usize,
        source: &ProfileRow,
        draft: &ConnectionDraft,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut fields = vec![ImportField::Name];
        let original = source.draft.as_ref().unwrap_or(draft);
        if crate::app::views::connections::file_based(draft.engine) {
            if !absolute_path(&original.file_path) {
                fields.push(ImportField::File);
            }
        } else {
            fields.extend(
                [
                    ImportField::Host,
                    ImportField::Port,
                    ImportField::Database,
                    ImportField::Username,
                ]
                .into_iter()
                .filter(|field| field.get(original).is_empty()),
            );
            if draft.ssh_on {
                fields.extend(
                    [
                        ImportField::SshHost,
                        ImportField::SshPort,
                        ImportField::SshUser,
                    ]
                    .into_iter()
                    .filter(|field| field.get(original).is_empty()),
                );
                if draft.ssh_auth == SshAuthKind::Key {
                    fields.push(ImportField::SshKey);
                }
            }
            if draft.tls_mode == TlsMode::Verify && draft.ca_mode == CaMode::File {
                fields.push(ImportField::CaFile);
            }
            if !original.client_cert.is_empty() || !original.client_key.is_empty() {
                fields.extend([ImportField::ClientCert, ImportField::ClientKey]);
            }
        }
        let mut editor = div()
            .flex()
            .flex_col()
            .gap(px(Space::SM))
            .max_w(px(PROFILE_EDITOR_MAX_WIDTH));
        for field in fields {
            let label = self.model.text(field.label());
            let input = self.input_for(
                DraftField::ImportProfile(index, field),
                &label,
                false,
                window,
                cx,
            );
            editor = editor.child(kit::form_row(SharedString::from(label), input, p));
        }
        editor.into_any_element()
    }

    fn profile_conflict(&self, conflict: &ImportConflict) -> String {
        let key = match conflict.kind {
            ImportConflictKind::Duplicate => "connections.profile.duplicate",
            ImportConflictKind::NameTaken => "connections.profile.nameTaken",
        };
        self.model
            .text(key)
            .replace("{name}", &conflict.existing_name)
            .replace("{target}", &conflict.existing_target)
    }

    fn profile_report(&self, report: &ImportReport, p: &Palette) -> AnyElement {
        let (saved, skipped, failed) = report.counts();
        let counts = self
            .model
            .text("connections.profile.counts")
            .replace("{saved}", &saved.to_string())
            .replace("{skipped}", &skipped.to_string())
            .replace("{failed}", &failed.to_string());
        let mut list = div()
            .id("profile-import-report-rows")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(Space::SM))
            .scroll_area_y();
        for row in &report.rows {
            let (detail, color) = match &row.result {
                ImportResult::Saved => (self.model.text("connections.profile.saved"), p.ok),
                ImportResult::Invalid(messages) => (messages.join(" · "), p.text_muted),
                ImportResult::Conflict(conflict) => (self.profile_conflict(conflict), p.text_muted),
                ImportResult::Unchecked => (
                    self.model.text("connections.profile.notSelected"),
                    p.text_muted,
                ),
                ImportResult::Failed => (self.model.text("connections.profile.failed"), p.danger),
            };
            list = list.child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(Space::XXS))
                    .py(px(Space::SM))
                    .border_b_1()
                    .border_color(p.line_soft)
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(SharedString::from(row.name.clone())),
                    )
                    .children(row.target.as_ref().map(|target| {
                        div()
                            .text_color(p.text_muted)
                            .child(SharedString::from(target.clone()))
                    }))
                    .child(div().text_color(color).child(SharedString::from(detail))),
            );
        }
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(Space::MD))
            .child(SharedString::from(counts))
            .child(list)
            .into_any_element()
    }
}

fn tls_label(mode: TlsMode) -> &'static str {
    match mode {
        TlsMode::Off => "connections.profile.tlsOff",
        TlsMode::Require => "connections.profile.tlsRequire",
        TlsMode::Verify => "connections.profile.tlsVerify",
    }
}
