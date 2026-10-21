use std::collections::BTreeMap;
use std::fs;

use super::data::diff_data;
use super::model::{
    needs_ephemeral_postgres, Category, ColumnShape, DataColumn, DataInput, DataKey, DataTable, Engine, IndexShape, Kind, ObjectDef, Side, TableShape,
};
use super::structure::diff_structure;
use crate::grid::{insert_sql, Cell, Column, TableSchema};

fn column(name: &str, type_name: &str, nullable: bool, primary: bool) -> ColumnShape {
    ColumnShape { name: name.to_owned(), type_name: type_name.to_owned(), nullable, default_sql: String::new(), primary }
}

fn shape(columns: Vec<ColumnShape>, indexes: Vec<IndexShape>) -> TableShape {
    TableShape { columns, indexes, foreign_keys: Vec::new() }
}

fn orders(type_name: &str) -> ObjectDef {
    ObjectDef::table("orders", shape(vec![column("id", "integer", false, true), column("note", type_name, true, false)], Vec::new()))
}

fn index(name: &str, unique: bool, columns: &[(&str, bool)]) -> IndexShape {
    IndexShape {
        name: name.to_owned(),
        unique,
        columns: columns.iter().map(|(name, descending)| ((*name).to_owned(), *descending)).collect(),
        protected: false,
        reason: String::new(),
        partial: false,
        expression: false,
        sql: String::new(),
    }
}

fn row(pairs: &[(&str, Option<&str>)]) -> BTreeMap<String, Cell> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), match value { Some(text) => Cell::Text((*text).to_owned()), None => Cell::Null }))
        .collect()
}

fn data_table(name: &str, columns: Vec<DataColumn>, keys: Vec<DataKey>, baseline: Vec<BTreeMap<String, Cell>>, target: Vec<BTreeMap<String, Cell>>) -> DataTable {
    DataTable { name: name.to_owned(), qualify: String::new(), columns, keys, chosen_key: None, baseline, target, read_error: None, only_side: None }
}

fn orders_columns() -> Vec<DataColumn> {
    vec![
        DataColumn { name: "id".to_owned(), type_name: "integer".to_owned(), nullable: false },
        DataColumn { name: "note".to_owned(), type_name: "text".to_owned(), nullable: true },
    ]
}

fn primary() -> Vec<DataKey> {
    vec![DataKey { name: "orders_pkey".to_owned(), columns: vec!["id".to_owned()], primary: true }]
}

#[test]
fn same_engine_structure_script_is_not_a_refusal() {
    let baseline = Side::ready("postgres", "app", "public", vec![orders("text")]);
    let target = Side::ready("postgres", "app", "public", vec![]);
    let report = diff_structure(&baseline, &target);
    assert!(report.refused.is_none());
    assert!(report.only_baseline.iter().any(|item| item.name == "orders"));
    assert!(report.script.contains("CREATE TABLE"));
    assert!(report.script.contains("orders"));
    assert!(report.script.contains("生成时没有执行"));
    assert!(!report.script.contains("没有结构差异"));
}

#[test]
fn column_type_difference_is_an_alter_statement() {
    let report = diff_structure(&Side::ready("postgres", "app", "public", vec![orders("text")]), &Side::ready("postgres", "app", "public", vec![orders("varchar(20)")]));
    assert!(report.changed.iter().any(|item| item.name == "orders"));
    assert!(report.script.contains("ALTER TABLE"));
    assert!(report.script.contains("note"));
    assert!(report.script.to_ascii_lowercase().contains("text"));
}

#[test]
fn identical_structure_has_no_change_statements() {
    let report = diff_structure(&Side::ready("mysql", "app", "", vec![orders("int")]), &Side::ready("mysql", "app", "", vec![orders("int")]));
    assert!(report.script.contains("没有结构差异"));
    let upper = report.script.to_ascii_uppercase();
    assert!(!upper.contains("CREATE"));
    assert!(!upper.contains("ALTER"));
    assert!(!upper.contains("DROP"));
}

