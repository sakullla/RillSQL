use std::path::PathBuf;

use crate::catalog::{kind_sql, metadata_sql, Engine as CatalogEngine, ObjectKind};
use crate::connections::model::Engine;
use crate::connections::CatalogSession;
use crate::diagram::assemble::assemble_diagram;
use crate::diagram::model::{
    empty_mysql_database, empty_postgres_schema, other_postgres_database, OutgoingKey, RangeScope, TableAttempt,
};
use crate::diagram::read::read_diagram;
use crate::diagram::sql::{diagram_statements, MYSQL_TABLES, POSTGRES_TABLES, SQLITE_TABLES};

fn key(name: &str, columns: &[&str], ref_schema: &str, ref_table: &str, ref_columns: &[&str]) -> OutgoingKey {
    OutgoingKey {
        name: name.to_owned(),
        columns: columns.iter().map(|item| (*item).to_owned()).collect(),
        ref_schema: ref_schema.to_owned(),
        ref_table: ref_table.to_owned(),
        ref_columns: ref_columns.iter().map(|item| (*item).to_owned()).collect(),
    }
}

fn ready(name: &str, columns: &[&str], keys: Vec<OutgoingKey>) -> TableAttempt {
    TableAttempt {
        name: name.to_owned(),
        columns: Ok(columns.iter().map(|item| (*item).to_owned()).collect()),
        foreign_keys: Ok(keys),
    }
}

fn failed(name: &str, reason: &str) -> TableAttempt {
    TableAttempt {
        name: name.to_owned(),
        columns: Err(reason.to_owned()),
        foreign_keys: Ok(Vec::new()),
    }
}

#[test]
fn diagram_links_in_range_foreign_keys_and_keeps_tables_without_them() {
    let scope = RangeScope {
        database: "bookstore".to_owned(),
        schema: "public".to_owned(),
        fold_names: false,
    };
    let diagram = assemble_diagram(
        &scope,
        vec![
            ready("orders", &["id", "customer_id"], vec![key("orders_customer_fkey", &["customer_id"], "public", "customers", &["id"])]),
            ready("customers", &["id", "name"], Vec::new()),
            ready("notes", &["id", "body"], Vec::new()),
        ],
    );
    assert!(diagram.notice.is_none());
    assert_eq!(
        diagram.tables.iter().map(|table| table.name.as_str()).collect::<Vec<_>>(),
        vec!["customers", "notes", "orders"]
    );
    assert_eq!(diagram.tables.iter().find(|table| table.name == "customers").unwrap().columns, vec!["id", "name"]);
    assert!(diagram.tables.iter().all(|table| table.name != "v_orders"));
    assert_eq!(diagram.edges.len(), 1);
    assert_eq!(diagram.edges[0].from_table, "orders");
    assert_eq!(diagram.edges[0].from_columns, vec!["customer_id"]);
    assert_eq!(diagram.edges[0].to_table, "customers");
    assert_eq!(diagram.edges[0].to_columns, vec!["id"]);
    assert!(diagram.omitted.is_empty());
}

#[test]
fn diagram_omits_one_failed_table_and_explains_an_empty_range() {
    let scope = RangeScope {
        database: "bookstore".to_owned(),
        schema: "public".to_owned(),
        fold_names: false,
    };
    let diagram = assemble_diagram(
        &scope,
        vec![
            ready("orders", &["id", "customer_id"], vec![key("orders_customer_fkey", &["customer_id"], "public", "customers", &["id"])]),
            failed("customers", "permission denied"),
            ready("notes", &["id"], Vec::new()),
        ],
    );
    assert!(diagram.tables.iter().all(|table| table.name != "customers"));
    assert_eq!(diagram.tables.iter().map(|table| table.name.as_str()).collect::<Vec<_>>(), vec!["notes", "orders"]);
    assert!(diagram.edges.is_empty());
    assert_eq!(diagram.omitted.len(), 1);
    assert_eq!(diagram.omitted[0].name, "customers");
    assert!(diagram.omitted[0].message.contains("customers"));
    assert!(diagram.omitted[0].message.contains("permission denied"));
    assert!(diagram.notice.is_none());

    let empty = assemble_diagram(&scope, Vec::new());
    assert!(empty.tables.is_empty());
    assert_eq!(empty.notice.as_deref(), Some("当前范围没有表"));
}

