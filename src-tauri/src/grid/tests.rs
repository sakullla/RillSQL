use std::fs;

use super::export::{plan_export, render_csv, start_export, ExportScope, Origin, WriteControl};
use super::sql::select_export_gate;
use super::store::{commit_changes, customers_fixture, nokey_fixture, query_page, Cell, Change, Filter, FilterOp};
use super::module_id;

fn filt(column: &str, op: FilterOp, value: Option<&str>) -> Filter {
    Filter { column: column.to_owned(), op, value: value.map(str::to_owned), second: None, values: Vec::new() }
}

#[test]
fn module_name_stays_grid() {
    assert_eq!(module_id(), "grid");
}

#[test]
fn batch_is_truncated_until_the_next_read() {
    let store = customers_fixture(130);
    let page = query_page(&store, &[], None, 0, 100).unwrap();
    assert_eq!(page.rows.len(), 100);
    assert!(page.truncated);
    assert!(page.sql.contains("LIMIT 100"));
    assert_eq!(page.rows[0].cells.get("note"), Some(&Cell::Null));
    assert_eq!(page.rows[1].cells.get("note"), Some(&Cell::Text(String::new())));
    let rest = query_page(&store, &[], None, 100, 100).unwrap();
    assert_eq!(page.rows.len() + rest.rows.len(), 130);
    assert!(!rest.truncated);
}

#[test]
fn server_filter_sees_rows_outside_the_first_page() {
    let store = customers_fixture(130);
    let page = query_page(&store, &[filt("id", FilterOp::Gt, Some("100"))], None, 0, 100).unwrap();
    assert_eq!(page.rows.len(), 30);
    assert_eq!(page.rows[0].cells.get("id"), Some(&Cell::Text("101".to_owned())));
    assert!(page.sql.contains("> 100"));
    assert!(!page.sql.contains(" OR "));
    let injected = query_page(&store, &[filt("name", FilterOp::Eq, Some("a' OR '1'='1"))], None, 0, 100).unwrap();
    assert!(injected.sql.contains("'a'' OR ''1''=''1'"));
    assert!(injected.rows.is_empty());
    let nulls = query_page(&store, &[filt("note", FilterOp::Null, None)], None, 0, 10).unwrap();
    let blanks = query_page(&store, &[filt("note", FilterOp::Empty, None)], None, 0, 10).unwrap();
    assert_eq!(nulls.rows.len(), 1);
    assert_eq!(blanks.rows[0].cells.get("note"), Some(&Cell::Text(String::new())));
    assert!(nulls.sql.contains("IS NULL"));
}

#[test]
fn failed_read_is_not_an_empty_match() {
    let mut store = customers_fixture(4);
    store.read_error = Some("连接已断开".to_owned());
    let error = query_page(&store, &[filt("id", FilterOp::Eq, Some("1"))], None, 0, 10).unwrap_err();
    assert_eq!(error.1, false);
    assert!(error.0.contains("连接已断开"));
}

#[test]
fn nokey_insert_stays_unlocated_and_conflict_does_not_roll_back() {
    let mut notes = nokey_fixture();
    let mut values = std::collections::BTreeMap::new();
    values.insert("label".to_owned(), Some(Cell::Text("新".to_owned())));
    let inserted = commit_changes(&mut notes, &[Change::Insert { id: "new".to_owned(), values }]);
    assert!(!inserted.rolled_back);
    assert!(inserted.failed.is_none());
    assert!(inserted.applied[0].sql.starts_with("INSERT"));
    assert!(!inserted.applied[0].sql.contains("BEGIN"));
    let saved = notes.rows.iter().find(|row| row.cells.get("label") == Some(&Cell::Text("新".to_owned()))).unwrap();
    assert!(!saved.located);

    let mut store = customers_fixture(4);
    let mut rename = std::collections::BTreeMap::new();
    rename.insert("name".to_owned(), Cell::Text("已改".to_owned()));
    let mut clash = std::collections::BTreeMap::new();
    clash.insert("email".to_owned(), Cell::Text("u1@example.com".to_owned()));
    let report = commit_changes(
        &mut store,
        &[
            Change::Update { id: "u:1".to_owned(), row_id: "1".to_owned(), sets: rename },
            Change::Update { id: "u:2".to_owned(), row_id: "2".to_owned(), sets: clash },
            Change::Delete { id: "d:3".to_owned(), row_id: "3".to_owned() },
        ],
    );
    assert!(!report.rolled_back);
    assert_eq!(report.applied.len(), 1);
    assert!(report.failed.unwrap().message.contains("duplicate key"));
    assert_eq!(report.pending_ids, vec!["u:2".to_owned(), "d:3".to_owned()]);
    assert_eq!(store.rows.iter().find(|row| row.id == "1").unwrap().cells.get("name"), Some(&Cell::Text("已改".to_owned())));
    assert_eq!(store.rows.iter().find(|row| row.id == "2").unwrap().cells.get("email"), Some(&Cell::Text("u2@example.com".to_owned())));
    assert!(store.rows.iter().any(|row| row.id == "3"));
}

#[test]
fn delete_stops_when_the_locator_matches_two_rows() {
    let mut store = customers_fixture(1);
    let mut twin = store.rows[0].clone();
    twin.id = "dup".to_owned();
    store.rows.push(twin);
    let report = commit_changes(&mut store, &[Change::Delete { id: "d:1".to_owned(), row_id: "1".to_owned() }]);
    assert!(report.applied.is_empty());
    assert!(report.failed.unwrap().message.contains("影响 2 行"));
    assert!(!report.rolled_back);
    assert_eq!(store.rows.len(), 2);
}

