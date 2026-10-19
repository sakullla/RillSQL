use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::cancel::{self, fresh_cancel_channel};
use super::exec::{self, classify_failure, database_gate, run_batch, DatabaseGate, StatementDriver, StatementStop, StatementValue};
use super::history::{self, HistoryDraft, HistoryEntry};
use super::split::{self, Piece};

struct Scripted {
    steps: Mutex<Vec<Result<StatementValue, StatementStop>>>,
    calls: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl StatementDriver for Scripted {
    async fn run(&self, sql: &str, _limit: usize, _cancel: &Arc<AtomicBool>) -> Result<StatementValue, StatementStop> {
        self.calls.lock().expect("calls").push(sql.to_owned());
        self.steps.lock().expect("steps").remove(0)
    }
}

fn piece(sql: &str) -> Piece {
    Piece {
        text: sql.to_owned(),
        start: 0,
        end: sql.len(),
    }
}

fn ok_count() -> StatementValue {
    StatementValue {
        columns: Vec::new(),
        column_types: Vec::new(),
        rows: Vec::new(),
        truncated: false,
        affected: Some(1),
    }
}

#[test]
fn split_keeps_quotes_comments_and_current_statement() {
    let sql = "SELECT 'a;b';\n-- c;\nUPDATE t SET n = 1;\nSELECT $$do;$$;";
    let parts = split::split_sql("postgres", sql).pieces;
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].text, "SELECT 'a;b';");
    assert_eq!(parts[1].text, "UPDATE t SET n = 1;");
    assert!(parts[2].text.contains("$$do;$$"));
    let current = split::statement_at("postgres", sql, parts[1].start + 2).expect("光标所在语句");
    assert_eq!(current.text, "UPDATE t SET n = 1;");
    assert!(split::is_write(&current.text));
    assert!(!split::is_write("SELECT 1"));
}

#[test]
fn engine_boundaries_keep_routine_bodies_and_hash_rules() {
    let mysql = "DELIMITER $$\nCREATE PROCEDURE p()\nBEGIN\n  UPDATE t SET n = 1;\nEND$$\nDELIMITER ;\nSELECT 1;";
    let report = split::split_sql("mysql", mysql);
    assert!(report.refusal.is_none(), "{:?}", report.refusal);
    assert!(report.notices.is_empty());
    assert_eq!(report.pieces.len(), 2);
    assert!(report.pieces[0].text.contains("UPDATE t SET n = 1;"));
    assert!(report.pieces[0].text.contains("END"));
    assert!(!report.pieces.iter().any(|piece| piece.text.contains("DELIMITER")));
    assert_eq!(report.pieces[1].text, "SELECT 1;");

    let hash = "SELECT 1 # keep; this\n + 2;";
    assert_eq!(split::split_sql("mysql", hash).pieces.len(), 1);
    assert_eq!(split::split_sql("postgres", "SELECT 1;\nSELECT 2 #>;").pieces.len(), 2);
    assert!(split::split_sql("postgres", "SELECT 1 #> '{a}';").pieces[0].text.contains("#>"));

    let routine = split::split_sql("mysql", "CREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n");
    assert_eq!(routine.refusal.as_deref(), Some("例程体未闭合，没有发送。"));
    let transaction = split::split_sql("postgres", "BEGIN;\nUPDATE t SET n = 1;\n");
    assert!(transaction.refusal.is_none());
    assert_eq!(transaction.pieces.len(), 2);

    let unclosed = split::split_sql("postgres", "SELECT 'a;");
    assert_eq!(unclosed.refusal.as_deref(), Some("引号没有闭合，没有发送。"));
    let postgres_delimiter = split::split_sql("sqlite", "DELIMITER $$\nSELECT 1;");
    assert!(postgres_delimiter.refusal.is_none());
    assert_eq!(postgres_delimiter.pieces.len(), 1);
    assert_eq!(postgres_delimiter.pieces[0].text, "SELECT 1;");
    assert!(postgres_delimiter.notices[0].contains("不使用 DELIMITER"));
}

#[test]
fn database_gate_does_not_open_another_postgres_connection() {
    assert_eq!(database_gate("postgres", "app", "app"), DatabaseGate::Ready);
    assert_eq!(database_gate("postgres", "app", "other"), DatabaseGate::Refuse);
    assert_eq!(database_gate("mysql", "app", "other"), DatabaseGate::Switch);
    assert_eq!(database_gate("sqlite", "main", "main"), DatabaseGate::Ready);
    assert_eq!(database_gate("sqlite", "main", "other"), DatabaseGate::Refuse);
    assert!(exec::refuse_message("app", "other").contains("没有另开连接"));
}

