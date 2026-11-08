//! 对象结构。预览和执行只走 application::ddl，取消预览不执行。

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, relative,
};

use crate::ui::kit::{self, FieldSpec, Fields, Palette};

use crate::application::AppServices;
use crate::app::views::grid::{operation_banner, GridTarget};
use crate::ddl::{preview_plan, Action, ColumnDraft, ForeignKeyDraft, IndexColumnDraft, Plan, Request, Snapshot};
use crate::i18n::{self, Locale};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Columns,
    Indexes,
    ForeignKeys,
    View,
    Danger,
}

pub struct StructureSession {
    pub target: GridTarget,
    pub section: Section,
    pub column_name: String,
    pub column_type: String,
    /// 非空时字段预览使用完整草稿。只带新列会被领域规则拦住，避免漏改已有列。
    pub kept_columns: Vec<ColumnDraft>,
    pub index_name: String,
    pub index_column: String,
    pub fk_name: String,
    pub fk_column: String,
    pub fk_ref_schema: String,
    pub fk_ref_table: String,
    pub fk_ref_column: String,
    pub view_sql: String,
    preview: Option<Plan>,
    live: bool,
    open: bool,
    pub locale: Locale,
    pub note: String,
}

impl StructureSession {
    pub fn new(target: GridTarget) -> Self {
        Self {
            target,
            section: Section::Columns,
            column_name: String::new(),
            column_type: "TEXT".to_owned(),
            kept_columns: Vec::new(),
            index_name: String::new(),
            index_column: String::new(),
            fk_name: String::new(),
            fk_column: String::new(),
            fk_ref_schema: String::new(),
            fk_ref_table: String::new(),
            fk_ref_column: String::new(),
            view_sql: String::new(),
            preview: None,
            live: false,
            open: false,
            locale: Locale::ZhCn,
            note: String::new(),
        }
    }

    pub fn banner(&self) -> String {
        operation_banner(&self.target)
    }

    pub fn request(&self) -> Result<Request, String> {
        crate::application::ddl::request(self.target.database.clone(), self.target.schema.clone(), &self.target.engine, self.action())
    }

    pub fn action(&self) -> Action {
        let table = self.target.table.clone();
        match self.section {
            Section::Columns => {
                let mut columns = self.kept_columns.clone();
                if !self.column_name.is_empty() && columns.iter().all(|column| column.name != self.column_name) {
                    columns.push(ColumnDraft {
                        original: None,
                        name: self.column_name.clone(),
                        type_name: self.column_type.clone(),
                        nullable: true,
                        default_sql: String::new(),
                        primary: false,
                        auto: false,
                        dropped: false,
                    });
                }
                if columns.is_empty() {
                    columns.push(ColumnDraft {
                        original: None,
                        name: self.column_name.clone(),
                        type_name: self.column_type.clone(),
                        nullable: true,
                        default_sql: String::new(),
                        primary: false,
                        auto: false,
                        dropped: false,
                    });
                }
                Action::AlterColumns { table, columns }
            }
            Section::Indexes => Action::CreateIndex {
                table,
                name: self.index_name.clone(),
                unique: false,
                columns: vec![IndexColumnDraft { name: self.index_column.clone(), descending: false }],
            },
            Section::ForeignKeys => Action::CreateForeignKey {
                table,
                foreign_key: ForeignKeyDraft {
                    name: self.fk_name.clone(),
                    columns: vec![self.fk_column.clone()],
                    ref_schema: self.fk_ref_schema.clone(),
                    ref_table: self.fk_ref_table.clone(),
                    ref_columns: vec![self.fk_ref_column.clone()],
                    on_delete: "NO ACTION".to_owned(),
                    on_update: "NO ACTION".to_owned(),
                },
            },
            Section::View => Action::ReplaceView { table, definition: self.view_sql.clone() },
            Section::Danger => Action::DropTable { table },
        }
    }

    pub fn arm_live(&mut self, plan: Plan) {
        self.note.clear();
        self.preview = Some(plan);
        self.live = true;
        self.open = true;
    }

