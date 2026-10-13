use std::fs;

use super::export::{plan_export, render_csv, start_export, ExportScope, Origin, WriteControl};
use super::sql::select_export_gate;
use super::store::{commit_changes, customers_fixture, nokey_fixture, query_page, Cell, Change, Filter, FilterOp};
use super::module_id;

fn filt(column: &str, op: FilterOp, value: Option<&str>) -> Filter {
    Filter { column: column.to_owned(), op, value: value.map(str::to_owned) }
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
    assert!(scripted.contains("没有可查询"));

    let page = live::query_page(&session, "main", "", "customers", "sqlite", &[], None, 0, 100).await.unwrap();
    assert_eq!(page.rows.len(), 100);
    assert!(page.truncated);
    assert!(page.sql.contains("LIMIT 100"));
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
            Mutation { id: "u:1".to_owned(), kind: MutationKind::Update, sets: vec![("name".to_owned(), Cell::Text("已改".to_owned()))], locator: first, values: Vec::new() },
            Mutation { id: "u:2".to_owned(), kind: MutationKind::Update, sets: vec![("email".to_owned(), Cell::Text("u1@example.com".to_owned()))], locator: second, values: Vec::new() },
            Mutation { id: "d:3".to_owned(), kind: MutationKind::Delete, sets: Vec::new(), locator: third, values: Vec::new() },
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
        &[Mutation { id: "new-note".to_owned(), kind: MutationKind::Insert, sets: Vec::new(), locator: Vec::new(), values: vec![("label".to_owned(), Cell::Text("新".to_owned()))] }],
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
        &[Mutation { id: "d:o".to_owned(), kind: MutationKind::Delete, sets: Vec::new(), locator: vec![("label".to_owned(), Cell::Text("same".to_owned()))], values: Vec::new() }],
    )
    .await
    .unwrap();
    assert!(blocked_delete.applied.is_empty());
    assert!(blocked_delete.failed.unwrap().message.contains("无法唯一定位"));
    assert_eq!(live::count_rows(&session, "main", "", "notes", "sqlite", &[]).await.unwrap(), 3);

    let view = live::describe(&session, "main", "", "v_customers", "sqlite").await.unwrap();
    assert_eq!(view.kind, "view");
    let view_write = live::commit(
        &session,
        "main",
        "",
        "v_customers",
        "sqlite",
        &[Mutation { id: "new-view".to_owned(), kind: MutationKind::Insert, sets: Vec::new(), locator: Vec::new(), values: vec![("name".to_owned(), Cell::Text("x".to_owned()))] }],
    )
    .await
    .unwrap();
    assert!(view_write.failed.unwrap().message.contains("只有基础表"));

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