#[test]
fn cancel_phases_stay_distinct() {
    let mut view = cancel::on_request(true);
    assert_eq!(view.phase, "requested");
    assert!(!view.fresh_channel);
    cancel::on_delivery(&mut view, Ok(true), true);
    assert_eq!(view.phase, "requested");
    assert!(view.fresh_channel);
    assert!(view.message.contains("另一条"));
    cancel::on_statement_end(&mut view, "cancelled");
    assert_eq!(view.phase, "confirmed");

    let mut failed = cancel::on_request(false);
    cancel::on_delivery(&mut failed, Err("取消失败：权限不足。查询仍在执行。".to_owned()), false);
    assert_eq!(failed.phase, "failed");
    assert!(!failed.fresh_channel);

    let mut reused = cancel::on_request(true);
    cancel::on_delivery(&mut reused, Ok(false), true);
    assert_eq!(reused.phase, "failed");
    assert!(reused.message.contains("没能另开连接"));
}

#[test]
fn failure_marks_uncertain_writes_unknown_and_not_reads() {
    let unknown = classify_failure("UPDATE t SET n = 1", "connection reset by peer".to_owned(), false);
    assert!(matches!(unknown, StatementStop::Unknown { .. }));
    let read = classify_failure("SELECT 1", "connection reset by peer".to_owned(), false);
    assert!(matches!(read, StatementStop::Error { .. }));
    let cancelled = classify_failure("SELECT 1", "canceling statement due to user request".to_owned(), true);
    assert!(matches!(cancelled, StatementStop::Cancelled { .. }));
}

#[tokio::test]
async fn multi_statement_stops_on_failure_and_does_not_replay_unknown() {
    let driver = Scripted {
        steps: Mutex::new(vec![
            Ok(ok_count()),
            Err(StatementStop::Unknown {
                message: "连接中断，无法确认写入是否完成。不会自动重放。".to_owned(),
            }),
            Ok(ok_count()),
        ]),
        calls: Mutex::new(Vec::new()),
    };
    let flag = Arc::new(AtomicBool::new(false));
    let parts = vec![piece("INSERT INTO t VALUES (1)"), piece("UPDATE t SET n = 2"), piece("DELETE FROM t")];
    let report = run_batch(&parts, 100, &flag, false, &driver).await;
    assert_eq!(report.status, "unknown");
    assert_eq!(report.calls, 2);
    assert_eq!(report.statements[0].status, "ok");
    assert_eq!(report.statements[1].status, "unknown");
    assert_eq!(report.statements[2].status, "skipped");
    assert_eq!(driver.calls.lock().expect("calls").iter().filter(|sql| sql.starts_with("UPDATE")).count(), 1);
}

#[tokio::test]
async fn ssh_cancel_accepts_a_second_direct_tcpip() {
    use crate::connections::forward::{Forwarder, RecordingOpener};

    let opener = Arc::new(RecordingOpener::new());
    let forwarder = Forwarder::bind().await.expect("转发器");
    let port = forwarder.local_port().expect("端口");
    let (shutdown, stop) = tokio::sync::oneshot::channel();
    let task_opener = Arc::clone(&opener);
    tokio::spawn(async move {
        let _ = forwarder
            .serve_until(
                task_opener,
                "db.example.test".to_owned(),
                5432,
                "db.example.test".to_owned(),
                "bastion.example.test".to_owned(),
                stop,
            )
            .await;
    });
    let exec = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.expect("执行通道");
    wait_opens(&opener, 1).await;
    let busy = opener.opens.lock().expect("opens")[0].id;
    let cancel = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.expect("取消通道");
    wait_opens(&opener, 2).await;
    let opens = opener.opens.lock().expect("opens").clone();
    let fresh = fresh_cancel_channel(&opens, busy).expect("新通道");
    assert_eq!(fresh.host, "db.example.test");
    assert_eq!(fresh.port, 5432);
    assert_ne!(fresh.host, "bastion.example.test");
    assert_ne!(fresh.id, busy);
    drop(exec);
    drop(cancel);
    let _ = shutdown.send(());
}

async fn wait_opens(opener: &crate::connections::forward::RecordingOpener, count: usize) {
    for _ in 0..50 {
        if opener.opens.lock().expect("opens").len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("没有等到 {count} 条 direct-tcpip");
}

#[test]
fn cancel_code_does_not_reuse_the_executing_stream() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cancel = std::fs::read_to_string(root.join("src/query/cancel.rs")).unwrap();
    let exec = std::fs::read_to_string(root.join("src/query/exec.rs")).unwrap();
    assert!(cancel.contains("Pool::new"));
    assert!(cancel.contains("cancel_query"));
    assert!(!cancel.contains("cancel_query_raw"));
    assert!(!exec.contains("open_sqlite"));
    assert!(!exec.contains("open_mysql"));
    assert!(!exec.contains("open_postgres"));
    assert!(!exec.contains("Pool::new"));
    let route = std::fs::read_to_string(root.join("src/connections/route.rs")).unwrap();
    assert!(route.contains("verify_hostname"));
    assert!(route.contains("不会改用 localhost，也不会关闭主机名验证"));
}

