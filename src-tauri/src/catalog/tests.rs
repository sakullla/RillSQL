use std::path::Path;

use super::model::{
    metadata_sql, schema_status, ConnStatus, ConnectionNode, DatabaseNode, Engine, ObjectRow,
    SchemaNode, SchemaStatus,
};
use super::sqlite::{read_sqlite_file, read_sqlite_pool};
use super::tree::{
    direct_objects, object_path, refresh_connection, refresh_schema, search_objects, take_page,
    visible_database_names, visible_schema_names,
};

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
    assert!(postgres_sql.schemas.is_some());
    assert!(mysql_sql.objects.contains("information_schema.tables"));
    assert!(maria_sql.objects.contains("information_schema.tables"));
    assert!(mysql_sql.schemas.is_none() && maria_sql.schemas.is_none());
    assert_eq!(mysql_sql.objects, maria_sql.objects);
    assert!(sqlite_sql.objects.contains("sqlite_master"));
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
