use super::*;
use crate::ddl::{Column, ColumnDraft, DdlEngine, ForeignKey, IncomingForeignKey, Index, IndexColumn, Snapshot};

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

#[test]
fn ddl_tab_never_shows_the_placeholder_comment_as_the_definition() {
    let mut mysql = Snapshot::bare(DdlEngine::Mysql, "shop", "", "task_list_track_code");
    mysql.columns = vec![column("id", "bigint(20) unsigned", true), column("track_code", "varchar(128)", false)];
    mysql.table_sql = "-- shop.task_list_track_code".to_owned();
    let (text, _) = definition_text(&mysql);
    assert!(text.contains("CREATE TABLE"), "{text}");
    assert!(text.contains("track_code"), "{text}");
    mysql.table_sql = "CREATE TABLE `task_list_track_code` (\n  `id` bigint unsigned NOT NULL\n);".to_owned();
    assert_eq!(definition_text(&mysql).0, mysql.table_sql);
    assert!(is_placeholder_definition("-- a.b\n\n-- c"));
    assert!(!is_placeholder_definition("-- note\nCREATE TABLE t (id int)"));
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
fn previewing_an_untouched_table_is_a_neutral_no_op() {
    let mut session = StructureSession::new(target("postgres", "orders"));
    let mut snapshot = Snapshot::bare(DdlEngine::Postgres, "app", "public", "orders");
    let mut pk = column("id", "bigint", true);
    pk.default_sql = Some("nextval('orders_id_seq'::regclass)".to_owned());
    snapshot.columns = vec![pk, column("status", "text", false)];
    session.kept_columns = column_drafts(&snapshot);
    session.arm_fixture(&snapshot);
    assert!(session.preview_sql().is_empty(), "{:?}", session.preview_sql());
    assert!(session.preview_unchanged(), "{:?}", session.blocked());
    session.kept_columns[1].name = "state".into();
    session.arm_fixture(&snapshot);
    assert!(!session.preview_unchanged());
    assert!(session.blocked().is_empty(), "{:?}", session.blocked());
    assert_eq!(session.preview_sql().len(), 1);
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
fn dangerous_ddl_needs_a_second_confirm_and_target_change_drops_the_draft() {
    let mut session = StructureSession::new(target("sqlite", "customers"));
    session.target.database = "main".into();
    session.target.schema.clear();
    session.section = Section::Danger;
    let snapshot = Snapshot::bare(DdlEngine::Sqlite, "main", "", "customers");
    session.arm_fixture(&snapshot);
    assert!(session.requires_second_confirm(), "DROP 必须进入二次确认");
    assert!(session.preview_risks().iter().any(|(level, _)| level == "danger"));
    assert!(session.preview_rule().contains("逐条执行"));
    assert!(session.preview_target().contains("customers"), "{}", session.preview_target());
    session.arm_confirm();
    assert!(session.armed());
    assert!(!session.requires_second_confirm(), "第二次确认后不再拦");
    session.cancel();
    assert!(!session.armed(), "取消后必须重置二次确认");

    let dir = std::env::temp_dir().join(format!("rillsql-structure-reset-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(
        crate::connections::Service::new_for_test(dir.join("connections.sqlite")),
    )));
    let mut pane = StructurePane::new(services);
    pane.bind(target("sqlite", "customers"));
    pane.type_into("column_name", "nickname");
    pane.session.section = Section::Indexes;
    pane.session.arm_fixture(&snapshot);
    assert!(pane.session.preview_open());
    pane.bind(target("sqlite", "orders"));
    assert_eq!(pane.session.target.table, "orders");
    assert!(pane.session.column_name.is_empty(), "换表不能带走上一张表的列名");
    assert_eq!(pane.session.section, Section::Columns);
    assert!(!pane.session.preview_open(), "换表不能带走上一张表的预览");
    assert!(pane.session.note().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn notes_and_load_errors_follow_a_later_locale_switch() {
    let cjk = crate::ddl::english::has_cjk;
    let mut session = StructureSession::new(target("sqlite", "customers"));
    session.note = Note::raw("这条连接还没有打开，没有另开新的连接。".to_owned());
    assert!(session.note().contains("没有打开"), "{}", session.note());
    session.locale = Locale::En;
    assert!(session.note().contains("isn't open") && !cjk(&session.note()), "{}", session.note());
    session.locale = Locale::ZhCn;
    assert!(session.note().contains("没有打开"), "{}", session.note());

    session.note = Note::Raw(vec!["生成计划失败".to_owned(), "找不到表或视图 orders".to_owned()]);
    session.locale = Locale::En;
    assert_eq!(session.note(), "Generating the plan failed Table or view orders was not found");

    session.target.database = "main".into();
    session.target.schema.clear();
    session.section = Section::Danger;
    session.locale = Locale::ZhCn;
    session.arm_fixture(&Snapshot::bare(DdlEngine::Sqlite, "main", "", "customers"));
    assert_eq!(session.note(), i18n::text(Locale::ZhCn, "admin.preview.unconnected"));
    session.arm_confirm();
    assert!(session.note().contains("再次点击"), "{}", session.note());
    session.locale = Locale::En;
    assert!(session.note().contains("Confirm again") && !cjk(&session.note()), "{}", session.note());
    assert!(!cjk(&session.confirm_sql().unwrap_err()), "未连接的拒绝原因按英文给出");

    let dir = std::env::temp_dir().join(format!("rillsql-structure-locale-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(
        crate::connections::Service::new_for_test(dir.join("connections.sqlite")),
    )));
    let mut pane = StructurePane::new(services);
    pane.bind(target("sqlite", "customers"));
    pane.load_error = "找不到表或视图 customers".to_owned();
    assert!(pane.load_error_text().contains("找不到"), "{}", pane.load_error_text());
    pane.set_locale(Locale::En);
    assert_eq!(pane.load_error_text(), "Table or view customers was not found");
    pane.set_locale(Locale::ZhCn);
    assert!(pane.load_error_text().contains("找不到"), "{}", pane.load_error_text());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn typed_structure_fields_are_the_preview_draft() {
    let dir = std::env::temp_dir().join(format!("rillsql-structure-draft-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(
        crate::connections::Service::new_for_test(dir.join("connections.sqlite")),
    )));
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
        ColumnDraft {
            original: Some("id".into()),
            name: "id".into(),
            type_name: "INTEGER".into(),
            nullable: false,
            default: crate::ddl::DefaultSpec::None,
            comment: String::new(),
            primary: true,
            auto: true,
            dropped: false,
        },
        ColumnDraft {
            original: Some("name".into()),
            name: "name".into(),
            type_name: "TEXT".into(),
            nullable: true,
            default: crate::ddl::DefaultSpec::None,
            comment: String::new(),
            primary: false,
            auto: false,
            dropped: false,
        },
        ColumnDraft {
            original: None,
            name: "extra".into(),
            type_name: "TEXT".into(),
            nullable: true,
            default: crate::ddl::DefaultSpec::None,
            comment: String::new(),
            primary: false,
            auto: false,
            dropped: false,
        },
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

#[test]
fn tabs_follow_the_object_and_original_names_let_alter_precheck_proceed() {
    let mut session = StructureSession::new(target("postgres", "orders"));
    assert_eq!(
        session.visible_sections(),
        vec![Section::Columns, Section::Indexes, Section::ForeignKeys, Section::Ddl, Section::Danger]
    );
    session.object_kind = "view".into();
    assert_eq!(session.visible_sections(), vec![Section::Columns, Section::View, Section::Danger]);
    assert!(!session.visible_sections().contains(&Section::Ddl));
    session.mode = StructureMode::Create;
    assert_eq!(session.visible_sections(), vec![Section::Columns]);

    let mut snapshot = Snapshot::bare(DdlEngine::Postgres, "app", "public", "orders");
    snapshot.kind = "table".into();
    snapshot.columns.push(column("name", "text", false));
    let mut drafts = column_drafts(&snapshot);
    assert_eq!(drafts[0].original.as_deref(), Some("name"));
    drafts[0].name = "title".into();
    let plan = preview_plan(&snapshot, &Action::AlterColumns { table: "orders".into(), columns: drafts });
    assert!(!plan.blocked.iter().any(|item| item.contains("没有出现")), "{:?}", plan.blocked);

    let mut created = StructureSession::new(target("sqlite", "ignored"));
    created.mode = StructureMode::Create;
    created.table_name = "notes".into();
    created.kept_columns = vec![default_id("sqlite")];
    match created.action() {
        Action::CreateTable { name, columns, .. } => {
            assert_eq!(name, "notes");
            assert_eq!(columns[0].name, "id");
            assert!(columns[0].primary && columns[0].original.is_none());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn indexes_and_foreign_keys_preview_drops_unique_order_and_actions() {
    let mut session = StructureSession::new(target("postgres", "orders"));
    session.section = Section::Indexes;
    session.index_name = "ix_note".into();
    session.index_unique = true;
    session.index_columns = vec![
        IndexColumnDraft { name: "note".into(), descending: false },
        IndexColumnDraft { name: "id".into(), descending: true },
    ];
    match session.action() {
        Action::CreateIndex { unique, columns, .. } => {
            assert!(unique);
            assert_eq!(columns.len(), 2);
            assert!(columns[1].descending);
        }
        other => panic!("{other:?}"),
    }
    session.intent = EditIntent::DropIndex { name: "orders_pkey".into() };
    assert!(matches!(session.action(), Action::DropIndex { name, .. } if name == "orders_pkey"));

    session.section = Section::ForeignKeys;
    session.intent = EditIntent::Apply;
    session.fk_name = "orders_parent".into();
    session.fk_column = "parent_id".into();
    session.fk_ref_schema = "public".into();
    session.fk_ref_table = "parents".into();
    session.fk_ref_column = "id".into();
    session.fk_on_delete = "CASCADE".into();
    session.fk_on_update = "RESTRICT".into();
    match session.action() {
        Action::CreateForeignKey { foreign_key, .. } => {
            assert_eq!(foreign_key.ref_schema, "public");
            assert_eq!(foreign_key.on_delete, "CASCADE");
            assert_eq!(foreign_key.on_update, "RESTRICT");
        }
        other => panic!("{other:?}"),
    }
    session.intent = EditIntent::DropForeignKey { name: "orders_parent".into() };
    assert!(matches!(session.action(), Action::DropForeignKey { .. }));
    assert!(fk_action_choices("postgres").contains(&"SET DEFAULT"));
    assert!(!fk_action_choices("mysql").contains(&"SET DEFAULT"));
    assert_eq!(fk_action_choices("mysql"), fk_action_choices("mariadb"));
}

#[test]
fn production_danger_needs_the_object_name_and_a_failed_note_keeps_the_draft() {
    let mut session = StructureSession::new(target("sqlite", "customers"));
    session.target.environment = "prod".into();
    session.section = Section::Danger;
    assert!(!session.danger_name_ok());
    session.danger_name = "customers".into();
    assert!(session.danger_name_ok());
    session.danger_name = "other".into();
    assert!(!session.danger_name_ok());

    let dir = std::env::temp_dir().join(format!("rillsql-structure-keep-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(
        crate::connections::Service::new_for_test(dir.join("connections.sqlite")),
    )));
    let mut pane = StructurePane::new(services);
    pane.begin_create(target("sqlite", "customers"));
    assert_eq!(pane.session.mode, StructureMode::Create);
    assert_eq!(pane.session.kept_columns[0].name, "id");
    pane.type_into("table_name", "notes");
    pane.type_into("column_name", "nickname");
    pane.session.note = Note::raw("生成计划失败".into());
    assert_eq!(pane.session.table_name, "notes");
    assert_eq!(pane.session.column_name, "nickname");
    assert_eq!(pane.session.kept_columns[0].name, "id");
    match pane.session.action() {
        Action::CreateTable { name, columns, .. } => {
            assert_eq!(name, "notes");
            assert!(columns.iter().any(|column| column.name == "nickname"));
        }
        other => panic!("{other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn snapshot_with(columns: usize) -> Snapshot {
    let mut snapshot = Snapshot::bare(DdlEngine::Postgres, "app", "public", "orders");
    snapshot.kind = "table".into();
    for index in 0..columns {
        snapshot.columns.push(column(&format!("col_{index}"), if index == 0 { "bigint" } else { "integer" }, index == 0));
    }
    snapshot
}

fn offline_target(table: &str) -> GridTarget {
    GridTarget {
        connection_id: String::new(),
        connection_name: String::new(),
        database: "app".into(),
        schema: "public".into(),
        table: table.into(),
        engine: "postgres".into(),
        environment: "dev".into(),
    }
}

fn pane_with_snapshot(services: std::sync::Arc<crate::application::AppServices>, columns: usize) -> StructurePane {
    let mut pane = StructurePane::new(services);
    pane.bind(offline_target("orders"));
    let snapshot = snapshot_with(columns);
    pane.session.object_kind = snapshot.kind.clone();
    pane.session.kept_columns = column_drafts(&snapshot);
    pane.session.index_columns.clear();
    pane.drafts_ready = true;
    pane.snapshot = Some(snapshot);
    pane
}

fn offline_services(name: &str) -> (std::sync::Arc<crate::application::AppServices>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("rillsql-structure-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(
        crate::connections::Service::new_for_test(dir.join("connections.sqlite")),
    )));
    (services, dir)
}

#[test]
fn structure_lists_share_one_row_height_and_fit_the_narrowest_window() {
    for metrics in [kit::Metrics::compact(), kit::Metrics::standard(), kit::Metrics::relaxed()] {
        let spec = TableSpec::of(metrics);
        assert_eq!(spec.header_h, metrics.row_h, "表头高取 row_h");
        assert_eq!(spec.row_h, metrics.row_h, "行高取 row_h，不再各表 +2");
        assert_eq!(spec.pad_x, TableSpec::PAD_X, "三张表共用同一左右内边距");
        for section in [Section::Columns, Section::Indexes, Section::ForeignKeys] {
            let columns = table_columns(section);
            assert!(columns.len() >= 3, "{section:?} 至少要能显示标识、内容与状态列");
            let total: f32 = columns.iter().map(|column| column.min_w).sum();
            let available = 640. - 2. * metrics.page_pad - 2. * spec.pad_x;
            assert!(total <= available, "{section:?}: 最小列宽之和 {total} 超过 640 宽可用的 {available}");
            assert!(columns.iter().all(|column| column.weight > 0.), "{section:?}: 每列都要能伸缩");
        }
    }
    assert_ne!(TableSpec::of(kit::Metrics::compact()).row_h, TableSpec::of(kit::Metrics::standard()).row_h);
}

#[test]
fn empty_lists_have_no_header_and_loading_wins_over_error() {
    assert_eq!(list_state(false, "", 0), ListState::Empty);
    assert!(!shows_table_header(ListState::Empty), "空列表不能保留表头加破折号");
    assert_eq!(list_state(false, "", 3), ListState::Rows);
    assert!(shows_table_header(ListState::Rows));
    assert_eq!(list_state(true, "", 0), ListState::Loading);
    assert_eq!(list_state(true, "找不到表或视图 orders", 3), ListState::Loading, "加载优先于错误");
    assert_eq!(list_state(false, "找不到表或视图 orders", 0), ListState::Error);
}

#[test]
fn production_warning_name_gates_the_confirm_button() {
    let mut session = StructureSession::new(target("postgres", "orders"));
    session.target.environment = "prod".into();
    session.section = Section::Danger;
    session.arm_fixture(&snapshot_with(1));
    assert!(session.confirm_blocked(), "生产环境未输入对象名时确认必须禁用");
    assert!(session.preview_dangerous());
    session.danger_name = "orders".into();
    assert!(!session.confirm_blocked());
    session.section = Section::Columns;
    assert!(!session.confirm_blocked(), "非危险页签不受对象名约束");
    assert_eq!(session.object_label(), "orders");

    session.mode = StructureMode::Create;
    session.table_name = "notes".into();
    assert_eq!(session.object_label(), "notes", "新建表确认层标题用输入的表名");
}

#[gpui_kit::test]
fn primary_action_stays_outside_the_scrolled_body_at_every_width(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = offline_services("actions");
    cx.update(gpui_kit::init);
    for width in [1360., 900., 640.] {
        let (window, _pane) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds {
                origin: gpui_kit::Point::default(),
                size: gpui_kit::size(gpui_kit::px(width), gpui_kit::px(900.)),
            };
            let (window, pane) = gpui_kit::open_window(
                gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |_window, cx| {
                    cx.new(|_| {
                        // 预览按钮只在有改动时出现：先改一处。
                        let mut pane = pane_with_snapshot(services.clone(), 26);
                        pane.session.kept_columns[1].name = "renamed_col".into();
                        pane
                    })
                },
            )
            .expect("structure pane");
            (window, pane)
        });
        let (button, body, pane_bounds, row_h) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (
                    window.find("structure-preview").bounds(),
                    window.find("structure-body").bounds(),
                    window.find("structure-pane").bounds(),
                    window.find("structure-row-columns-0").bounds(),
                )
            })
            .unwrap();
        let bottom = f32::from(button.origin.y + button.size.height);
        assert!(bottom <= f32::from(pane_bounds.size.height) + 0.5, "{width}: 主操作被裁在窗口外（底 {bottom}）");
        assert!(
            f32::from(button.origin.y) >= f32::from(body.origin.y + body.size.height) - 0.5,
            "{width}: 主操作不能落在滚动内容里"
        );
        let expected = cx.update(|cx| kit::Metrics::of(cx).row_h);
        assert!((f32::from(row_h.size.height) - expected).abs() < 0.5, "{width}: 行高应为 Metrics::row_h {expected}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn row_menu_actions_only_edit_the_draft_and_never_execute(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    let (services, dir) = offline_services("menu");
    let pane = cx.update(|cx| cx.new(|_| pane_with_snapshot(services, 2)));
    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            let mono = SharedString::from("mono");
            let rows = pane.list_rows(Section::Columns, Locale::ZhCn, &mono, &Palette::light());
            assert_eq!(rows.len(), 2, "两列就该有两个可右键的行");
            let ids = rows[1].menu.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>();
            assert_eq!(ids, vec!["copy-column-1", "drop-column-1"], "字段行菜单：复制名称 + 标记删除");
            assert!(rows[1].menu.iter().all(|entry| !entry.disabled));

            pane.apply_row_action(RowAction::Copy("col_1".into()), cx);
            pane.apply_row_action(RowAction::ToggleColumnDrop(1), cx);
            assert!(pane.session.kept_columns[1].dropped, "标记删除落在草稿上");
            assert!(!pane.session.preview_open(), "菜单动作不能绕过预览");
            assert!(!pane.session.armed(), "也不能把计划推到可执行状态");
            pane.apply_row_action(RowAction::ToggleColumnDrop(1), cx);
            assert!(!pane.session.kept_columns[1].dropped, "再点一次取消删除标记");
        });
    });
    let copied = cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("col_1"), "复制名称进剪贴板");
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn structure_lists_follow_density_in_both_themes(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = offline_services("density");
    for dark in [false, true] {
        for metrics in [kit::Metrics::compact(), kit::Metrics::standard()] {
            let expected = metrics.row_h;
            cx.update(|cx| {
                gpui_kit::init(cx);
                kit::apply_theme(dark, cx);
                metrics.publish(cx);
            });
            let (window, _pane) = cx.update(|cx| {
                let bounds = gpui_kit::Bounds {
                    origin: gpui_kit::Point::default(),
                    size: gpui_kit::size(gpui_kit::px(900.), gpui_kit::px(900.)),
                };
                let (window, pane) = gpui_kit::open_window(
                    gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
                    cx,
                    |_window, cx| {
                        cx.new(|_| {
                            let mut pane = pane_with_snapshot(services.clone(), 3);
                            pane.session.kept_columns[2].name = "renamed_col".into();
                            pane
                        })
                    },
                )
                .expect("structure pane");
                (window, pane)
            });
            let (row, head, action) = cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    (
                        window.find("structure-row-columns-0").bounds(),
                        window.find("structure-table-head").bounds(),
                        window.find("structure-preview").visible(),
                    )
                })
                .unwrap();
            let theme = if dark { "深色" } else { "浅色" };
            assert!((f32::from(row.size.height) - expected).abs() < 0.5, "{theme}: 行高 {} 应为 {expected}", f32::from(row.size.height));
            assert!((f32::from(head.size.height) - expected).abs() < 0.5, "{theme}: 表头高应为 {expected}");
            assert!(action, "{theme}: 主操作在两种密度两种主题下都在位");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compact_field_widths_fit_the_narrowest_content_column() {
    let rail = crate::app::shell::layout::RAIL_WIDTH as f32;
    for metrics in [kit::Metrics::compact(), kit::Metrics::standard(), kit::Metrics::relaxed()] {
        let content_w = 640. - rail - 2. * metrics.page_pad - 2.;
        for create in [false, true] {
            let widths = FieldWidths::compact_for(create);
            let needed = widths.min_total(create);
            assert!(needed <= content_w, "{metrics:?} create={create}: 紧凑行需要 {needed}，640 开侧栏只有 {content_w}");
        }
        let widths = FieldWidths::compact();
        assert_eq!(FieldWidths::of(content_w, true).min_total(true), widths.min_total(true), "{metrics:?}: 窄内容列应取紧凑档");
    }
    assert!(FieldWidths::wide().min_total(false) > FieldWidths::compact_for(false).min_total(false));
    for (content_w, create) in [(1040., false), (1040., true), (2400., false)] {
        let widths = FieldWidths::of(content_w, create);
        assert!(widths.default > FieldWidths::wide().default, "{content_w}: 默认值列应变宽");
        assert!(widths.default <= FieldWidths::wide().default + 260.);
        assert!(widths.min_total(create) <= content_w, "{content_w}: 行宽 {} 超出", widths.min_total(create));
    }
}

#[gpui_kit::test]
fn empty_index_list_replaces_the_table_with_an_empty_state(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = offline_services("empty-indexes");
    cx.update(gpui_kit::init);
    let (window, _pane) = cx.update(|cx| {
        let bounds = gpui_kit::Bounds {
            origin: gpui_kit::Point::default(),
            size: gpui_kit::size(gpui_kit::px(900.), gpui_kit::px(900.)),
        };
        let (window, pane) = gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |_window, cx| {
                cx.new(|_| {
                    let mut pane = pane_with_snapshot(services.clone(), 2);
                    pane.session.section = Section::Indexes;
                    pane
                })
            },
        )
        .expect("structure pane");
        (window, pane)
    });
    let (head, empty, dash_rows) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.try_find("structure-table-head").map(|head| head.visible()),
                window.try_find("empty-state").map(|block| block.visible()),
                window.find_all("structure-row-indexes-0").len(),
            )
        })
        .unwrap();
    assert!(head.is_none(), "空索引列表不能保留表头");
    assert_eq!(empty, Some(true), "空索引列表要换成空态块");
    assert_eq!(dash_rows, 0, "空列表不能留占位行");
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn danger_dialog_keeps_its_actions_visible_in_a_short_narrow_window(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = offline_services("dialog");
    cx.update(gpui_kit::init);
    let (window, _pane) = cx.update(|cx| {
        let bounds = gpui_kit::Bounds {
            origin: gpui_kit::Point::default(),
            size: gpui_kit::size(gpui_kit::px(640.), gpui_kit::px(640.)),
        };
        let (window, pane) = gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |_window, cx| {
                cx.new(|_| {
                    let mut pane = pane_with_snapshot(services.clone(), 8);
                    pane.session.section = Section::Danger;
                    let snapshot = snapshot_with(8);
                    pane.session.arm_fixture(&snapshot);
                    pane
                })
            },
        )
        .expect("structure pane");
        (window, pane)
    });
    let (dialog, body, confirm, cancel, window_size) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.find("structure-preview-dialog").bounds(),
                window.find("structure-preview-body").bounds(),
                window.find("structure-confirm").bounds(),
                window.find("structure-cancel").bounds(),
                window.find("structure-pane").bounds(),
            )
        })
        .unwrap();
    let limit = f32::from(window_size.size.height) * 0.92 + 1.;
    assert!(f32::from(dialog.size.height) <= limit, "确认层高 {} 超出窗口可用高度 {limit}", f32::from(dialog.size.height));
    assert!(f32::from(confirm.size.height) > 0., "确认按钮要真的画出来");
    for (name, bounds) in [("确认", confirm), ("取消", cancel)] {
        let bottom = f32::from(bounds.origin.y + bounds.size.height);
        assert!(bottom <= f32::from(window_size.size.height) + 0.5, "{name}按钮被裁在窗口外（底 {bottom}）");
    }
    assert!(
        f32::from(body.size.height) < f32::from(dialog.size.height),
        "正文与页脚分工：正文滚动，页脚固定"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_structure_copy_key_used_in_the_source_exists_in_the_catalog() {
    let catalog = i18n::catalog_keys();
    let prefixes = ["structure.", "ui.structure.", "common.", "admin.", "catalog."];
    let mut checked = 0;
    for literal in string_literals(include_str!("mod.rs")) {
        if prefixes.contains(&literal.as_str()) || !prefixes.iter().any(|prefix| literal.starts_with(prefix)) {
            continue;
        }
        if literal.contains('{') || literal.contains(' ') {
            continue;
        }
        assert!(catalog.contains(&literal.as_str()), "{literal:?} 被当作文案键使用，但字典里没有它");
        checked += 1;
    }
    assert!(checked >= 20, "只扫到 {checked} 个文案键，扫描本身可能失效了");
}

fn string_literals(source: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut chars = source.chars();
    while let Some(ch) = chars.next() {
        if ch != '"' {
            continue;
        }
        let mut literal = String::new();
        let mut escaped = false;
        for inner in chars.by_ref() {
            if escaped {
                escaped = false;
                literal.push(inner);
                continue;
            }
            match inner {
                '\\' => escaped = true,
                '"' => break,
                _ => literal.push(inner),
            }
        }
        literals.push(literal);
    }
    literals
}

#[gpui_kit::test]
fn an_unopened_connection_offers_connect_instead_of_an_error(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = offline_services("unopened");
    cx.update(gpui_kit::init);
    let (window, pane) = cx.update(|cx| {
        gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, |_window, cx| {
            cx.new(|_| {
                let mut pane = pane_with_snapshot(services.clone(), 2);
                pane.session.target.connection_id = "closed-connection".into();
                pane.snapshot = None;
                pane.session.kept_columns.clear();
                pane.drafts_ready = false;
                pane.loaded_for = "closed-connection|app|public|orders".into();
                pane.load_error = "这条连接还没有打开，没有另开新的连接。".to_owned();
                pane
            })
        })
        .expect("structure pane")
    });
    let (error, empty, label) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.try_find("error-state").is_some(),
                window.try_find("empty-state").is_some(),
                window.find("structure-retry").label().map(str::to_owned),
            )
        })
        .unwrap();
    assert!(!error, "连接没打开不是读取错误");
    assert!(empty, "给中性的说明");
    assert_eq!(label.as_deref(), Some(i18n::text(Locale::ZhCn, "grid.action.connectRead").as_str()));
    cx.update_window(window, |_, window, cx| window.click("structure-retry", cx)).unwrap();
    assert_eq!(cx.update(|cx| pane.update(cx, |pane, _| pane.take_connect_request())).as_deref(), Some("closed-connection"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui_kit::test]
fn definition_tabs_share_the_loading_and_load_failure_states(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = offline_services("definition-tabs");
    cx.update(gpui_kit::init);
    for section in [Section::Ddl, Section::Danger, Section::View] {
        let (window, _pane) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds {
                origin: gpui_kit::Point::default(),
                size: gpui_kit::size(gpui_kit::px(900.), gpui_kit::px(900.)),
            };
            gpui_kit::open_window(
                gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |_window, cx| {
                    cx.new(|_| {
                        let mut pane = pane_with_snapshot(services.clone(), 2);
                        pane.session.section = section;
                        pane.snapshot = None;
                        pane.loading = true;
                        pane
                    })
                },
            )
            .expect("structure pane")
        });
        let (skeleton, empty) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (window.try_find("skeleton-state").map(|block| block.visible()), window.try_find("empty-state").is_some())
            })
            .unwrap();
        assert_eq!(skeleton, Some(true), "{section:?}: 读取中要给骨架");
        assert!(!empty, "{section:?}: 读取中不能闪「没有可显示的 SQL 定义」");

        let (window, pane) = cx.update(|cx| {
            let bounds = gpui_kit::Bounds {
                origin: gpui_kit::Point::default(),
                size: gpui_kit::size(gpui_kit::px(900.), gpui_kit::px(900.)),
            };
            gpui_kit::open_window(
                gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
                cx,
                |_window, cx| {
                    cx.new(|_| {
                        let mut pane = pane_with_snapshot(services.clone(), 2);
                        pane.session.section = section;
                        pane.snapshot = None;
                        pane.load_error = "找不到表或视图 orders".to_owned();
                        pane
                    })
                },
            )
            .expect("structure pane")
        });
        let (error, empty, label, detail) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (
                    window.try_find("error-state").map(|block| block.visible()),
                    window.try_find("empty-state").is_some(),
                    window.find("structure-retry").label().map(str::to_owned),
                    window.find("error-state-detail").label().map(str::to_owned),
                )
            })
            .unwrap();
        assert_eq!(error, Some(true), "{section:?}: 读取失败要给错误态");
        assert!(!empty, "{section:?}: 读取失败不能退化成「没有可显示的 SQL 定义」");
        assert_eq!(label.as_deref(), Some("重试"), "{section:?}: 重试按钮文案要翻译过，不能是键名");
        assert!(detail.is_none() || detail.as_deref() != Some("structure.empty.definition"));
        cx.update_window(window, |_, window, cx| window.click("structure-retry", cx)).unwrap();
        assert!(cx.update(|cx| pane.read(cx).load_error_text().is_empty()), "{section:?}: 重试要清掉失败原因");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn choice_matching_separates_no_options_from_no_match() {
    let choices: Vec<String> = ["orders", "Order_Items", "customers", "products"].iter().map(|item| (*item).to_owned()).collect();
    let all: Vec<&String> = choices.iter().collect();
    assert_eq!(match_choices(&[], ""), ChoiceMatch::Unavailable, "没有选项");
    assert_eq!(match_choices(&[], "orders"), ChoiceMatch::Unavailable, "有输入但原本没有选项，仍是无可用项而不是无命中");
    assert_eq!(match_choices(&choices, ""), ChoiceMatch::Items(all.clone()), "空输入列全部");
    assert_eq!(match_choices(&choices, "   "), ChoiceMatch::Items(all.clone()), "只有空白等同空输入");
    assert_eq!(match_choices(&choices, "orders"), ChoiceMatch::Items(all.clone()), "已是精确值时列全部，便于换一个");
    assert_eq!(match_choices(&choices, " orders "), ChoiceMatch::Items(all.clone()), "精确值忽略首尾空白");
    assert_eq!(match_choices(&choices, "ORDER"), ChoiceMatch::Items(vec![&choices[0], &choices[1]]), "大小写不敏感的部分命中");
    assert_eq!(match_choices(&choices, "omer"), ChoiceMatch::Items(vec![&choices[2]]));
    assert_eq!(match_choices(&choices, "zzz"), ChoiceMatch::NoMatch, "无命中不能退回完整列表");
    assert_eq!(match_choices(&choices, ""), ChoiceMatch::Items(all));
    for locale in [Locale::ZhCn, Locale::En] {
        let empty = i18n::text(locale, "structure.choice.empty");
        let no_match = i18n::text(locale, "structure.choice.noMatch");
        assert_ne!(empty, no_match, "{locale:?}: 无可用项与无命中要用不同说明");
        assert_ne!(i18n::text(locale, "structure.choice.noMatchHint"), "structure.choice.noMatchHint");
    }
    assert_ne!(i18n::text(Locale::ZhCn, "structure.choice.noMatch"), i18n::text(Locale::En, "structure.choice.noMatch"));
}