#[test]
fn different_engines_do_not_generate_a_script() {
    let report = diff_structure(&Side::ready("mysql", "app", "", vec![orders("int")]), &Side::ready("mariadb", "app", "", vec![orders("int")]));
    assert!(report.refused.unwrap().contains("引擎不同"));
    assert!(report.script.is_empty());
    assert!(report.only_baseline.is_empty());
}

#[test]
fn index_read_failure_omits_index_statements_but_keeps_table_diffs() {
    let mut baseline = Side::ready(
        "postgres",
        "app",
        "public",
        vec![ObjectDef::table(
            "orders",
            shape(vec![column("id", "integer", false, true), column("note", "text", true, false)], vec![index("orders_note", false, &[("note", false)])]),
        )],
    );
    baseline.category_errors.push((Category::Index, "permission denied".to_owned()));
    let target = Side::ready("postgres", "app", "public", vec![orders("varchar(20)")]);
    let report = diff_structure(&baseline, &target);
    assert!(report.changed.iter().any(|item| item.kind == "表" && item.name == "orders"));
    assert!(report.incomplete);
    assert!(report.script.contains("不完整"));
    assert!(report.script.contains("索引读取失败"));
    let upper = report.script.to_ascii_uppercase();
    assert!(!upper.contains("CREATE INDEX"));
    assert!(!upper.contains("DROP INDEX"));
    assert!(upper.contains("ALTER TABLE"));
}

#[test]
fn routines_are_definition_text_only() {
    let baseline = Side::ready("postgres", "app", "public", vec![ObjectDef::defined(Kind::Function, "add_customer", "CREATE FUNCTION add_customer() RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$")]);
    let target = Side::ready("postgres", "app", "public", vec![ObjectDef::defined(Kind::Function, "add_customer", "CREATE FUNCTION add_customer() RETURNS integer LANGUAGE sql AS $$ SELECT 2 $$")]);
    let report = diff_structure(&baseline, &target);
    assert!(report.changed.iter().any(|item| item.kind == "函数" && item.detail.contains("定义文本")));
    assert!(report.script.contains("SELECT 1"));
    assert!(!report.script.contains("例程表单"));
}

#[test]
fn cancelled_listing_is_incomplete_and_does_not_drop_or_create() {
    let baseline = Side::ready("postgres", "app", "public", vec![orders("integer")]);
    let mut target = Side::ready("postgres", "app", "public", vec![]);
    target.cancelled = true;
    target.listing_complete = false;
    let report = diff_structure(&baseline, &target);
    assert!(report.only_baseline.iter().any(|item| item.name == "orders"));
    assert!(report.incomplete);
    assert!(report.script.contains("不完整"));
    assert!(!report.script.contains("没有结构差异"));
    assert!(!report.script.to_ascii_uppercase().contains("CREATE"));
    assert!(!report.script.to_ascii_uppercase().contains("DROP"));
}

#[test]
fn data_script_inserts_updates_and_deletes_without_executing_a_connection() {
    let table = data_table(
        "orders",
        orders_columns(),
        primary(),
        vec![row(&[("id", Some("1")), ("note", Some("a"))]), row(&[("id", Some("3")), ("note", None)]), row(&[("id", Some("4")), ("note", None)])],
        vec![row(&[("id", Some("1")), ("note", Some("b"))]), row(&[("id", Some("2")), ("note", Some("x"))]), row(&[("id", Some("4")), ("note", Some(""))])],
    );
    let report = diff_data(&DataInput { baseline_engine: "sqlite".to_owned(), target_engine: "sqlite".to_owned(), tables: vec![table], cancelled: false });
    assert!(report.refused.is_none());
    assert!(report.script.contains("INSERT"));
    assert!(report.script.contains("DELETE"));
    assert!(report.script.contains("UPDATE"));
    assert!(report.script.contains("NULL"));
    assert!(report.diffs.iter().any(|item| item.action == "update" && item.key == "4"));
    let schema = TableSchema {
        engine: crate::grid::Engine::Sqlite,
        schema: String::new(),
        name: "orders".to_owned(),
        kind: "table",
        columns: vec![
            Column { name: "id".to_owned(), type_name: "integer".to_owned(), nullable: false, primary: true, unique: false, auto: false },
            Column { name: "note".to_owned(), type_name: "text".to_owned(), nullable: true, primary: false, unique: false, auto: false },
        ],
        key: vec!["id".to_owned()],
    };
    let expected = insert_sql(&schema, &[("id".to_owned(), Cell::Text("3".to_owned())), ("note".to_owned(), Cell::Null)]);
    assert!(report.script.contains(&expected));
    assert!(report.script.contains("生成时没有执行"));
}

