//! 数据库管理。SQLite / PostgreSQL / MySQL / MariaDB 的动作仍经 DDL 预览。

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, relative,
};

use crate::ui::kit::{self, FieldSpec, Fields, Palette};

use crate::application::AppServices;
use crate::app::views::grid::{operation_banner, GridTarget};
use crate::ddl::{preview_plan, Action, Plan, Request, Snapshot};
use crate::i18n::{self, Locale};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdminSections {
    pub pragma: bool,
    pub attached: bool,
    pub maintenance: bool,
    pub extensions: bool,
    pub sequences: bool,
    pub events: bool,
    pub partitions: bool,
}

pub fn sections_for(engine: &str) -> AdminSections {
    let sqlite = engine == "sqlite";
    let postgres = engine == "postgres";
    let mysql = engine == "mysql" || engine == "mariadb";
    AdminSections {
        pragma: sqlite,
        attached: sqlite,
        maintenance: sqlite,
        extensions: postgres,
        sequences: postgres,
        events: mysql,
        partitions: mysql,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdminOp {
    Vacuum,
    Reindex { target: Option<String> },
    SetForeignKeys { enabled: bool },
    SetPragma { pragma: String, value: String },
    Attach { path: String, name: String },
    SetEvent { event: String, enabled: bool },
    AlterSequence { sequence: String, increment: Option<i64> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminInventory {
    pub connected: bool,
    pub probed: bool,
    pub source: String,
    pub sequences: Vec<String>,
    pub events: Vec<String>,
    pub extensions: Vec<String>,
    pub partitions: Vec<String>,
    pub attached: Vec<String>,
}

impl AdminInventory {
    pub fn offline(target: &GridTarget) -> Self {
        Self {
            connected: false,
            probed: false,
            source: format!("{} · {} · {}", target.engine, target.database, i18n::text(Locale::ZhCn, "admin.preview.unconnected")),
            sequences: Vec::new(),
            events: Vec::new(),
            extensions: Vec::new(),
            partitions: Vec::new(),
            attached: Vec::new(),
        }
    }
}

pub struct AdminSession {
    pub target: GridTarget,
    pub operation: AdminOp,
    pub reindex_target: String,
    pub attach_path: String,
    pub attach_name: String,
    pub foreign_keys: bool,
    pub pragma_name: String,
    pub pragma_value: String,
    pub sequence_name: String,
    pub sequence_step: String,
    pub event_name: String,
    pub event_enabled: bool,
    pub inventory: AdminInventory,
    preview: Option<Plan>,
    live: bool,
    open: bool,
    pub locale: Locale,
    pub note: String,
}

impl AdminSession {
    pub fn new(target: GridTarget) -> Self {
        let inventory = AdminInventory::offline(&target);
        Self {
            target,
            operation: AdminOp::Vacuum,
            reindex_target: String::new(),
            attach_path: String::new(),
            attach_name: String::new(),
            foreign_keys: true,
            pragma_name: "user_version".to_owned(),
            pragma_value: String::new(),
            sequence_name: String::new(),
            sequence_step: String::new(),
            event_name: String::new(),
            event_enabled: false,
            inventory,
            preview: None,
            live: false,
            open: false,
            locale: Locale::ZhCn,
            note: String::new(),
        }
    }

    pub fn apply_draft(&mut self, kind: &str) {
        let fallback = self.operation.clone();
        self.operation = match kind {
            "vacuum" => AdminOp::Vacuum,
            "reindex" => AdminOp::Reindex { target: if self.reindex_target.is_empty() { None } else { Some(self.reindex_target.clone()) } },
            "foreign" => AdminOp::SetForeignKeys { enabled: self.foreign_keys },
            "pragma" => AdminOp::SetPragma { pragma: self.pragma_name.clone(), value: self.pragma_value.clone() },
            "attach" => AdminOp::Attach { path: self.attach_path.clone(), name: self.attach_name.clone() },
            "sequence" => AdminOp::AlterSequence {
                sequence: self.sequence_name.clone(),
                increment: self.sequence_step.parse().ok(),
            },
            "event" => AdminOp::SetEvent { event: self.event_name.clone(), enabled: self.event_enabled },
            _ => fallback,
        };
        self.cancel();
    }

    pub fn sections(&self) -> AdminSections {
        sections_for(&self.target.engine)
    }

    pub fn banner(&self) -> String {
        operation_banner(&self.target)
    }

    pub fn request(&self) -> Result<Request, String> {
        crate::application::ddl::request(self.target.database.clone(), self.target.schema.clone(), &self.target.engine, self.action())
    }

    pub fn action(&self) -> Action {
        match &self.operation {
            AdminOp::Vacuum => Action::Vacuum,
            AdminOp::Reindex { target } => Action::Reindex { target: target.clone() },
            AdminOp::SetForeignKeys { enabled } => Action::SetForeignKeys { enabled: *enabled },
            AdminOp::SetPragma { pragma, value } => Action::SetPragma { pragma: pragma.clone(), value: value.clone() },
            AdminOp::Attach { path, name } => Action::AttachDatabase { path: path.clone(), name: name.clone() },
            AdminOp::SetEvent { event, enabled } => Action::SetEventEnabled { event: event.clone(), enabled: *enabled },
            AdminOp::AlterSequence { sequence, increment } => Action::AlterSequence {
                sequence: sequence.clone(),
                start: None,
                increment: *increment,
                min_value: None,
                max_value: None,
                cycle: None,
            },
        }
    }

    pub fn arm_live(&mut self, plan: Plan) {
        self.preview = Some(plan);
        self.live = true;
        self.open = true;
        self.note.clear();
    }

    pub fn arm_fixture(&mut self, snapshot: &Snapshot) {
        let plan = preview_plan(snapshot, &self.action());
        self.preview = Some(plan);
        self.live = false;
        self.open = true;
        self.note = i18n::text(self.locale, "admin.preview.unconnected");
    }

    pub fn preview_open(&self) -> bool {
        self.open
    }

    pub fn preview_sql(&self) -> Vec<String> {
        self.preview.as_ref().map(|plan| plan.steps.iter().map(|step| step.sql.clone()).collect()).unwrap_or_default()
    }

    pub fn preview_risks(&self) -> Vec<(String, String)> {
        self.preview
            .as_ref()
            .map(|plan| plan.risks.iter().map(|risk| (risk.level.clone(), risk.text.clone())).collect())
            .unwrap_or_default()
    }

    pub fn preview_rule(&self) -> String {
        self.preview.as_ref().map(|plan| plan.rule.clone()).unwrap_or_default()
    }

    pub fn preview_target(&self) -> String {
        self.preview.as_ref().map(|plan| plan.target.clone()).unwrap_or_default()
    }

    pub fn blocked(&self) -> Vec<String> {
        self.preview.as_ref().map(|plan| plan.blocked.clone()).unwrap_or_default()
    }

    pub fn cancel(&mut self) {
        self.open = false;
    }

    pub fn confirm_sql(&self) -> Result<Vec<String>, String> {
        if !self.open {
            return Err(i18n::text(self.locale, "structure.preview.need"));
        }
        if !self.live {
            return Err(i18n::text(self.locale, "admin.preview.unconnected"));
        }
        let plan = self.preview.as_ref().ok_or_else(|| i18n::text(self.locale, "structure.preview.need"))?;
        if !plan.blocked.is_empty() {
            return Err(plan.blocked.join(" "));
        }
        Ok(plan.steps.iter().map(|step| step.sql.clone()).collect())
    }
}

pub struct AdminPane {
    pub session: AdminSession,
    services: Arc<AppServices>,
    fields: Fields<&'static str>,
}

impl AdminPane {
    pub fn new(services: Arc<AppServices>) -> Self {
        Self { session: AdminSession::new(GridTarget::default()), services, fields: Fields::default() }
    }

    pub fn bind(&mut self, target: GridTarget) {
        // 管理页只有一份操作草稿；重新打开同一目标也要丢弃上一工作项的
        // PRAGMA、ATTACH 或预览继续带过去。
        let locale = self.session.locale;
        self.session = AdminSession::new(target);
        self.session.locale = locale;
        self.fields = Fields::default();
    }

    pub fn type_into(&mut self, field: &str, text: &str) {
        let slot = match field {
            "reindex" => &mut self.session.reindex_target,
            "attach_path" => &mut self.session.attach_path,
            "attach_name" => &mut self.session.attach_name,
            "pragma_value" => &mut self.session.pragma_value,
            "sequence" => &mut self.session.sequence_name,
            "sequence_step" => &mut self.session.sequence_step,
            "event" => &mut self.session.event_name,
            _ => return,
        };
        if text == "backspace" {
            slot.pop();
        } else {
            slot.push_str(text);
        }
        self.session.cancel();
    }

    pub fn choose(&mut self, kind: &str) {
        if kind == "foreign" {
            self.session.foreign_keys = !self.session.foreign_keys;
        }
        self.session.apply_draft(kind);
    }

    pub fn refresh_inventory(&mut self, cx: &mut Context<Self>) {
        let services = Arc::clone(&self.services);
        let target = self.session.target.clone();
        let expected = target.clone();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let inventory = read_inventory(&services.connections, &target).await;
                let _ = tx.send(inventory);
            });
            if let Ok(inventory) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            if this.session.target != expected {
                                return;
                            }
                            this.session.inventory = inventory;
                            if !this.session.inventory.connected {
                                this.session.note = this.session.inventory.source.clone();
                            }
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.session.locale = locale;
    }

    pub fn preview(&mut self, cx: &mut Context<Self>) {
        let Ok(request) = self.session.request() else {
            return;
        };
        if !self.session.sections().allows(&self.session.operation) {
            self.session.note = i18n::text(self.session.locale, "admin.section.none");
            cx.notify();
            return;
        }
        let services = Arc::clone(&self.services);
        let target = self.session.target.clone();
        let connection = self.session.target.connection_id.clone();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = crate::application::ddl::preview_request(&services.connections, &connection, &request).await;
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            if this.session.target != target {
                                this.session.note = "目标已切换，本次预览结果已丢弃。".to_owned();
                                cx.notify();
                                return;
                            }
                            match result {
                                Ok(plan) => this.session.arm_live(plan),
                                Err(message) => this.session.note = message,
                            }
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        let sql = match self.session.confirm_sql() {
            Ok(sql) => sql,
            Err(message) => {
                self.session.note = message;
                cx.notify();
                return;
            }
        };
        let Ok(request) = self.session.request() else {
            return;
        };
        let services = Arc::clone(&self.services);
        let target = self.session.target.clone();
        let connection = self.session.target.connection_id.clone();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let refused = crate::application::ddl::execute_request(&services.connections, &connection, &request, &sql, false).await;
                let done = if refused.is_err() {
                    crate::application::ddl::execute_request(&services.connections, &connection, &request, &sql, true).await
                } else {
                    Err("未确认的管理操作不应执行".to_owned())
                };
                let _ = tx.send(done);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            if this.session.target != target {
                                this.session.note = "目标已切换，本次执行结果已丢弃。".to_owned();
                                cx.notify();
                                return;
                            }
                            this.session.note = match result {
                                Ok(receipt) if receipt.blocked.is_empty() => {
                                    this.session.open = false;
                                    i18n::text(this.session.locale, "structure.preview.done")
                                }
                                Ok(receipt) => receipt.blocked.join(" "),
                                Err(message) => message,
                            };
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }
}

impl AdminSections {
    fn allows(self, operation: &AdminOp) -> bool {
        match operation {
            AdminOp::Vacuum | AdminOp::Reindex { .. } => self.maintenance,
            AdminOp::SetForeignKeys { .. } | AdminOp::SetPragma { .. } => self.pragma,
            AdminOp::Attach { .. } => self.attached,
            AdminOp::SetEvent { .. } => self.events,
            AdminOp::AlterSequence { .. } => self.sequences,
        }
    }
}

impl AdminPane {
    fn set_value(&mut self, field: &'static str, value: String) {
        let slot = match field {
            "reindex" => &mut self.session.reindex_target,
            "attach_path" => &mut self.session.attach_path,
            "attach_name" => &mut self.session.attach_name,
            "pragma_value" => &mut self.session.pragma_value,
            "sequence" => &mut self.session.sequence_name,
            "sequence_step" => &mut self.session.sequence_step,
            "event" => &mut self.session.event_name,
            _ => return,
        };
        *slot = value;
    }

    fn value_of(&self, field: &'static str) -> String {
        match field {
            "reindex" => self.session.reindex_target.clone(),
            "attach_path" => self.session.attach_path.clone(),
            "attach_name" => self.session.attach_name.clone(),
            "pragma_value" => self.session.pragma_value.clone(),
            "sequence" => self.session.sequence_name.clone(),
            "sequence_step" => self.session.sequence_step.clone(),
            "event" => self.session.event_name.clone(),
            _ => String::new(),
        }
    }

    fn admin_field(&mut self, field: &'static str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let value = self.value_of(field);
        let state = self.fields.bind(field, &value, FieldSpec { placeholder, masked: false }, window, cx, |this: &mut Self, field, value, _| this.set_value(field, value));
        div().flex_1().min_w(px(0.)).child(kit::text_input(SharedString::from(format!("admin-field-{field}")), &state, placeholder.to_owned()).small()).into_any_element()
    }
}

#[allow(clippy::too_many_arguments)]
fn op_card(id: &'static str, kind: &'static str, icon: IconName, title: SharedString, inputs: Vec<AnyElement>, inventory: Vec<String>, entity: &Entity<AdminPane>, locale: Locale, p: &Palette) -> AnyElement {
    let entity = entity.clone();
    let mut row = div().flex().items_center().gap(px(8.));
    for input in inputs {
        row = row.child(input);
    }
    let preview = kit::secondary_button(id, Some(IconName::Eye), SharedString::from(i18n::text(locale, "structure.action.preview")), move |_, _, cx| {
        entity.update(cx, |pane, cx| {
            pane.choose(kind);
            pane.preview(cx);
        });
    })
    .accessibility_label(title.clone());
    let mut card = kit::card(p)
        .p(px(14.))
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().flex_none().size(px(30.)).rounded(px(8.)).bg(p.accent_soft).flex().items_center().justify_center().child(kit::icon(icon, 15., p.accent_text)))
                .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(title))
                .child(preview),
        );
    if row_has_children(&inventory) {
        let mut list = div().flex().flex_wrap().gap(px(6.));
        for item in inventory {
            list = list.child(kit::pill(SharedString::from(item), p.text_muted, p.hover));
        }
        card = card.child(list);
    }
    card.child(row).into_any_element()
}