#[gpui_kit::test]
fn fk_table_menu_shows_no_match_keeps_input_and_recovers_after_clearing(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::{TestQueryExt as _, TestWindowExt};
    let (services, dir) = offline_services("choice-menu");
    cx.update(gpui_kit::init);
    let tables: Vec<String> = (0..60).map(|index| format!("table_{index:02}")).collect();
    let (window, pane) = cx.update(|cx| {
        let bounds = gpui_kit::Bounds {
            origin: gpui_kit::Point::default(),
            size: gpui_kit::size(gpui_kit::px(1360.), gpui_kit::px(900.)),
        };
        gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |_window, cx| {
                cx.new(|_| {
                    let mut pane = pane_with_snapshot(services.clone(), 3);
                    pane.session.section = Section::ForeignKeys;
                    pane.composer_open = true;
                    pane.fk_tables = tables.clone();
                    pane.session.fk_ref_table = "zzz".into();
                    pane.session.fk_column = "col_1".into();
                    pane
                })
            },
        )
        .expect("structure pane")
    });
    let menu_items = |window: &mut Window| {
        window
            .find_all_by_role(gpui_kit::Role::MenuItem)
            .into_iter()
            .map(|item| (item.label().unwrap_or_default().to_owned(), item.disabled().unwrap_or(false)))
            .collect::<Vec<_>>()
    };

    let items = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("structure-fk-tables", cx);
            window.render_frame(cx);
            menu_items(window)
        })
        .unwrap();
    let no_match = i18n::text(Locale::ZhCn, "structure.choice.noMatch");
    let hint = i18n::text(Locale::ZhCn, "structure.choice.noMatchHint");
    assert_eq!(
        items.iter().map(|(label, _)| label.clone()).collect::<Vec<_>>(),
        vec![no_match.clone(), hint.clone()],
        "无命中不能退回完整列表: {items:?}"
    );
    cx.update_window(window, |_, window, cx| {
        window.click(gpui_kit::ElementId::Integer(0), cx);
        window.render_frame(cx);
        window.click(gpui_kit::ElementId::Integer(1), cx);
        window.render_frame(cx);
    })
    .unwrap();
    let still_open = cx.update_window(window, |_, window, _| window.try_find("popup-menu").is_some()).unwrap();
    assert!(still_open, "点说明行不算选择，菜单保持打开");
    cx.update_window(window, |_, window, cx| {
        window.press("down", cx);
        window.press("enter", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let pane = pane.read(cx);
        assert_eq!(pane.session.fk_ref_table, "zzz", "点无命中说明不改输入");
        assert_eq!(pane.session.fk_column, "col_1", "也不改其它未提交草稿");
        assert!(!pane.session.preview_open());
    });
    cx.update_window(window, |_, window, cx| {
        window.press("escape", cx);
        window.render_frame(cx);
    })
    .unwrap();

    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            pane.set_field("fk_ref_table", String::new());
            cx.notify();
        })
    });
    let (items, menu_h) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("structure-fk-tables", cx);
            window.render_frame(cx);
            (menu_items(window), window.find("popup-menu").bounds().size.height)
        })
        .unwrap();
    assert_eq!(items.len(), tables.len(), "清空输入恢复全部选项");
    assert!(items.iter().all(|(_, disabled)| !*disabled));
    assert!(f32::from(menu_h) <= 360. + 16., "长列表菜单仍限高（{}）", f32::from(menu_h));
    cx.update_window(window, |_, window, cx| {
        window.press("escape", cx);
        window.render_frame(cx);
    })
    .unwrap();

    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            pane.set_field("fk_ref_table", "LE_4".into());
            cx.notify();
        })
    });
    let items = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("structure-fk-tables", cx);
            window.render_frame(cx);
            menu_items(window)
        })
        .unwrap();
    let labels = items.iter().map(|(label, _)| label.clone()).collect::<Vec<_>>();
    assert_eq!(labels, (40..50).map(|index| format!("table_{index}")).collect::<Vec<_>>(), "部分匹配只列命中项");
    cx.update_window(window, |_, window, cx| {
        let target = window.find_by_label("table_42");
        window.click(target.path().last().unwrap().clone(), cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let pane = pane.read(cx);
        assert_eq!(pane.session.fk_ref_table, "table_42", "选中项写入引用表");
        assert_eq!(pane.session.fk_column, "col_1", "本表列不被改写");
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// 新增尚未入库的字段点删除时直接移出草稿，不留下虚假的“1 处未应用”；已有字段标记删除或单行还原。
#[test]
fn unsaved_draft_column_removes_cleanly_while_existing_column_marks_and_reverts() {
    let (services, dir) = offline_services("draft-revert");
    let mut pane = pane_with_snapshot(services, 3);
    assert_eq!(pane.pending_changes(), 0);

    pane.add_column();
    assert_eq!(pane.session.kept_columns.len(), 4);
    assert_eq!(pane.pending_changes(), 1, "新增字段计入未应用变更");
    pane.toggle_column(3, "drop");
    assert_eq!(pane.session.kept_columns.len(), 3, "尚未落库的新增字段直接移除");
    assert_eq!(pane.pending_changes(), 0, "移除新增草稿行后不再虚报未应用变更");

    pane.session.kept_columns[1].name = "renamed_col".into();
    pane.session.kept_columns[1].type_name = "bigint".into();
    assert_eq!(pane.pending_changes(), 1);
    pane.revert_column(1);
    assert_eq!(pane.session.kept_columns[1].name, "col_1");
    assert_eq!(pane.session.kept_columns[1].type_name, "integer");
    assert_eq!(pane.pending_changes(), 0, "单行还原恢复快照原值");

    pane.toggle_column(2, "drop");
    assert!(pane.session.kept_columns[2].dropped, "已有字段标记为待删除");
    assert_eq!(pane.pending_changes(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 字段草稿前置诊断：重名、空名、非法类型、非法默认值、已有行表的新增非空无默认值均在预览前给出清晰原因。
#[test]
fn draft_diagnostics_catch_duplicates_invalid_types_and_missing_not_null_defaults() {
    let (services, dir) = offline_services("diagnostics");
    let mut pane = pane_with_snapshot(services, 2);
    if let Some(snap) = pane.snapshot.as_mut() {
        snap.row_count = Some(42);
    }
    assert!(pane.draft_diagnostic().is_none());

    // 重名
    pane.session.kept_columns[1].name = "COL_0".into();
    assert!(pane.draft_diagnostic().unwrap().contains("COL_0"));
    pane.revert_column(1);
    assert!(pane.draft_diagnostic().is_none());

    // 非法类型
    pane.session.kept_columns[1].type_name = "not_a_pg_type".into();
    assert!(pane.draft_diagnostic().unwrap().contains("not_a_pg_type"));
    pane.revert_column(1);

    // 非法默认值
    pane.session.kept_columns[1].default = crate::ddl::DefaultSpec::Expression("DROP TABLE orders".into());
    assert!(pane.draft_diagnostic().is_some());
    pane.revert_column(1);

    // 已有数据的表新增非空且无默认值列
    pane.add_column();
    let last = pane.session.kept_columns.len() - 1;
    pane.session.kept_columns[last].nullable = false;
    assert!(pane.draft_diagnostic().is_some(), "非空表新增无默认值的 NOT NULL 列需提前预警");
    pane.session.kept_columns[last].default = crate::ddl::DefaultSpec::Literal("0".into());
    assert!(pane.draft_diagnostic().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// 新建表的常用字段预设与实时建表 SQL 预览。
#[test]
fn create_table_presets_and_live_sql_preview_update_immediately() {
    let (services, dir) = offline_services("presets");
    let mut pane = StructurePane::new(services);
    pane.begin_create(target("postgres", ""));
    assert!(pane.live_composer_sql().is_none(), "未填表名前不生成无效 SQL");
    pane.session.table_name = "audit_events".into();
    pane.apply_template(crate::ddl::templates::BUILTIN_TIMESTAMPS);
    pane.apply_template(crate::ddl::templates::BUILTIN_STATUS_REMARK);
    // 重复点击同一预设不会插入重名列
    let count_before = pane.session.kept_columns.len();
    let (added, skipped) = pane.apply_template(crate::ddl::templates::BUILTIN_TIMESTAMPS);
    assert_eq!(pane.session.kept_columns.len(), count_before);
    assert!(added.is_empty() && skipped == vec!["created_at".to_owned(), "updated_at".to_owned()], "重名时提示并跳过：{skipped:?}");
    assert!(pane.session.note().contains("created_at"), "重名提示就近显示：{}", pane.session.note());

    let sql = pane.live_composer_sql().expect("实时建表 SQL");
    assert!(sql.contains("CREATE TABLE") && sql.contains("audit_events"), "{sql}");
    assert!(sql.contains("created_at") && sql.contains("updated_at") && sql.contains("status"), "{sql}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 字段筛选与“仅看已改”在大表上过滤显示，无命中时提供一键清除恢复。
#[gpui_kit::test]
fn column_filter_and_only_changed_narrow_large_tables_and_recover(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = offline_services("col-filter");
    cx.update(gpui_kit::init);
    let (window, pane) = cx.update(|cx| {
        let bounds = gpui_kit::Bounds {
            origin: gpui_kit::Point::default(),
            size: gpui_kit::size(gpui_kit::px(1100.), gpui_kit::px(800.)),
        };
        gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |_window, cx| {
                cx.new(|_| {
                    let mut pane = pane_with_snapshot(services.clone(), 12);
                    pane.session.kept_columns[5].name = "shipping_postal_code".into();
                    pane
                })
            },
        )
        .expect("structure pane")
    });

    // 按关键字筛选：只渲染命中的第 5 列（保持原始索引 id）。
    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            pane.set_column_filter("postal".into());
            cx.notify();
        })
    });
    let (hit_visible, other_visible) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.try_find("structure-row-columns-5").is_some(),
                window.try_find("structure-row-columns-0").is_some(),
            )
        })
        .unwrap();
    assert!(hit_visible && !other_visible, "筛选只留下匹配列且保留原始行下标");

    // 无命中：显示 no-match-state 与清除筛选按钮，点击恢复全部列。
    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            pane.set_column_filter("nonexistent_xyz".into());
            cx.notify();
        })
    });
    let no_match = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("no-match-state").is_some()
        })
        .unwrap();
    assert!(no_match, "筛选无命中显示无命中空态");
    cx.update_window(window, |_, window, cx| {
        window.click("structure-clear-col-filter", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let recovered = cx
        .update_window(window, |_, window, _| window.try_find("structure-row-columns-0").is_some())
        .unwrap();
    assert!(recovered, "清除筛选后恢复全部字段");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 索引：选列时自动生成规范索引名、支持复合索引列顺序调整，并渲染主键/唯一/部分索引徽标。
#[gpui_kit::test]
fn index_composer_auto_names_reorders_columns_and_renders_index_badges(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = offline_services("index-ux");
    cx.update(gpui_kit::init);
    let (window, pane) = cx.update(|cx| {
        let bounds = gpui_kit::Bounds {
            origin: gpui_kit::Point::default(),
            size: gpui_kit::size(gpui_kit::px(1100.), gpui_kit::px(800.)),
        };
        gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |_window, cx| {
                cx.new(|_| {
                    let mut pane = pane_with_snapshot(services.clone(), 4);
                    if let Some(snap) = pane.snapshot.as_mut() {
                        snap.indexes = vec![
                            Index {
                                name: "orders_pkey".into(),
                                unique: true,
                                columns: vec![IndexColumn { name: "col_0".into(), descending: false }],
                                protected: true,
                                reason: "由主键约束维护".into(),
                                partial: false,
                                expression: false,
                                sql: None,
                            },
                            Index {
                                name: "idx_orders_active".into(),
                                unique: false,
                                columns: vec![IndexColumn { name: "col_1".into(), descending: true }],
                                protected: false,
                                reason: String::new(),
                                partial: true,
                                expression: false,
                                sql: Some("CREATE INDEX idx_orders_active ON public.orders (col_1 DESC) WHERE col_2 > 0".into()),
                            },
                        ];
                    }
                    pane.session.section = Section::Indexes;
                    pane.composer_open = true;
                    pane
                })
            },
        )
        .unwrap()
    });

    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            pane.toggle_index_column("col_1");
            pane.toggle_index_column("col_2");
            assert_eq!(pane.session.index_name, "idx_orders_col_1_col_2");
            pane.move_index_column(1, -1);
            assert_eq!(pane.session.index_name, "idx_orders_col_2_col_1", "调整复合索引列顺序后自动命名同步更新");
            pane.type_into("index_name", "_custom");
            pane.toggle_index_column("col_3");
            assert!(pane.session.index_name.ends_with("_custom"), "手动改名后不再覆盖用户输入");
            cx.notify();
        })
    });

    let (row0, row1, lock0, quick0_disabled) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.try_find("structure-row-indexes-0").is_some(),
                window.try_find("structure-row-indexes-1").is_some(),
                window.try_find("structure-index-lock-0").is_some(),
                window.find("structure-quick-drop-index-0").disabled(),
            )
        })
        .unwrap();
    assert!(row0 && row1 && lock0);
    assert_eq!(quick0_disabled, Some(true), "受主键保护的索引删除按钮保持禁用");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 外键页：同时呈现本表外键（含级联规则与一键跳引用表）和入向被引用关系（一键跳子表）。
