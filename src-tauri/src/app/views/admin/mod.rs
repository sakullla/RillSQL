//! 数据库管理。SQLite / PostgreSQL / MySQL / MariaDB 的动作仍经 DDL 预览。

use std::sync::Arc;

use gpui_kit::{
    Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

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

    pub fn preview_sql(&self) -> Vec<String> {
        self.preview.as_ref().map(|plan| plan.steps.iter().map(|step| step.sql.clone()).collect()).unwrap_or_default()
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
}

impl AdminPane {
    pub fn new(services: Arc<AppServices>) -> Self {
        Self { session: AdminSession::new(GridTarget::default()), services }
    }

    pub fn bind(&mut self, target: GridTarget) {
        self.session.inventory = AdminInventory::offline(&target);
        self.session.target = target;
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

impl Render for AdminPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.session.inventory.probed && !self.session.target.connection_id.is_empty() {
            self.session.inventory.probed = true;
            self.refresh_inventory(cx);
        }
        let locale = self.session.locale;
        let sections = self.session.sections();
        let entity = cx.entity();
        let inventory = self.session.inventory.clone();
        let mut column = div()
            .id("admin-pane")
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(div().id("admin-banner").child(SharedString::from(self.session.banner())))
            .child(div().id("admin-source").child(SharedString::from(inventory.source.clone())));
        if sections.maintenance {
            column = column
                .child(kind_button("admin-vacuum", "admin.action.vacuum", "vacuum", entity.clone(), locale))
                .child(admin_box("reindex", "admin.action.reindex", &self.session.reindex_target, entity.clone(), locale))
                .child(kind_button("admin-reindex", "admin.action.reindex", "reindex", entity.clone(), locale));
        }
        if sections.pragma {
            column = column
                .child(admin_box("pragma_value", "admin.action.pragma", &self.session.pragma_value, entity.clone(), locale))
                .child(kind_button("admin-pragma", "admin.action.pragma", "pragma", entity.clone(), locale))
                .child(kind_button("admin-foreign", "admin.action.foreignKeys", "foreign", entity.clone(), locale));
        }
        if sections.attached {
            column = column
                .child(admin_box("attach_path", "admin.action.attach", &self.session.attach_path, entity.clone(), locale))
                .child(admin_box("attach_name", "admin.action.attach", &self.session.attach_name, entity.clone(), locale))
                .child(kind_button("admin-attach", "admin.action.attach", "attach", entity.clone(), locale));
            for name in &inventory.attached {
                column = column.child(SharedString::from(format!("attached {name}")));
            }
        }
        if sections.sequences {
            column = column
                .child(admin_box("sequence", "admin.action.sequence", &self.session.sequence_name, entity.clone(), locale))
                .child(admin_box("sequence_step", "admin.action.sequence", &self.session.sequence_step, entity.clone(), locale))
                .child(kind_button("admin-sequence", "admin.action.sequence", "sequence", entity.clone(), locale));
            for name in &inventory.sequences {
                column = column.child(SharedString::from(format!("sequence {name}")));
            }
        }
        if sections.events {
            column = column
                .child(admin_box("event", "admin.action.event", &self.session.event_name, entity.clone(), locale))
                .child(kind_button("admin-event", "admin.action.event", "event", entity.clone(), locale));
            for name in &inventory.events {
                column = column.child(SharedString::from(format!("event {name}")));
            }
        }
        if sections.extensions {
            if inventory.extensions.is_empty() {
                column = column.child(SharedString::from(format!("extensions {}", inventory.source)));
            }
            for name in &inventory.extensions {
                column = column.child(SharedString::from(format!("extension {name}")));
            }
        }
        if sections.partitions {
            if inventory.partitions.is_empty() {
                column = column.child(SharedString::from(format!("partitions {}", inventory.source)));
            }
            for name in &inventory.partitions {
                column = column.child(SharedString::from(format!("partition {name}")));
            }
        }
        if !sections.pragma && !sections.maintenance && !sections.sequences && !sections.events && !sections.extensions && !sections.partitions && !sections.attached {
            column = column.child(SharedString::from(i18n::text(locale, "admin.section.none")));
        }
        column = column.child(plain_button("admin-preview", "structure.action.preview", entity.clone(), locale));
        if self.session.open {
            column = column.child(
                div()
                    .id("admin-preview-dialog")
                    .role(gpui_kit::Role::Dialog)
                    .aria_label(SharedString::from(i18n::text(locale, "structure.preview.title")))
                    .child(SharedString::from(self.session.banner()))
                    .child(SharedString::from(self.session.preview_sql().join("\n")))
                    .child(SharedString::from(self.session.blocked().join(" ")))
                    .child(plain_button("admin-confirm", "structure.action.confirm", entity.clone(), locale))
                    .child(plain_button("admin-cancel", "structure.action.cancel", entity, locale)),
            );
        }
        if !self.session.note.is_empty() {
            column = column.child(SharedString::from(self.session.note.clone()));
        }
        column
    }
}

fn kind_button(id: &'static str, key: &'static str, kind: &'static str, entity: Entity<AdminPane>, locale: Locale) -> impl IntoElement {
    let label = SharedString::from(i18n::text(locale, key));
    div()
        .id(id)
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .child(label)
        .on_click(move |_, _, cx| {
            entity.update(cx, |pane, cx| {
                pane.choose(kind);
                cx.notify();
            });
        })
}

fn admin_box(field: &'static str, key: &'static str, value: &str, entity: Entity<AdminPane>, locale: Locale) -> impl IntoElement {
    let label = SharedString::from(format!("{} {}", i18n::text(locale, key), value));
    div()
        .id(SharedString::from(format!("admin-field-{field}")))
        .role(gpui_kit::Role::TextInput)
        .aria_label(label.clone())
        .child(label)
        .on_key_down(move |event: &gpui_kit::KeyDownEvent, _, cx| {
            let Some(text) = admin_typed(event) else { return };
            entity.update(cx, |pane, cx| {
                pane.type_into(field, &text);
                cx.notify();
            });
        })
}

fn admin_typed(event: &gpui_kit::KeyDownEvent) -> Option<String> {
    if event.keystroke.modifiers.control || event.keystroke.modifiers.platform || event.keystroke.modifiers.alt {
        return None;
    }
    match event.keystroke.key.as_str() {
        "backspace" => Some("backspace".into()),
        "escape" | "enter" | "tab" | "up" | "down" | "left" | "right" => None,
        _ => event.keystroke.key_char.clone().filter(|text| !text.is_empty()),
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

fn plain_button(id: &'static str, key: &'static str, entity: Entity<AdminPane>, locale: Locale) -> impl IntoElement {
    let label = SharedString::from(i18n::text(locale, key));
    div()
        .id(id)
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .child(label)
        .on_click(move |_, _, cx| {
            entity.update(cx, |pane, cx| match id {
                "admin-preview" => pane.preview(cx),
                "admin-confirm" => pane.confirm(cx),
                _ => {
                    pane.session.cancel();
                    cx.notify();
                }
            });
        })
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