#[test]
fn query_commands_are_appended_without_dropping_catalog_or_grid() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let capability: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("capabilities/default.json")).unwrap()).unwrap();
    let permissions = capability["permissions"].as_array().unwrap();
    let acl = std::fs::read_to_string(root.join("permissions/app-commands.toml")).unwrap();
    let registration = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
    let wire = registration.split("fn wire").nth(1).unwrap_or("");
    let wire = wire.split("pub fn run").next().unwrap_or("");
    for command in [
        "query_start",
        "query_poll",
        "query_cancel",
        "query_history",
        "query_history_clear",
        "query_saved_list",
        "query_saved_save",
        "query_saved_rename",
        "query_saved_delete",
    ] {
        let allow = format!("allow-{}", command.replace('_', "-"));
        assert!(permissions.iter().any(|item| item.as_str() == Some(allow.as_str())), "{allow}");
        assert!(acl.contains(&format!("commands.allow = [\"{command}\"]")), "{command}");
        assert!(wire.contains(&format!("query::commands::{command}")), "{command}");
    }
    for command in ["catalog_sessions", "catalog_refresh", "catalog_page", "grid_describe", "grid_page", "grid_count", "grid_commit", "grid_export", "grid_export_cancel"] {
        assert!(wire.contains(command), "{command}");
    }
}