#[test]
fn tables_without_a_key_do_not_invent_rows() {
    let table = data_table("notes", vec![DataColumn { name: "label".to_owned(), type_name: "text".to_owned(), nullable: true }], Vec::new(), vec![row(&[("label", Some("a"))])], Vec::new());
    let report = diff_data(&DataInput { baseline_engine: "sqlite".to_owned(), target_engine: "sqlite".to_owned(), tables: vec![table], cancelled: false });
    assert!(report.script.contains("无法对应行"));
    assert!(!report.script.to_ascii_uppercase().contains("INSERT"));
    assert!(!report.script.to_ascii_uppercase().contains("DELETE"));
}

#[test]
fn multiple_unique_keys_wait_for_a_choice() {
    let mut waiting = data_table(
        "twin",
        vec![
            DataColumn { name: "email".to_owned(), type_name: "text".to_owned(), nullable: true },
            DataColumn { name: "code".to_owned(), type_name: "text".to_owned(), nullable: true },
        ],
        vec![DataKey { name: "email".to_owned(), columns: vec!["email".to_owned()], primary: false }, DataKey { name: "code".to_owned(), columns: vec!["code".to_owned()], primary: false }],
        vec![row(&[("email", Some("a@b.c")), ("code", Some("a"))])],
        Vec::new(),
    );
    let held = diff_data(&DataInput { baseline_engine: "mysql".to_owned(), target_engine: "mysql".to_owned(), tables: vec![waiting.clone()], cancelled: false });
    assert!(held.script.contains("选定前"));
    assert!(!held.script.to_ascii_uppercase().contains("INSERT"));
    waiting.chosen_key = Some("email".to_owned());
    let chosen = diff_data(&DataInput { baseline_engine: "mysql".to_owned(), target_engine: "mysql".to_owned(), tables: vec![waiting], cancelled: false });
    assert!(chosen.script.contains("INSERT"));
    assert!(chosen.script.contains("email"));
    assert!(chosen.notes.iter().any(|note| note.contains("email")));
}

#[test]
fn unsafe_literals_skip_the_row() {
    let table = data_table(
        "geo",
        vec![DataColumn { name: "id".to_owned(), type_name: "integer".to_owned(), nullable: false }, DataColumn { name: "g".to_owned(), type_name: "geometry".to_owned(), nullable: true }],
        vec![DataKey { name: "PRIMARY".to_owned(), columns: vec!["id".to_owned()], primary: true }],
        vec![row(&[("id", Some("1")), ("g", Some("POINT(1 2)"))])],
        Vec::new(),
    );
    let report = diff_data(&DataInput { baseline_engine: "postgres".to_owned(), target_engine: "postgres".to_owned(), tables: vec![table], cancelled: false });
    assert!(report.diffs.iter().any(|item| item.action == "rejected" && item.key == "1"));
    assert!(report.script.contains("未能生成语句"));
    assert!(!report.script.to_ascii_uppercase().contains("INSERT"));
    assert!(!report.script.contains("POINT(1 2)"));
}