fn row_has_children(items: &[String]) -> bool {
    !items.is_empty()
}

impl Render for AdminPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.session.inventory.probed && !self.session.target.connection_id.is_empty() {
            self.session.inventory.probed = true;
            self.refresh_inventory(cx);
        }
        let p = Palette::of(cx);
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let sections = self.session.sections();
        let entity = cx.entity();
        let inventory = self.session.inventory.clone();
        let mut cards = div().flex().flex_col().gap(px(12.));
        if sections.maintenance {
            cards = cards.child(op_card("admin-vacuum", "vacuum", IconName::BrushCleaning, t("admin.action.vacuum"), Vec::new(), Vec::new(), &entity, locale, &p));
            let target = self.admin_field("reindex", &i18n::text(locale, "ui.conn.optional"), window, cx);
            cards = cards.child(op_card("admin-reindex", "reindex", IconName::ListRestart, t("admin.action.reindex"), vec![target], Vec::new(), &entity, locale, &p));
        }
        if sections.pragma {
            let value = self.admin_field("pragma_value", "user_version", window, cx);
            cards = cards
                .child(op_card("admin-pragma", "pragma", IconName::SlidersHorizontal, t("admin.action.pragma"), vec![value], Vec::new(), &entity, locale, &p))
                .child(op_card(
                    "admin-foreign",
                    "foreign",
                    IconName::Link,
                    SharedString::from(format!("{} · {}", i18n::text(locale, "admin.action.foreignKeys"), if self.session.foreign_keys { "ON" } else { "OFF" })),
                    Vec::new(),
                    Vec::new(),
                    &entity,
                    locale,
                    &p,
                ));
        }
        if sections.attached {
            let path = self.admin_field("attach_path", "/path/to/other.sqlite", window, cx);
            let name = self.admin_field("attach_name", "other", window, cx);
            cards = cards.child(op_card("admin-attach", "attach", IconName::Paperclip, t("admin.action.attach"), vec![path, name], inventory.attached.clone(), &entity, locale, &p));
        }
        if sections.sequences {
            let name = self.admin_field("sequence", "orders_id_seq", window, cx);
            let step = self.admin_field("sequence_step", "1", window, cx);
            cards = cards.child(op_card("admin-sequence", "sequence", IconName::ListOrdered, t("admin.action.sequence"), vec![name, step], inventory.sequences.clone(), &entity, locale, &p));
        }
        if sections.events {
            let name = self.admin_field("event", "event_name", window, cx);
            cards = cards.child(op_card("admin-event", "event", IconName::CalendarClock, t("admin.action.event"), vec![name], inventory.events.clone(), &entity, locale, &p));
        }
        if sections.extensions || sections.partitions {
            let mut list = div().flex().flex_wrap().gap(px(6.));
            for name in inventory.extensions.iter().chain(inventory.partitions.iter()) {
                list = list.child(kit::pill(SharedString::from(name.clone()), p.text_muted, p.hover));
            }
            cards = cards.child(kit::card(&p).p(px(14.)).flex().flex_col().gap(px(8.)).child(kit::muted(SharedString::from(inventory.source.clone()), &p)).child(list));
        }
        if !sections.pragma && !sections.maintenance && !sections.sequences && !sections.events && !sections.extensions && !sections.partitions && !sections.attached {
            cards = cards.child(kit::empty_state(IconName::Gauge, t("ui.tool.admin"), t("admin.section.none"), &p));
        }
        let mut column = div()
            .id("admin-pane")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px(px(24.))
                    .pt(px(18.))
                    .pb(px(14.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .border_b_1()
                    .border_color(p.line)
                    .child(kit::engine_badge(&self.session.target.engine, 30.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(kit::title(t("ui.tool.admin"), 16., &p))
                            .child(div().id("admin-banner").truncate().text_size(px(12.)).text_color(p.text_muted).child(SharedString::from(self.session.banner()))),
                    )
                    .child(div().id("admin-source").text_size(px(11.5)).text_color(p.text_faint).max_w(px(320.)).truncate().child(SharedString::from(inventory.source.clone()))),
            )
            .child(div().id("admin-body").flex_1().min_h(px(0.)).overflow_y_scroll().p(px(24.)).child(div().max_w(px(760.)).mx_auto().flex().flex_col().gap(px(12.)).child(cards).when(!self.session.note.is_empty(), |this| {
                this.child(div().px(px(12.)).py(px(8.)).rounded(px(8.)).bg(p.hover).text_size(px(12.5)).child(SharedString::from(self.session.note.clone())))
            })))
            .child(div().hidden().child(admin_button("admin-preview", "structure.action.preview", false, entity.clone(), locale)));
        if self.session.open {
            let blocked = self.session.blocked();
            column = column.child(
                div().absolute().inset_0().bg(gpui_kit::hsla(0., 0., 0., 0.25)).flex().items_center().justify_center().child(
                    div()
                        .id("admin-preview-dialog")
                        .role(gpui_kit::Role::Dialog)
                        .aria_label(t("structure.preview.title"))
                        .occlude()
                        .w(px(600.))
                        .max_w(relative(0.92))
                        .p(px(18.))
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .bg(p.raised)
                        .border_1()
                        .border_color(p.line)
                        .rounded(px(12.))
                        .shadow_lg()
                        .child(kit::title(t("structure.preview.title"), 15., &p))
                        .child(kit::muted(SharedString::from(self.session.banner()), &p))
                        .child(
                            div()
                                .id("admin-preview-sql")
                                .max_h(px(260.))
                                .overflow_y_scroll()
                                .p(px(12.))
                                .rounded(px(8.))
                                .bg(p.chrome)
                                .border_1()
                                .border_color(p.line)
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(px(12.5))
                                .child(SharedString::from(self.session.preview_sql().join("\n"))),
                        )
                        .child(kit::muted(SharedString::from(self.session.preview_target()), &p))
                        .when(!self.session.preview_risks().is_empty(), |this| {
                            let mut risks = div().flex().flex_col().gap(px(4.));
                            for (level, text) in self.session.preview_risks() {
                                let (fg, bg) = if level == "danger" { (p.danger, p.danger_soft) } else if level == "warn" { (p.warn, p.warn_soft) } else { (p.text_muted, p.hover) };
                                risks = risks.child(div().px(px(10.)).py(px(6.)).rounded(px(6.)).bg(bg).text_color(fg).text_size(px(12.)).child(SharedString::from(text)));
                            }
                            this.child(risks)
                        })
                        .when(!self.session.preview_rule().is_empty(), |this| this.child(kit::muted(SharedString::from(self.session.preview_rule()), &p)))
                        .when(!blocked.is_empty(), |this| this.child(div().px(px(12.)).py(px(8.)).rounded(px(8.)).bg(p.danger_soft).text_color(p.danger).text_size(px(12.5)).child(SharedString::from(blocked.join(" ")))))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(admin_button("admin-cancel", "structure.action.cancel", false, entity.clone(), locale))
                                .child(admin_button("admin-confirm", "structure.action.confirm", true, entity, locale)),
                        ),
                ),
            );
        }
        column
    }
}

