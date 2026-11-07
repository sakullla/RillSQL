use std::sync::Arc;

use super::{run_blocking, AppServices, TaskOutcome};
use crate::application::grid::require_commit;
use crate::application::redis;
use crate::connections::Service;
use crate::compare::{DataRequest, DataTableRequest, SideRequest, StructureRequest};
use crate::connections::commands::ConnectionsState;
use crate::connections::model::{ConnectionDraft, Engine as ConnEngine};
use crate::ddl::{Action, StepStatus};
use crate::dump::dump::DumpRequest;
use crate::migrate::migrate::{MigrateRequest, MigrateTableRef};
use crate::query::commands::StartRequest;
use crate::query::{RenameRequest, SaveRequest};
use crate::state::{AppState, Platform, SessionState, ShellSettings};
use crate::grid::commands::{ChangeArg, CommitRequest, TableRef, ValueArg};
use crate::mongo::docs;

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rillsql-app-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn value(column: &str, text: &str) -> ValueArg {
    ValueArg { column: column.to_owned(), value: Some(text.to_owned()), resolved: false }
}

fn commit_request(connection_id: &str, table: &str) -> CommitRequest {
    CommitRequest {
        table: TableRef {
            connection_id: connection_id.to_owned(),
            database: "main".to_owned(),
            schema: String::new(),
            table: table.to_owned(),
            engine: "sqlite".to_owned(),
            include_foreign_keys: false,
        },
        changes: vec![ChangeArg {
            id: "u:1".to_owned(),
            kind: "update".to_owned(),
            sets: vec![value("name", "乙")],
            locator: vec![value("id", "1")],
            values: Vec::new(),
        }],
    }
}

#[test]
fn late_results_do_not_replace_a_newer_generation_and_cancel_wins() {
    let tasks = super::TaskBoard::new();
    let first = tasks.begin("query-tab");
    let second = tasks.begin("query-tab");
    assert_ne!(first.id, second.id);
    assert_eq!(first.generation, second.generation);
    tasks.supersede("query-tab");
    assert!(matches!(tasks.accept("query-tab", first, "旧结果"), TaskOutcome::Stale { .. }));
    assert!(matches!(tasks.accept("query-tab", second, "旧结果"), TaskOutcome::Stale { .. }));
    let current = tasks.begin("query-tab");
    assert!(matches!(tasks.accept("query-tab", current, "新结果"), TaskOutcome::Current("新结果")));

    let running = tasks.begin("export");
    assert!(tasks.cancel("export", running.id));
    tasks.supersede("export");
    assert!(matches!(tasks.accept("export", running, "迟到"), TaskOutcome::Cancelled { id } if id == running.id));
}

#[tokio::test]
async fn blocking_work_leaves_the_caller_thread() {
    let caller = std::thread::current().id();
    let seen = run_blocking(move || Ok(std::thread::current().id())).await.unwrap();
    assert_ne!(caller, seen);
}

