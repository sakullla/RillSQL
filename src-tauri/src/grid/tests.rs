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