fn admin_button(id: &'static str, key: &'static str, primary: bool, entity: Entity<AdminPane>, locale: Locale) -> gpui_kit::component::button::Button {
    let click = move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
        entity.update(cx, |pane, cx| match id {
            "admin-preview" => pane.preview(cx),
            "admin-confirm" => pane.confirm(cx),
            _ => {
                pane.session.cancel();
                cx.notify();
            }
        });
    };
    let label = SharedString::from(i18n::text(locale, key));
    if primary {
        kit::primary_button(id, Some(IconName::Check), label, click)
    } else {
        kit::secondary_button(id, None, label, click)
    }
}

async fn read_inventory(service: &crate::connections::Service, target: &GridTarget) -> AdminInventory {
    let Some(session) = service.catalog_session(&target.connection_id) else {
        let mut inventory = AdminInventory::offline(target);
        inventory.probed = true;
        return inventory;
    };
    match session {
        crate::connections::CatalogSession::Sqlite { pool, .. } => {
            let listed: Result<Vec<(i64, String, String)>, sqlx::Error> = sqlx::query_as("PRAGMA database_list").fetch_all(&pool).await;
            let attached = listed.map(|rows| rows.into_iter().map(|(_, name, _)| name).collect()).unwrap_or_default();
            AdminInventory {
                connected: true,
                probed: true,
                source: format!("sqlite · {}", target.database),
                sequences: Vec::new(),
                events: Vec::new(),
                extensions: Vec::new(),
                partitions: Vec::new(),
                attached,
            }
        }
        _ => AdminInventory {
            connected: false,
            probed: true,
            source: format!("{} · {} · 目录会话没有列出序列或事件，拒绝把夹具当成已连接", target.engine, target.database),
            sequences: Vec::new(),
            events: Vec::new(),
            extensions: Vec::new(),
            partitions: Vec::new(),
            attached: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddl::{DdlEngine, SequenceState, Snapshot};

    fn target(engine: &str) -> GridTarget {
        GridTarget {
            connection_id: "notes".into(),
            connection_name: "笔记".into(),
            database: "app".into(),
            schema: "public".into(),
            table: "customers".into(),
            engine: engine.into(),
            environment: "dev".into(),
        }
    }

    #[test]
    fn remote_admin_fixtures_stay_unconnected_and_sqlserver_has_no_section() {
        let sqlserver = sections_for("sqlserver");
        assert!(!sqlserver.pragma && !sqlserver.maintenance && !sqlserver.sequences && !sqlserver.events && !sqlserver.extensions && !sqlserver.partitions && !sqlserver.attached);

        let mut postgres = AdminSession::new(target("postgres"));
        postgres.operation = AdminOp::AlterSequence { sequence: "ids".into(), increment: Some(2) };
        let mut snapshot = Snapshot::bare(DdlEngine::Postgres, "app", "public", "ids");
        snapshot.sequence = Some(SequenceState { increment: Some(1), ..SequenceState::default() });
        postgres.arm_fixture(&snapshot);
        assert!(postgres.banner().contains("笔记") && postgres.banner().contains("dev") && postgres.banner().contains("postgres"), "{}", postgres.banner());
        assert!(postgres.blocked().is_empty(), "{:?}", postgres.blocked());
        assert!(postgres.preview_sql().join("\n").to_ascii_uppercase().contains("SEQUENCE"), "{:?}", postgres.preview_sql());
        assert!(postgres.confirm_sql().unwrap_err().contains("没有连接"));

        for engine in ["mysql", "mariadb"] {
            let mut session = AdminSession::new(target(engine));
            session.operation = AdminOp::SetEvent { event: "sweep".into(), enabled: false };
            let mut snapshot = Snapshot::bare(if engine == "mysql" { DdlEngine::Mysql } else { DdlEngine::Mariadb }, "app", "", "sweep");
            snapshot.event_enabled = Some(true);
            session.arm_fixture(&snapshot);
            assert!(session.sections().events, "{engine}");
            assert!(session.blocked().is_empty(), "{engine} {:?}", session.blocked());
            let sql = session.preview_sql().join("\n").to_ascii_uppercase();
            assert!(sql.contains("EVENT") && sql.contains("DISABLE"), "{engine} {sql}");
            assert!(session.confirm_sql().is_err(), "{engine}");
        }
    }

    #[test]
    fn admin_inputs_preview_the_requested_operation_and_unconnected_sources_refuse() {
        let dir = std::env::temp_dir().join(format!("rillsql-admin-draft-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let mut pane = AdminPane::new(services);
        pane.bind(target("postgres"));
        assert!(!pane.session.inventory.connected);
        assert!(pane.session.inventory.source.contains("postgres"), "{}", pane.session.inventory.source);
        pane.type_into("reindex", "orders");
        pane.choose("reindex");
        match &pane.session.operation {
            AdminOp::Reindex { target } => assert_eq!(target.as_deref(), Some("orders")),
            other => panic!("{other:?}"),
        }
        pane.type_into("attach_path", "/tmp/other.sqlite");
        pane.type_into("attach_name", "other");
        pane.choose("attach");
        assert!(matches!(pane.session.operation, AdminOp::Attach { .. }));
        pane.choose("foreign");
        assert!(matches!(pane.session.operation, AdminOp::SetForeignKeys { enabled: false }));
        let mut snapshot = Snapshot::bare(DdlEngine::Postgres, "app", "public", "orders");
        pane.session.operation = AdminOp::Reindex { target: Some("orders".into()) };
        pane.session.arm_fixture(&snapshot);
        assert!(pane.session.confirm_sql().unwrap_err().contains("没有连接"));
        snapshot.name = "orders".into();
        pane.session.inventory.sequences.clear();
        assert!(pane.session.inventory.sequences.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    fn failed_admin_confirm_keeps_the_preview_and_names_the_reason(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext as _;
        let dir = std::env::temp_dir().join(format!("rillsql-admin-fail-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let pane = cx.update(|cx| cx.new(|_| AdminPane::new(services)));
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.bind(target("sqlite"));
                let mut snapshot = Snapshot::bare(DdlEngine::Sqlite, "app", "", "customers");
                snapshot.name = "customers".into();
                pane.session.operation = AdminOp::Vacuum;
                pane.session.arm_fixture(&snapshot);
                assert!(pane.session.preview_open());
                pane.confirm(cx);
                assert!(pane.session.preview_open(), "失败后预览应保留，方便查看原因和重试");
                assert!(pane.session.note.contains("没有连接"), "{}", pane.session.note);
            });
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn admin_target_change_drops_the_previous_operation_and_preview() {
        let dir = std::env::temp_dir().join(format!("rillsql-admin-reset-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let mut pane = AdminPane::new(services);
        pane.bind(target("postgres"));
        pane.type_into("reindex", "orders");
        pane.choose("reindex");
        let mut snapshot = Snapshot::bare(DdlEngine::Postgres, "app", "public", "orders");
        snapshot.name = "orders".into();
        pane.session.arm_fixture(&snapshot);
        assert!(pane.session.open);
        assert!(matches!(pane.session.operation, AdminOp::Reindex { .. }));

        let mut other = target("postgres");
        other.table = "customers_archive".into();
        pane.bind(other);
        assert_eq!(pane.session.target.table, "customers_archive");
        assert!(matches!(pane.session.operation, AdminOp::Vacuum), "换表后不能沿用上一个索引操作");
        assert!(pane.session.reindex_target.is_empty());
        assert!(!pane.session.open, "换表后不能沿用上一个预览");
        assert!(pane.session.note.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sqlite_inventory_reads_attached_databases_and_remote_engines_stay_offline() {
        let dir = std::env::temp_dir().join(format!("rillsql-admin-inventory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("notes.sqlite");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
            .await
            .unwrap();
        pool.close().await;
        let service = std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")));
        service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
        let inventory = read_inventory(
            &service,
            &GridTarget {
                connection_id: "notes".into(),
                connection_name: "笔记".into(),
                database: "main".into(),
                schema: String::new(),
                table: "customers".into(),
                engine: "sqlite".into(),
                environment: "dev".into(),
            },
        )
        .await;
        assert!(inventory.connected);
        assert!(inventory.attached.iter().any(|name| name == "main"), "{:?}", inventory.attached);
        let mut remote_target = target("postgres");
        remote_target.connection_id = "missing-postgres".into();
        let remote = read_inventory(&service, &remote_target).await;
        assert!(!remote.connected);
        assert!(remote.source.contains("postgres"), "{}", remote.source);
        assert!(remote.sequences.is_empty());
        assert!(remote.events.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sqlite_pragma_cancel_keeps_user_version_and_confirm_writes_it() {
        let dir = std::env::temp_dir().join(format!("rillsql-admin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("notes.sqlite");
        let setup = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
            .await
            .unwrap();
        sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY)").execute(&setup).await.unwrap();
        setup.close().await;
        let service = std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")));
        service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
        let mut session = AdminSession::new(GridTarget {
            connection_id: "notes".into(),
            connection_name: "笔记".into(),
            database: "main".into(),
            schema: String::new(),
            table: "customers".into(),
            engine: "sqlite".into(),
            environment: "dev".into(),
        });
        assert!(session.sections().pragma && session.sections().maintenance);
        session.operation = AdminOp::SetPragma { pragma: "user_version".into(), value: "7".into() };
        let request = session.request().unwrap();
        let plan = crate::application::ddl::preview_request(&service, "notes", &request).await.unwrap();
        assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
        session.arm_live(plan);
        assert!(session.banner().contains("笔记") && session.banner().contains("dev") && session.banner().contains("sqlite"), "{}", session.banner());
        let sql = session.confirm_sql().unwrap();
        assert!(sql.iter().any(|item| item.contains("user_version")), "{sql:?}");
        session.cancel();
        assert!(session.confirm_sql().is_err());
        let refused = match crate::application::ddl::execute_request(&service, "notes", &request, &sql, false).await {
            Err(message) => message,
            Ok(_) => panic!("未确认的管理操作应当被拒绝"),
        };
        assert!(refused.contains("确认"), "{refused}");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let version: i64 = sqlx::query_scalar("PRAGMA user_version").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(version, 0);
        session.open = true;
        crate::application::ddl::execute_request(&service, "notes", &request, &sql, true).await.unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let version: i64 = sqlx::query_scalar("PRAGMA user_version").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(version, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