#[test]
fn cancelled_data_keeps_rows_already_read() {
    let table = data_table("orders", orders_columns(), primary(), vec![row(&[("id", Some("1")), ("note", Some("a"))])], Vec::new());
    let report = diff_data(&DataInput { baseline_engine: "sqlite".to_owned(), target_engine: "sqlite".to_owned(), tables: vec![table], cancelled: true });
    assert!(report.incomplete);
    assert!(report.script.contains("不完整"));
    assert!(report.script.contains("INSERT"));
    assert!(!report.script.contains("没有数据差异"));
}

#[test]
fn one_sided_table_is_structural() {
    let mut table = data_table("orders", orders_columns(), primary(), Vec::new(), Vec::new());
    table.only_side = Some("基准".to_owned());
    let report = diff_data(&DataInput { baseline_engine: "sqlite".to_owned(), target_engine: "sqlite".to_owned(), tables: vec![table], cancelled: false });
    assert!(report.script.contains("结构差异"));
    assert!(!report.script.to_ascii_uppercase().contains("INSERT"));
}

#[test]
fn postgres_other_database_needs_an_ephemeral_session() {
    assert!(needs_ephemeral_postgres("postgres", "app", "other"));
    assert!(!needs_ephemeral_postgres("postgres", "app", "app"));
    assert!(!needs_ephemeral_postgres("mysql", "app", "other"));
    assert_eq!(Engine::Mysql.as_str(), "mysql");
}

#[test]
fn compare_commands_are_registered_and_do_not_focus_or_execute() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let capability: serde_json::Value = serde_json::from_str(&fs::read_to_string(root.join("capabilities/default.json")).unwrap()).unwrap();
    let permissions = capability["permissions"].as_array().unwrap();
    let acl = fs::read_to_string(root.join("permissions/app-commands.toml")).unwrap();
    let registration = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    let wire = registration.split("fn wire").nth(1).unwrap_or("");
    let wire = wire.split("pub fn run").next().unwrap_or("");
    for command in ["compare_structure", "compare_data", "compare_cancel"] {
        let allow = format!("allow-{}", command.replace('_', "-"));
        assert!(permissions.iter().any(|item| item.as_str() == Some(allow.as_str())), "{allow}");
        assert!(acl.contains(&format!("commands.allow = [\"{command}\"]")), "{command}");
        assert!(wire.contains(&format!("compare::commands::{command}")), "{command}");
    }
    let reader = include_str!("read.rs");
    let commands = include_str!("commands.rs");
    assert!(!reader.contains("focus_database"));
    assert!(!commands.contains("focus_database"));
    assert!(!commands.contains("query_start"));
    assert!(!commands.contains("ddl_execute"));
    let service = include_str!("../connections/service.rs");
    let start = service.find("fn open_ephemeral_postgres").unwrap();
    let body = &service[start..];
    let end = body.find("pub fn disconnect").unwrap();
    let body = &body[..end];
    assert!(!body.contains("focus_database"));
    assert!(!body.contains(".insert("));
    assert!(!body.contains(".save("));
}