#[gpui_kit::test]
fn foreign_keys_render_cascade_rules_incoming_references_and_navigation_sink(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = offline_services("fk-ux");
    let jumped: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let jumped_sink = Arc::clone(&jumped);
    cx.update(gpui_kit::init);
    let (window, _pane) = cx.update(|cx| {
        let bounds = gpui_kit::Bounds {
            origin: gpui_kit::Point::default(),
            size: gpui_kit::size(gpui_kit::px(1100.), gpui_kit::px(800.)),
        };
        gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |_window, cx| {
                cx.new(|_| {
                    let mut pane = pane_with_snapshot(services.clone(), 3);
                    pane.set_structure_nav_sink(Arc::new(move |target, _| {
                        jumped_sink.lock().unwrap().push(format!("{}.{}", target.schema, target.table));
                    }));
                    if let Some(snap) = pane.snapshot.as_mut() {
                        snap.foreign_keys = vec![ForeignKey {
                            name: "fk_orders_customer".into(),
                            columns: vec!["col_1".into()],
                            ref_schema: "public".into(),
                            ref_table: "customers".into(),
                            ref_columns: vec!["id".into()],
                            on_delete: "CASCADE".into(),
                            on_update: "NO ACTION".into(),
                        }];
                        snap.incoming = vec![IncomingForeignKey {
                            name: "fk_items_order".into(),
                            table: "order_items".into(),
                            schema: "public".into(),
                            columns: vec!["order_id".into()],
                            ref_columns: vec!["col_0".into()],
                        }];
                    }
                    pane.session.section = Section::ForeignKeys;
                    pane
                })
            },
        )
        .unwrap()
    });

    let has_incoming = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("structure-incoming-fks").is_some()
        })
        .unwrap();
    assert!(has_incoming, "被其他表引用的入向外键卡片可见");

    cx.update_window(window, |_, window, cx| {
        window.click("structure-fk-jump-0", cx);
        window.click("structure-incoming-jump-0", cx);
    })
    .unwrap();
    assert_eq!(*jumped.lock().unwrap(), vec!["public.customers".to_owned(), "public.order_items".to_owned()]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 预览对话框打开时按 Esc 立即取消关闭，不执行任何语句。
#[gpui_kit::test]
fn escape_closes_the_preview_dialog_without_executing(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = offline_services("esc-preview");
    cx.update(gpui_kit::init);
    let (window, pane) = cx.update(|cx| {
        gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, |_window, cx| {
            cx.new(|_| {
                let mut pane = pane_with_snapshot(services.clone(), 2);
                pane.session.kept_columns[1].name = "renamed".into();
                let snap = snapshot_with(2);
                pane.session.arm_fixture(&snap);
                pane
            })
        })
        .unwrap()
    });
    let opened = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            if let Some(focus) = pane.read(cx).focus.clone() {
                window.focus(&focus, cx);
            }
            window.render_frame(cx);
            window.try_find("structure-preview-dialog").is_some()
        })
        .unwrap();
    assert!(opened);
    cx.update_window(window, |_, window, cx| {
        window.press("escape", cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert!(!cx.update(|cx| pane.read(cx).session.preview_open()), "Esc 关闭预览确认层");
    let _ = std::fs::remove_dir_all(&dir);
}

/// R2：新建表的草稿问题逐行就近给出（表名、空名、重名、非法类型、自增非主键、非法默认值），有问题时预览不可提交。
#[gpui_kit::test]
fn create_table_issues_sit_on_their_rows_and_block_preview(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = offline_services("create-issues");
    cx.update(gpui_kit::init);
    let (window, pane) = cx.update(|cx| {
        let bounds = gpui_kit::Bounds { origin: gpui_kit::Point::default(), size: gpui_kit::size(gpui_kit::px(1360.), gpui_kit::px(900.)) };
        gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |_window, cx| {
                cx.new(|_| {
                    let mut pane = StructurePane::new(services.clone());
                    pane.begin_create(target("mysql", ""));
                    pane
                })
            },
        )
        .expect("structure pane")
    });
    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            assert!(pane.draft_issues().iter().any(|issue| issue.field == IssueField::TableName && issue.row.is_none()), "未填表名");
            assert!(pane.create_blocked());
            pane.session.table_name = "orders".into();
            assert!(pane.draft_issues().is_empty(), "{:?}", pane.draft_issues());
            pane.add_column();
            pane.add_column();
            pane.add_column();
            pane.session.kept_columns[1].name = "title".into();
            pane.session.kept_columns[2].name = "TITLE".into();
            pane.session.kept_columns[3].name = "  ".into();
            let issues = pane.draft_issues();
            assert!(issues.iter().any(|issue| issue.row == Some(2) && issue.field == IssueField::Name && issue.text.contains("TITLE")), "{issues:?}");
            assert!(issues.iter().any(|issue| issue.row == Some(3) && issue.field == IssueField::Name), "{issues:?}");
            assert!(!issues.iter().any(|issue| issue.row == Some(1)), "第一个同名字段不报：{issues:?}");
            pane.session.kept_columns[2].name = "price".into();
            pane.session.kept_columns[2].type_name = "money!".into();
            pane.session.kept_columns[3].name = "seq".into();
            pane.session.kept_columns[3].auto = true;
            pane.session.kept_columns[1].default = crate::ddl::DefaultSpec::Null;
            pane.session.kept_columns[1].nullable = false;
            let issues = pane.draft_issues();
            assert!(issues.iter().any(|issue| issue.row == Some(2) && issue.field == IssueField::Type), "{issues:?}");
            assert!(issues.iter().any(|issue| issue.row == Some(3) && issue.field == IssueField::Auto), "{issues:?}");
            assert!(issues.iter().any(|issue| issue.row == Some(1) && issue.field == IssueField::Default), "{issues:?}");
            assert!(pane.create_blocked());
            assert!(pane.live_composer_sql().is_none(), "有问题时不生成可执行 SQL");
            pane.preview_current(cx);
            assert!(!pane.previewing && !pane.session.preview_open(), "预览不可提交");
            assert_eq!(pane.session.note, Note::Key("structure.diag.fixFirst"));
            cx.notify();
        })
    });
    let (row_issue, preview_disabled) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (window.try_find("structure-row-issue-2").is_some(), window.find("structure-preview").disabled().unwrap_or(false))
        })
        .unwrap();
    assert!(row_issue, "问题显示在该行下方");
    assert!(preview_disabled, "预览按钮不可用");
    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            pane.session.kept_columns[1].nullable = true;
            pane.session.kept_columns[2].type_name = "decimal(10,2)".into();
            pane.session.kept_columns[3].auto = false;
            assert!(pane.draft_issues().is_empty(), "{:?}", pane.draft_issues());
            let sql = pane.live_composer_sql().expect("修正后实时生成");
            assert!(sql.contains("title varchar(255) DEFAULT NULL") && sql.contains("price decimal(10,2)"), "{sql}");
            cx.notify();
        })
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// R2：只用键盘完成 表名 → 3 个字段 → 主键 → 预览 SQL；Alt+↑/↓ 调整顺序，拖拽同样能调整。
#[gpui_kit::test]
fn keyboard_only_create_table_reaches_the_preview(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = offline_services("create-keyboard");
    // 最后一步的预览走真实的后台运行时（连接未打开，只会得到说明）。
    cx.executor().allow_parking();
    cx.update(gpui_kit::init);
    let (window, pane) = cx.update(|cx| {
        let bounds = gpui_kit::Bounds { origin: gpui_kit::Point::default(), size: gpui_kit::size(gpui_kit::px(1360.), gpui_kit::px(900.)) };
        gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |_window, cx| {
                cx.new(|_| {
                    let mut pane = StructurePane::new(services.clone());
                    pane.begin_create(target("mysql", ""));
                    pane
                })
            },
        )
        .expect("structure pane")
    });
    let step = |cx: &mut gpui_kit::TestAppContext, action: &dyn Fn(&mut gpui_kit::Window, &mut App)| {
        cx.update_window(window, |_, window, cx| {
            action(window, cx);
            window.render_frame(cx);
        })
        .unwrap();
    };
    let focused = |cx: &mut gpui_kit::TestAppContext, key: u32| cx.update_window(window, |_, window, cx| pane.read(cx).draft_fields.is_focused(key, window, cx)).unwrap();

    step(cx, &|window, cx| window.render_frame(cx));
    assert!(focused(cx, 4), "打开新建表时表名框获得焦点");
    step(cx, &|window, cx| window.input("orders", cx));
    step(cx, &|window, cx| window.press("enter", cx));
    assert!(focused(cx, 10_000), "表名回车进入第一个字段名");
    // 主键：Alt+P 取消再设回。
    step(cx, &|window, cx| window.press("alt-p", cx));
    assert!(!cx.update(|cx| pane.read(cx).session.kept_columns[0].primary));
    step(cx, &|window, cx| window.press("alt-p", cx));
    assert!(cx.update(|cx| pane.read(cx).session.kept_columns[0].primary), "Alt+P 设为主键");
    // 回车新增字段：名称获得焦点并全选，直接输入即改写。
    step(cx, &|window, cx| window.press("enter", cx));
    step(cx, &|window, cx| window.render_frame(cx));
    assert!(focused(cx, 10_001), "新增字段的名称获得焦点");
    step(cx, &|window, cx| window.input("title", cx));
    step(cx, &|window, cx| window.press("enter", cx));
    step(cx, &|window, cx| window.render_frame(cx));
    assert!(focused(cx, 10_002));
    step(cx, &|window, cx| window.input("amount", cx));
    // Alt+↑ 把 amount 移到 title 前面，焦点跟着字段走。
    step(cx, &|window, cx| window.press("alt-up", cx));
    let names = cx.update(|cx| pane.read(cx).session.kept_columns.iter().map(|column| column.name.clone()).collect::<Vec<_>>());
    assert_eq!(names, vec!["id", "amount", "title"]);
    assert!(focused(cx, 10_001), "移动后焦点仍在 amount");
    step(cx, &|window, cx| window.press("alt-down", cx));
    let names = cx.update(|cx| pane.read(cx).session.kept_columns.iter().map(|column| column.name.clone()).collect::<Vec<_>>());
    assert_eq!(names, vec!["id", "title", "amount"]);
    // Tab 能走到类型框。
    step(cx, &|window, cx| window.press("tab", cx));
    assert!(focused(cx, 20_002), "Tab 从字段名到类型");

    let sql = cx.update(|cx| pane.read(cx).live_composer_sql()).expect("实时 SQL");
    assert!(sql.contains("CREATE TABLE") && sql.contains("orders") && sql.contains("PRIMARY KEY (id)"), "{sql}");
    assert!(sql.find("title").unwrap() < sql.find("amount").unwrap(), "{sql}");

    // 拖拽：把 amount 拖到第一行。
    step(cx, &|window, cx| window.drag_to("structure-col-grip-2", "structure-row-columns-0", cx));
    let names = cx.update(|cx| pane.read(cx).session.kept_columns.iter().map(|column| column.name.clone()).collect::<Vec<_>>());
    assert_eq!(names, vec!["amount", "id", "title"], "拖拽调整顺序");

    // ⌘/Ctrl+Enter 预览 SQL。
    step(cx, &|window, cx| window.press("ctrl-enter", cx));
    let (previewing, columns) = cx.update(|cx| (pane.read(cx).previewing, pane.read(cx).session.kept_columns.len()));
    assert!(previewing, "组合回车触发预览");
    assert_eq!(columns, 3, "组合回车不会新增字段");
    let _ = std::fs::remove_dir_all(&dir);
}