    pub fn arm_fixture(&mut self, snapshot: &Snapshot) {
        let plan = preview_plan(snapshot, &self.action());
        self.note = i18n::text(self.locale, "admin.preview.unconnected");
        self.preview = Some(plan);
        self.live = false;
        self.open = true;
    }

    pub fn preview_open(&self) -> bool {
        self.open
    }

    pub fn blocked(&self) -> Vec<String> {
        self.preview.as_ref().map(|plan| plan.blocked.clone()).unwrap_or_default()
    }

    pub fn preview_sql(&self) -> Vec<String> {
        self.preview.as_ref().map(|plan| plan.steps.iter().map(|step| step.sql.clone()).collect()).unwrap_or_default()
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

pub struct StructurePane {
    pub session: StructureSession,
    services: Arc<AppServices>,
    fields: Fields<&'static str>,
    snapshot: Option<Snapshot>,
    loaded_for: String,
    loading: bool,
    load_error: String,
}

impl StructurePane {
    pub fn new(services: Arc<AppServices>) -> Self {
        Self {
            session: StructureSession::new(GridTarget::default()),
            services,
            fields: Fields::default(),
            snapshot: None,
            loaded_for: String::new(),
            loading: false,
            load_error: String::new(),
        }
    }

    pub fn bind(&mut self, target: GridTarget) {
        self.session.target = target;
    }

    pub fn set_locale(&mut self, locale: Locale) {
        self.session.locale = locale;
    }

    pub fn type_into(&mut self, field: &str, text: &str) {
        let slot = match field {
            "column_name" => &mut self.session.column_name,
            "column_type" => &mut self.session.column_type,
            "index_name" => &mut self.session.index_name,
            "index_column" => &mut self.session.index_column,
            "fk_name" => &mut self.session.fk_name,
            "fk_column" => &mut self.session.fk_column,
            "fk_ref_schema" => &mut self.session.fk_ref_schema,
            "fk_ref_table" => &mut self.session.fk_ref_table,
            "fk_ref_column" => &mut self.session.fk_ref_column,
            "view_sql" => &mut self.session.view_sql,
            _ => return,
        };
        if text == "backspace" {
            slot.pop();
        } else if field == "column_type" && slot.as_str() == "TEXT" {
            slot.clear();
            slot.push_str(text);
        } else {
            slot.push_str(text);
        }
        self.session.cancel();
    }

    pub fn preview(&mut self, cx: &mut Context<Self>) {
        let Ok(request) = self.session.request() else {
            self.session.note = i18n::text(self.session.locale, "structure.preview.need");
            cx.notify();
            return;
        };
        let services = Arc::clone(&self.services);
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
        let Ok(sql) = self.session.confirm_sql() else {
            cx.notify();
            return;
        };
        let Ok(request) = self.session.request() else {
            return;
        };
        let services = Arc::clone(&self.services);
        let connection = self.session.target.connection_id.clone();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = crate::application::ddl::execute_request(&services.connections, &connection, &request, &sql, true).await;
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            this.session.open = false;
                            this.session.note = match result {
                                Ok(receipt) if receipt.blocked.is_empty() => i18n::text(this.session.locale, "structure.preview.done"),
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

impl StructurePane {
    fn ensure_snapshot(&mut self, cx: &mut Context<Self>) {
        let target = &self.session.target;
        if target.connection_id.is_empty() || target.table.is_empty() {
            return;
        }
        let key = format!("{}|{}|{}|{}", target.connection_id, target.database, target.schema, target.table);
        if self.loaded_for == key {
            return;
        }
        self.loaded_for = key.clone();
        self.snapshot = None;
        self.load_error.clear();
        self.loading = true;
        let services = Arc::clone(&self.services);
        let target = target.clone();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::app::views::grid::runtime_for_views().spawn(async move {
                let result = crate::application::ddl::describe_saved(&services.connections, &target.connection_id, target.database.clone(), target.schema.clone(), &target.engine, &target.table).await;
                let _ = tx.send(result);
            });
            if let Ok(result) = rx.await {
                cx.update(|cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |this, cx| {
                            if this.loaded_for != key {
                                return;
                            }
                            this.loading = false;
                            match result {
                                Ok(snapshot) => this.snapshot = Some(snapshot),
                                Err(message) => this.load_error = message,
                            }
                            cx.notify();
                        });
                    }
                });
            }
        })
        .detach();
    }