#[tokio::test]
async fn redis_mongo_and_grid_confirmation_rejects_before_a_session() {
    let dir = temp_dir("guards");
    let service = Arc::new(Service::new_for_test(dir.join("connections.sqlite")));
    let redis_deleted = redis::delete_keys(&service, "missing", vec!["user:1".to_owned()], false).await.unwrap_err();
    assert!(redis_deleted.contains("确认"), "{redis_deleted}");
    let redis_keys = redis::exec(&service, "missing", "KEYS".to_owned(), vec!["*".to_owned()], true).await.unwrap_err();
    assert!(redis_keys.contains("已禁用"), "{redis_keys}");
    let redis_set = redis::exec(&service, "missing", "SET".to_owned(), vec!["a".to_owned(), "b".to_owned()], false).await.unwrap_err();
    assert!(redis_set.contains("确认"), "{redis_set}");
    let redis_ok_guard = redis::exec(&service, "missing", "GET".to_owned(), vec!["a".to_owned()], false).await.unwrap_err();
    assert!(redis_ok_guard.contains("还没有打开"), "{redis_ok_guard}");

    let mongo_bad = crate::application::mongo::replace_document(&service, "missing", "订单", "customers", "1", "{\"status\": paid}").await.unwrap_err();
    assert!(mongo_bad.contains("语法错误"), "{mongo_bad}");
    assert!(docs::parse_id("{bad").is_err());
    let mongo_unconfirmed = crate::application::mongo::delete_document(&service, "missing", "订单", "customers", "\"65f2c1e4a1b2c3d4e5f60718\"", false).await.unwrap_err();
    assert!(mongo_unconfirmed.contains("确认"), "{mongo_unconfirmed}");

    let app = AppServices::new(Arc::clone(&service));
    let rejected = match app.commit_grid(&commit_request("missing", "customers"), false).await {
        Err(message) => message,
        Ok(_) => panic!("未确认的网格提交应当被拒绝"),
    };
    assert!(rejected.contains("确认"), "{rejected}");
    assert!(require_commit(false).is_err());
    let mut unknown = commit_request("missing", "customers");
    unknown.changes[0].kind = "merge".to_owned();
    let unknown = match app.commit_grid(&unknown, true).await {
        Err(message) => message,
        Ok(_) => panic!("未知变更应当被拒绝"),
    };
    assert!(unknown.contains("未知变更"), "{unknown}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn confirmed_grid_update_and_ddl_execute_hit_sqlite_and_reject_a_stale_preview() {
    let dir = temp_dir("sqlite");
    let database = dir.join("notes.sqlite");
    let setup = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT)").execute(&setup).await.unwrap();
    sqlx::query("INSERT INTO customers (id, name) VALUES (1, '甲')").execute(&setup).await.unwrap();
    setup.close().await;

    let service = Arc::new(Service::new_for_test(dir.join("connections.sqlite")));
    service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
    let app = AppServices::new(Arc::clone(&service));
    let committed = match app.commit_grid(&commit_request("notes", "customers"), true).await {
        Ok(value) => value,
        Err(message) => panic!("{message}"),
    };
    assert!(committed.failed.is_none(), "{}", committed.failed.as_ref().map(|item| item.message.clone()).unwrap_or_default());
    assert_eq!(committed.applied.len(), 1);
    let check = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).read_only(true))
        .await
        .unwrap();
    let name: String = sqlx::query_scalar("SELECT name FROM customers WHERE id = 1").fetch_one(&check).await.unwrap();
    assert_eq!(name, "乙");
    check.close().await;

    let request = crate::application::ddl::request("main".to_owned(), String::new(), "sqlite", Action::Vacuum).expect("sqlite vacuum");
    let refused = match crate::application::ddl::execute_request(&service, "notes", &request, &["VACUUM;".to_owned()], false).await {
        Err(message) => message,
        Ok(_) => panic!("未确认的结构变更应当被拒绝"),
    };
    assert!(refused.contains("确认"), "{refused}");
    let plan = match crate::application::ddl::preview_request(&service, "notes", &request).await {
        Ok(value) => value,
        Err(message) => panic!("{message}"),
    };
    assert!(!plan.steps.is_empty(), "{}", plan.blocked.join("; "));
    let confirmed: Vec<String> = plan.steps.iter().map(|step| step.sql.clone()).collect();
    let done = match crate::application::ddl::execute_request(&service, "notes", &request, &confirmed, true).await {
        Ok(value) => value,
        Err(message) => panic!("{message}"),
    };
    assert!(done.blocked.is_empty(), "{}", done.blocked.join("; "));
    assert!(done.steps.iter().all(|step| step.status == StepStatus::Ok), "{}", done.steps.iter().map(|step| format!("{:?} {}", step.status, step.message)).collect::<Vec<_>>().join("; "));
    let stale = match crate::application::ddl::execute_request(&service, "notes", &request, &["DROP TABLE \"customers\";".to_owned()], true).await {
        Err(message) => message,
        Ok(_) => panic!("与预览不一致的 SQL 应当被拒绝"),
    };
    assert!(stale.contains("没有执行") || stale.contains("不一致"), "{stale}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancelling_a_background_job_is_visible_and_discards_its_result() {
    let dir = temp_dir("cancel");
    let app = AppServices::new(Arc::new(Service::new_for_test(dir.join("connections.sqlite"))));
    let token = app.tasks.begin("compare");
    super::cancel_compare(&app.tasks, "compare", "cmp-1");
    assert!(super::compare_cancelled("cmp-1"));
    assert!(matches!(app.tasks.accept("compare", token, "差异"), TaskOutcome::Cancelled { .. }));
    app.cancel_grid_export("grid", "export-1");
    super::cancel_dump(&app.tasks, "dump", "dump-1");
    super::cancel_migrate(&app.tasks, "migrate", "migrate-1");
    let _ = std::fs::remove_dir_all(&dir);
}

