use std::path::Path;

use super::commands::{load_catalog, page_for_session};
use super::model::{
    metadata_sql, schema_status, ConnStatus, ConnectionNode, DatabaseNode, Engine, ObjectRow,
    SchemaNode, SchemaStatus,
};
use super::read::{assemble_postgres, read_catalog_session, read_definition, read_limit, ObjectWindow};
use super::sqlite::{read_sqlite_file, read_sqlite_pool};
use super::tree::{
    direct_objects, object_path, refresh_connection, refresh_schema, search_objects, take_page,
    visible_database_names, visible_schema_names,
};
use crate::connections::{CatalogSession, Service};

fn row(name: &str) -> ObjectRow {
    ObjectRow::table(name, Some(1))
}

fn schema(name: &str, objects: Vec<ObjectRow>) -> SchemaNode {
    let total = objects.len();
    SchemaNode {
        name: name.to_owned(),
        objects,
        total,
        error: None,
        kind_reports: Vec::new(),
    }
}

fn connection(engine: Engine, status: ConnStatus, databases: Vec<DatabaseNode>) -> ConnectionNode {
    ConnectionNode {
        id: engine.as_str().to_owned(),
        name: engine.as_str().to_owned(),
        engine,
        status,
        foreign_keys: if engine.shows_foreign_keys() {
            Some(false)
        } else {
            None
        },
        databases,
        error: None,
    }
}

#[test]
fn engines_keep_their_own_levels() {
    assert!(Engine::Postgres.shows_databases() && Engine::Postgres.shows_schemas());
    assert!(!Engine::Postgres.shows_foreign_keys());
    assert!(Engine::Mysql.shows_databases() && !Engine::Mysql.shows_schemas());
    assert!(Engine::Mariadb.shows_databases() && !Engine::Mariadb.shows_schemas());
    assert_ne!(Engine::Mysql, Engine::Mariadb);
    assert!(!Engine::Sqlite.shows_databases() && !Engine::Sqlite.shows_schemas());
    assert!(Engine::Sqlite.shows_foreign_keys());

    let postgres_sql = metadata_sql(Engine::Postgres);
    let mysql_sql = metadata_sql(Engine::Mysql);
    let maria_sql = metadata_sql(Engine::Mariadb);
    let sqlite_sql = metadata_sql(Engine::Sqlite);
    assert!(postgres_sql.objects.contains("pg_namespace"));
    assert!(postgres_sql.objects.contains("'m'"));
    let function_sql = super::kind_sql(Engine::Postgres, super::model::ObjectKind::Function);
    assert!(function_sql.list.contains("pg_get_function_identity_arguments"));
    assert!(function_sql.list.contains("pg_namespace"));
    let procedure_sql = super::kind_sql(Engine::Postgres, super::model::ObjectKind::Procedure);
    assert!(procedure_sql.list.contains("prokind = 'p'"));
    let trigger_sql = super::kind_sql(Engine::Mysql, super::model::ObjectKind::Trigger);
    assert!(trigger_sql.list.contains("information_schema.triggers"));
    assert!(super::kind_sql(Engine::Mariadb, super::model::ObjectKind::Function).list.contains("information_schema.routines"));
    assert!(!super::model::listed_kinds(Engine::Sqlite).contains(&super::model::ObjectKind::Function));
    assert!(super::model::listed_kinds(Engine::Sqlite).contains(&super::model::ObjectKind::Trigger));
    assert!(postgres_sql.schemas.is_some());
    assert!(mysql_sql.objects.contains("information_schema.tables"));
    assert!(maria_sql.objects.contains("information_schema.tables"));
    assert!(mysql_sql.schemas.is_none() && maria_sql.schemas.is_none());
    assert_eq!(mysql_sql.objects, maria_sql.objects);
    assert!(sqlite_sql.objects.contains("sqlite_master"));
    assert!(sqlite_sql.objects.contains("trigger"));
    assert_eq!(sqlite_sql.foreign_keys, Some("PRAGMA foreign_keys"));
    assert!(sqlite_sql.databases.is_empty());
}