    fn set_field(&mut self, field: &'static str, value: String) {
        let slot = match field {
            "column_name" => &mut self.session.column_name,
            "column_type" => &mut self.session.column_type,
            "index_name" => &mut self.session.index_name,
            "index_column" => &mut self.session.index_column,
            "fk_name" => &mut self.session.fk_name,
            "fk_column" => &mut self.session.fk_column,
            "fk_ref_schema" => &mut self.session.fk_ref_schema,
            "fk_ref_table" => &mut self.session.fk_ref_table,
            "fk_ref_column" => &mut self.session.fk_ref_column,
            "view_sql" => &mut self.session.view_sql,
            _ => return,
        };
        if *slot != value {
            *slot = value;
            self.session.cancel();
        }
    }

    fn field_value(&self, field: &'static str) -> String {
        match field {
            "column_name" => self.session.column_name.clone(),
            "column_type" => self.session.column_type.clone(),
            "index_name" => self.session.index_name.clone(),
            "index_column" => self.session.index_column.clone(),
            "fk_name" => self.session.fk_name.clone(),
            "fk_column" => self.session.fk_column.clone(),
            "fk_ref_schema" => self.session.fk_ref_schema.clone(),
            "fk_ref_table" => self.session.fk_ref_table.clone(),
            "fk_ref_column" => self.session.fk_ref_column.clone(),
            "view_sql" => self.session.view_sql.clone(),
            _ => String::new(),
        }
    }

    fn field(&mut self, field: &'static str, key: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let value = self.field_value(field);
        let state = self.fields.bind(field, &value, FieldSpec { placeholder, masked: false }, window, cx, |this: &mut Self, field, value, _| this.set_field(field, value));
        kit::form_row(
            SharedString::from(i18n::text(self.session.locale, key)),
            kit::text_input(SharedString::from(format!("structure-field-{field}")), &state, i18n::text(self.session.locale, key)),
            &p,
        )
    }

    fn overview(&self, p: &Palette, cx: &App) -> AnyElement {
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        if self.loading {
            return div().p(px(16.)).flex().items_center().gap(px(8.)).text_color(p.text_muted).child(Spinner::new().small()).child(t("ui.structure.loading")).into_any_element();
        }
        if !self.load_error.is_empty() {
            return div().p(px(16.)).text_color(p.danger).child(SharedString::from(self.load_error.clone())).into_any_element();
        }
        let Some(snapshot) = &self.snapshot else {
            return div().p(px(16.)).text_color(p.text_faint).child(t("ui.structure.none")).into_any_element();
        };
        let mono = cx.theme().mono_font_family.clone();
        match self.session.section {
            Section::Columns => {
                let rows = snapshot
                    .columns
                    .iter()
                    .map(|column| {
                        vec![
                            cell_main(column.name.clone(), column.primary.then_some(IconName::Key), p),
                            cell_mono(column.type_name.clone(), &mono, p),
                            cell_text(if column.nullable { "✓".into() } else { "—".into() }, p),
                            cell_mono(column.default_sql.clone().unwrap_or_default(), &mono, p),
                        ]
                    })
                    .collect();
                grid_table(vec![t("structure.field.column"), t("structure.field.type"), t("ui.structure.nullable"), t("ui.structure.default")], vec![2., 1.4, 0.6, 1.4], rows, p)
            }
            Section::Indexes => {
                let rows = snapshot
                    .indexes
                    .iter()
                    .map(|index| {
                        vec![
                            cell_main(index.name.clone(), index.unique.then_some(IconName::Hash), p),
                            cell_mono(index.columns.iter().map(|column| if column.descending { format!("{} DESC", column.name) } else { column.name.clone() }).collect::<Vec<_>>().join(", "), &mono, p),
                            cell_text(if index.unique { "✓".into() } else { "—".into() }, p),
                        ]
                    })
                    .collect();
                grid_table(vec![t("structure.field.index"), t("structure.section.columns"), t("ui.structure.unique")], vec![2., 2., 0.6], rows, p)
            }
            Section::ForeignKeys => {
                let rows = snapshot
                    .foreign_keys
                    .iter()
                    .map(|key| {
                        let reference = if key.ref_schema.is_empty() { key.ref_table.clone() } else { format!("{}.{}", key.ref_schema, key.ref_table) };
                        vec![
                            cell_main(if key.name.is_empty() { "—".into() } else { key.name.clone() }, None, p),
                            cell_mono(key.columns.join(", "), &mono, p),
                            cell_mono(format!("{reference}({})", key.ref_columns.join(", ")), &mono, p),
                        ]
                    })
                    .collect();
                grid_table(vec![t("structure.section.foreign"), t("structure.section.columns"), t("ui.structure.references")], vec![1.4, 1.4, 2.], rows, p)
            }
            Section::View | Section::Danger => div()
                .id("structure-ddl")
                .max_h(px(260.))
                .overflow_y_scroll()
                .p(px(14.))
                .font_family(mono)
                .text_size(px(12.))
                .text_color(p.text_muted)
                .child(SharedString::from(if snapshot.table_sql.is_empty() { i18n::text(locale, "ui.structure.none") } else { snapshot.table_sql.clone() }))
                .into_any_element(),
        }
    }
}