#[test]
fn diagram_notes_an_outside_foreign_key_without_adding_a_node() {
    let scope = RangeScope {
        database: "bookstore".to_owned(),
        schema: "public".to_owned(),
        fold_names: false,
    };
    let diagram = assemble_diagram(
        &scope,
        vec![ready(
            "orders",
            &["id", "account_id"],
            vec![key("orders_account_fkey", &["account_id"], "other", "accounts", &["id"])],
        )],
    );
    assert_eq!(diagram.tables.len(), 1);
    assert!(diagram.tables.iter().all(|table| table.name != "accounts"));
    assert!(diagram.edges.is_empty());
    let note = diagram.tables[0].outside.join(" ");
    assert!(note.contains("orders_account_fkey"));
    assert!(note.contains("other.accounts"));
    assert!(note.contains("不在当前范围"));
}

#[test]
fn diagram_range_refuses_an_empty_postgres_schema_and_another_database() {
    assert_eq!(empty_postgres_schema("").as_deref(), Some("PostgreSQL 关系图需要一个 schema。"));
    assert_eq!(empty_postgres_schema("  ").as_deref(), Some("PostgreSQL 关系图需要一个 schema。"));
    assert!(empty_postgres_schema("public").is_none());
    assert_eq!(
        other_postgres_database("bookstore", "other").as_deref(),
        Some("当前会话连在 bookstore，没有另开连接读取 other")
    );
    assert!(other_postgres_database("bookstore", "bookstore").is_none());
    assert!(empty_mysql_database("").is_some());
    assert!(empty_mysql_database("shop").is_none());
}

#[test]
fn diagram_statements_are_a_dedicated_read_of_base_tables() {
    let statements = diagram_statements();
    for statement in statements {
        let lowered = statement.to_ascii_lowercase();
        assert!(lowered.trim_start().starts_with("select "), "{statement}");
        assert!(!lowered.contains("limit") && !lowered.contains("offset"), "{statement}");
        for word in [
            "insert ", "update ", "delete ", "alter ", "create ", "drop ", "truncate ", "replace ", "grant ", "revoke ", "attach ", "detach ",
            "vacuum", "reindex", "analyze", "analyse", "commit", "rollback", "begin ", "use ",
        ] {
            assert!(!lowered.contains(word), "{statement} contains {word}");
        }
    }
    assert!(POSTGRES_TABLES.contains("relkind IN ('r', 'p')"));
    assert!(!POSTGRES_TABLES.contains("'v'") && !POSTGRES_TABLES.to_ascii_lowercase().contains("view"));
    assert!(!POSTGRES_TABLES.to_ascii_lowercase().contains("function"));
    assert!(!POSTGRES_TABLES.to_ascii_lowercase().contains("procedure"));
    assert!(!POSTGRES_TABLES.to_ascii_lowercase().contains("trigger"));
    assert!(!POSTGRES_TABLES.to_ascii_lowercase().contains("sequence"));
    assert!(MYSQL_TABLES.contains("TABLE_TYPE = 'BASE TABLE'"));
    assert!(!MYSQL_TABLES.contains("VIEW"));
    assert!(SQLITE_TABLES.contains("type = 'table'"));
    assert!(!SQLITE_TABLES.contains("'view'") && !SQLITE_TABLES.contains("'trigger'"));

    for engine in [CatalogEngine::Postgres, CatalogEngine::Mysql, CatalogEngine::Mariadb, CatalogEngine::Sqlite] {
        let meta = metadata_sql(engine);
        for statement in statements {
            assert_ne!(*statement, meta.databases);
            assert_ne!(*statement, meta.objects);
            if let Some(schemas) = meta.schemas {
                assert_ne!(*statement, schemas);
            }
        }
        for kind in [
            ObjectKind::Table,
            ObjectKind::View,
            ObjectKind::Function,
            ObjectKind::Procedure,
            ObjectKind::Trigger,
            ObjectKind::Sequence,
        ] {
            let sql = kind_sql(engine, kind);
            for statement in statements {
                assert_ne!(*statement, sql.count);
                assert_ne!(*statement, sql.list);
            }
        }
    }
}

#[test]
fn diagram_source_does_not_reuse_catalog_paging_or_structure_describe() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/diagram");
    for name in ["assemble.rs", "commands.rs", "model.rs", "read.rs", "sql.rs", "mod.rs"] {
        let text = std::fs::read_to_string(root.join(name)).unwrap_or_else(|_| panic!("missing {name}"));
        for needle in ["metadata_sql", "kind_sql", "ddl_describe", "ddl::", "catalog::read", "connections_open", "connections_focus", "ATTACH", "USE "] {
            assert!(!text.contains(needle), "{name} contains {needle}");
        }
    }
}