fn err_text(result: Result<impl Sized, String>) -> String {
    match result {
        Err(message) => message,
        Ok(_) => panic!("应当返回错误"),
    }
}

#[tokio::test]
async fn typed_service_reaches_connections_catalog_query_files_and_state() {
    let dir = temp_dir("facade");
    let service = Arc::new(Service::new_for_test(dir.join("connections.sqlite")));
    let app = AppServices::new(Arc::clone(&service));
    let hosted = AppServices::from_state(&ConnectionsState { service: Arc::clone(&service) });
    assert!(hosted.list_connections().await.expect("list").is_empty());
    let tag = app.save_tag(None, "预发😀".to_owned(), "local".to_owned()).await.expect("tag");
    assert_eq!(tag.label, "预发😀");
    assert!(app.list_tags().await.expect("tags").iter().any(|item| item.id == tag.id));
    app.delete_tag(&tag.id).await.expect("delete tag");
    assert!(app.list_tags().await.expect("tags").iter().all(|item| item.id != tag.id));
    let rejected = match app.save_connection(ConnectionDraft::blank(ConnEngine::Sqlite), None, None, None).await {
        Err(error) => error.message(),
        Ok(_) => panic!("空的 SQLite 草稿应当被拒绝"),
    };
    assert!(rejected.contains("数据库文件"), "{rejected}");
    assert!(err_text(app.delete_connection("missing").await).contains("找不到这条连接"));
    assert!(err_text(app.test_connection("missing", None, None, None).await).contains("找不到这条连接"));
    assert!(err_text(app.open_connection("missing", None, None, None).await).contains("找不到这条连接"));
    app.disconnect("missing");
    assert!(err_text(app.focus_database("missing", "").await).contains("数据库名"));
    assert!(err_text(app.confirm_host_key("missing", "aa:bb").await).contains("没有等待确认的主机密钥"));
    assert!(err_text(app.forget_host_key("missing").await).contains("找不到这条连接"));
    let export_path = dir.join("connections.json");
    assert_eq!(app.export_connections(None, None, &export_path).await.expect("export"), 0);
    assert!(err_text(app.import_connections(&dir.join("missing.json"), None).await).contains("读不到"));

    assert!(super::work::catalog_sessions(&service).await.is_empty());
    assert!(super::work::catalog_refresh(&service, &[], Some(0), None, true).await.expect("refresh").is_empty());
    assert!(err_text(super::work::catalog_page(&service, "missing", "main", "", 0, 50).await).contains("找不到这条连接"));
    assert!(err_text(super::work::catalog_definition(&service, "missing", "main", "", "customers", "table", "", "").await).contains("还没有打开"));

    let side = SideRequest { connection_id: "missing".to_owned(), database: "main".to_owned(), schema: String::new(), engine: "sqlite".to_owned() };
    let structure = super::work::compare_structure(
        &service,
        &StructureRequest { id: "cmp".to_owned(), baseline: side.clone(), target: side.clone() },
    )
    .await;
    assert_eq!(structure.refused.as_deref(), Some("有一侧还没有打开，不生成脚本。"));
    let data = super::work::compare_data(
        &service,
        &DataRequest {
            id: "data".to_owned(),
            baseline: side.clone(),
            target: side,
            tables: vec![DataTableRequest { name: "customers".to_owned(), key: None }],
        },
    )
    .await;
    assert_eq!(data.refused.as_deref(), Some("这一侧还没有打开 不生成脚本。"));
    assert!(err_text(super::work::compare_schemas(&service, "missing", "main").await).contains("还没有打开"));
    assert!(err_text(super::work::read_diagram(&service, "missing", "main", "").await).contains("还没有打开"));
    assert!(err_text(
        super::work::dump_database(
            &service,
            &DumpRequest {
                id: "dump".to_owned(),
                connection_id: "missing".to_owned(),
                engine: "sqlite".to_owned(),
                database: "main".to_owned(),
                schemas: Vec::new(),
                tables: Vec::new(),
                include_data: false,
                path: dir.join("dump.sql").to_string_lossy().to_string(),
            },
            &|_| {},
        )
        .await
    )
    .contains("还没有打开"));
    let table = MigrateTableRef {
        connection_id: "missing".to_owned(),
        engine: "sqlite".to_owned(),
        database: "main".to_owned(),
        schema: String::new(),
        table: "customers".to_owned(),
    };
    assert!(err_text(
        super::work::migrate_data(
            &service,
            &MigrateRequest { id: "move".to_owned(), source: table.clone(), target: table, mapping: Vec::new(), options: None },
            &|_| {},
        )
        .await
    )
    .contains("还没有打开"));

    assert!(app.query.history().await.expect("history db").list().await.expect("history").is_empty());
    assert!(super::work::list_history(&app.query).await.expect("history").is_empty());
    let saved = super::work::save_saved(
        &app.query,
        &SaveRequest {
            name: "林夏😀".to_owned(),
            sql: "SELECT 1".to_owned(),
            connection_id: "keep".to_owned(),
            connection_name: "客户".to_owned(),
            database_name: "main".to_owned(),
            engine: "sqlite".to_owned(),
        },
    )
    .await
    .expect("save query");
    let renamed = super::work::rename_saved(&app.query, &RenameRequest { id: saved.query.id.clone(), name: "草稿".to_owned() }).await.expect("rename");
    assert_eq!(renamed.name, "草稿");
    assert_eq!(super::work::list_saved(&app.query).await.expect("saved").len(), 1);
    super::work::delete_saved(&app.query, &renamed.id).await.expect("delete saved");
    super::work::clear_history(&app.query).await.expect("clear");
    super::work::delete_history(&app.query, "missing").await.expect("delete missing history");
    let started = super::work::start_query(
        &service,
        &app.query,
        &app.tasks,
        "query-tab",
        StartRequest {
            connection_id: "missing".to_owned(),
            connection_name: "客户".to_owned(),
            database: "main".to_owned(),
            engine: "sqlite".to_owned(),
            sql: "   ".to_owned(),
            limit: 100,
            timeout_ms: None,
        },
    )
    .await;
    assert!(err_text(started).contains("没有发送"));
    let token = app.tasks.begin("query-tab");
    assert!(err_text(super::work::poll_query(&app.query, &app.tasks, "query-tab", token, "missing-run", false).await).contains("找不到这次执行"));
    assert!(!super::work::release_query(&app.query, &app.tasks, "query-tab", "missing-run"));
    let token = app.tasks.begin("query-tab");
    assert!(err_text(super::work::cancel_query(&service, &app.query, &app.tasks, "query-tab", token, "missing-run").await).contains("找不到这次执行"));

    let relative = crate::application::files::write_loaded("notes.csv".to_owned(), "a".to_owned()).await;
    assert!(err_text(relative).contains("完整的文件"));
    let dropped = dir.join("拖放.csv");
    crate::application::files::write_loaded(dropped.to_string_lossy().to_string(), "名称\n林夏😀\n".to_owned()).await.expect("write");
    let preview = crate::application::files::read_dropped_preview(dropped.to_string_lossy().to_string(), Some(1024)).await.expect("preview");
    assert_eq!(preview.format, "csv");
    assert!(preview.text.contains("林夏😀"));

    assert!(app.load_state().is_err());
    let skipped = app.import_legacy(&dir.join("no-legacy"), Platform::Windows).await;
    assert!(!skipped.imported && !skipped.wrote_state);
    assert!(!app.state.path().exists());
    app.save_state(&AppState::new(ShellSettings::default(), SessionState::default())).await.expect("save state");
    assert_eq!(app.load_state().expect("load").version, 1);
    let bytes = std::fs::read(app.state.path()).expect("state file");
    let text = String::from_utf8(bytes).expect("utf8");
    assert!(!text.contains("password"));
    assert!(!text.contains("passphrase"));
    let _ = std::fs::remove_dir_all(&dir);
}