fn grid_table(headers: Vec<SharedString>, weights: Vec<f32>, rows: Vec<Vec<AnyElement>>, p: &Palette) -> AnyElement {
    let mut head = div().flex().px(px(14.)).h(px(32.)).items_center().bg(p.chrome).border_b_1().border_color(p.line).text_size(px(11.5)).text_color(p.text_faint);
    for (header, weight) in headers.into_iter().zip(weights.iter()) {
        head = head.child(div().flex_grow(*weight).flex_shrink(1.).flex_basis(px(0.)).min_w(px(0.)).child(header));
    }
    let mut body = div().flex().flex_col();
    let empty = rows.is_empty();
    for row in rows {
        let mut line = div().flex().px(px(14.)).min_h(px(34.)).items_center().border_b_1().border_color(p.line_soft).hover(|style| style.bg(p.hover));
        for (cell, weight) in row.into_iter().zip(weights.iter()) {
            line = line.child(div().flex_grow(*weight).flex_shrink(1.).flex_basis(px(0.)).min_w(px(0.)).pr(px(8.)).child(cell));
        }
        body = body.child(line);
    }
    if empty {
        body = body.child(div().px(px(14.)).py(px(12.)).text_size(px(12.)).text_color(p.text_faint).child("—"));
    }
    div().flex().flex_col().child(head).child(body).into_any_element()
}

fn cell_main(text: String, icon: Option<IconName>, p: &Palette) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .min_w(px(0.))
        .child(div().truncate().font_weight(FontWeight::MEDIUM).child(SharedString::from(text)))
        .when_some(icon, |this, icon| this.child(kit::icon(icon, 12., p.warn)))
        .into_any_element()
}

fn cell_mono(text: String, mono: &SharedString, p: &Palette) -> AnyElement {
    div().truncate().font_family(mono.clone()).text_size(px(12.)).text_color(p.text_muted).child(SharedString::from(text)).into_any_element()
}

fn cell_text(text: String, p: &Palette) -> AnyElement {
    div().text_color(p.text_muted).child(SharedString::from(text)).into_any_element()
}