/// R19：自定义模板在管理面板里新建保存后交给外壳持久化；新建表与修改表的常用字段菜单同源，套用重名时提示。
#[gpui_kit::test]
fn custom_templates_are_managed_saved_and_shared_by_create_and_alter_menus(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    use gpui_kit::test::{TestQueryExt as _, TestWindowExt};
    let (services, dir) = offline_services("templates");
    cx.update(gpui_kit::init);
    let saved: std::sync::Arc<std::sync::Mutex<Vec<crate::ddl::templates::TemplateStore>>> = Default::default();
    let (window, pane) = cx.update(|cx| {
        let bounds = gpui_kit::Bounds { origin: gpui_kit::Point::default(), size: gpui_kit::size(gpui_kit::px(1360.), gpui_kit::px(900.)) };
        gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |_window, cx| {
                cx.new(|_| {
                    let mut pane = StructurePane::new(services.clone());
                    pane.begin_create(target("mysql", ""));
                    pane.session.table_name = "orders".into();
                    pane
                })
            },
        )
        .expect("structure pane")
    });
    let store_saved = saved.clone();
    cx.update(|cx| {
        pane.update(cx, |pane, _| {
            let store_saved = store_saved.clone();
            pane.set_templates_sink(std::sync::Arc::new(move |templates, _| store_saved.lock().unwrap().push(templates)));
        })
    });
    // 打开管理面板，新建“审计字段”两字段模板并保存。
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("structure-column-presets", cx);
        window.render_frame(cx);
        let manage = window.find_by_label(&i18n::text(Locale::ZhCn, "structure.templates.manage"));
        window.click(manage.path().last().unwrap().clone(), cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert!(cx.update(|cx| pane.read(cx).template_manager_open()), "菜单末项打开管理面板");
    cx.update_window(window, |_, window, cx| {
        window.click("structure-template-new", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            pane.write_draft(8, "审计字段".into());
            pane.write_draft(50_000, "created_user_id".into());
            pane.write_draft(60_000, "bigint".into());
            pane.template_add_column();
            pane.write_draft(50_001, "modified_user_id".into());
            pane.write_draft(60_001, "bigint".into());
            pane.template_toggle_engine(None);
            cx.notify();
        })
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("structure-template-save", cx);
        window.render_frame(cx);
    })
    .unwrap();
    {
        let saved = saved.lock().unwrap();
        let last = saved.last().expect("保存后交给外壳持久化");
        assert_eq!(last.custom.len(), 1);
        assert_eq!(last.custom[0].name, "审计字段");
        assert_eq!(last.custom[0].columns.iter().map(|column| column.name.as_str()).collect::<Vec<_>>(), vec!["created_user_id", "modified_user_id"]);
    }
    // 隐藏内置“软删除”，菜单不再列出。
    cx.update_window(window, |_, window, cx| {
        window.click("structure-template-hide-builtin:softDelete", cx);
        window.render_frame(cx);
        window.press("escape", cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert!(!cx.update(|cx| pane.read(cx).template_manager_open()), "Esc 关闭管理面板");
    let labels = cx
        .update_window(window, |_, window, cx| {
            window.click("structure-column-presets", cx);
            window.render_frame(cx);
            window.find_all_by_role(gpui_kit::Role::MenuItem).into_iter().map(|item| item.label().unwrap_or_default().to_owned()).collect::<Vec<_>>()
        })
        .unwrap();
    assert!(labels.iter().any(|label| label.starts_with("审计字段")), "{labels:?}");
    assert!(labels.iter().any(|label| label == &i18n::text(Locale::ZhCn, "structure.preset.uuid")), "{labels:?}");
    assert!(!labels.iter().any(|label| label == &i18n::text(Locale::ZhCn, "structure.preset.softDelete")), "隐藏的内置模板不再出现：{labels:?}");
    cx.update_window(window, |_, window, cx| {
        let item = window.find_all_by_role(gpui_kit::Role::MenuItem).into_iter().find(|item| item.label().unwrap_or_default().starts_with("审计字段")).unwrap();
        window.click(item.path().last().unwrap().clone(), cx);
        window.render_frame(cx);
    })
    .unwrap();
    let names = cx.update(|cx| pane.read(cx).session.kept_columns.iter().map(|column| column.name.clone()).collect::<Vec<_>>());
    assert_eq!(names, vec!["id", "created_user_id", "modified_user_id"], "一次点击加入两字段");
    let id = cx.update(|cx| pane.read(cx).templates().custom[0].id.clone());
    cx.update(|cx| {
        pane.update(cx, |pane, _| {
            let (added, skipped) = pane.apply_template(&id);
            assert!(added.is_empty() && skipped.len() == 2, "重名不重复添加");
            assert!(pane.session.note().contains("created_user_id"));
        })
    });

    // 修改表的菜单与新建表同源：含 UUID 与自定义模板（适用于全部引擎）。
    let store = saved.lock().unwrap().last().cloned().unwrap();
    let mut alter = pane_with_snapshot(services.clone(), 3);
    alter.set_templates(store);
    let ids = alter.available_templates().into_iter().map(|template| template.id).collect::<Vec<_>>();
    assert!(ids.contains(&crate::ddl::templates::BUILTIN_UUID.to_owned()) && ids.contains(&id), "{ids:?}");
    alter.apply_template(&id);
    assert!(alter.session.kept_columns.iter().any(|column| column.name == "created_user_id" && column.original.is_none()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// “在查询中打开”的取样语句按方言写；引擎识别不出来时不猜（不给出带 LIMIT 的通用语句）。
#[test]
fn sample_query_follows_the_dialect_and_skips_unknown_engines() {
    let sql = select_sample_sql(&target("sqlserver", "orders")).expect("sqlserver");
    assert!(sql.contains("TOP 100") && !sql.contains("LIMIT"), "{sql}");
    let sql = select_sample_sql(&target("postgres", "orders")).expect("postgres");
    assert!(sql.contains("LIMIT 100"), "{sql}");
    assert_eq!(select_sample_sql(&target("unknown-engine", "orders")), None);
}

fn open_pane(
    cx: &mut gpui_kit::TestAppContext,
    width: f32,
    height: f32,
    build: impl FnOnce() -> StructurePane + 'static,
) -> (gpui_kit::AnyWindowHandle, Entity<StructurePane>) {
    use gpui_kit::AppContext as _;
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        let bounds = gpui_kit::Bounds { origin: gpui_kit::Point::default(), size: gpui_kit::size(gpui_kit::px(width), gpui_kit::px(height)) };
        gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |_window, cx| cx.new(|_| build()),
        )
        .expect("structure pane")
    })
}

/// R9：几百个字段时只渲染可见行；默认文本浏览，点击某行才换成输入控件，Esc 退出并保留草稿；
/// 没有改动时底部不显示“预览 SQL”，改了一处后显示改动计数与预览，预览里只有这一处。
#[gpui_kit::test]
fn columns_browse_virtualized_and_edit_one_row_at_a_time(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = offline_services("browse");
    let (window, pane) = open_pane(cx, 1100., 800., move || pane_with_snapshot(services, 300));

    let (first, last, rows, inputs, preview, hint, comment_head) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let rows = (0..300).filter(|ix| window.try_find(SharedString::from(format!("structure-row-columns-{ix}"))).is_some()).count();
            (
                window.try_find("structure-row-columns-0").is_some(),
                window.try_find("structure-row-columns-299").is_some(),
                rows,
                window.try_find("structure-draft-10000").is_some(),
                window.try_find("structure-preview").is_some(),
                window.try_find("structure-browse-hint").is_some(),
                window.try_find("structure-head-comment").is_some(),
            )
        })
        .unwrap();
    assert!(first && !last, "虚拟列表只渲染可见行");
    assert!(rows > 5 && rows < 60, "300 个字段只渲染了 {rows} 行");
    assert!(!inputs, "浏览态没有输入控件");
    assert!(!preview && hint, "没有改动时不显示预览按钮，只有只读说明");
    assert!(comment_head, "宽窗口里注释列始终可见");

    // 点击第 3 行进入编辑：只有这一行变成输入控件。
    cx.update_window(window, |_, window, cx| {
        window.click("structure-row-columns-3", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let (editing, row3_input, row4_input) = cx
        .update_window(window, |_, window, cx| {
            (pane.read(cx).editing_row(), window.try_find("structure-draft-10003").is_some(), window.try_find("structure-draft-10004").is_some())
        })
        .unwrap();
    assert_eq!(editing, Some(3));
    assert!(row3_input && !row4_input, "只有被点击的行进入编辑");

    // 改名，再点另一行：前一行回到文本，草稿保留；改动计数为 1。
    cx.update(|cx| pane.update(cx, |pane, _| pane.write_draft(10_003, "renamed_three".into())));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("structure-row-columns-5", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let pane = pane.read(cx);
        assert_eq!(pane.editing_row(), Some(5));
        assert_eq!(pane.session.kept_columns[3].name, "renamed_three", "切到别的行，草稿保留");
        assert_eq!(pane.pending_changes(), 1);
    });

    // Esc 退出编辑，草稿仍在；预览按钮出现。
    cx.update(|cx| pane.update(cx, |pane, _| assert!(pane.handle_escape())));
    let (editing, preview, row5_input) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (pane.read(cx).editing_row(), window.try_find("structure-preview").is_some(), window.try_find("structure-draft-10005").is_some())
        })
        .unwrap();
    assert_eq!(editing, None);
    assert!(!row5_input, "Esc 后回到文本浏览");
    assert!(preview, "有改动时显示预览按钮");

    // 只改了一处：预览里只有这一处。
    cx.update(|cx| {
        pane.update(cx, |pane, _| {
            let snapshot = pane.snapshot.clone().unwrap();
            assert_eq!(pane.session.kept_columns[3].name, "renamed_three");
            pane.session.arm_fixture(&snapshot);
            let sql = pane.session.preview_sql();
            assert_eq!(sql.len(), 1, "{sql:?}");
            assert!(sql[0].contains("col_3") && sql[0].contains("renamed_three"), "{sql:?}");
            pane.session.cancel();
            pane.revert_column(3);
            assert_eq!(pane.pending_changes(), 0);
            assert!(!pane.preview_button_visible(), "改回原样后预览按钮又隐藏");
        })
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// 新增字段直接进入编辑；删掉这条未落库的新增行后编辑行复位。主键的可空与类型不可改，并说明原因。
#[test]
fn added_rows_enter_edit_and_locked_attributes_explain_why() {
    let (services, dir) = offline_services("browse-add");
    let mut pane = pane_with_snapshot(services, 3);
    pane.add_column();
    assert_eq!(pane.editing_row(), Some(3), "新增的行直接进入编辑");
    assert_eq!(pane.pending_changes(), 1);
    pane.toggle_column(3, "drop");
    assert_eq!(pane.editing_row(), None, "删掉新增行后不再指向它");
    pane.begin_edit_row(2);
    pane.add_column();
    pane.begin_edit_row(3);
    pane.toggle_column(2, "drop");
    assert_eq!(pane.editing_row(), Some(3), "删除已有列只是标记，不影响编辑行");
    pane.reset_draft();
    assert_eq!(pane.editing_row(), None, "放弃改动后退出编辑");

    assert_eq!(i18n::text(Locale::ZhCn, "structure.field.pkNullLocked"), "主键列必须非空，不能改为可空");
    assert!(i18n::static_text(Locale::En, "structure.field.pkTypeLocked").is_some());
    let entries = pane.column_menu_entries(0, &pane.session.kept_columns[0].clone(), ColumnChange::Clean, false);
    let drop = entries.iter().find(|entry| entry.id == "edit-drop-col-0").unwrap();
    assert_eq!(drop.reason.as_deref(), Some(i18n::text(Locale::ZhCn, "structure.field.pkLocked").as_str()), "主键列的删除项说明原因");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 窄窗口按优先级处理列：注释始终可见（先收窄），放不下时隐藏默认值；“显示全部列”强制显示默认值。
#[test]
fn narrow_windows_keep_comment_and_hide_default_unless_show_all() {
    use super::browse::BrowseColumns;
    let wide = BrowseColumns::of(1200., false);
    assert!(wide.default && wide.comment);
    assert_eq!(wide.widths.comment, BrowseColumns::COMMENT_W);
    assert!(wide.widths.min_total(false) <= 1200.);
    let unknown = BrowseColumns::of(0., false);
    assert!(unknown.default && unknown.comment, "还没量到宽度时按宽窗口");
    // 最窄的真实内容宽度（534）：注释收窄但仍可见，默认值也在。
    for w in [534., 600., 700., 776.] {
        let cols = BrowseColumns::of(w, false);
        assert!(cols.default && cols.comment, "{w}: 全部可见");
        assert!(cols.widths.comment >= BrowseColumns::COMMENT_MIN, "{w}: 注释不小于最小宽度");
        assert!(cols.widths.min_total(false) <= w + 0.01, "{w}: 放得下");
        assert!(!BrowseColumns::hides_any(w));
    }
    let medium = BrowseColumns::of(500., false);
    assert!(!medium.default && medium.comment, "先隐藏默认值，注释保留");
    assert!(medium.widths.comment >= BrowseColumns::COMMENT_MIN);
    assert!(BrowseColumns::hides_any(500.));
    let narrow = BrowseColumns::of(380., false);
    assert!(!narrow.default && narrow.comment, "再窄注释也可见");
    let forced = BrowseColumns::of(380., true);
    assert!(forced.default && forced.comment, "显示全部列");
    assert!(!BrowseColumns::hides_any(1200.));
}

/// R8 / R9：新建索引在表单内的多选选择器里勾选列，已选列可调整顺序与升降序；
/// 生成的 SQL 列顺序与所选顺序一致；名字按设置里的模板生成，按方言截断，重名就地提示。
#[gpui_kit::test]
fn index_picker_orders_columns_and_names_follow_the_template(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = offline_services("index-picker");
    let (window, pane) = open_pane(cx, 1100., 800., move || {
        let mut pane = pane_with_snapshot(services, 6);
        if let Some(snap) = pane.snapshot.as_mut() {
            snap.indexes = vec![Index {
                name: "idx_orders_col_4".into(),
                unique: false,
                columns: vec![IndexColumn { name: "col_4".into(), descending: false }],
                protected: false,
                reason: String::new(),
                partial: false,
                expression: false,
                sql: None,
            }];
        }
        pane.session.section = Section::Indexes;
        pane.composer_open = true;
        pane
    });
    let (picker, order) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (window.try_find("picker").is_some(), window.try_find("structure-index-order").is_some())
        })
        .unwrap();
    assert!(picker && order, "列选择器与已选列表都在表单内");

    // 在选择器里依次勾选第 3、第 2 项（col_2、col_1）：草稿按勾选先后排列。
    let picker = cx.update(|cx| pane.read(cx).index_picker.as_ref().map(|(picker, _)| picker.clone()).unwrap());
    cx.update(|cx| {
        picker.update(cx, |picker, cx| {
            picker.choose_at(2, cx);
            picker.choose_at(1, cx);
        })
    });
    cx.update(|cx| {
        let pane = pane.read(cx);
        let names: Vec<&str> = pane.session.index_columns.iter().map(|column| column.name.as_str()).collect();
        assert_eq!(names, ["col_2", "col_1"]);
        assert_eq!(pane.session.index_name, "idx_orders_col_2_col_1");
        let sql = pane.live_composer_sql().expect("实时 SQL");
        assert!(sql.find("col_2").unwrap() < sql.find("col_1").unwrap(), "SQL 列顺序与所选顺序一致：{sql}");
    });

    // 调整顺序与升降序。
    cx.update(|cx| {
        pane.update(cx, |pane, _| {
            pane.move_index_column(1, -1);
            pane.toggle_index_order("col_2");
            let sql = pane.live_composer_sql().unwrap();
            let one = sql.find("col_1").unwrap();
            let two = sql.find("col_2").unwrap();
            assert!(one < two, "{sql}");
            assert!(sql[two..].contains("DESC"), "{sql}");
            assert_eq!(pane.session.index_name, "idx_orders_col_1_col_2", "自动命名跟着顺序走");
        })
    });

    // 从有序列表移除一列后，选择器的勾选随之同步。
    cx.update(|cx| pane.update(cx, |pane, _| pane.toggle_index_column("col_1")));
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    cx.update(|cx| {
        let pane = pane.read(cx);
        let picker = pane.index_picker.as_ref().unwrap().0.read(cx);
        let chosen: Vec<&str> = picker.model().chosen().iter().map(|id| id.as_ref()).collect();
        assert_eq!(chosen, ["col_2"]);
    });

    // 模板来自设置：改模板后自动名称立即重算；手动改名后不再覆盖。
    cx.update(|cx| {
        pane.update(cx, |pane, _| {
            pane.set_index_naming(crate::ddl::naming::IndexNaming { normal: "ix_{column1}__{table}".into(), unique: "uq_{columns}".into(), joiner: "$".into() });
            assert_eq!(pane.session.index_name, "ix_col_2__orders");
            pane.session.index_unique = true;
            pane.sync_auto_index_name();
            pane.set_index_selection(&["col_2", "col_3"]);
            assert_eq!(pane.session.index_name, "uq_col_2$col_3");
            pane.set_field("index_name", "my_index".into());
            pane.set_index_selection(&["col_2", "col_3", "col_5"]);
            assert_eq!(pane.session.index_name, "my_index", "手动名称不被增删列覆盖");
            pane.set_index_selection(&["col_3"]);
            assert_eq!(pane.session.index_name, "my_index");
        })
    });

    // 与已有索引重名：提交前就地提示，预览也拦下。
    cx.update(|cx| {
        pane.update(cx, |pane, _| {
            pane.set_field("index_name", "IDX_orders_col_4".into());
            assert!(pane.index_name_conflict());
        })
    });
    let conflict = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("structure-index-name-conflict").is_some()
        })
        .unwrap();
    assert!(conflict, "重名提示在表单里");
    cx.update(|cx| {
        pane.update(cx, |pane, _| {
            let snapshot = pane.snapshot.clone().unwrap();
            pane.session.arm_fixture(&snapshot);
            assert!(!pane.session.blocked().is_empty(), "预览拦下重名");
            pane.session.cancel();
            pane.set_field("index_name", "idx_fresh".into());
            assert!(!pane.index_name_conflict());
        })
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// 索引名按方言长度上限截断并加哈希（PostgreSQL 63）。
#[test]
fn suggested_index_names_respect_the_dialect_limit() {
    let naming = crate::ddl::naming::IndexNaming::default();
    let columns: Vec<IndexColumnDraft> =
        (0..8).map(|ix| IndexColumnDraft { name: format!("a_rather_long_column_name_{ix}"), descending: false }).collect();
    for (engine, limit) in [("postgres", 63), ("mysql", 64), ("mariadb", 64), ("sqlserver", 128)] {
        let name = suggested_index_name(&naming, engine, "orders", false, &columns);
        assert_eq!(name.len(), limit, "{engine}: {name}");
        assert!(name.starts_with("idx_orders_a_rather_long_column_name_0"), "{engine}: {name}");
    }
    let sqlite = suggested_index_name(&naming, "sqlite", "orders", false, &columns);
    assert!(sqlite.len() > 128, "SQLite 不截断");
    assert_eq!(suggested_index_name(&naming, "postgres", "orders", false, &[]), "");
}