#[test]
fn search_keeps_paths_and_skips_closed_connections() {
    let mut postgres = connection(
        Engine::Postgres,
        ConnStatus::Connected,
        vec![DatabaseNode {
            name: "atlas_dev".to_owned(),
            error: None,
            schemas: vec![
                schema("public", vec![row("customers"), row("orders")]),
                schema("analytics", vec![row("customers")]),
                SchemaNode::failed("restricted", "permission denied for schema restricted"),
            ],
        }],
    );
    postgres.name = "Atlas".to_owned();
    let mut closed = connection(
        Engine::Mariadb,
        ConnStatus::Disconnected,
        vec![DatabaseNode {
            name: "analytics".to_owned(),
            error: None,
            schemas: vec![schema("", vec![row("events")])],
        }],
    );
    closed.name = "Analytics".to_owned();
    let found = search_objects(&[postgres, closed], "customers");
    assert_eq!(
        found
            .hits
            .iter()
            .map(|hit| hit.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Atlas / atlas_dev / public",
            "Atlas / atlas_dev / analytics"
        ]
    );
    assert_eq!(found.skipped, vec!["Analytics".to_owned()]);
    assert!(found.hits.iter().all(|hit| hit.name != "events"));
    assert_eq!(
        object_path("Shop", "shop_staging", ""),
        "Shop / shop_staging"
    );
}

#[test]
fn batch_then_continue_and_failure_is_not_an_empty_schema() {
    let objects: Vec<ObjectRow> = (0..120)
        .map(|index| row(&format!("audit_{index:04}")))
        .collect();
    let first = take_page(&objects, 0, 100);
    assert_eq!(first.items.len(), 100);
    assert_eq!(first.total, 120);
    assert!(first.has_more);
    let second = take_page(&objects, 100, 100);
    assert_eq!(second.items.len(), 20);
    assert!(!second.has_more);

    let previous = schema("restricted", vec![row("secret")]);
    let failed = refresh_schema(
        &previous,
        &SchemaNode::failed("restricted", "permission denied for schema restricted"),
    );
    assert!(matches!(
        schema_status(&failed),
        SchemaStatus::Failed { .. }
    ));
    assert_eq!(failed.objects.len(), 1);
    let empty = schema("public", vec![]);
    assert!(matches!(schema_status(&empty), SchemaStatus::Empty));
}

#[test]
fn external_delete_is_stale_until_the_next_refresh() {
    let previous = connection(
        Engine::Postgres,
        ConnStatus::Connected,
        vec![DatabaseNode {
            name: "atlas_dev".to_owned(),
            error: None,
            schemas: vec![schema("public", vec![row("orders"), row("categories")])],
        }],
    );
    let mut incoming = previous.clone();
    incoming.databases[0].schemas[0].objects = vec![row("orders")];
    incoming.databases[0].schemas[0].total = 1;
    let once = refresh_connection(&previous, &incoming);
    let objects = &once.databases[0].schemas[0].objects;
    assert_eq!(objects[0].name, "orders");
    assert!(!objects[0].stale);
    assert_eq!(objects[1].name, "categories");
    assert!(objects[1].stale);
    let twice = refresh_connection(&once, &incoming);
    let names: Vec<&str> = twice.databases[0].schemas[0]
        .objects
        .iter()
        .map(|item| item.name.as_str())
        .collect();
    assert_eq!(names, vec!["orders"]);
}

#[test]
fn mysql_lists_databases_and_sqlite_lists_objects_directly() {
    let mysql = connection(
        Engine::Mysql,
        ConnStatus::Connected,
        vec![DatabaseNode {
            name: "shop_staging".to_owned(),
            error: None,
            schemas: vec![schema("", vec![row("products")])],
        }],
    );
    assert_eq!(
        visible_database_names(&mysql),
        vec!["shop_staging".to_owned()]
    );
    assert!(visible_schema_names(&mysql).is_empty());
    assert!(direct_objects(&mysql).is_empty());

    let sqlite = connection(
        Engine::Sqlite,
        ConnStatus::Connected,
        vec![DatabaseNode {
            name: "main".to_owned(),
            error: None,
            schemas: vec![schema(
                "",
                vec![row("notes"), ObjectRow::view("v_note_summary")],
            )],
        }],
    );
    assert!(visible_database_names(&sqlite).is_empty());
    assert!(visible_schema_names(&sqlite).is_empty());
    assert_eq!(sqlite.foreign_keys, Some(false));
    assert_eq!(direct_objects(&sqlite).len(), 2);
}

