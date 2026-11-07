use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::application::tasks::TaskBoard;
use crate::connections::Service;
use crate::dropped::PREVIEW_LIMIT_BYTES;
use crate::dump::dump::DumpRequest;
use crate::grid::commands::{ExportRequest, TableRef};
use crate::i18n::{self, Locale};
use crate::migrate::migrate::{MigrateRequest, MigrateTableRef};
use crate::query::split_sql;

use super::desk::{commit_import, export_full, mapping_for, open_import, stage_loaded, watch_dump, watch_migrate, CancelPoint, ImportEntry};
use super::parse::{parse_csv, required_gaps, TableField};
use super::tasks::Phase;
use super::xlsx::{chart_first_zip, read_xlsx, second_sheet_fixture, workbook_initializations, workbook_test_lock, write_typed, write_xlsx, TypedCell};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rillsql-exchange-{tag}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

async fn sqlite_file(path: &Path, statements: &[&str]) {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(path).create_if_missing(true))
        .await
        .unwrap();
    for statement in statements {
        sqlx::query(sqlx::AssertSqlSafe((*statement).to_owned())).execute(&pool).await.unwrap();
    }
    pool.close().await;
}

async fn open_service(dir: &Path, id: &str, file: &Path) -> Arc<Service> {
    let service = Arc::new(Service::new_for_test(dir.join(format!("{id}-connections.sqlite"))));
    service.open_sqlite_for_test(id, id, file).await.unwrap();
    service
}

async fn scalar_count(path: &Path, sql: &str) -> i64 {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(path).read_only(true))
        .await
        .unwrap();
    let value: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned())).fetch_one(&pool).await.unwrap();
    pool.close().await;
    value
}

async fn scalar_text(path: &Path, sql: &str) -> String {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(path).read_only(true))
        .await
        .unwrap();
    let value: String = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned())).fetch_one(&pool).await.unwrap();
    pool.close().await;
    value
}

fn people_table(id: &str, table: &str) -> TableRef {
    TableRef {
        connection_id: id.to_owned(),
        database: "main".to_owned(),
        schema: String::new(),
        table: table.to_owned(),
        engine: "sqlite".to_owned(),
        include_foreign_keys: false,
    }
}

#[allow(clippy::too_many_arguments)]
fn export_request(id: &str, mode: &str, path: &Path, format: &str, limit: u64, truncated: bool, statement: &str, acknowledged: bool) -> ExportRequest {
    ExportRequest {
        id: id.to_owned(),
        mode: mode.to_owned(),
        table: people_table("items", "items"),
        filters: Vec::new(),
        sort: None,
        search: None,
        limit,
        truncated,
        statement: statement.to_owned(),
        acknowledged,
        path: path.display().to_string(),
        delimiter: ",".to_owned(),
        null_token: "NULL".to_owned(),
        bom: false,
        header: true,
        format: format.to_owned(),
    }
}

fn dump_request(id: &str, path: &Path) -> DumpRequest {
    DumpRequest {
        id: id.to_owned(),
        connection_id: "notes".to_owned(),
        engine: "sqlite".to_owned(),
        database: "main".to_owned(),
        schemas: Vec::new(),
        tables: Vec::new(),
        include_data: true,
        path: path.display().to_string(),
    }
}

fn migrate_ref(id: &str, table: &str) -> MigrateTableRef {
    MigrateTableRef {
        connection_id: id.to_owned(),
        engine: "sqlite".to_owned(),
        database: "main".to_owned(),
        schema: String::new(),
        table: table.to_owned(),
    }
}

fn migrate_request(id: &str, source: &str, target: &str) -> MigrateRequest {
    MigrateRequest {
        id: id.to_owned(),
        source: migrate_ref(source, "people"),
        target: migrate_ref(target, "people"),
        mapping: Vec::new(),
        options: None,
    }
}

#[test]
fn ordinary_query_split_does_not_initialize_a_workbook() {
    let _guard = workbook_test_lock();
    let before = workbook_initializations();
    let report = split_sql("sqlite", "select 1");
    assert!(!report.pieces.is_empty());
    assert_eq!(workbook_initializations(), before);
}

