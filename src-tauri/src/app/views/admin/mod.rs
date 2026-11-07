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

pub struct AdminSession {
    pub target: GridTarget,
    pub operation: AdminOp,
    preview: Option<Plan>,
    live: bool,
    open: bool,
    pub locale: Locale,
    pub note: String,
}

impl AdminSession {
    pub fn new(target: GridTarget) -> Self {
        Self {
            target,
            operation: AdminOp::Vacuum,
            preview: None,
            live: false,
            open: false,
            locale: Locale::ZhCn,
            note: String::new(),
        }
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
        self.session.target = target;
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
                let _ = cx.update(|cx| {
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
                let _ = cx.update(|cx| {
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
        let locale = self.session.locale;
        let sections = self.session.sections();
        let entity = cx.entity();
        let mut column = div()
            .id("admin-pane")
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(div().id("admin-banner").child(SharedString::from(self.session.banner())));
        if sections.maintenance {
            column = column.child(op_button("admin-vacuum", "admin.action.vacuum", AdminOp::Vacuum, entity.clone(), locale));
        }
        if sections.pragma {
            column = column.child(op_button(
                "admin-pragma",
                "admin.action.pragma",
                AdminOp::SetPragma { pragma: "user_version".into(), value: "7".into() },
                entity.clone(),
                locale,
            ));
        }
        if sections.sequences {
            column = column.child(op_button(
                "admin-sequence",
                "admin.action.sequence",
                AdminOp::AlterSequence { sequence: "ids".into(), increment: Some(2) },
                entity.clone(),
                locale,
            ));
        }
        if sections.events {
            column = column.child(op_button(
                "admin-event",
                "admin.action.event",
                AdminOp::SetEvent { event: "sweep".into(), enabled: false },
                entity.clone(),
                locale,
            ));
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

fn op_button(id: &'static str, key: &'static str, operation: AdminOp, entity: Entity<AdminPane>, locale: Locale) -> impl IntoElement {
    let label = SharedString::from(i18n::text(locale, key));
    div()
        .id(id)
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .child(label)
        .on_click(move |_, _, cx| {
            let operation = operation.clone();
            entity.update(cx, |pane, cx| {
                pane.session.operation = operation;
                pane.session.cancel();
                cx.notify();
            });
        })
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