#[tokio::test]
async fn sqlite_file_pages_objects_reports_foreign_keys_and_does_not_pretend_a_missing_file_is_empty(
) {
    let path =
        std::env::temp_dir().join(format!("rillsql-catalog-{}.sqlite", uuid::Uuid::new_v4()));
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .foreign_keys(false);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE VIEW v_notes AS SELECT 1 AS id")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE INDEX idx_customers_id ON customers(id)")
        .execute(&pool)
        .await
        .unwrap();
    for index in 0..103 {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE TABLE audit_{index:04} (id INTEGER)"
        )))
        .execute(&pool)
        .await
        .unwrap();
    }
    let first = read_sqlite_pool(&pool, 0, 100).await.unwrap();
    assert_eq!(first.total, 105);
    assert_eq!(first.objects.len(), 100);
    assert!(!first.foreign_keys);
    assert!(first
        .objects
        .iter()
        .all(|item| item.kind != super::model::ObjectKind::View || item.name == "v_notes"));
    assert!(first
        .objects
        .iter()
        .all(|item| item.name != "idx_customers_id"));
    let second = read_sqlite_pool(&pool, 100, 100).await.unwrap();
    assert_eq!(second.objects.len(), 5);
    assert!(second
        .objects
        .iter()
        .any(|item| item.name == "v_notes" && item.kind == super::model::ObjectKind::View));
    pool.close().await;
    let enabled_options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(false)
        .foreign_keys(true);
    let enabled_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(enabled_options)
        .await
        .unwrap();
    let enabled = read_sqlite_pool(&enabled_pool, 0, 1).await.unwrap();
    assert!(enabled.foreign_keys);
    drop(enabled_pool);
    let _ = std::fs::remove_file(&path);

    let missing = read_sqlite_file(
        Path::new("C:/no/such/rillsql-catalog-missing.sqlite"),
        0,
        100,
    )
    .await;
    let message = missing.expect_err("缺少文件时必须失败");
    let failed = SchemaNode::failed("", message);
    assert!(matches!(
        schema_status(&failed),
        SchemaStatus::Failed { .. }
    ));
    assert!(failed.objects.is_empty());
}

#[test]
fn refresh_keeps_the_loaded_window_and_a_failed_list_is_not_empty() {
    assert_eq!(read_limit(120, 100), 120);
    assert_eq!(read_limit(0, 100), 100);
    assert_eq!(read_limit(0, 0), 100);

    let nodes = assemble_postgres(
        &["app".to_owned(), "other".to_owned()],
        "app",
        Ok(vec![schema("public", vec![row("orders")])]),
    );
    let other = nodes.iter().find(|item| item.name == "other").unwrap();
    assert!(other.error.as_deref().unwrap().contains("没有另开连接"));
    assert!(other.schemas.is_empty());
    let current = nodes.iter().find(|item| item.name == "app").unwrap();
    assert!(current.error.is_none());
    assert_eq!(current.schemas[0].objects[0].name, "orders");

    let failed_list = assemble_postgres(&["app".to_owned()], "app", Err("读取库表失败：permission denied".to_owned()));
    assert!(failed_list[0].error.is_some());
    assert!(failed_list[0].schemas.is_empty());

    let previous = connection(
        Engine::Postgres,
        ConnStatus::Connected,
        vec![DatabaseNode {
            name: "app".to_owned(),
            error: None,
            schemas: vec![schema("public", vec![row("orders"), row("customers")])],
        }],
    );
    let mut incoming = previous.clone();
    incoming.databases.clear();
    incoming.error = Some("读取库表失败：down".to_owned());
    let kept = refresh_connection(&previous, &incoming);
    assert_eq!(kept.databases[0].schemas[0].objects.len(), 2);
    assert!(kept.error.as_deref().unwrap().contains("读取库表失败"));
}

#[test]
fn catalog_commands_use_the_open_session_reader() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let capability: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("capabilities/default.json")).unwrap()).unwrap();
    let permissions = capability["permissions"].as_array().unwrap();
    let acl = std::fs::read_to_string(root.join("permissions/app-commands.toml")).unwrap();
    let registration = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
    let wire = registration.split("fn wire").nth(1).unwrap_or("");
    let wire = wire.split("pub fn run").next().unwrap_or("");
    for command in ["catalog_sessions", "catalog_refresh", "catalog_page", "catalog_definition"] {
        let allow = format!("allow-{}", command.replace('_', "-"));
        assert!(permissions.iter().any(|item| item.as_str() == Some(allow.as_str())), "{allow}");
        assert!(acl.contains(&format!("commands.allow = [\"{command}\"]")), "{command}");
        assert!(wire.contains(&format!("catalog::commands::{command}")), "{command}");
    }
    assert!(wire.contains("connections::commands::connections_open"));
    let reader = std::fs::read_to_string(root.join("src/catalog/read.rs")).unwrap();
    assert!(reader.contains("metadata_sql"));
    assert!(!reader.contains("PRAGMA foreign_keys"));
    assert!(!reader.contains("read_sqlite_file"));
    assert!(!reader.contains("read_sqlite_pool"));
    assert!(!reader.contains("open_sqlite"));
    assert!(!reader.contains("open_mysql"));
    assert!(!reader.contains("open_postgres"));
}