#[tokio::test]
async fn ephemeral_open_keeps_the_current_session_and_saved_config() {
    let dir = std::env::temp_dir().join(format!("rillsql-compare-ephemeral-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let database = dir.join("notes.sqlite");
    let setup = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
        .await
        .unwrap();
    setup.close().await;
    let service = crate::connections::Service::new_for_test(dir.join("connections.sqlite"));
    service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
    let before = service.list().await.unwrap();
    let error = match service.open_ephemeral_postgres("notes", "other").await {
        Err(error) => error,
        Ok(_) => panic!("SQLite 不应打开临时读取会话"),
    };
    assert!(error.contains("只有 PostgreSQL"));
    assert!(service.catalog_session("notes").is_some());
    let after = service.list().await.unwrap();
    assert_eq!(before[0].saved.database, after[0].saved.database);
    assert_eq!(before[0].saved.file_path, after[0].saved.file_path);
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sqlite_compare_reads_both_connections_and_does_not_execute_the_script() {
    let dir = std::env::temp_dir().join(format!("rillsql-compare-sqlite-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let baseline_file = dir.join("baseline.sqlite");
    let target_file = dir.join("target.sqlite");
    let baseline = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&baseline_file).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT)")
        .execute(&baseline)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE orders (id INTEGER PRIMARY KEY, note TEXT)")
        .execute(&baseline)
        .await
        .unwrap();
    sqlx::query("INSERT INTO orders (id, note) VALUES (1, 'a'), (3, NULL)")
        .execute(&baseline)
        .await
        .unwrap();
    baseline.close().await;
    let target = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&target_file).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE orders (id INTEGER PRIMARY KEY, note INTEGER)")
        .execute(&target)
        .await
        .unwrap();
    sqlx::query("INSERT INTO orders (id, note) VALUES (1, 2), (2, 9)")
        .execute(&target)
        .await
        .unwrap();
    target.close().await;

    let service = crate::connections::Service::new_for_test(dir.join("connections.sqlite"));
    service.open_sqlite_for_test("base", "基准", &baseline_file).await.unwrap();
    service.open_sqlite_for_test("other", "对照", &target_file).await.unwrap();
    let structure = super::read::read_structure(
        &service,
        &super::model::StructureRequest {
            id: "structure".to_owned(),
            baseline: super::model::SideRequest { connection_id: "base".to_owned(), database: "main".to_owned(), schema: String::new(), engine: "sqlite".to_owned() },
            target: super::model::SideRequest { connection_id: "other".to_owned(), database: "main".to_owned(), schema: String::new(), engine: "sqlite".to_owned() },
        },
    )
    .await;
    assert!(structure.refused.is_none(), "{:?}", structure.refused);
    assert!(structure.only_baseline.iter().any(|item| item.name == "customers"), "{:?}", structure.only_baseline);
    assert!(structure.script.contains("customers"));
    assert!(structure.script.to_ascii_uppercase().contains("CREATE TABLE"));
    assert!(structure.changed.iter().any(|item| item.name == "orders"), "{:?}", structure.changed);
    assert!(structure.script.to_ascii_uppercase().contains("TEXT"));

    let session = service.catalog_session("other").unwrap();
    let missing = crate::grid::describe(&session, "main", "", "customers", "sqlite").await.unwrap_err();
    assert!(missing.contains("找不到"));
    let described = crate::grid::describe(&session, "main", "", "orders", "sqlite").await.unwrap();
    let note = described.columns.iter().find(|column| column.name == "note").unwrap();
    assert!(note.type_name.eq_ignore_ascii_case("INTEGER"));

    let data = super::read::read_data(
        &service,
        &super::model::DataRequest {
            id: "data".to_owned(),
            baseline: super::model::SideRequest { connection_id: "base".to_owned(), database: "main".to_owned(), schema: String::new(), engine: "sqlite".to_owned() },
            target: super::model::SideRequest { connection_id: "other".to_owned(), database: "main".to_owned(), schema: String::new(), engine: "sqlite".to_owned() },
            tables: vec![super::model::DataTableRequest { name: "orders".to_owned(), key: None }, super::model::DataTableRequest { name: "customers".to_owned(), key: None }],
        },
    )
    .await;
    assert!(data.refused.is_none(), "{:?}", data.refused);
    assert!(data.script.contains("INSERT"), "{}", data.script);
    assert!(data.script.contains("DELETE"), "{}", data.script);
    assert!(data.script.contains("UPDATE"), "{}", data.script);
    assert!(data.script.contains("结构差异"), "{}", data.script);
    let crate::connections::CatalogSession::Sqlite { pool, .. } = service.catalog_session("other").unwrap() else {
        panic!("对照会话还在");
    };
    let note: i64 = sqlx::query_scalar("SELECT note FROM orders WHERE id = 1").fetch_one(&pool).await.unwrap();
    assert_eq!(note, 2);
    let extra: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orders WHERE id = 2").fetch_one(&pool).await.unwrap();
    assert_eq!(extra, 1);
    let _ = fs::remove_dir_all(&dir);
}
