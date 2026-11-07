//! 对象结构。预览和执行只走 application::ddl，取消预览不执行。

use std::sync::Arc;

use gpui_kit::{
    Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

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
}

impl StructurePane {
    pub fn new(services: Arc<AppServices>) -> Self {
        Self { session: StructureSession::new(GridTarget::default()), services }
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
    fn draft_inputs(&self, entity: Entity<Self>, locale: Locale) -> impl IntoElement {
        let mut row = div().id("structure-draft").flex().gap(px(4.));
        match self.session.section {
            Section::Columns => {
                row = row
                    .child(draft_box("column_name", "structure.field.column", &self.session.column_name, entity.clone(), locale))
                    .child(draft_box("column_type", "structure.field.type", &self.session.column_type, entity, locale));
            }
            Section::Indexes => {
                row = row
                    .child(draft_box("index_name", "structure.field.index", &self.session.index_name, entity.clone(), locale))
                    .child(draft_box("index_column", "structure.section.columns", &self.session.index_column, entity, locale));
            }
            Section::ForeignKeys => {
                row = row
                    .child(draft_box("fk_name", "structure.section.foreign", &self.session.fk_name, entity.clone(), locale))
                    .child(draft_box("fk_column", "structure.field.column", &self.session.fk_column, entity.clone(), locale))
                    .child(draft_box("fk_ref_table", "structure.section.foreign", &self.session.fk_ref_table, entity.clone(), locale))
                    .child(draft_box("fk_ref_column", "structure.field.column", &self.session.fk_ref_column, entity, locale));
            }
            Section::View => {
                row = row.child(draft_box("view_sql", "structure.field.view", &self.session.view_sql, entity, locale));
            }
            Section::Danger => {}
        }
        row
    }
}

fn draft_box(field: &'static str, key: &'static str, value: &str, entity: Entity<StructurePane>, locale: Locale) -> impl IntoElement {
    let label = SharedString::from(format!("{} {}", i18n::text(locale, key), value));
    div()
        .id(SharedString::from(format!("structure-field-{field}")))
        .role(gpui_kit::Role::TextInput)
        .aria_label(label.clone())
        .child(label)
        .on_key_down(move |event: &gpui_kit::KeyDownEvent, _, cx| {
            let Some(text) = typed_text(event) else { return };
            entity.update(cx, |pane, cx| {
                pane.type_into(field, &text);
                cx.notify();
            });
        })
}

fn typed_text(event: &gpui_kit::KeyDownEvent) -> Option<String> {
    if event.keystroke.modifiers.control || event.keystroke.modifiers.platform || event.keystroke.modifiers.alt {
        return None;
    }
    match event.keystroke.key.as_str() {
        "backspace" => Some("backspace".into()),
        "escape" | "enter" | "tab" | "up" | "down" | "left" | "right" => None,
        _ => event.keystroke.key_char.clone().filter(|text| !text.is_empty()),
    }
}

impl Render for StructurePane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locale = self.session.locale;
        let banner = SharedString::from(self.session.banner());
        let entity = cx.entity();
        let mut column = div()
            .id("structure-pane")
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(div().id("structure-banner").child(banner))
            .child(section_button(Section::Columns, "structure.section.columns", entity.clone(), locale))
            .child(section_button(Section::Indexes, "structure.section.indexes", entity.clone(), locale))
            .child(section_button(Section::ForeignKeys, "structure.section.foreign", entity.clone(), locale))
            .child(section_button(Section::View, "structure.section.view", entity.clone(), locale))
            .child(section_button(Section::Danger, "structure.section.danger", entity.clone(), locale))
            .child(self.draft_inputs(entity.clone(), locale))
            .child(action_button("structure-preview", "structure.action.preview", entity.clone(), locale));
        if !self.session.note.is_empty() {
            column = column.child(SharedString::from(self.session.note.clone()));
        }
        if self.session.preview_open() {
            column = column.child(
                div()
                    .id("structure-preview")
                    .role(gpui_kit::Role::Dialog)
                    .aria_label(SharedString::from(i18n::text(locale, "structure.preview.title")))
                    .child(SharedString::from(self.session.banner()))
                    .child(SharedString::from(self.session.preview_sql().join("\n")))
                    .child(SharedString::from(self.session.blocked().join(" ")))
                    .child(action_button("structure-confirm", "structure.action.confirm", entity.clone(), locale))
                    .child(action_button("structure-cancel", "structure.action.cancel", entity, locale)),
            );
        }
        column
    }
}

fn section_button(section: Section, key: &'static str, entity: Entity<StructurePane>, locale: Locale) -> impl IntoElement {
    let label = SharedString::from(i18n::text(locale, key));
    div()
        .id(key)
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .child(label)
        .on_click(move |_, _, cx| {
            entity.update(cx, |pane, cx| {
                pane.session.section = section;
                pane.session.cancel();
                cx.notify();
            });
        })
}

fn action_button(id: &'static str, key: &'static str, entity: Entity<StructurePane>, locale: Locale) -> impl IntoElement {
    let label = SharedString::from(i18n::text(locale, key));
    div()
        .id(id)
        .role(gpui_kit::Role::Button)
        .aria_label(label.clone())
        .child(label)
        .on_click(move |_, _, cx| {
            entity.update(cx, |pane, cx| match id {
                "structure-preview" => pane.preview(cx),
                "structure-confirm" => pane.confirm(cx),
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