#[tokio::test]
async fn scripted_and_unreachable_sessions_fail_instead_of_an_empty_catalog() {
    let scripted = read_catalog_session(Engine::Mysql, &CatalogSession::Scripted, 100, &[]).await;
    assert!(scripted.databases.is_empty());
    assert!(scripted.error.as_deref().unwrap().contains("没有连着可查询"));

    let closed = page_for_session(None, "main", "", 0, 100).await;
    assert!(closed.objects.is_empty());
    assert!(closed.error.as_deref().unwrap().contains("还没有打开"));

    let opts = mysql_async::OptsBuilder::default()
        .ip_or_hostname("127.0.0.1")
        .tcp_port(1)
        .prefer_socket(Some(false))
        .user(Some("catalog"))
        .pass(Some("catalog"))
        .db_name(Some("catalog"))
        .pool_opts(mysql_async::PoolOpts::new().with_constraints(mysql_async::PoolConstraints::new(0, 1).unwrap()));
    let pool = mysql_async::Pool::new(opts);
    let failed = read_catalog_session(Engine::Mysql, &CatalogSession::Mysql(pool.clone()), 100, &[]).await;
    drop(pool);
    assert!(failed.databases.is_empty());
    assert!(failed.error.as_deref().unwrap().contains("读取库表失败"));
}