#[test]
fn csv_distinguishes_null_and_partial_files_follow_cancel_or_failure() {
    let text = render_csv(&["id".to_owned(), "note".to_owned()], &[vec![Some("1".to_owned()), None], vec![Some("2".to_owned()), Some(String::new())], vec![Some("3".to_owned()), Some("他说\"你好\"，\n下一行".to_owned())]], "");
    assert!(text.contains("\"1\","));
    assert!(text.contains("\"2\",\"\""));
    assert!(text.contains("他说\"\"你好\"\""));
    let partial = plan_export(Origin::Table, ExportScope::Loaded, None, true, "customers.csv");
    assert!(partial.partial);
    assert_eq!(partial.file_name, "customers（部分）.csv");
    assert!(partial.notice.contains("不是完整结果"));
    assert!(select_export_gate("/* c */ SELECT 1").is_ok());
    assert!(select_export_gate("DELETE FROM t").is_err());
    assert!(select_export_gate("SELECT 1; DELETE FROM t").unwrap_err().contains("单条"));
    let blocked = plan_export(Origin::Query, ExportScope::Full, Some("UPDATE t SET a = 1"), true, "q.csv");
    assert!(!blocked.allowed);
    let allowed = plan_export(Origin::Query, ExportScope::Full, Some("SELECT 1"), false, "q.csv");
    assert!(allowed.reexecutes);
    assert!(allowed.notice.contains("重新执行这条 SELECT"));

    let dir = std::env::temp_dir().join(format!("rillsql-grid-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    let path = dir.join("out.csv");
    let mut reads = 0;
    let denied = {
        let mut read = || {
            reads += 1;
            vec![vec![Some("9".to_owned())]]
        };
        start_export(&allowed, false, &path, &["id".to_owned()], &[vec![Some("1".to_owned())]], None, Some(&mut read))
    };
    assert_eq!(denied.status, "blocked");
    assert!(!denied.success);
    assert_eq!(reads, 0);
    let done = {
        let mut read = || {
            reads += 1;
            vec![vec![Some("9".to_owned())]]
        };
        start_export(&allowed, true, &path, &["id".to_owned()], &[vec![Some("1".to_owned())]], None, Some(&mut read))
    };
    assert!(done.success);
    assert_eq!(reads, 1);
    assert!(fs::read_to_string(&path).unwrap().contains("\"9\""));

    let fail_path = dir.join("part.csv");
    let control = WriteControl { fail_after_rows: Some(1), fail_message: Some("磁盘空间不足".to_owned()), cancel_after_rows: None };
    let failed = start_export(&partial, false, &fail_path, &["id".to_owned(), "note".to_owned()], &[vec![Some("1".to_owned()), Some("甲".to_owned())], vec![Some("2".to_owned()), Some("乙".to_owned())]], Some(&control), None);
    assert!(!failed.success);
    assert!(failed.message.contains("不会冒充完整结果"));
    assert!(!fail_path.exists());
    let residual = failed.residual_path.unwrap();
    let body = fs::read_to_string(&residual).unwrap();
    assert!(body.contains("甲"));
    assert!(!body.contains("乙"));

    let cancel_path = dir.join("cancel.csv");
    let cancel = WriteControl { fail_after_rows: None, fail_message: None, cancel_after_rows: Some(1) };
    let cancelled = start_export(&partial, false, &cancel_path, &["id".to_owned()], &[vec![Some("1".to_owned())], vec![Some("2".to_owned())]], Some(&cancel), None);
    assert_eq!(cancelled.status, "cancelled");
    assert!(cancelled.message.contains("未完成的文件已删除"));
    assert!(!cancel_path.exists());
    assert!(!PathBuf::from(format!("{}.partial", cancel_path.display())).exists());
    let _ = fs::remove_dir_all(&dir);
}

use std::path::PathBuf;

#[test]
fn grid_commands_are_registered_beside_catalog() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let capability: serde_json::Value = serde_json::from_str(&fs::read_to_string(root.join("capabilities/default.json")).unwrap()).unwrap();
    let permissions = capability["permissions"].as_array().unwrap();
    let acl = fs::read_to_string(root.join("permissions/app-commands.toml")).unwrap();
    let registration = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    let wire = registration.split("fn wire").nth(1).unwrap_or("");
    let wire = wire.split("pub fn run").next().unwrap_or("");
    for command in ["grid_describe", "grid_page", "grid_count", "grid_commit", "grid_export", "grid_export_cancel"] {
        let allow = format!("allow-{}", command.replace('_', "-"));
        assert!(permissions.iter().any(|item| item.as_str() == Some(allow.as_str())), "{allow}");
        assert!(acl.contains(&format!("commands.allow = [\"{command}\"]")), "{command}");
        assert!(wire.contains(&format!("grid::commands::{command}")), "{command}");
    }
    for command in ["catalog_sessions", "catalog_refresh", "catalog_page"] {
        assert!(wire.contains(&format!("catalog::commands::{command}")), "{command}");
    }
    let drivers = fs::read_to_string(root.join("src/connections/drivers.rs")).unwrap();
    let open = drivers.split("pub async fn open_sqlite").nth(1).unwrap();
    let open = open.split("pub struct MysqlOpen").next().unwrap();
    assert!(open.contains("read_only(false)"));
    assert!(!open.contains("read_only(true)"));
    let live = fs::read_to_string(root.join("src/grid/live.rs")).unwrap();
    assert!(!live.contains("open_sqlite"));
    assert!(!live.contains("open_mysql"));
    assert!(!live.contains("open_postgres"));
    assert!(!live.contains("danger_accept_invalid"));
}

#[test]
fn preview_sql_literals_and_split_follow_the_query_scanner() {
    use super::sql::{self, select_export_gate_for};
    use super::store::{Cell, Column, Engine, TableSchema};

    fn col(name: &str, type_name: &str) -> Column {
        Column { name: name.to_owned(), type_name: type_name.to_owned(), nullable: true, primary: false, unique: false, auto: false }
    }
    fn schema(engine: Engine, kind: &'static str, key: &[&str], columns: Vec<Column>) -> TableSchema {
        TableSchema {
            engine,
            schema: "public".to_owned(),
            name: "customers".to_owned(),
            kind,
            columns,
            key: key.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    let columns = vec![col("id", "integer"), col("note", "text"), col("flag", "boolean"), col("blob", "bytea"), col("shape", "geometry")];
    let keyed = schema(Engine::Postgres, "table", &["id"], columns.clone());
    let browse = sql::select_list(&keyed, false);
    assert!(browse.contains("::text"), "{browse}");
    assert!(browse.contains("chr(1)"), "{browse}");
    assert!(browse.contains("encode("), "{browse}");
    let opened = sql::select_list(&keyed, true);
    assert!(opened.contains("::text"), "{opened}");
    assert!(opened.contains("encode("), "{opened}");
    assert!(!opened.contains("chr(1)"), "{opened}");
    assert!(!sql::reads_full_values(&keyed, &[]));
    assert!(sql::reads_full_values(&keyed, &[filt("id", FilterOp::Eq, Some("1"))]));
    let view = sql::select_list(&schema(Engine::Postgres, "view", &["id"], columns.clone()), true);
    assert!(!view.contains("chr(1)"), "{view}");
    let nokey = sql::select_list(&schema(Engine::Postgres, "table", &[], columns), true);
    assert!(!nokey.contains("chr(1)"), "{nokey}");
    assert!(nokey.contains("encode("), "{nokey}");

    let mysql = schema(Engine::Mysql, "table", &["id"], vec![col("id", "int"), col("note", "varchar"), col("blob", "blob")]);
    let mysql_sql = sql::select_list(&mysql, false);
    assert!(mysql_sql.contains("CAST(") && mysql_sql.contains("AS CHAR"), "{mysql_sql}");
    assert!(mysql_sql.contains("CHAR(1)"), "{mysql_sql}");
    let sqlite = schema(Engine::Sqlite, "table", &["id"], vec![col("id", "integer"), col("note", "text")]);
    let sqlite_sql = sql::select_list(&sqlite, false);
    assert!(sqlite_sql.contains("CAST(") && sqlite_sql.contains("AS TEXT"), "{sqlite_sql}");

    let keyed_text = schema(Engine::Postgres, "table", &["token"], vec![col("token", "text"), col("note", "text"), col("payload", "bytea")]);
    let text_sql = sql::select_list(&keyed_text, false);
    assert!(text_sql.starts_with("\"token\"::text,"), "{text_sql}");
    assert!(!text_sql.contains("left(\"token\"::text"), "{text_sql}");
    assert!(text_sql.contains("chr(1)"), "{text_sql}");
    let keyed_blob = schema(Engine::Postgres, "table", &["payload"], vec![col("payload", "bytea"), col("blob", "bytea")]);
    let blob_sql = sql::select_list(&keyed_blob, false);
    assert!(blob_sql.starts_with("CASE WHEN \"payload\" IS NULL THEN NULL ELSE encode(\"payload\", 'hex') END"), "{blob_sql}");
    assert!(!blob_sql.contains("octet_length(\"payload\")"), "{blob_sql}");
    assert!(blob_sql.contains("chr(1)"), "{blob_sql}");
    let mysql_key = schema(Engine::Mysql, "table", &["token"], vec![col("token", "varchar"), col("note", "text")]);
    let mysql_key_sql = sql::select_list(&mysql_key, false);
    assert!(mysql_key_sql.starts_with("CAST(`token` AS CHAR),"), "{mysql_key_sql}");
    assert!(!mysql_key_sql.contains("LEFT(CAST(`token` AS CHAR)"), "{mysql_key_sql}");
    let sqlite_key = schema(Engine::Sqlite, "table", &["token"], vec![col("token", "text"), col("note", "text")]);
    let sqlite_key_sql = sql::select_list(&sqlite_key, false);
    assert!(sqlite_key_sql.starts_with("CAST(\"token\" AS TEXT),"), "{sqlite_key_sql}");
    assert!(!sqlite_key_sql.contains("substr(CAST(\"token\" AS TEXT)"), "{sqlite_key_sql}");

    let flag = col("flag", "bool");
    assert_eq!(sql::sql_cell(Engine::Postgres, Some(&flag), &Cell::Text("t".to_owned())), "TRUE");
    assert_eq!(sql::sql_cell(Engine::Mysql, Some(&flag), &Cell::Text("0".to_owned())), "FALSE");
    let blob = col("blob", "bytea");
    assert_eq!(sql::sql_cell(Engine::Postgres, Some(&blob), &Cell::Text("0A0B".to_owned())), "'\\x0a0b'");
    assert_eq!(sql::sql_cell(Engine::Mysql, Some(&blob), &Cell::Text("aa".to_owned())), "X'aa'");
    assert_eq!(sql::reject_literal(&blob, &Cell::Text("zz".to_owned())).as_deref(), Some("这不是十六进制"));
    assert!(sql::reject_literal(&col("note", "text"), &Cell::Text("\u{1}t:11:hello".to_owned())).is_none());
    let synthetic = format!("\u{1}t:3000:{}", "a".repeat(1024));
    assert_eq!(sql::reject_literal(&col("note", "text"), &Cell::Text(synthetic.clone())).as_deref(), Some("预览内容不能写回"));
    assert!(sql::reject_literal_with(&col("note", "text"), &Cell::Text(synthetic), true).is_none());
    assert_eq!(sql::reject_literal(&col("shape", "geometry"), &Cell::Null).as_deref(), Some("几何类型不能从格子保存"));
    assert!(sql::reject_literal(&col("mystery", ""), &Cell::Text("a".to_owned())).unwrap().contains("类型未知"));
    assert!(sql::reject_literal(&col("weird", "interval"), &Cell::Null).unwrap().contains("不能判断写回方式"));
    assert!(sql::reject_literal(&flag, &Cell::Text("maybe".to_owned())).unwrap().contains("布尔"));

    let dollar = "SELECT $$a;b$$";
    assert_eq!(sql::split_sql("postgres", dollar).len(), 1);
    assert!(select_export_gate_for("postgres", dollar).is_ok());
    assert_eq!(sql::split_sql("mysql", "SELECT 1 # c;\n").len(), 1);
    assert_eq!(sql::split_sql("mysql", "SELECT 1; # comment\nSELECT 2").len(), 2);
    assert!(select_export_gate_for("mysql", "SELECT 1; # comment\nSELECT 2").is_err());
    assert_eq!(sql::split_sql("mysql", "SELECT 1 # c;\nSELECT 2").len(), 1);
    assert_eq!(sql::split_sql("postgres", "SELECT 1 # c;\nSELECT 2").len(), 2);
    let forwarded = crate::query::split_sql("postgres", "SELECT 1; SELECT 2");
    assert_eq!(
        sql::split_sql("postgres", "SELECT 1; SELECT 2"),
        forwarded.pieces.into_iter().map(|piece| piece.text).filter(|text| !text.trim().is_empty()).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn open_session_reads_writes_and_exports_without_a_second_connection() {
    use super::live::{self, ExportMode, ExportSpec, Mutation, MutationKind};
    use crate::connections::{CatalogSession, Service};

    let dir = std::env::temp_dir().join(format!("rillsql-grid-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let database = dir.join("notes.sqlite");
    let setup = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT UNIQUE, note TEXT)")
        .execute(&setup)
        .await
        .unwrap();
    for index in 0..130 {
        let id = index + 1;
        let note: Option<&str> = if index == 0 {
            None
        } else if index == 1 {
            Some("")
        } else if index == 2 {
            Some("他说\"你好\"，\n下一行,还有逗号")
        } else {
            Some("备注")
        };
        sqlx::query("INSERT INTO customers (id, name, email, note) VALUES (?1, ?2, ?3, ?4)")
            .bind(id)
            .bind(format!("n{id:03}"))
            .bind(format!("u{id}@example.com"))
            .bind(note)
            .execute(&setup)
            .await
            .unwrap();
    }
    sqlx::query("CREATE TABLE notes (label TEXT)").execute(&setup).await.unwrap();
    sqlx::query("INSERT INTO notes (label) VALUES ('same'), ('same')").execute(&setup).await.unwrap();
    sqlx::query("CREATE VIEW v_customers AS SELECT id, name FROM customers").execute(&setup).await.unwrap();
    setup.close().await;

    let service = Service::new_for_test(dir.join("connections.sqlite"));
    service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
    let session = service.catalog_session("notes").expect("已打开会话");
    assert!(service.catalog_session("notes").is_some());

    let schema = live::describe(&session, "main", "", "customers", "sqlite").await.unwrap();
    assert_eq!(schema.kind, "table");
    assert_eq!(schema.key, vec!["id".to_owned()]);
    assert!(schema.columns.iter().any(|column| column.name == "id" && column.primary && column.auto));
    let refused = live::describe(&session, "other", "", "customers", "sqlite").await.unwrap_err();
    assert!(refused.contains("没有另开连接"));
    let scripted = live::describe(&CatalogSession::Scripted, "main", "", "customers", "sqlite").await.unwrap_err();
    assert!(scripted.contains("没有连着可查询"));

    let page = live::query_page(&session, "main", "", "customers", "sqlite", &[], None, 0, 100).await.unwrap();
    assert_eq!(page.rows.len(), 100);
    assert!(page.truncated);
    assert!(page.sql.contains("LIMIT 100"));
    assert!(page.sql.contains("CAST(") && page.sql.contains("AS TEXT"), "{}", page.sql);
    assert!(page.sql.contains("char(1)"), "{}", page.sql);
    assert!(page.rows[0].located);
    assert_eq!(page.rows[0].cells.get("note"), Some(&Cell::Null));
    assert_eq!(page.rows[1].cells.get("note"), Some(&Cell::Text(String::new())));
    let filtered = live::query_page(&session, "main", "", "customers", "sqlite", &[filt("id", FilterOp::Gt, Some("100"))], None, 0, 100)
        .await
        .unwrap();
    assert_eq!(filtered.rows.len(), 30);
    assert!(!filtered.truncated);
    assert!(filtered.sql.contains("> 100"));
    assert!(!filtered.sql.contains(" OR "));
    let injected = live::query_page(&session, "main", "", "customers", "sqlite", &[filt("name", FilterOp::Eq, Some("a' OR '1'='1"))], None, 0, 50)
        .await
        .unwrap();
    assert!(injected.rows.is_empty());
    assert!(injected.sql.contains("''"));
    let nulls = live::query_page(&session, "main", "", "customers", "sqlite", &[filt("note", FilterOp::Null, None)], None, 0, 10).await.unwrap();
    let blanks = live::query_page(&session, "main", "", "customers", "sqlite", &[filt("note", FilterOp::Empty, None)], None, 0, 10).await.unwrap();
    assert_eq!(nulls.rows.len(), 1);
    assert_eq!(blanks.rows[0].cells.get("note"), Some(&Cell::Text(String::new())));
    assert_eq!(live::count_rows(&session, "main", "", "customers", "sqlite", &[filt("id", FilterOp::Gt, Some("100"))]).await.unwrap(), 30);

    let locator_for = |page: &live::LivePage, id: &str| {
        page.rows
            .iter()
            .find(|row| row.cells.get("id") == Some(&Cell::Text(id.to_owned())))
            .expect("row")
            .locator
            .clone()
    };
    let first = locator_for(&page, "1");
    let second = locator_for(&page, "2");
    let third = locator_for(&page, "3");
    let report = live::commit(
        &session,
        "main",
        "",
        "customers",
        "sqlite",
        &[
            Mutation { id: "u:1".to_owned(), kind: MutationKind::Update, sets: vec![("name".to_owned(), Cell::Text("已改".to_owned()))], locator: first, values: Vec::new(), resolved: Vec::new() },
            Mutation { id: "u:2".to_owned(), kind: MutationKind::Update, sets: vec![("email".to_owned(), Cell::Text("u1@example.com".to_owned()))], locator: second, values: Vec::new(), resolved: Vec::new() },
            Mutation { id: "d:3".to_owned(), kind: MutationKind::Delete, sets: Vec::new(), locator: third, values: Vec::new(), resolved: Vec::new() },
        ],
    )
    .await
    .unwrap();
    assert!(!report.rolled_back);
    assert_eq!(report.applied.len(), 1);
    assert!(!report.applied[0].sql.contains("BEGIN"));
    assert!(report.failed.unwrap().message.to_ascii_lowercase().contains("unique"));
    let kept = live::query_page(&session, "main", "", "customers", "sqlite", &[filt("id", FilterOp::Eq, Some("1"))], None, 0, 5).await.unwrap();
    assert_eq!(kept.rows[0].cells.get("name"), Some(&Cell::Text("已改".to_owned())));
    let email = live::query_page(&session, "main", "", "customers", "sqlite", &[filt("id", FilterOp::Eq, Some("2"))], None, 0, 5).await.unwrap();
    assert_eq!(email.rows[0].cells.get("email"), Some(&Cell::Text("u2@example.com".to_owned())));
    assert!(live::query_page(&session, "main", "", "customers", "sqlite", &[filt("id", FilterOp::Eq, Some("3"))], None, 0, 5).await.unwrap().rows.len() == 1);

    let fresh = live::commit(
        &session,
        "main",
        "",
        "customers",
        "sqlite",
        &[Mutation {
            id: "new-customer".to_owned(),
            kind: MutationKind::Insert,
            sets: Vec::new(),
            locator: Vec::new(),
            values: vec![("name".to_owned(), Cell::Text("新客户".to_owned())), ("email".to_owned(), Cell::Text("fresh@example.com".to_owned()))],
            resolved: Vec::new(),
        }],
    )
    .await
    .unwrap();
    assert!(fresh.failed.is_none());
    let saved = live::query_page(&session, "main", "", "customers", "sqlite", &[filt("email", FilterOp::Eq, Some("fresh@example.com"))], None, 0, 5).await.unwrap();
    assert_eq!(saved.rows.len(), 1);
    assert!(saved.rows[0].located);

    let notes = live::describe(&session, "main", "", "notes", "sqlite").await.unwrap();
    assert!(notes.key.is_empty());
    let note_page = live::query_page(&session, "main", "", "notes", "sqlite", &[], None, 0, 10).await.unwrap();
    assert!(note_page.rows.iter().all(|row| !row.located));
    let inserted = live::commit(
        &session,
        "main",
        "",
        "notes",
        "sqlite",
        &[Mutation { id: "new-note".to_owned(), kind: MutationKind::Insert, sets: Vec::new(), locator: Vec::new(), values: vec![("label".to_owned(), Cell::Text("新".to_owned()))], resolved: Vec::new() }],
    )
    .await
    .unwrap();
    assert!(inserted.failed.is_none());
    let note_after = live::query_page(&session, "main", "", "notes", "sqlite", &[], None, 0, 10).await.unwrap();
    assert_eq!(note_after.rows.len(), 3);
    assert!(note_after.rows.iter().all(|row| !row.located));
    let blocked_delete = live::commit(
        &session,
        "main",
        "",
        "notes",
        "sqlite",
        &[Mutation { id: "d:o".to_owned(), kind: MutationKind::Delete, sets: Vec::new(), locator: vec![("label".to_owned(), Cell::Text("same".to_owned()))], values: Vec::new(), resolved: Vec::new() }],
    )
    .await
    .unwrap();
    assert!(blocked_delete.applied.is_empty());
    assert!(blocked_delete.failed.unwrap().message.contains("无法唯一确定"));
    assert_eq!(live::count_rows(&session, "main", "", "notes", "sqlite", &[]).await.unwrap(), 3);

    let view = live::describe(&session, "main", "", "v_customers", "sqlite").await.unwrap();
    assert_eq!(view.kind, "view");
    let view_write = live::commit(
        &session,
        "main",
        "",
        "v_customers",
        "sqlite",
        &[Mutation { id: "new-view".to_owned(), kind: MutationKind::Insert, sets: Vec::new(), locator: Vec::new(), values: vec![("name".to_owned(), Cell::Text("x".to_owned()))], resolved: Vec::new() }],
    )
    .await
    .unwrap();
    assert!(view_write.failed.unwrap().message.contains("只有数据表"));

    let spec = |id: &str, mode: ExportMode, path: &str, limit: u64, truncated: bool, statement: &str, acknowledged: bool| ExportSpec {
        id: id.to_owned(),
        mode,
        database: "main".to_owned(),
        schema: String::new(),
        table: "customers".to_owned(),
        engine_hint: "sqlite".to_owned(),
        filters: Vec::new(),
        sort: None,
        limit,
        truncated,
        statement: statement.to_owned(),
        acknowledged,
        path: path.to_owned(),
        delimiter: ",".to_owned(),
        null_token: String::new(),
        bom: false,
        header: true,
        format: "csv".to_owned(),
        fail_after_rows: None,
        cancel_after_rows: None,
    };
    let partial_path = dir.join("customers.csv");
    let partial = live::run_export(&session, &spec("partial", ExportMode::TableLoaded, &partial_path.display().to_string(), 100, true, "", true)).await;
    assert!(partial.success);
    assert!(partial.partial);
    assert!(partial.message.contains("不是完整结果"));
    let partial_file = PathBuf::from(partial.path.unwrap());
    assert!(partial_file.display().to_string().contains("（部分）"));
    assert_eq!(partial.written_rows, 100);
    let partial_body = fs::read_to_string(&partial_file).unwrap();
    assert!(partial_body.contains("他说\"\"你好\"\""));
    assert!(partial_body.contains("下一行,还有逗号"));
    let null_line = partial_body.lines().find(|line| line.starts_with("\"1\",")).expect("id 1");
    assert!(null_line.ends_with(','), "{null_line}");
    assert!(!null_line.ends_with("\"\""));
    let empty_line = partial_body.lines().find(|line| line.starts_with("\"2\",")).expect("id 2");
    assert!(empty_line.ends_with(",\"\""));

    let full_path = dir.join("customers-full.csv");
    let full = live::run_export(&session, &spec("full", ExportMode::TableFull, &full_path.display().to_string(), 0, true, "", true)).await;
    assert!(full.success);
    assert!(!full.partial);
    assert_eq!(full.written_rows, 131);
    assert!(!full.message.contains("不是完整结果"));

    let query_path = dir.join("query.csv");
    let query = live::run_export(
        &session,
        &spec("query", ExportMode::Query, &query_path.display().to_string(), 0, true, "SELECT id, note FROM customers WHERE id = 3", true),
    )
    .await;
    assert!(query.success);
    assert_eq!(query.written_rows, 1);
    assert!(fs::read_to_string(query.path.unwrap()).unwrap().contains("他说\"\"你好\"\""));
    let denied = live::run_export(&session, &spec("denied", ExportMode::Query, &dir.join("denied.csv").display().to_string(), 0, true, "DELETE FROM customers", true)).await;
    assert!(!denied.success);
    assert_eq!(denied.status, "blocked");
    assert!(!dir.join("denied.csv").exists());
    let multi = live::run_export(&session, &spec("multi", ExportMode::Query, &dir.join("multi.csv").display().to_string(), 0, true, "SELECT 1; DELETE FROM customers", true)).await;
    assert!(!multi.success);
    assert!(!dir.join("multi.csv").exists());
    let quiet = live::run_export(&session, &spec("quiet", ExportMode::Query, &dir.join("quiet.csv").display().to_string(), 0, true, "SELECT id FROM customers", false)).await;
    assert!(!quiet.success);
    assert_eq!(quiet.status, "blocked");
    assert!(!dir.join("quiet.csv").exists());

    live::request_cancel("job-cancel");
    let cancelled = live::run_export(&session, &spec("job-cancel", ExportMode::TableFull, &dir.join("cancel.csv").display().to_string(), 0, false, "", true)).await;
    assert!(!cancelled.success);
    assert_eq!(cancelled.status, "cancelled");
    assert!(cancelled.message.contains("未完成的文件已删除"));
    assert!(!dir.join("cancel.csv").exists());
    assert!(!PathBuf::from(format!("{}.partial", dir.join("cancel.csv").display())).exists());

    let mut failing = spec("fail", ExportMode::TableLoaded, &dir.join("fail.csv").display().to_string(), 100, true, "", true);
    failing.fail_after_rows = Some(1);
    let failed = live::run_export(&session, &failing).await;
    assert!(!failed.success);
    assert!(failed.message.contains("不会冒充完整结果"));
    let residual = PathBuf::from(failed.residual_path.unwrap());
    assert!(residual.exists());
    let residual_body = fs::read_to_string(&residual).unwrap();
    assert!(residual_body.contains("\"1\""));
    assert!(!residual_body.contains("n130"));
    let mut mid_cancel = spec("mid-cancel", ExportMode::TableLoaded, &dir.join("mid.csv").display().to_string(), 10, true, "", true);
    mid_cancel.cancel_after_rows = Some(1);
    let mid = live::run_export(&session, &mid_cancel).await;
    assert_eq!(mid.status, "cancelled");
    assert!(!mid.success);
    assert!(!dir.join("mid（部分）.csv").exists());
    assert!(!PathBuf::from(format!("{}.partial", dir.join("mid（部分）.csv").display())).exists());

    let mut json_spec = spec("json", ExportMode::TableLoaded, &dir.join("rows.json").display().to_string(), 2, false, "", true);
    json_spec.format = "json".to_owned();
    let json = live::run_export(&session, &json_spec).await;
    assert!(json.success);
    assert_eq!(json.written_rows, 2);
    let json_body = fs::read_to_string(json.path.unwrap()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json_body).unwrap();
    assert!(parsed.as_array().unwrap().len() == 2);
    assert!(json_body.contains("\"note\":null"));
    assert!(json_body.contains("\"note\":\"\""));
    let mut insert_spec = spec("insert", ExportMode::TableLoaded, &dir.join("rows.sql").display().to_string(), 2, false, "", true);
    insert_spec.format = "insert".to_owned();
    let inserted_file = live::run_export(&session, &insert_spec).await;
    assert!(inserted_file.success);
    let insert_body = fs::read_to_string(inserted_file.path.unwrap()).unwrap();
    assert!(insert_body.contains("INSERT INTO \"customers\""));
    assert!(insert_body.contains("NULL"));
    assert!(!insert_body.contains("CREATE TABLE"));
    let kept = dir.join("kept.json");
    fs::write(&kept, "[{\"id\":\"old\"}]").unwrap();
    let mut failed_json = spec("json-fail", ExportMode::TableLoaded, &kept.display().to_string(), 2, false, "", true);
    failed_json.format = "json".to_owned();
    failed_json.fail_after_rows = Some(0);
    let failed_keep = live::run_export(&session, &failed_json).await;
    assert!(!failed_keep.success);
    assert_eq!(fs::read_to_string(&kept).unwrap(), "[{\"id\":\"old\"}]");
    let unnamed = spec("unnamed", ExportMode::Query, &dir.join("no-table.sql").display().to_string(), 0, false, "SELECT 1", true);
    let mut unnamed = unnamed;
    unnamed.format = "insert".to_owned();
    unnamed.table = String::new();
    let blocked_insert = live::run_export(&session, &unnamed).await;
    assert_eq!(blocked_insert.status, "blocked");
    assert!(!dir.join("no-table.sql").exists());

    let missing = live::run_export(&session, &spec("missing", ExportMode::TableFull, &dir.join("missing").join("out.csv").display().to_string(), 0, false, "", true)).await;
    assert!(!missing.success);
    assert!(missing.message.contains("路径不可写"));
    assert!(missing.residual_path.is_none());

    match &session {
        CatalogSession::Sqlite { pool, .. } => {
            let tables: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'customers'")
                .fetch_one(pool)
                .await
                .unwrap();
            assert_eq!(tables, 1);
        }
        _ => panic!("会话应该仍是原来的 SQLite 池"),
    }
    drop(session);
    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn located_rows_preview_until_a_key_reread_and_bad_literals_are_not_sent() {
    use super::live::{self, ExportMode, ExportSpec, Mutation, MutationKind};
    use crate::connections::Service;

    let dir = std::env::temp_dir().join(format!("rillsql-grid-preview-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let database = dir.join("typed.sqlite");
    let setup = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE docs (id INTEGER PRIMARY KEY, note TEXT, flag BOOLEAN, blob BLOB, g GEOMETRY, mystery)")
        .execute(&setup)
        .await
        .unwrap();
    let long = "a".repeat(3000);
    sqlx::query("INSERT INTO docs (id, note, flag, blob, g, mystery) VALUES (1, 'short', 't', x'abcd', 'POINT(0 0)', '2020-01-01 00:00:00')")
        .execute(&setup)
        .await
        .unwrap();
    sqlx::query("INSERT INTO docs (id, note, flag, blob, g, mystery) VALUES (2, ?1, 'f', NULL, NULL, '')")
        .bind(&long)
        .execute(&setup)
        .await
        .unwrap();
    sqlx::query("CREATE VIEW v_docs AS SELECT id, note FROM docs").execute(&setup).await.unwrap();
    sqlx::query("CREATE TABLE loose_notes (label TEXT)").execute(&setup).await.unwrap();
    sqlx::query("INSERT INTO loose_notes (label) VALUES (?1)").bind(&long).execute(&setup).await.unwrap();
    setup.close().await;

    let service = Service::new_for_test(dir.join("connections.sqlite"));
    service.open_sqlite_for_test("typed", "类型", &database).await.unwrap();
    let session = service.catalog_session("typed").expect("已打开会话");
    let page = live::query_page(&session, "main", "", "docs", "sqlite", &[], None, 0, 10).await.unwrap();
    assert!(page.sql.contains("CAST(") && page.sql.contains("AS TEXT"), "{}", page.sql);
    assert!(page.sql.contains("char(1)"), "{}", page.sql);
    let short = page.rows.iter().find(|row| row.cells.get("id") == Some(&Cell::Text("1".to_owned()))).unwrap();
    let wide = page.rows.iter().find(|row| row.cells.get("id") == Some(&Cell::Text("2".to_owned()))).unwrap();
    assert_eq!(short.cells.get("note"), Some(&Cell::Text("short".to_owned())));
    match wide.cells.get("note") {
        Some(Cell::Text(text)) => {
            assert!(text.starts_with("\u{1}t:3000:"), "{text}");
            assert_eq!(text.trim_start_matches("\u{1}t:3000:").len(), 1024);
            assert!(!text.contains('\u{fffd}'));
        }
        other => panic!("长文本应该是预览信封 {other:?}"),
    }
    match short.cells.get("blob") {
        Some(Cell::Text(text)) => assert_eq!(text, "\u{1}b:2"),
        other => panic!("二进制应该只带字节数 {other:?}"),
    }
    assert_eq!(short.cells.get("g"), Some(&Cell::Text("POINT(0 0)".to_owned())));
    assert_eq!(short.cells.get("mystery"), Some(&Cell::Text("2020-01-01 00:00:00".to_owned())));

    let opened = live::query_page(&session, "main", "", "docs", "sqlite", &[filt("id", FilterOp::Eq, Some("2"))], None, 0, 1).await.unwrap();
    assert!(!opened.sql.contains("char(1)"), "{}", opened.sql);
    assert!(opened.sql.contains("lower(hex("), "{}", opened.sql);
    assert_eq!(opened.rows[0].cells.get("note"), Some(&Cell::Text(long.clone())));
    assert_eq!(opened.rows[0].cells.get("blob"), Some(&Cell::Null));

    let locator = short.locator.clone();
    let blob_rejected = live::commit(
        &session,
        "main",
        "",
        "docs",
        "sqlite",
        &[Mutation { id: "bad-blob".to_owned(), kind: MutationKind::Update, sets: vec![("blob".to_owned(), Cell::Text("zz".to_owned()))], locator: locator.clone(), values: Vec::new(), resolved: Vec::new() }],
    )
    .await
    .unwrap();
    let blob_failed = blob_rejected.failed.expect("zz 不能发送");
    assert!(blob_failed.sql.is_empty());
    assert!(blob_failed.message.contains("这不是十六进制"));
    let geometry_rejected = live::commit(
        &session,
        "main",
        "",
        "docs",
        "sqlite",
        &[Mutation { id: "bad-geo".to_owned(), kind: MutationKind::Update, sets: vec![("g".to_owned(), Cell::Null)], locator: locator.clone(), values: Vec::new(), resolved: Vec::new() }],
    )
    .await
    .unwrap();
    let geometry_failed = geometry_rejected.failed.expect("几何不能保存");
    assert!(geometry_failed.sql.is_empty());
    assert!(geometry_failed.message.contains("几何"));
    let unknown_rejected = live::commit(
        &session,
        "main",
        "",
        "docs",
        "sqlite",
        &[Mutation { id: "bad-type".to_owned(), kind: MutationKind::Update, sets: vec![("mystery".to_owned(), Cell::Text("x".to_owned()))], locator: locator.clone(), values: Vec::new(), resolved: Vec::new() }],
    )
    .await
    .unwrap();
    let unknown_failed = unknown_rejected.failed.expect("类型未知不能编辑");
    assert!(unknown_failed.sql.is_empty());
    assert!(unknown_failed.message.contains("类型未知"));

    let saved = live::commit(
        &session,
        "main",
        "",
        "docs",
        "sqlite",
        &[Mutation { id: "flag".to_owned(), kind: MutationKind::Update, sets: vec![("flag".to_owned(), Cell::Text("yes".to_owned()))], locator, values: Vec::new(), resolved: Vec::new() }],
    )
    .await
    .unwrap();
    assert!(saved.failed.is_none());
    assert!(saved.applied[0].sql.contains("TRUE"));
    assert!(!saved.applied[0].sql.contains("'yes'"));

    let preview_rejected = live::commit(
        &session,
        "main",
        "",
        "docs",
        "sqlite",
        &[Mutation {
            id: "preview".to_owned(),
            kind: MutationKind::Update,
            sets: vec![("note".to_owned(), Cell::Text(format!("\u{1}t:3000:{}", "a".repeat(1024))))],
            locator: short.locator.clone(),
            values: Vec::new(),
            resolved: Vec::new(),
        }],
    )
    .await
    .unwrap();
    let preview_failed = preview_rejected.failed.expect("预览不能写回");
    assert!(preview_failed.sql.is_empty());
    assert!(preview_failed.message.contains("预览"));

    let export_path = dir.join("docs-full.csv");
    let exported = live::run_export(
        &session,
        &ExportSpec {
            id: "docs-full".to_owned(),
            mode: ExportMode::TableFull,
            database: "main".to_owned(),
            schema: String::new(),
            table: "docs".to_owned(),
            engine_hint: "sqlite".to_owned(),
            filters: Vec::new(),
            sort: None,
            limit: 0,
            truncated: false,
            statement: String::new(),
            acknowledged: true,
            path: export_path.display().to_string(),
            delimiter: ",".to_owned(),
            null_token: String::new(),
            bom: false,
            header: true,
            format: "csv".to_owned(),
            fail_after_rows: None,
            cancel_after_rows: None,
        },
    )
    .await;
    assert!(exported.success, "{}", exported.message);
    let body = fs::read_to_string(exported.path.unwrap()).unwrap();
    assert!(body.contains(&long), "{body}");
    assert!(body.contains("abcd"), "{body}");
    assert!(!body.contains('\u{1}'), "{body}");

    let view = live::query_page(&session, "main", "", "v_docs", "sqlite", &[], None, 0, 10).await.unwrap();
    assert!(!view.sql.contains("char(1)"), "{}", view.sql);
    let view_long = view.rows.iter().find(|row| row.cells.get("id") == Some(&Cell::Text("2".to_owned()))).unwrap();
    assert_eq!(view_long.cells.get("note"), Some(&Cell::Text(long.clone())));
    let loose = live::query_page(&session, "main", "", "loose_notes", "sqlite", &[], None, 0, 10).await.unwrap();
    assert!(!loose.sql.contains("char(1)"), "{}", loose.sql);
    assert_eq!(loose.rows[0].cells.get("label"), Some(&Cell::Text(long)));
    assert!(loose.rows.iter().all(|row| !row.located));

    drop(session);
    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn key_columns_keep_full_text_and_hex_for_locator_reads() {
    use super::live::{self, Mutation, MutationKind};
    use crate::connections::Service;

    let dir = std::env::temp_dir().join(format!("rillsql-grid-key-preview-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let database = dir.join("keys.sqlite");
    let setup = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE blob_keys (id BLOB PRIMARY KEY, note TEXT, extra BLOB)").execute(&setup).await.unwrap();
    let long = "a".repeat(3000);
    sqlx::query("INSERT INTO blob_keys (id, note, extra) VALUES (x'abcd', ?1, x'ffff')").bind(&long).execute(&setup).await.unwrap();
    sqlx::query("INSERT INTO blob_keys (id, note, extra) VALUES (x'abce', 'short', x'ffff')").execute(&setup).await.unwrap();
    sqlx::query("CREATE TABLE text_keys (id TEXT PRIMARY KEY, note TEXT)").execute(&setup).await.unwrap();
    sqlx::query("INSERT INTO text_keys (id, note) VALUES (?1, 'x')").bind(&long).execute(&setup).await.unwrap();
    let literal = "\u{1}t:11:hello";
    sqlx::query("INSERT INTO text_keys (id, note) VALUES (?1, 'y')").bind(literal).execute(&setup).await.unwrap();
    setup.close().await;

    let service = Service::new_for_test(dir.join("connections.sqlite"));
    service.open_sqlite_for_test("keys", "主键", &database).await.unwrap();
    let session = service.catalog_session("keys").expect("已打开会话");

    let blobs = live::query_page(&session, "main", "", "blob_keys", "sqlite", &[], None, 0, 10).await.unwrap();
    assert!(blobs.sql.contains("lower(hex(\"id\"))"), "{}", blobs.sql);
    assert!(!blobs.sql.contains("length(\"id\")"), "{}", blobs.sql);
    assert!(blobs.sql.contains("char(1)"), "{}", blobs.sql);
    assert_eq!(blobs.rows.len(), 2);
    assert_ne!(blobs.rows[0].id, blobs.rows[1].id);
    let first = blobs.rows.iter().find(|row| row.cells.get("id") == Some(&Cell::Text("abcd".to_owned()))).unwrap();
    let second = blobs.rows.iter().find(|row| row.cells.get("id") == Some(&Cell::Text("abce".to_owned()))).unwrap();
    assert!(first.located && second.located);
    assert_eq!(first.locator, vec![("id".to_owned(), Cell::Text("abcd".to_owned()))]);
    assert_eq!(first.cells.get("extra"), Some(&Cell::Text("\u{1}b:2".to_owned())));
    match first.cells.get("note") {
        Some(Cell::Text(text)) => assert!(text.starts_with("\u{1}t:3000:"), "{text}"),
        other => panic!("非主键长文本仍应是预览 {other:?}"),
    }

    let opened = live::query_page(&session, "main", "", "blob_keys", "sqlite", &[filt("id", FilterOp::Eq, Some("abcd"))], None, 0, 1).await.unwrap();
    assert_eq!(opened.rows.len(), 1);
    assert_eq!(opened.rows[0].cells.get("note"), Some(&Cell::Text(long.clone())));
    assert_eq!(opened.rows[0].cells.get("extra"), Some(&Cell::Text("ffff".to_owned())));

    let texts = live::query_page(&session, "main", "", "text_keys", "sqlite", &[], None, 0, 10).await.unwrap();
    assert!(texts.sql.starts_with("SELECT CAST(\"id\" AS TEXT),"), "{}", texts.sql);
    assert!(!texts.sql.contains("substr(CAST(\"id\" AS TEXT)"), "{}", texts.sql);
    let wide = texts.rows.iter().find(|row| row.cells.get("note") == Some(&Cell::Text("x".to_owned()))).unwrap();
    let shaped = texts.rows.iter().find(|row| row.cells.get("note") == Some(&Cell::Text("y".to_owned()))).unwrap();
    assert_eq!(wide.cells.get("id"), Some(&Cell::Text(long.clone())));
    assert!(wide.located);
    assert!(!wide.id.contains('\u{1}'));
    assert_eq!(shaped.cells.get("id"), Some(&Cell::Text(literal.to_owned())));
    assert_eq!(shaped.locator, vec![("id".to_owned(), Cell::Text(literal.to_owned()))]);

    let written = live::commit(
        &session,
        "main",
        "",
        "text_keys",
        "sqlite",
        &[Mutation {
            id: "literal".to_owned(),
            kind: MutationKind::Update,
            sets: vec![("note".to_owned(), Cell::Text(literal.to_owned()))],
            locator: shaped.locator.clone(),
            values: Vec::new(),
            resolved: Vec::new(),
        }],
    )
    .await
    .unwrap();
    assert!(written.failed.is_none(), "{:?}", written.failed);
    let synthetic = format!("\u{1}t:3000:{}", "a".repeat(1024));
    let blocked = live::commit(
        &session,
        "main",
        "",
        "text_keys",
        "sqlite",
        &[Mutation {
            id: "preview".to_owned(),
            kind: MutationKind::Update,
            sets: vec![("note".to_owned(), Cell::Text(synthetic.clone()))],
            locator: shaped.locator.clone(),
            values: Vec::new(),
            resolved: Vec::new(),
        }],
    )
    .await
    .unwrap();
    assert!(blocked.failed.expect("未展开的预览不能写回").message.contains("预览"));
    let allowed = live::commit(
        &session,
        "main",
        "",
        "text_keys",
        "sqlite",
        &[Mutation {
            id: "same-text".to_owned(),
            kind: MutationKind::Update,
            sets: vec![("note".to_owned(), Cell::Text(synthetic.clone()))],
            locator: wide.locator.clone(),
            values: Vec::new(),
            resolved: vec![true],
        }],
    )
    .await
    .unwrap();
    assert!(allowed.failed.is_none(), "{:?}", allowed.failed);
    let checked = live::query_page(&session, "main", "", "text_keys", "sqlite", &[filt("id", FilterOp::Eq, Some(long.as_str()))], None, 0, 1).await.unwrap();
    assert_eq!(checked.rows[0].cells.get("note"), Some(&Cell::Text(synthetic)));

    drop(session);
    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn markdown_export_writes_header_rule_escapes_pipes_and_marks_partial() {
    use super::live::{self, ExportMode, ExportSpec};
    use crate::connections::Service;

    let dir = std::env::temp_dir().join(format!("rillsql-grid-markdown-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let database = dir.join("notes.sqlite");
    let setup = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT)")
        .execute(&setup)
        .await
        .unwrap();
    sqlx::query("INSERT INTO customers (id, name, note) VALUES (1, 'a|b', NULL)").execute(&setup).await.unwrap();
    sqlx::query("INSERT INTO customers (id, name, note) VALUES (2, 'n2', '换\n行')").execute(&setup).await.unwrap();
    setup.close().await;

    let service = Service::new_for_test(dir.join("connections.sqlite"));
    service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
    let session = service.catalog_session("notes").expect("已打开会话");

    let spec = |id: &str, mode: ExportMode, path: String, limit: u64, truncated: bool| ExportSpec {
        id: id.to_owned(),
        mode,
        database: "main".to_owned(),
        schema: String::new(),
        table: "customers".to_owned(),
        engine_hint: "sqlite".to_owned(),
        filters: Vec::new(),
        sort: None,
        limit,
        truncated,
        statement: String::new(),
        acknowledged: true,
        path,
        delimiter: ",".to_owned(),
        null_token: String::new(),
        bom: false,
        header: true,
        format: "markdown".to_owned(),
        fail_after_rows: None,
        cancel_after_rows: None,
    };

    let full = live::run_export(&session, &spec("md-full", ExportMode::TableFull, dir.join("customers.md").display().to_string(), 0, false)).await;
    assert!(full.success, "{}", full.message);
    let body = fs::read_to_string(full.path.unwrap()).unwrap();
    assert_eq!(body.lines().next().unwrap(), "| id | name | note |");
    assert_eq!(body.lines().nth(1).unwrap(), "| --- | --- | --- |");
    assert!(body.contains("| 1 | a\\|b |  |"), "{body}");
    assert!(!body.contains("a|b"), "{body}");
    assert!(body.contains("| 2 | n2 | 换 行 |"), "{body}");

    let mut marked = spec("md-null", ExportMode::TableFull, dir.join("marked.md").display().to_string(), 0, false);
    marked.null_token = "NULL".to_owned();
    let nulls = live::run_export(&session, &marked).await;
    assert!(nulls.success, "{}", nulls.message);
    assert!(fs::read_to_string(nulls.path.unwrap()).unwrap().contains("| 1 | a\\|b | NULL |"));

    let partial = live::run_export(&session, &spec("md-partial", ExportMode::TableLoaded, dir.join("loaded.md").display().to_string(), 1, true)).await;
    assert!(partial.success, "{}", partial.message);
    assert!(partial.partial);
    let partial_file = PathBuf::from(partial.path.unwrap());
    assert!(partial_file.display().to_string().contains("（部分）.md"), "{}", partial_file.display());
    assert_eq!(partial.written_rows, 1);

    drop(session);
    drop(service);
    let _ = fs::remove_dir_all(&dir);
}