#[tokio::test]
async fn diagram_refuses_a_closed_or_unscoped_session_without_writing() {
    let empty_schema = read_diagram(Engine::Postgres, &CatalogSession::Scripted, "other", "  ").await.unwrap_err();
    assert_eq!(empty_schema, "PostgreSQL 关系图需要一个 schema。");
    let closed = read_diagram(Engine::Postgres, &CatalogSession::Scripted, "bookstore", "public").await.unwrap_err();
    assert!(closed.contains("没有连着可查询的数据库"));
    let mysql = read_diagram(Engine::Mysql, &CatalogSession::Scripted, "  ", "").await.unwrap_err();
    assert!(mysql.contains("需要一个数据库"));
}

#[tokio::test]
async fn sqlite_diagram_reads_base_tables_and_does_not_write() {
    let options = sqlx::sqlite::SqliteConnectOptions::new().in_memory(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect_with(options).await.unwrap();
    sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT)").execute(&pool).await.unwrap();
    sqlx::query("CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER, note TEXT, FOREIGN KEY (customer_id) REFERENCES customers(id))")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)").execute(&pool).await.unwrap();
    sqlx::query("CREATE TABLE invoices (id INTEGER PRIMARY KEY, customer_id INTEGER, FOREIGN KEY (customer_id) REFERENCES missing_customers(id))")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE VIEW v_orders AS SELECT id FROM orders").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER notes_touch AFTER UPDATE ON notes BEGIN SELECT 1; END").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO customers (id, name) VALUES (1, 'Ada')").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO orders (id, customer_id, note) VALUES (1, 1, 'one')").execute(&pool).await.unwrap();

    let master_before: Vec<(String, String, String)> =
        sqlx::query_as("SELECT type, name, COALESCE(sql, '') FROM sqlite_master ORDER BY type, name").fetch_all(&pool).await.unwrap();
    let rows_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orders").fetch_one(&pool).await.unwrap();
    let keys_before: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(&pool).await.unwrap();

    let diagram = read_diagram(Engine::Sqlite, &CatalogSession::Sqlite { pool: pool.clone(), foreign_keys: Some(false) }, "ignored", "ignored")
        .await
        .unwrap();

    let master_after: Vec<(String, String, String)> =
        sqlx::query_as("SELECT type, name, COALESCE(sql, '') FROM sqlite_master ORDER BY type, name").fetch_all(&pool).await.unwrap();
    let rows_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orders").fetch_one(&pool).await.unwrap();
    let keys_after: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(&pool).await.unwrap();
    assert_eq!(master_before, master_after);
    assert_eq!(rows_before, rows_after);
    assert_eq!(keys_before, keys_after);

    assert_eq!(
        diagram.tables.iter().map(|table| table.name.as_str()).collect::<Vec<_>>(),
        vec!["customers", "invoices", "notes", "orders"]
    );
    assert!(diagram.tables.iter().all(|table| table.name != "v_orders" && table.name != "notes_touch"));
    let customers = diagram.tables.iter().find(|table| table.name == "customers").unwrap();
    assert_eq!(customers.columns, vec!["id", "name"]);
    let orders = diagram.edges.iter().find(|edge| edge.from_table == "orders").unwrap();
    assert_eq!(orders.from_columns, vec!["customer_id"]);
    assert_eq!(orders.to_table, "customers");
    assert_eq!(orders.to_columns, vec!["id"]);
    assert!(diagram.tables.iter().any(|table| table.name == "notes"));
    assert!(!diagram.edges.iter().any(|edge| edge.from_table == "notes" || edge.to_table == "notes"));
    let invoices = diagram.tables.iter().find(|table| table.name == "invoices").unwrap();
    assert!(invoices.outside.iter().any(|note| note.contains("missing_customers") && note.contains("不在当前范围")));
    assert!(diagram.tables.iter().all(|table| table.name != "missing_customers"));
    assert!(diagram.notice.is_none());

    let empty_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().in_memory(true))
        .await
        .unwrap();
    let empty = read_diagram(Engine::Sqlite, &CatalogSession::Sqlite { pool: empty_pool, foreign_keys: None }, "main", "").await.unwrap();
    assert_eq!(empty.notice.as_deref(), Some("当前范围没有表"));
    assert!(empty.tables.is_empty());
}