#[tokio::test]
async fn sqlite_catalog_reads_the_open_session_not_a_new_connection() {
    let dir = std::env::temp_dir().join(format!("rillsql-catalog-session-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let database = dir.join("notes.sqlite");
    let setup = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY)")
        .execute(&setup)
        .await
        .unwrap();
    sqlx::query("CREATE VIEW v_notes AS SELECT 1 AS id")
        .execute(&setup)
        .await
        .unwrap();
    sqlx::query("CREATE INDEX idx_customers_id ON customers(id)")
        .execute(&setup)
        .await
        .unwrap();
    for index in 0..103 {
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE TABLE audit_{index:04} (id INTEGER)")))
            .execute(&setup)
            .await
            .unwrap();
    }
    setup.close().await;

    let service = Service::new_for_test(dir.join("connections.sqlite"));
    service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
    assert!(service.catalog_session("notes").is_some());
    assert_eq!(service.open_session_ids(), vec!["notes".to_owned()]);
    assert!(service.set_catalog_foreign_keys("notes", Some(true)));

    let loaded = load_catalog(&service, 100, &[], None, false).await.unwrap();
    assert_eq!(loaded.len(), 1);
    let connection = &loaded[0];
    assert_eq!(connection.engine, "sqlite");
    assert_eq!(connection.status, "connected");
    assert_eq!(connection.foreign_keys, Some(true));
    assert!(connection.error.is_none());
    let schema = &connection.databases[0].schemas[0];
    assert_eq!(schema.total, 105);
    assert_eq!(schema.objects.len(), 100);
    assert!(schema.objects.iter().all(|item| item.name != "idx_customers_id"));
    assert!(schema.error.is_none());
    let json = serde_json::to_value(connection).unwrap();
    assert!(json.get("foreignKeys").is_some());
    assert_eq!(json["databases"][0]["schemas"][0]["objects"][0]["estimatedRows"], serde_json::Value::Null);

    let session = service.catalog_session("notes").unwrap();
    let next = page_for_session(Some(&session), "main", "", 100, 100).await;
    assert!(next.error.is_none());
    assert_eq!(next.objects.len(), 5);
    assert!(next.objects.iter().any(|item| item.name == "v_notes" && item.kind == "view"));
    assert_eq!(next.total, 105);

    let refreshed = read_catalog_session(
        Engine::Sqlite,
        &session,
        100,
        &[ObjectWindow {
            database: "main".to_owned(),
            schema: String::new(),
            limit: 105,
        }],
    )
    .await;
    assert!(refreshed.error.is_none());
    let full = &refreshed.databases[0].schemas[0];
    assert_eq!(full.objects.len(), 105);
    assert_eq!(full.total, 105);
    let mut previous = connection_node_from_page(&connection.databases[0].schemas[0].objects, &next.objects);
    previous.foreign_keys = Some(true);
    let merged = refresh_connection(&previous, &body_as_node(&refreshed));
    assert!(merged.databases[0].schemas[0].objects.iter().all(|item| !item.stale));
    assert_eq!(merged.databases[0].schemas[0].objects.len(), 105);

    match &session {
        CatalogSession::Sqlite { pool, .. } => pool.close().await,
        _ => panic!("会话应该是已经打开的 SQLite 池"),
    }
    let after_close = read_catalog_session(Engine::Sqlite, &session, 100, &[]).await;
    assert!(after_close.databases.is_empty());
    assert!(after_close.error.as_deref().unwrap().contains("读取库表失败"));
    assert_eq!(after_close.foreign_keys, Some(true));
    let kept = refresh_connection(&merged, &body_as_node(&after_close));
    assert_eq!(kept.databases[0].schemas[0].objects.len(), 105);
    assert!(kept.error.is_some());
    drop(service);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_failed_kind_keeps_objects_from_the_other_kinds() {
    use super::model::{KindReport, ObjectKind, ObjectRow};
    let previous = schema(
        "public",
        vec![row("orders"), ObjectRow::fresh("touch", ObjectKind::Function)],
    );
    let mut incoming = schema("public", vec![row("orders")]);
    incoming.kind_reports = vec![
        KindReport {
            kind: ObjectKind::Table,
            total: 1,
            error: None,
        },
        KindReport {
            kind: ObjectKind::Function,
            total: 0,
            error: Some("permission denied for function touch".to_owned()),
        },
    ];
    let merged = refresh_schema(&previous, &incoming);
    assert!(merged.error.is_none());
    let orders = merged.objects.iter().find(|item| item.name == "orders").unwrap();
    let touch = merged.objects.iter().find(|item| item.name == "touch").unwrap();
    assert!(!orders.stale);
    assert!(!touch.stale);
    assert!(merged.kind_reports.iter().any(|item| item.kind == ObjectKind::Function && item.error.is_some()));
}

#[tokio::test]
async fn sqlite_lists_a_trigger_and_reads_its_sql_without_inventing_routines() {
    let path = std::env::temp_dir().join(format!("rillsql-catalog-trigger-{}.sqlite", uuid::Uuid::new_v4()));
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE notes (id INTEGER PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER notes_touch AFTER UPDATE ON notes BEGIN SELECT 1; END")
        .execute(&pool)
        .await
        .unwrap();
    let page = read_sqlite_pool(&pool, 0, 100).await.unwrap();
    assert!(page.objects.iter().any(|item| item.name == "notes_touch" && item.kind == super::model::ObjectKind::Trigger));
    assert!(page.objects.iter().all(|item| item.kind != super::model::ObjectKind::Function));
    let session = CatalogSession::Sqlite {
        pool: pool.clone(),
        foreign_keys: Some(false),
    };
    let sql = read_definition(&session, "main", "", "notes_touch", "trigger", "", "").await.unwrap();
    assert!(sql.to_ascii_uppercase().contains("CREATE TRIGGER"));
    let missing = read_definition(&session, "main", "", "missing", "trigger", "", "").await.unwrap_err();
    assert!(missing.contains("没有读到定义"));
    assert!(!missing.trim().is_empty());
    let routine = read_definition(&session, "main", "", "notes", "function", "", "").await.unwrap_err();
    assert!(routine.contains("没有函数"));
    pool.close().await;
    let _ = std::fs::remove_file(path);
}

fn connection_node_from_page(first: &[super::commands::ObjectDto], second: &[super::commands::ObjectDto]) -> ConnectionNode {
    let mut objects = Vec::new();
    for item in first.iter().chain(second) {
        objects.push(if item.kind == "view" {
            ObjectRow::view(item.name.clone())
        } else {
            ObjectRow::table(item.name.clone(), item.estimated_rows)
        });
    }
    let total = objects.len();
    connection(
        Engine::Sqlite,
        ConnStatus::Connected,
        vec![DatabaseNode {
            name: "main".to_owned(),
            error: None,
            schemas: vec![SchemaNode {
                name: String::new(),
                objects,
                total,
                error: None,
                kind_reports: Vec::new(),
            }],
        }],
    )
}

fn body_as_node(body: &super::read::ConnectionBody) -> ConnectionNode {
    ConnectionNode {
        id: "notes".to_owned(),
        name: "笔记".to_owned(),
        engine: Engine::Sqlite,
        status: ConnStatus::Connected,
        foreign_keys: body.foreign_keys,
        databases: body.databases.clone(),
        error: body.error.clone(),
    }
}