impl Render for StructurePane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_snapshot(cx);
        let p = Palette::of(cx);
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let section = self.session.section;
        let entity = cx.entity();
        let mut items = Vec::new();
        for (id, key, value) in [
            ("structure.section.columns", "structure.section.columns", Section::Columns),
            ("structure.section.indexes", "structure.section.indexes", Section::Indexes),
            ("structure.section.foreign", "structure.section.foreign", Section::ForeignKeys),
            ("structure.section.view", "structure.section.view", Section::View),
            ("structure.section.danger", "structure.section.danger", Section::Danger),
        ] {
            let entity = entity.clone();
            items.push((
                SharedString::from(id),
                t(key),
                section == value,
                kit::click(move |_, _, cx| {
                    entity.update(cx, |pane, cx| {
                        pane.session.section = value;
                        pane.session.cancel();
                        cx.notify();
                    });
                }),
            ));
        }
        let mut form = div().flex().flex_col().gap(px(12.));
        match section {
            Section::Columns => {
                let name = self.field("column_name", "structure.field.column", "new_column", window, cx);
                let kind = self.field("column_type", "structure.field.type", "TEXT", window, cx);
                form = form.child(div().flex().gap(px(12.)).child(div().flex_1().child(name)).child(div().w(px(200.)).child(kind)));
            }
            Section::Indexes => {
                let name = self.field("index_name", "structure.field.index", "ix_table_column", window, cx);
                let column = self.field("index_column", "structure.section.columns", "column", window, cx);
                form = form.child(div().flex().gap(px(12.)).child(div().flex_1().child(name)).child(div().flex_1().child(column)));
            }
            Section::ForeignKeys => {
                let name = self.field("fk_name", "structure.section.foreign", "fk_name", window, cx);
                let column = self.field("fk_column", "structure.field.column", "parent_id", window, cx);
                let table = self.field("fk_ref_table", "ui.structure.refTable", "parents", window, cx);
                let target = self.field("fk_ref_column", "ui.structure.refColumn", "id", window, cx);
                form = form
                    .child(div().flex().gap(px(12.)).child(div().flex_1().child(name)).child(div().flex_1().child(column)))
                    .child(div().flex().gap(px(12.)).child(div().flex_1().child(table)).child(div().flex_1().child(target)));
            }
            Section::View => {
                let sql = self.field("view_sql", "structure.field.view", "CREATE VIEW … AS SELECT …", window, cx);
                form = form.child(sql);
            }
            Section::Danger => {
                form = form.child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .p(px(12.))
                        .rounded(px(8.))
                        .bg(p.danger_soft)
                        .text_color(p.danger)
                        .child(kit::icon(IconName::TriangleAlert, 16., p.danger))
                        .child(div().flex().flex_col().gap(px(2.)).child(div().font_weight(FontWeight::SEMIBOLD).child(t("ui.structure.dropTitle"))).child(div().text_size(px(12.)).child(t("ui.structure.dropDetail")))),
                );
            }
        }
        let preview_click = {
            let entity = entity.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| entity.update(cx, |pane, cx| pane.preview(cx))
        };
        form = form.child(div().flex().justify_end().child(if section == Section::Danger {
            kit::danger_button("structure-preview", Some(IconName::Eye), t("structure.action.preview"), preview_click).into_any_element()
        } else {
            kit::primary_button("structure-preview", Some(IconName::Eye), t("structure.action.preview"), preview_click).into_any_element()
        }));

        let title = if self.session.target.table.is_empty() { t("structure.section.columns") } else { SharedString::from(self.session.target.table.clone()) };
        let mut column = div()
            .id("structure-pane")
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
                    .flex_wrap()
                    .items_center()
                    .gap(px(14.))
                    .border_b_1()
                    .border_color(p.line)
                    .child(kit::engine_badge(&self.session.target.engine, 30.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.))
                            .flex()
                            .flex_col()
                            .child(kit::title(title, 16., &p))
                            .child(div().id("structure-banner").text_size(px(12.)).text_color(p.text_muted).truncate().child(SharedString::from(self.session.banner()))),
                    )
                    .child(kit::segmented(items, &p)),
            );
        let mut body = div().id("structure-body").flex_1().min_h(px(0.)).overflow_y_scroll().p(px(24.)).flex().flex_col().gap(px(20.));
        body = body
            .child(div().flex().flex_col().gap(px(8.)).child(kit::eyebrow(t("ui.structure.existing"), &p)).child(kit::card(&p).overflow_hidden().child(self.overview(&p, cx))))
            .child(div().flex().flex_col().gap(px(8.)).child(kit::eyebrow(t(if section == Section::Danger { "structure.section.danger" } else { "ui.structure.add" }), &p)).child(kit::card(&p).p(px(16.)).child(form)));
        if !self.session.note.is_empty() {
            body = body.child(div().px(px(12.)).py(px(8.)).rounded(px(8.)).bg(p.hover).text_size(px(12.5)).child(SharedString::from(self.session.note.clone())));
        }
        column = column.child(body);
        if self.session.preview_open() {
            let blocked = self.session.blocked();
            let confirm = {
                let entity = entity.clone();
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| entity.update(cx, |pane, cx| pane.confirm(cx))
            };
            let cancel = {
                let entity = entity.clone();
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                    entity.update(cx, |pane, cx| {
                        pane.session.cancel();
                        cx.notify();
                    })
                }
            };
            column = column.child(
                div().absolute().inset_0().bg(gpui_kit::hsla(0., 0., 0., 0.25)).flex().items_center().justify_center().child(
                    div()
                        .id("structure-preview")
                        .role(gpui_kit::Role::Dialog)
                        .aria_label(t("structure.preview.title"))
                        .occlude()
                        .w(px(620.))
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
                                .id("structure-preview-sql")
                                .max_h(px(280.))
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
                        .when(!blocked.is_empty(), |this| {
                            this.child(div().px(px(12.)).py(px(8.)).rounded(px(8.)).bg(p.danger_soft).text_color(p.danger).text_size(px(12.5)).child(SharedString::from(blocked.join(" "))))
                        })
                        .when(!self.session.note.is_empty(), |this| this.child(kit::muted(SharedString::from(self.session.note.clone()), &p)))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(kit::secondary_button("structure-cancel", None, t("structure.action.cancel"), cancel))
                                .child(if section == Section::Danger {
                                    kit::danger_button("structure-confirm", Some(IconName::Check), t("structure.action.confirm"), confirm).into_any_element()
                                } else {
                                    kit::primary_button("structure-confirm", Some(IconName::Check), t("structure.action.confirm"), confirm).into_any_element()
                                }),
                        ),
                ),
            );
        }
        column
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddl::{Column, ColumnDraft, DdlEngine, Snapshot};

    fn column(name: &str, type_name: &str, primary: bool) -> Column {
        Column {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            nullable: !primary,
            default_sql: None,
            primary,
            auto: false,
            generated: false,
            default_constraint: None,
        }
    }

    fn target(engine: &str, table: &str) -> GridTarget {
        GridTarget {
            connection_id: "notes".into(),
            connection_name: "笔记".into(),
            database: "app".into(),
            schema: "public".into(),
            table: table.into(),
            engine: engine.into(),
            environment: "dev".into(),
        }
    }

    #[test]
    fn fixtures_preview_columns_indexes_keys_views_and_danger_without_a_live_engine() {
        let mut session = StructureSession::new(target("postgres", "orders"));
        session.section = Section::Indexes;
        session.index_name = "ix_note".into();
        session.index_column = "note".into();
        let mut snapshot = Snapshot::bare(DdlEngine::Postgres, "app", "public", "orders");
        snapshot.columns.push(column("note", "text", false));
        session.arm_fixture(&snapshot);
        assert!(session.banner().contains("笔记") && session.banner().contains("dev") && session.banner().contains("orders"), "{}", session.banner());
        assert!(session.preview_sql().iter().any(|sql| sql.to_ascii_uppercase().contains("INDEX")), "{:?}", session.preview_sql());
        session.cancel();
        assert!(session.confirm_sql().is_err());

        session.section = Section::ForeignKeys;
        session.fk_name = "orders_parent".into();
        session.fk_column = "parent_id".into();
        session.fk_ref_schema = "public".into();
        session.fk_ref_table = "parents".into();
        session.fk_ref_column = "id".into();
        let mut child = Snapshot::bare(DdlEngine::Postgres, "app", "public", "orders");
        child.columns.push(column("parent_id", "integer", false));
        let mut parent = Snapshot::bare(DdlEngine::Postgres, "app", "public", "parents");
        parent.columns.push(column("id", "integer", true));
        child.reference = Some(Box::new(parent));
        session.arm_fixture(&child);
        assert!(session.blocked().is_empty(), "{:?}", session.blocked());
        let foreign = session.preview_sql().join("\n").to_ascii_uppercase();
        assert!(foreign.contains("FOREIGN") || foreign.contains("REFERENCES"), "{foreign}");
        assert!(session.confirm_sql().unwrap_err().contains("没有连接"));

        session.target = target("postgres", "v");
        session.section = Section::View;
        session.view_sql = "CREATE VIEW public.v AS SELECT 2".into();
        let mut view = Snapshot::bare(DdlEngine::Postgres, "app", "public", "v");
        view.kind = "view".into();
        view.table_sql = "CREATE VIEW public.v AS SELECT 1".into();
        session.arm_fixture(&view);
        assert!(session.blocked().is_empty(), "{:?}", session.blocked());
        assert!(session.preview_sql().join("\n").to_ascii_uppercase().contains("VIEW"), "{:?}", session.preview_sql());
        assert!(session.confirm_sql().is_err());

        session.target = target("sqlite", "customers");
        session.target.database = "main".into();
        session.target.schema.clear();
        session.section = Section::Danger;
        let danger = Snapshot::bare(DdlEngine::Sqlite, "main", "", "customers");
        session.arm_fixture(&danger);
        assert!(session.banner().contains("customers") && session.banner().contains("sqlite") && session.banner().contains("dev"), "{}", session.banner());
        assert!(session.preview_sql().iter().any(|sql| sql.to_ascii_uppercase().contains("DROP")), "{:?}", session.preview_sql());
        session.cancel();
        assert!(session.confirm_sql().is_err());
    }

    #[test]
    fn typed_structure_fields_are_the_preview_draft() {
        let dir = std::env::temp_dir().join(format!("rillsql-structure-draft-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
        let mut pane = StructurePane::new(services);
        pane.bind(target("sqlite", "customers"));
        pane.type_into("column_name", "nickname");
        match pane.session.action() {
            crate::ddl::Action::AlterColumns { columns, .. } => assert_eq!(columns[0].name, "nickname"),
            other => panic!("{other:?}"),
        }
        pane.session.section = Section::Indexes;
        pane.type_into("index_name", "ix_nick");
        pane.type_into("index_column", "nickname");
        match pane.session.action() {
            crate::ddl::Action::CreateIndex { name, columns, .. } => {
                assert_eq!(name, "ix_nick");
                assert_eq!(columns[0].name, "nickname");
            }
            other => panic!("{other:?}"),
        }
        pane.session.section = Section::ForeignKeys;
        pane.type_into("fk_name", "fk_parent");
        pane.type_into("fk_column", "parent_id");
        pane.type_into("fk_ref_table", "parents");
        pane.type_into("fk_ref_column", "id");
        match pane.session.action() {
            crate::ddl::Action::CreateForeignKey { foreign_key, .. } => {
                assert_eq!(foreign_key.name, "fk_parent");
                assert_eq!(foreign_key.columns, vec!["parent_id".to_owned()]);
                assert_eq!(foreign_key.ref_table, "parents");
            }
            other => panic!("{other:?}"),
        }
        pane.session.section = Section::View;
        pane.type_into("view_sql", "select 1");
        match pane.session.action() {
            crate::ddl::Action::ReplaceView { definition, .. } => assert_eq!(definition, "select 1"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sqlite_structure_cancel_leaves_the_file_and_confirm_applies_the_preview() {
        let dir = std::env::temp_dir().join(format!("rillsql-structure-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("notes.sqlite");
        let setup = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
            .await
            .unwrap();
        sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT)").execute(&setup).await.unwrap();
        sqlx::query("CREATE TABLE scratch (id INTEGER PRIMARY KEY)").execute(&setup).await.unwrap();
        setup.close().await;
        let service = std::sync::Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")));
        service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
        let mut session = StructureSession::new(GridTarget {
            connection_id: "notes".into(),
            connection_name: "笔记".into(),
            database: "main".into(),
            schema: String::new(),
            table: "customers".into(),
            engine: "sqlite".into(),
            environment: "dev".into(),
        });
        session.section = Section::Columns;
        session.column_name = "extra".into();
        session.column_type = "TEXT".into();
        let blocked = crate::application::ddl::preview_request(&service, "notes", &session.request().unwrap()).await.unwrap();
        assert!(!blocked.blocked.is_empty(), "缺字段的草稿必须停在预览");
        session.arm_live(blocked);
        assert!(session.banner().contains("笔记") && session.banner().contains("dev") && session.banner().contains("customers"), "{}", session.banner());
        assert!(session.confirm_sql().is_err());
        session.kept_columns = vec![
            ColumnDraft { original: Some("id".into()), name: "id".into(), type_name: "INTEGER".into(), nullable: false, default_sql: String::new(), primary: true, auto: true, dropped: false },
            ColumnDraft { original: Some("name".into()), name: "name".into(), type_name: "TEXT".into(), nullable: true, default_sql: String::new(), primary: false, auto: false, dropped: false },
            ColumnDraft { original: None, name: "extra".into(), type_name: "TEXT".into(), nullable: true, default_sql: String::new(), primary: false, auto: false, dropped: false },
        ];
        let request = session.request().unwrap();
        let plan = crate::application::ddl::preview_request(&service, "notes", &request).await.unwrap();
        assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
        session.arm_live(plan);
        let sql = session.confirm_sql().unwrap();
        session.cancel();
        assert!(session.confirm_sql().is_err());
        let refused = match crate::application::ddl::execute_request(&service, "notes", &request, &sql, false).await {
            Err(message) => message,
            Ok(_) => panic!("未确认的结构变更应当被拒绝"),
        };
        assert!(refused.contains("确认"), "{refused}");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let extra: i64 = sqlx::query_scalar("SELECT count(*) FROM pragma_table_info('customers') WHERE name = 'extra'").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(extra, 0);
        session.open = true;
        let done = crate::application::ddl::execute_request(&service, "notes", &request, &sql, true).await.unwrap();
        assert!(done.blocked.is_empty(), "{}", done.blocked.join("; "));
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let extra: i64 = sqlx::query_scalar("SELECT count(*) FROM pragma_table_info('customers') WHERE name = 'extra'").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(extra, 1);

        session.section = Section::Indexes;
        session.index_name = "ix_name".into();
        session.index_column = "name".into();
        let request = session.request().unwrap();
        let plan = crate::application::ddl::preview_request(&service, "notes", &request).await.unwrap();
        assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
        session.arm_live(plan);
        let index_sql = session.confirm_sql().unwrap();
        session.cancel();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let indexes: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name = 'ix_name'").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(indexes, 0);
        session.open = true;
        crate::application::ddl::execute_request(&service, "notes", &request, &index_sql, true).await.unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let indexes: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name = 'ix_name'").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(indexes, 1);

        session.target.table = "scratch".into();
        session.section = Section::Danger;
        let request = session.request().unwrap();
        let plan = crate::application::ddl::preview_request(&service, "notes", &request).await.unwrap();
        assert!(plan.steps.iter().any(|step| step.sql.to_ascii_uppercase().contains("DROP")), "{:?}", plan.blocked);
        session.arm_live(plan);
        let drop_sql = session.confirm_sql().unwrap();
        session.cancel();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let present: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name = 'scratch'").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(present, 1);
        session.open = true;
        crate::application::ddl::execute_request(&service, "notes", &request, &drop_sql, true).await.unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
            .await
            .unwrap();
        let present: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name = 'scratch'").fetch_one(&pool).await.unwrap();
        pool.close().await;
        assert_eq!(present, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