#[tokio::test]
async fn history_records_execution_without_a_password_column_and_restore_does_not_execute() {
    let dir = std::env::temp_dir().join(format!("rillsql-history-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = history::HistoryStore::open(&dir.join("query-history.sqlite")).await.unwrap();
    let columns = store.column_names().await.unwrap();
    assert!(!columns.iter().any(|name| name.to_ascii_lowercase().contains("password")));
    store
        .insert(&HistoryDraft {
            id: "h1".to_owned(),
            sql: "DELETE FROM customers WHERE id = 1".to_owned(),
            connection_id: "gone".to_owned(),
            connection_name: "旧连接".to_owned(),
            database_name: "app".to_owned(),
            engine: "postgres".to_owned(),
            started_at: "2026-10-14T00:00:00Z".to_owned(),
            summary: "执行中".to_owned(),
        })
        .await
        .unwrap();
    store.finish("h1", "2026-10-14T00:00:01Z", "ok", 12, "1 条语句完成").await.unwrap();
    let listed = store.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].sql, "DELETE FROM customers WHERE id = 1");
    let restored = history::restored_draft(&listed[0], false);
    assert!(restored.needs_target);
    assert!(!restored.execute);
    assert!(restored.connection_id.is_none());
    let kept = history::restored_draft(&listed[0], true);
    assert!(!kept.needs_target);
    assert!(!kept.execute);
    assert_eq!(kept.connection_id.as_deref(), Some("gone"));
    store.clear().await.unwrap();
    assert!(store.list().await.unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
    let _typed = HistoryEntry {
        id: "x".to_owned(),
        sql: String::new(),
        connection_id: String::new(),
        connection_name: String::new(),
        database_name: String::new(),
        engine: String::new(),
        started_at: String::new(),
        finished_at: None,
        status: "ok".to_owned(),
        elapsed_ms: None,
        summary: String::new(),
    };
    let _ = _typed;
}

#[tokio::test]
async fn saved_queries_roundtrip_without_writing_execution_history() {
    use super::saved::{RenameRequest, SaveRequest};

    let dir = std::env::temp_dir().join(format!("rillsql-saved-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = history::HistoryStore::open(&dir.join("query-history.sqlite")).await.unwrap();
    let columns = store.saved_column_names().await.unwrap();
    assert!(!columns.iter().any(|name| name.to_ascii_lowercase().contains("password")));
    assert!(columns.iter().any(|name| name == "name"));
    assert!(columns.iter().any(|name| name == "sql"));

    let blank = store
        .save_named(&SaveRequest {
            name: "   ".to_owned(),
            sql: "SELECT 1".to_owned(),
            connection_id: String::new(),
            connection_name: String::new(),
            database_name: String::new(),
            engine: String::new(),
        })
        .await
        .unwrap_err();
    assert_eq!(blank, "请填写名称");
    assert!(store.list_saved().await.unwrap().is_empty());

    let created = store
        .save_named(&SaveRequest {
            name: " 日报 ".to_owned(),
            sql: "SELECT id FROM orders".to_owned(),
            connection_id: "shop".to_owned(),
            connection_name: "Shop".to_owned(),
            database_name: "shop_staging".to_owned(),
            engine: "mysql".to_owned(),
        })
        .await
        .unwrap();
    assert!(created.created);
    assert_eq!(created.query.name, "日报");
    assert_eq!(created.query.sql, "SELECT id FROM orders");
    store
        .insert(&HistoryDraft {
            id: "h1".to_owned(),
            sql: "SELECT 1".to_owned(),
            connection_id: "shop".to_owned(),
            connection_name: "Shop".to_owned(),
            database_name: "shop_staging".to_owned(),
            engine: "mysql".to_owned(),
            started_at: "2026-10-15T00:00:00Z".to_owned(),
            summary: "执行中".to_owned(),
        })
        .await
        .unwrap();
    store.finish("h1", "2026-10-15T00:00:01Z", "ok", 4, "1 条语句完成").await.unwrap();

    let updated = store
        .save_named(&SaveRequest {
            name: "日报".to_owned(),
            sql: "SELECT id, amount FROM orders".to_owned(),
            connection_id: "gone".to_owned(),
            connection_name: "旧连接".to_owned(),
            database_name: "app".to_owned(),
            engine: "postgres".to_owned(),
        })
        .await
        .unwrap();
    assert!(!updated.created);
    assert_eq!(updated.query.id, created.query.id);
    assert_eq!(updated.query.sql, "SELECT id, amount FROM orders");
    let listed = store.list_saved().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(store.list().await.unwrap().len(), 1);

    store
        .save_named(&SaveRequest {
            name: "周报".to_owned(),
            sql: "SELECT 2".to_owned(),
            connection_id: String::new(),
            connection_name: String::new(),
            database_name: String::new(),
            engine: String::new(),
        })
        .await
        .unwrap();
    let names = store.list_saved().await.unwrap().into_iter().map(|item| item.name).collect::<Vec<_>>();
    assert_eq!(names, vec!["周报".to_owned(), "日报".to_owned()]);

    let clash = store
        .rename_saved(&RenameRequest {
            id: updated.query.id.clone(),
            name: "周报".to_owned(),
        })
        .await
        .unwrap_err();
    assert_eq!(clash, "已经有同名的保存查询");
    let renamed = store
        .rename_saved(&RenameRequest {
            id: updated.query.id.clone(),
            name: "晨报".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(renamed.name, "晨报");
    assert_eq!(renamed.sql, "SELECT id, amount FROM orders");
    assert!(store
        .rename_saved(&RenameRequest {
            id: "missing".to_owned(),
            name: "别的".to_owned(),
        })
        .await
        .unwrap_err()
        .contains("找不到"));

    store.clear().await.unwrap();
    assert!(store.list().await.unwrap().is_empty());
    assert_eq!(store.list_saved().await.unwrap().len(), 2);
    store.delete_saved(&renamed.id).await.unwrap();
    let left = store.list_saved().await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].name, "周报");
    assert!(store.delete_saved(&renamed.id).await.unwrap_err().contains("找不到"));
    assert!(store.list().await.unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn open_session_runs_sql_stops_and_can_cancel_without_a_second_execution_connection() {
    use crate::connections::Service;
    use super::commands::{request_cancel, start_run, QueryState, StartRequest};

    let dir = std::env::temp_dir().join(format!("rillsql-query-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let database = dir.join("notes.sqlite");
    let setup = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
        .execute(&setup)
        .await
        .unwrap();
    setup.close().await;

    let service = Arc::new(Service::new_for_test(dir.join("connections.sqlite")));
    service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
    assert!(service.catalog_session("notes").is_some());
    let state = QueryState::new(dir.join("query-history.sqlite"));
    let started = start_run(
        &service,
        &state,
        StartRequest {
            connection_id: "notes".to_owned(),
            connection_name: "笔记".to_owned(),
            database: "main".to_owned(),
            engine: "sqlite".to_owned(),
            sql: "INSERT INTO t (id, name) VALUES (1, '');\nSELECT id, name FROM missing;\nINSERT INTO t (id, name) VALUES (2, 'x');".to_owned(),
            limit: 50,
        },
    )
    .await
    .unwrap();
    let finished = wait_done(&state, &started.id).await;
    assert_eq!(finished.status, "error");
    assert_eq!(finished.statements[0].status, "ok");
    assert_eq!(finished.statements[1].status, "error");
    assert_eq!(finished.statements[2].status, "skipped");
    assert_eq!(finished.statements[0].affected, Some(1));
    let session = service.catalog_session("notes").unwrap();
    let check = exec::LiveDriver::attach(session, "sqlite", "main", service.cancel_transport("notes").unwrap())
        .await
        .unwrap();
    let rows = check.run("SELECT id, name FROM t", 10, &Arc::new(AtomicBool::new(false))).await.unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0][1], Some(String::new()));
    let history = state.history().await.unwrap().list().await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].status, "error");
    assert!(!history[0].sql.contains("password"));

    let slow = start_run(
        &service,
        &state,
        StartRequest {
            connection_id: "notes".to_owned(),
            connection_name: "笔记".to_owned(),
            database: "main".to_owned(),
            engine: "sqlite".to_owned(),
            sql: "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 5000000) SELECT sum(x) FROM c".to_owned(),
            limit: 10,
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let cancelling = request_cancel(&service, &state, &slow.id).await.unwrap();
    assert_eq!(cancelling.cancel, "requested");
    let ended = wait_done(&state, &slow.id).await;
    assert_eq!(ended.cancel, "confirmed");
    assert_eq!(ended.status, "cancelled");
    assert!(service.catalog_session("notes").is_some());
    let _ = std::fs::remove_dir_all(&dir);
    let _calls = AtomicUsize::new(0);
}

#[test]
fn result_cap_limits_the_server_read_without_raising_a_smaller_limit() {
    let capped = exec::cap_result_sql("postgres", "SELECT * FROM t", 10);
    assert_eq!(capped, "SELECT * FROM t LIMIT 11");
    assert_eq!(exec::cap_result_sql("postgres", "SELECT * FROM t;", 10), "SELECT * FROM t LIMIT 11;");
    assert_eq!(exec::cap_result_sql("postgres", "SELECT * FROM t -- tail", 10), "SELECT * FROM t LIMIT 11 -- tail");
    assert_eq!(exec::cap_result_sql("postgres", "SELECT * FROM t FOR UPDATE", 10), "SELECT * FROM t LIMIT 11 FOR UPDATE");
    assert_eq!(exec::cap_result_sql("postgres", "SELECT * FROM t OFFSET 4", 10), "SELECT * FROM t LIMIT 11 OFFSET 4");
    assert_eq!(exec::cap_result_sql("postgres", "SELECT * FROM t LIMIT 2", 10), "SELECT * FROM t LIMIT 2");
    assert_eq!(exec::cap_result_sql("postgres", "SELECT * FROM t LIMIT 2 FOR UPDATE", 10), "SELECT * FROM t LIMIT 2 FOR UPDATE");
    assert_eq!(exec::cap_result_sql("postgres", "SELECT * FROM t LIMIT 1000", 10), "SELECT * FROM t LIMIT 11");
    assert_eq!(exec::cap_result_sql("postgres", "SELECT * FROM t LIMIT 3, 1000", 10), "SELECT * FROM t LIMIT 3, 11");
    assert_eq!(
        exec::cap_result_sql("postgres", "SELECT * FROM t FETCH FIRST 1000 ROWS ONLY", 10),
        "SELECT * FROM t FETCH FIRST 11 ROWS ONLY"
    );
    assert_eq!(exec::cap_result_sql("postgres", "SELECT 'LIMIT 1000' FROM t", 10), "SELECT 'LIMIT 1000' FROM t LIMIT 11");
    assert_eq!(
        exec::cap_result_sql("postgres", "WITH c AS (SELECT * FROM t LIMIT 1000) SELECT * FROM c", 10),
        "WITH c AS (SELECT * FROM t LIMIT 1000) SELECT * FROM c LIMIT 11"
    );
    assert_eq!(exec::cap_result_sql("postgres", "SELECT * FROM t LIMIT @n", 10), "SELECT * FROM t LIMIT @n");
    assert_eq!(exec::cap_result_sql("postgres", "UPDATE t SET n = 1", 10), "UPDATE t SET n = 1");
    assert_eq!(exec::cap_result_sql("postgres", "SELECT * INTO OUTFILE 'x' FROM t", 10), "SELECT * INTO OUTFILE 'x' FROM t");
}

#[test]
fn mysql_batch_checks_out_once_and_does_not_reset_on_return() {
    let exec = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/query/exec.rs")).unwrap();
    let mysql_run = exec.split("async fn mysql_run").nth(1).unwrap();
    let mysql_run = mysql_run.split("fn mysql_cell").next().unwrap();
    assert!(!mysql_run.contains("get_conn"), "每条语句不能重新借连接");
    assert_eq!(exec.matches("get_conn").count(), 1);
    assert!(exec.contains("reset_connection(false)"));
    let drivers = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/connections/drivers.rs")).unwrap();
    assert!(drivers.contains("with_reset_connection(false)"), "池归还时不能把这条会话重置掉");
    assert!(drivers.contains("with_inactive_connection_ttl"), "空闲时必须留着这一条已打开的连接");
    assert!(!exec.contains("while result.next().await.map_err(|err| err.to_string())?.is_some() {}"));
    let truncated = mysql_run.split("if truncated").nth(1).expect("mysql truncated");
    assert!(!truncated.contains("while"), "截断之后不能把剩余行读完");
    let postgres = exec.split("async fn postgres_run").nth(1).unwrap();
    let postgres = postgres.split("pub fn clamp_limit").next().unwrap();
    assert!(postgres.contains("let bounded = cap_result_sql"));
    assert!(postgres.contains("prepare(&bounded)"));
    assert!(postgres.contains("simple_query(&bounded)"));
    assert!(postgres.contains("类型未知"));
    assert!(postgres.contains("if rows.len() >= limit"));
    assert!(!postgres.contains("query_raw"));
    assert!(!postgres.contains("pg_text"));
}

#[tokio::test]
async fn running_slot_is_held_before_any_await_so_a_later_use_cannot_start() {
    use super::commands::{start_run, QueryState};

    let dir = std::env::temp_dir().join(format!("rillsql-slot-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let service = notes_service(&dir).await;
    let state = QueryState::new(dir.join("query-history.sqlite"));
    let gate = state.lock_history_for_test().await;
    let state_task = state.clone();
    let service_task = Arc::clone(&service);
    let first = tokio::spawn(async move { start_run(&service_task, &state_task, run_request("SELECT 1", "main")).await });
    let mut reserved = false;
    for _ in 0..50 {
        if state.is_connection_busy("notes") {
            reserved = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(reserved, "历史 await 之前没有占住连接");
    let second = start_run(&service, &state, run_request("USE side", "main")).await.unwrap_err();
    assert!(second.contains("已有正在执行"), "{second}");
    drop(gate);
    let started = first.await.unwrap().unwrap();
    let done = wait_done(&state, &started.id).await;
    assert_eq!(done.status, "done");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn history_failure_releases_the_running_slot() {
    use super::commands::{start_run, QueryState};

    let dir = std::env::temp_dir().join(format!("rillsql-hist-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let service = notes_service(&dir).await;
    let blocked = QueryState::new(dir.clone());
    let err = start_run(&service, &blocked, run_request("SELECT 1", "main")).await.unwrap_err();
    assert!(!err.contains("已有正在执行"), "{err}");
    assert!(!blocked.is_connection_busy("notes"), "{err}");
    let again = start_run(&service, &blocked, run_request("SELECT 1", "main")).await.unwrap_err();
    assert!(!again.contains("已有正在执行"), "{again}");
    assert!(!blocked.is_connection_busy("notes"));

    let path = dir.join("query-history.sqlite");
    let state = QueryState::new(path.clone());
    state.history().await.unwrap();
    let other = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
        .await
        .unwrap();
    sqlx::query("DROP TABLE query_history").execute(&other).await.unwrap();
    other.close().await;
    let insert_err = start_run(&service, &state, run_request("SELECT 1", "main")).await.unwrap_err();
    assert!(!insert_err.contains("已有正在执行"), "{insert_err}");
    assert!(!state.is_connection_busy("notes"), "{insert_err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn failed_database_switch_releases_the_running_slot() {
    use super::commands::start_run;

    let dir = std::env::temp_dir().join(format!("rillsql-switch-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let service = notes_service(&dir).await;
    let state = super::commands::QueryState::new(dir.join("query-history.sqlite"));
    let err = start_run(&service, &state, run_request("SELECT 1", "other")).await.unwrap_err();
    assert!(err.contains("没有另开连接"), "{err}");
    assert!(!state.is_connection_busy("notes"), "{err}");
    let started = start_run(&service, &state, run_request("SELECT 1", "main")).await.unwrap();
    let done = wait_done(&state, &started.id).await;
    assert_eq!(done.status, "done");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mysql_batch_keeps_one_connection_and_stops_at_the_limit() {
    use mysql_async::prelude::Queryable;
    use tokio::net::TcpListener;

    use crate::connections::{CancelTransport, CatalogSession};

    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let stub = Arc::new(MysqlStub::default());
    let serve_stub = Arc::clone(&stub);
    let (shutdown, mut stop) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, _) = match accepted {
                        Ok(pair) => pair,
                        Err(_) => break,
                    };
                    serve_stub.accepts.fetch_add(1, Ordering::SeqCst);
                    let id = 40 + serve_stub.accepts.load(Ordering::SeqCst) as u32;
                    let stub = Arc::clone(&serve_stub);
                    tokio::spawn(async move {
                        if let Err(err) = handle_mysql(stream, id, stub.clone()).await {
                            stub.failures.lock().expect("failures").push(err);
                        }
                    });
                }
                _ = &mut stop => break,
            }
        }
    });
    let opts = mysql_async::OptsBuilder::default()
        .ip_or_hostname("127.0.0.1")
        .tcp_port(port)
        .prefer_socket(false)
        .user(Some("root"))
        .pass(Some(String::new()))
        .max_allowed_packet(Some(16 * 1024 * 1024))
        .wait_timeout(Some(28_800))
        .pool_opts(
            mysql_async::PoolOpts::new()
                .with_constraints(mysql_async::PoolConstraints::new(0, 1).unwrap())
                .with_reset_connection(true)
                .with_inactive_connection_ttl(Duration::from_secs(28_800)),
        );
    let pool = mysql_async::Pool::new(mysql_async::Opts::from(opts));
    let driver = exec::LiveDriver::attach(CatalogSession::Mysql(pool.clone()), "mysql", "app", CancelTransport::None)
        .await
        .unwrap_or_else(|err| panic!("{err} queries={:?} failures={:?}", stub.queries.lock().unwrap(), stub.failures.lock().unwrap()));
    let flag = Arc::new(AtomicBool::new(false));
    let report = run_batch(
        &[piece("SET @keep = 'stay'"), piece("SELECT @keep"), piece("SELECT CONNECTION_ID()")],
        10,
        &flag,
        false,
        &driver,
    )
    .await;
    assert_eq!(report.status, "done", "{:?}", report.statements);
    assert_eq!(report.statements[1].rows[0][0].as_deref(), Some("stay"));
    let first_id = report.statements[2].rows[0][0].clone();
    drop(driver);
    let mut again = pool.get_conn().await.unwrap();
    again.reset_connection(false);
    let kept: Option<Option<String>> = again.query_first("SELECT @keep").await.unwrap();
    let same: Option<Option<String>> = again.query_first("SELECT CONNECTION_ID()").await.unwrap();
    let queries = stub.queries.lock().expect("queries").clone();
    let failures = stub.failures.lock().expect("failures").clone();
    assert_eq!(stub.resets.load(Ordering::SeqCst), 0, "归还连接时执行了 COM_RESET_CONNECTION queries={queries:?} failures={failures:?}");
    assert_eq!(stub.accepts.load(Ordering::SeqCst), 1, "执行为同一批另开了连接 queries={queries:?} failures={failures:?}");
    assert_eq!(kept.as_ref().map(Option::as_deref), Some(Some("stay")), "queries={queries:?} failures={failures:?}");
    assert_eq!(same.flatten(), first_id, "queries={queries:?}");
    drop(again);
    assert_eq!(stub.resets.load(Ordering::SeqCst), 0);
    assert_eq!(stub.accepts.load(Ordering::SeqCst), 1);

    let driver = exec::LiveDriver::attach(CatalogSession::Mysql(pool.clone()), "mariadb", "app", CancelTransport::None)
        .await
        .unwrap();
    let limited = run_batch(&[piece("SELECT * FROM big")], 2, &flag, false, &driver).await;
    assert_eq!(limited.statements[0].rows.len(), 2, "{:?}", limited.statements);
    assert!(limited.statements[0].truncated);
    assert_eq!(stub.rows_sent.load(Ordering::SeqCst), 3);
    assert!(!stub.uncapped.load(Ordering::SeqCst));
    let queries = stub.queries.lock().expect("queries").clone();
    assert!(queries.iter().any(|sql| sql.to_ascii_uppercase().contains("LIMIT 3")), "{queries:?}");
    drop(driver);
    assert_eq!(stub.resets.load(Ordering::SeqCst), 0);
    assert_eq!(stub.accepts.load(Ordering::SeqCst), 1, "MariaDB 批次不能再拨一条连接");
    let _ = tokio::time::timeout(Duration::from_secs(2), pool.disconnect()).await;
    let _ = shutdown.send(());
}

#[derive(Default)]
struct MysqlStub {
    accepts: AtomicUsize,
    resets: AtomicUsize,
    rows_sent: AtomicUsize,
    uncapped: AtomicBool,
    queries: Mutex<Vec<String>>,
    failures: Mutex<Vec<String>>,
}

async fn handle_mysql(mut stream: tokio::net::TcpStream, id: u32, stub: Arc<MysqlStub>) -> Result<(), String> {
    write_packet(&mut stream, 0, &mysql_handshake(id)).await?;
    let (seq, _) = read_packet(&mut stream).await?;
    write_packet(&mut stream, seq.wrapping_add(1), &mysql_ok()).await?;
    let mut database = "app".to_owned();
    let mut vars: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    loop {
        let (seq, payload) = match read_packet(&mut stream).await {
            Ok(packet) => packet,
            Err(_) => break,
        };
        if payload.is_empty() {
            break;
        }
        let mut reply = seq.wrapping_add(1);
        match payload[0] {
            0x01 => break,
            0x0e => write_packet(&mut stream, reply, &mysql_ok()).await?,
            0x1f => {
                stub.resets.fetch_add(1, Ordering::SeqCst);
                database = "app".to_owned();
                vars.clear();
                write_packet(&mut stream, reply, &mysql_ok()).await?;
            }
            0x03 => {
                let sql = String::from_utf8_lossy(&payload[1..]).trim().trim_end_matches(';').trim().to_owned();
                stub.queries.lock().expect("queries").push(sql.clone());
                let upper = sql.to_ascii_uppercase();
                if upper.contains("CONNECTION_ID") {
                    write_text_rows(&mut stream, &mut reply, &[Some(id.to_string())]).await?;
                } else if upper.contains("DATABASE()") {
                    write_text_rows(&mut stream, &mut reply, &[Some(database.clone())]).await?;
                } else if upper.starts_with("USE") {
                    database = upper.trim_start_matches("USE").trim().trim_matches('`').to_owned();
                    write_packet(&mut stream, reply, &mysql_ok()).await?;
                } else if upper.starts_with("SET") {
                    if let Some((name, value)) = mysql_set_var(&sql) {
                        vars.insert(name, value);
                    }
                    write_packet(&mut stream, reply, &mysql_ok()).await?;
                } else if upper.starts_with("SELECT") && sql.contains('@') {
                    let name = sql.split('@').nth(1).unwrap_or("").split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_').next().unwrap_or("");
                    write_text_rows(&mut stream, &mut reply, &[vars.get(name).cloned()]).await?;
                } else if upper.starts_with("SELECT") {
                    let count = mysql_limit(&upper).unwrap_or_else(|| {
                        stub.uncapped.store(true, Ordering::SeqCst);
                        5
                    });
                    stub.rows_sent.fetch_add(count, Ordering::SeqCst);
                    let rows = (0..count).map(|index| Some(index.to_string())).collect::<Vec<_>>();
                    write_text_rows(&mut stream, &mut reply, &rows).await?;
                } else {
                    write_packet(&mut stream, reply, &mysql_ok()).await?;
                }
            }
            other => return Err(format!("未识别的命令 {other:#x}")),
        }
    }
    Ok(())
}

fn mysql_set_var(sql: &str) -> Option<(String, String)> {
    let at = sql.find('@')?;
    let rest = &sql[at + 1..];
    let name: String = rest.chars().take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_').collect();
    let value = sql.split('=').nth(1)?.trim().trim_matches('\'').trim_matches('"').to_owned();
    if name.is_empty() { None } else { Some((name, value)) }
}

fn mysql_limit(upper: &str) -> Option<usize> {
    let at = upper.rfind("LIMIT")?;
    let rest = upper[at + 5..].trim();
    let mut parts = rest.split(|ch: char| !ch.is_ascii_digit()).filter(|part| !part.is_empty());
    let first = parts.next()?.parse::<usize>().ok()?;
    Some(parts.next().and_then(|part| part.parse().ok()).unwrap_or(first))
}

fn mysql_handshake(connection_id: u32) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.push(10);
    payload.extend_from_slice(b"8.0.36\0");
    payload.extend_from_slice(&connection_id.to_le_bytes());
    payload.extend_from_slice(&[0u8; 8]);
    payload.push(0);
    let caps: u32 = 0x010F_A281;
    payload.extend_from_slice(&(caps as u16).to_le_bytes());
    payload.push(45);
    payload.extend_from_slice(&2u16.to_le_bytes());
    payload.extend_from_slice(&((caps >> 16) as u16).to_le_bytes());
    payload.push(21);
    payload.extend_from_slice(&[0u8; 10]);
    payload.extend_from_slice(&[0u8; 13]);
    payload.extend_from_slice(b"mysql_native_password\0");
    payload
}

fn mysql_ok() -> Vec<u8> {
    vec![0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00]
}

fn mysql_column(name: &str) -> Vec<u8> {
    let mut payload = vec![0x03, b'd', b'e', b'f', 0x00, 0x00, 0x00];
    payload.push(name.len() as u8);
    payload.extend_from_slice(name.as_bytes());
    payload.push(0x00);
    payload.push(0x0c);
    payload.extend_from_slice(&45u16.to_le_bytes());
    payload.extend_from_slice(&1024u32.to_le_bytes());
    payload.push(0xfd);
    payload.extend_from_slice(&0u16.to_le_bytes());
    payload.push(0);
    payload.extend_from_slice(&[0, 0]);
    payload
}

async fn write_text_rows(stream: &mut tokio::net::TcpStream, seq: &mut u8, rows: &[Option<String>]) -> Result<(), String> {
    write_packet(stream, *seq, &[1]).await?;
    *seq = seq.wrapping_add(1);
    write_packet(stream, *seq, &mysql_column("v")).await?;
    *seq = seq.wrapping_add(1);
    for row in rows {
        let payload = match row {
            None => vec![0xfb],
            Some(text) => {
                let bytes = text.as_bytes();
                let mut payload = Vec::with_capacity(bytes.len() + 1);
                payload.push(bytes.len() as u8);
                payload.extend_from_slice(bytes);
                payload
            }
        };
        write_packet(stream, *seq, &payload).await?;
        *seq = seq.wrapping_add(1);
    }
    write_packet(stream, *seq, &[0xfe, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00]).await?;
    Ok(())
}

async fn write_packet(stream: &mut tokio::net::TcpStream, seq: u8, payload: &[u8]) -> Result<(), String> {
    let len = payload.len() as u32;
    let header = [(len & 0xff) as u8, ((len >> 8) & 0xff) as u8, ((len >> 16) & 0xff) as u8, seq];
    stream.write_all(&header).await.map_err(|err| err.to_string())?;
    stream.write_all(payload).await.map_err(|err| err.to_string())?;
    Ok(())
}

async fn read_packet(stream: &mut tokio::net::TcpStream) -> Result<(u8, Vec<u8>), String> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await.map_err(|err| err.to_string())?;
    let len = u32::from_le_bytes([header[0], header[1], header[2], 0]) as usize;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await.map_err(|err| err.to_string())?;
    Ok((header[3], payload))
}

async fn notes_service(dir: &std::path::Path) -> Arc<crate::connections::Service> {
    use crate::connections::Service;

    let database = dir.join("notes.sqlite");
    let setup = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
        .execute(&setup)
        .await
        .unwrap();
    setup.close().await;
    let service = Arc::new(Service::new_for_test(dir.join("connections.sqlite")));
    service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
    service
}

fn run_request(sql: &str, database: &str) -> super::commands::StartRequest {
    super::commands::StartRequest {
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: database.to_owned(),
        engine: "sqlite".to_owned(),
        sql: sql.to_owned(),
        limit: 10,
    }
}

async fn wait_done(state: &super::commands::QueryState, id: &str) -> super::commands::RunDto {
    for _ in 0..200 {
        let snap = super::commands::poll_run(state, id, false).await.unwrap();
        if !matches!(snap.status.as_str(), "running" | "cancel-requested") {
            return snap;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("执行没有结束");
}