#[test]
fn xlsx_codec_covers_header_first_sheet_nulls_chart_and_corrupt_zip() {
    let _guard = workbook_test_lock();
    let bytes = write_xlsx(&["name".to_owned(), "note".to_owned()], &[vec![Some("甲".to_owned()), None], vec![Some(String::new()), Some(String::new())]]).unwrap();
    let table = read_xlsx(&bytes).unwrap();
    assert_eq!(table.columns, vec!["name".to_owned(), "note".to_owned()]);
    assert_eq!(table.rows, vec![vec![Some("甲".to_owned()), None]]);
    assert!(!String::from_utf8_lossy(&bytes).contains("只在第二张表"));

    let both = second_sheet_fixture().unwrap();
    let first = read_xlsx(&both).unwrap();
    assert_eq!(first.rows, vec![vec![Some("甲".to_owned())]]);
    assert!(!format!("{first:?}").contains("只在第二张表"));

    let typed = write_typed(
        &[
            TypedCell::Text("name".into()),
            TypedCell::Text("count".into()),
            TypedCell::Text("flag".into()),
            TypedCell::Text("blank".into()),
            TypedCell::Text("total".into()),
        ],
        &[vec![
            TypedCell::Text("甲".into()),
            TypedCell::Number(1.5),
            TypedCell::Bool(true),
            TypedCell::Empty,
            TypedCell::Formula { expr: "=1+2".into(), cached: Some(3.0) },
        ]],
    )
    .unwrap();
    let typed = read_xlsx(&typed).unwrap();
    assert_eq!(
        typed.columns,
        vec!["name".to_owned(), "count".to_owned(), "flag".to_owned(), "blank".to_owned(), "total".to_owned()]
    );
    assert_eq!(typed.rows[0][0].as_deref(), Some("甲"));
    assert_eq!(typed.rows[0][1].as_deref(), Some("1.5"));
    assert_eq!(typed.rows[0][2].as_deref(), Some("true"));
    assert_eq!(typed.rows[0][3], None);
    assert_eq!(typed.rows[0][4].as_deref(), Some("3"));
    assert_ne!(typed.rows[0][4].as_deref(), Some("=1+2"));

    let before = workbook_initializations();
    let chart = read_xlsx(&chart_first_zip()).unwrap_err();
    assert!(chart.contains("不是表"), "{chart}");
    assert!(!chart.contains("只在第二张表"), "{chart}");
    assert_eq!(workbook_initializations(), before);
    let corrupt = read_xlsx(b"PK\x03\x04broken").unwrap_err();
    assert!(corrupt.contains("损坏"), "{corrupt}");
    assert!(read_xlsx(b"").unwrap_err().contains("空"));
    assert!(read_xlsx(b"hello").unwrap_err().contains("不是表"));
    assert!(read_xlsx(&write_xlsx(&["email".into(), "email".into()], &[]).unwrap()).unwrap_err().contains("重复"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn drop_and_dialog_keep_xls_xlsx_and_two_megabyte_preview_limits() {
    let _guard = workbook_test_lock();
    let dir = temp_dir("preview");
    let csv = dir.join("rows.csv");
    fs::write(&csv, "name\n甲\n乙\n").unwrap();
    let preview = open_import(&csv.display().to_string(), ImportEntry::Drop).await.unwrap();
    assert!(Path::new(&preview.path).is_absolute(), "{}", preview.path);
    assert_eq!(preview.table.rows.len(), 2);
    assert!(!preview.truncated);

    for name in ["orders.xlsx", "orders.xls"] {
        let message = open_import(&dir.join(name).display().to_string(), ImportEntry::Drop).await.unwrap_err();
        assert!(message.contains("不支持"), "{name}: {message}");
        assert!(message.contains("导入对话框"), "{name}: {message}");
    }
    let xls = open_import(&dir.join("book.xls").display().to_string(), ImportEntry::Dialog).await.unwrap_err();
    assert!(xls.contains(".xls"), "{xls}");
    assert!(!xls.contains("不支持拖放"), "{xls}");

    let xlsx = dir.join("book.xlsx");
    fs::write(&xlsx, write_xlsx(&["name".into()], &[vec![Some("甲".into())]]).unwrap()).unwrap();
    let selected = open_import(&xlsx.display().to_string(), ImportEntry::Dialog).await.unwrap();
    assert_eq!(selected.format, "xlsx");
    assert_eq!(selected.table.rows, vec![vec![Some("甲".to_owned())]]);

    let huge = dir.join("huge.csv");
    let mut body = String::from("name\n");
    while body.len() <= PREVIEW_LIMIT_BYTES as usize {
        body.push_str("甲\n");
    }
    fs::write(&huge, &body).unwrap();
    let capped = open_import(&huge.display().to_string(), ImportEntry::Drop).await.unwrap();
    assert!(capped.file_size > PREVIEW_LIMIT_BYTES);
    assert_eq!(capped.read_bytes, PREVIEW_LIMIT_BYTES);
    assert!(capped.truncated);
    assert!(capped.notice.contains("2 MB"), "{}", capped.notice);
    assert!(capped.table.rows.len() < body.lines().count());

    let fat = dir.join("fat.xlsx");
    fs::write(&fat, vec![0u8; PREVIEW_LIMIT_BYTES as usize + 8]).unwrap();
    let before = workbook_initializations();
    let rejected = open_import(&fat.display().to_string(), ImportEntry::Dialog).await.unwrap_err();
    assert!(rejected.contains("2 MB"), "{rejected}");
    assert!(rejected.contains("没有打开工作簿"), "{rejected}");
    assert_eq!(workbook_initializations(), before);

    let missing = open_import(&dir.join("absent.csv").display().to_string(), ImportEntry::Drop).await.unwrap_err();
    assert!(missing.contains("无法读取"), "{missing}");
    let relative = open_import("rows.csv", ImportEntry::Drop).await.unwrap_err();
    assert!(relative.contains("路径不完整"), "{relative}");
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn confirmed_import_writes_sqlite_and_unconfirmed_or_invalid_targets_do_not() {
    let dir = temp_dir("import");
    let file = dir.join("people.sqlite");
    sqlite_file(
        &file,
        &[
            "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            "CREATE VIEW v_people AS SELECT id, name FROM people",
        ],
    )
    .await;
    let service = open_service(&dir, "people", &file).await;
    let csv = dir.join("people.csv");
    fs::write(&csv, "id,name\n1,甲\n2,\n").unwrap();
    let preview = open_import(&csv.display().to_string(), ImportEntry::Drop).await.unwrap();
    let fields = vec![
        TableField { name: "id".into(), nullable: false, auto: true },
        TableField { name: "name".into(), nullable: false, auto: false },
    ];
    let mapping = mapping_for(&fields, &preview.table);
    assert!(required_gaps(&fields, &mapping).is_empty(), "{mapping:?}");
    let skipped = commit_import(&service, &people_table("people", "people"), &preview.table, &mapping, false).await.unwrap();
    assert!(!skipped.wrote && !skipped.success);
    assert_eq!(scalar_count(&file, "SELECT COUNT(*) FROM people").await, 0);
    let refused = crate::application::grid::commit(
        &service,
        &crate::grid::commands::CommitRequest { table: people_table("people", "people"), changes: Vec::new() },
        false,
    )
    .await;
    let Err(message) = refused else {
        panic!("未确认的网格提交不应写库");
    };
    assert!(message.contains("确认"), "{message}");
    assert_eq!(scalar_count(&file, "SELECT COUNT(*) FROM people").await, 0);

    let partial = commit_import(&service, &people_table("people", "people"), &preview.table, &mapping, true).await.unwrap();
    assert!(!partial.success, "{}", partial.message);
    assert!(partial.message.contains("不是全部成功"), "{}", partial.message);
    assert_eq!(partial.applied, 1, "{}", partial.message);
    assert_eq!(scalar_text(&file, "SELECT name FROM people").await, "甲");

    let view = commit_import(&service, &people_table("people", "v_people"), &preview.table, &mapping, true).await.unwrap();
    assert!(!view.wrote && !view.success, "{}", view.message);
    assert!(view.message.contains("视图"), "{}", view.message);

    let mut blocked_map = mapping.clone();
    blocked_map.insert("name".into(), String::new());
    let gaps = commit_import(&service, &people_table("people", "people"), &preview.table, &blocked_map, true).await.unwrap();
    assert!(!gaps.wrote, "{}", gaps.message);
    assert_eq!(scalar_count(&file, "SELECT COUNT(*) FROM people").await, 1);
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn loaded_and_full_exports_write_real_files_and_keep_partial_rename() {
    let _guard = workbook_test_lock();
    let dir = temp_dir("export");
    let file = dir.join("items.sqlite");
    sqlite_file(&file, &["CREATE TABLE items (id INTEGER PRIMARY KEY, note TEXT)", "INSERT INTO items (id, note) VALUES (1, NULL), (2, '乙'), (3, '丙')"]).await;
    let service = open_service(&dir, "items", &file).await;
    let tasks = TaskBoard::new();
    let columns = vec!["id".into(), "note".into()];
    let loaded_rows = vec![vec![Some("1".into()), None]];
    for format in ["csv", "json", "xlsx", "markdown", "insert"] {
        let path = dir.join(format!("loaded.{format}"));
        let mut spec = stage_loaded("loaded", format, &path, columns.clone(), loaded_rows.clone(), true, "items");
        if format == "csv" {
            spec.bom = false;
        }
        let task = super::export_loaded(&spec);
        assert_eq!(task.phase, Phase::Partial, "{format} {}", task.detail);
        assert!(!task.success, "{format}");
        let written = PathBuf::from(task.path.unwrap());
        assert!(written.is_absolute(), "{format}");
        assert!(written.display().to_string().contains("（部分）"), "{format} {}", written.display());
        assert!(written.is_file(), "{format}");
        assert!(!dir.join(".rillsql-missing.partial").exists());
        assert!(fs::read_dir(&dir).unwrap().all(|entry| !entry.unwrap().file_name().to_string_lossy().ends_with(".partial")));
        if format == "xlsx" {
            let table = read_xlsx(&fs::read(&written).unwrap()).unwrap();
            assert_eq!(table.rows.len(), 1);
        }
    }

    for format in ["csv", "json", "xlsx", "markdown", "insert"] {
        let path = dir.join(format!("full.{format}"));
        let task = export_full(&service, &tasks, &export_request("full", "table-full", &path, format, 0, false, "", true), false).await.unwrap();
        assert_eq!(task.phase, Phase::Done, "{format} {}", task.detail);
        assert!(task.success, "{format} {}", task.detail);
        assert_eq!(task.done_rows, 3, "{format}");
        let written = PathBuf::from(task.path.unwrap());
        assert_eq!(written, path);
        assert!(written.is_absolute());
        assert!(!written.display().to_string().contains("（部分）"));
        if format == "csv" {
            let text = fs::read_to_string(&written).unwrap();
            assert!(text.contains("乙") && text.contains("丙"), "{text}");
        }
        if format == "xlsx" {
            assert!(fs::read(&written).unwrap().starts_with(b"PK"));
        }
    }

    let partial_path = dir.join("partial.csv");
    let partial = export_full(&service, &tasks, &export_request("part", "table-loaded", &partial_path, "csv", 1, true, "", true), false).await.unwrap();
    assert_eq!(partial.phase, Phase::Partial, "{}", partial.detail);
    assert!(!partial.success);
    assert_eq!(partial.done_rows, 1);
    assert!(partial.detail.contains("不是完整结果") || partial.path.as_deref().unwrap_or("").contains("（部分）"), "{}", partial.detail);

    let blocked = dir.join("blocked.csv");
    let denied = export_full(&service, &tasks, &export_request("deny", "query", &blocked, "csv", 0, false, "UPDATE items SET note = 'x'", true), false)
        .await
        .unwrap();
    assert_eq!(denied.phase, Phase::Blocked, "{}", denied.detail);
    assert!(!denied.success);
    assert!(!blocked.exists());

    let relative = stage_loaded("rel", "csv", Path::new("rows.csv"), columns.clone(), loaded_rows.clone(), false, "items");
    let rejected = super::export_loaded(&relative);
    assert_eq!(rejected.phase, Phase::Blocked, "{}", rejected.detail);
    assert!(!Path::new("rows.csv").exists());

    let mut cancel = stage_loaded("cancel", "csv", &dir.join("cancel.csv"), columns.clone(), vec![vec![Some("1".into())], vec![Some("2".into())]], false, "items");
    cancel.cancel_after_rows = Some(1);
    let cancelled = super::export_loaded(&cancel);
    assert_eq!(cancelled.phase, Phase::Cancelled, "{}", cancelled.detail);
    assert!(!cancelled.success);
    assert_eq!(cancelled.done_rows, 1);
    assert!(!dir.join("cancel.csv").exists());
    assert!(fs::read_dir(&dir).unwrap().all(|entry| !entry.unwrap().file_name().to_string_lossy().contains("cancel.csv")));

    let mut failed = stage_loaded("fail", "json", &dir.join("fail.json"), columns, loaded_rows, false, "items");
    failed.fail_after_rows = Some(1);
    let failure = super::export_loaded(&failed);
    assert_eq!(failure.phase, Phase::Failed, "{}", failure.detail);
    assert!(!failure.success);
    assert!(!dir.join("fail.json").exists());
    assert!(failure.path.as_deref().unwrap_or("").contains(".partial"), "{failure:?}");

    let late_path = dir.join("late.csv");
    let late = export_full(&service, &tasks, &export_request("late", "table-full", &late_path, "csv", 0, false, "", true), true).await.unwrap();
    assert_eq!(late.phase, Phase::Cancelled, "{}", late.detail);
    assert!(!late.success);
    assert_eq!(late.done_rows, 3, "{}", late.detail);
    assert!(late.detail.contains("已取消"), "{}", late.detail);
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sqlite_dump_and_migrate_report_real_ranges_and_do_not_call_cancel_success() {
    let dir = temp_dir("move");
    let source_file = dir.join("source.sqlite");
    let target_file = dir.join("target.sqlite");
    let conflict_file = dir.join("conflict.sqlite");
    sqlite_file(&source_file, &["CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT)", "INSERT INTO people (id, name) VALUES (1, '甲'), (2, '乙')"]).await;
    sqlite_file(&target_file, &["CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT)"]).await;
    sqlite_file(&conflict_file, &["CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT)", "INSERT INTO people (id, name) VALUES (1, '原值')"]).await;
    let service = Arc::new(Service::new_for_test(dir.join("connections.sqlite")));
    service.open_sqlite_for_test("notes", "笔记", &source_file).await.unwrap();
    service.open_sqlite_for_test("copy", "副本", &target_file).await.unwrap();
    service.open_sqlite_for_test("clash", "冲突", &conflict_file).await.unwrap();
    let tasks = TaskBoard::new();

    let dump_path = dir.join("notes.sql");
    let dumped = watch_dump(&service, &tasks, &dump_request("dump-ok", &dump_path), CancelPoint::No).await.unwrap();
    assert_eq!(dumped.phase, Phase::Done, "{}", dumped.detail);
    assert!(dumped.success, "{}", dumped.detail);
    let sql = fs::read_to_string(dumped.path.unwrap()).unwrap();
    assert!(sql.contains("CREATE TABLE"), "{sql}");
    assert!(sql.contains("甲"), "{sql}");
    assert!(dump_path.is_absolute());
    assert!(!dump_path.with_file_name(".rillsql-x.partial").exists());

    let moved = watch_migrate(&service, &tasks, &migrate_request("move-ok", "notes", "copy"), CancelPoint::No).await.unwrap();
    assert_eq!(moved.phase, Phase::Done, "{}", moved.detail);
    assert!(moved.success, "{}", moved.detail);
    assert_eq!(scalar_count(&target_file, "SELECT COUNT(*) FROM people").await, 2);
    assert_eq!(scalar_text(&target_file, "SELECT name FROM people WHERE id = 1").await, "甲");

    let clash = watch_migrate(&service, &tasks, &migrate_request("move-clash", "notes", "clash"), CancelPoint::No).await.unwrap();
    assert_eq!(clash.phase, Phase::Partial, "{}", clash.detail);
    assert!(!clash.success, "{}", clash.detail);
    assert!(clash.detail.contains("不是全部成功"), "{}", clash.detail);
    assert_eq!(scalar_text(&conflict_file, "SELECT name FROM people WHERE id = 1").await, "原值");

    let cancel_path = dir.join("cancel.sql");
    let cancelled = watch_dump(&service, &tasks, &dump_request("dump-stop", &cancel_path), CancelPoint::DumpOnStructure).await.unwrap();
    assert_eq!(cancelled.phase, Phase::Cancelled, "{}", cancelled.detail);
    assert!(!cancelled.success, "{}", cancelled.detail);
    assert!(cancelled.detail.contains("已取消"), "{}", cancelled.detail);
    assert!(!cancelled.detail.contains("进行中"), "{}", cancelled.detail);
    assert!(!cancel_path.exists(), "{}", cancelled.detail);

    let late = watch_dump(&service, &tasks, &dump_request("dump-late", &dir.join("late.sql")), CancelPoint::BoardOnData).await.unwrap();
    assert_eq!(late.phase, Phase::Cancelled, "{}", late.detail);
    assert!(!late.success, "{}", late.detail);
    assert!(late.done_rows > 0, "{}", late.detail);

    let stopped = watch_migrate(&service, &tasks, &migrate_request("move-stop", "notes", "copy"), CancelPoint::MigrateOnProgress).await.unwrap();
    assert_eq!(stopped.phase, Phase::Cancelled, "{}", stopped.detail);
    assert!(!stopped.success, "{}", stopped.detail);
    assert!(stopped.detail.contains("已取消"), "{}", stopped.detail);
    assert!(!stopped.detail.contains("进行中"), "{}", stopped.detail);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn both_locales_have_exchange_labels() {
    for key in [
        "export.open",
        "export.task.cancelled",
        "export.task.running",
        "export.import.unconfirmed",
        "export.import.choose",
        "export.preview.truncated",
        "export.task.range",
    ] {
        assert_ne!(i18n::text(Locale::ZhCn, key), key);
        assert_ne!(i18n::text(Locale::En, key), key);
    }
    let csv = parse_csv("name,note\n甲,\n\"\",乙\n", ',').unwrap();
    assert_eq!(csv.rows[0], vec![Some("甲".into()), None]);
    assert_eq!(csv.rows[1][0], Some(String::new()));
}
