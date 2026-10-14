use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
        rows: Vec::new(),
        truncated: false,
        affected: Some(1),
    }
}

#[test]
fn split_keeps_quotes_comments_and_current_statement() {
    let sql = "SELECT 'a;b';\n-- c;\nUPDATE t SET n = 1;\nSELECT $$do;$$;";
    let parts = split::split_sql(sql);
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].text, "SELECT 'a;b';");
    assert_eq!(parts[1].text, "UPDATE t SET n = 1;");
    assert!(parts[2].text.contains("$$do;$$"));
    let current = split::statement_at(sql, parts[1].start + 2).expect("光标所在语句");
    assert_eq!(current.text, "UPDATE t SET n = 1;");
    assert!(split::is_write(&current.text));
    assert!(!split::is_write("SELECT 1"));
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
    assert!(reused.message.contains("没有另开通道"));
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
    for command in ["query_start", "query_poll", "query_cancel", "query_history", "query_history_clear"] {
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
    let check = exec::LiveDriver {
        session,
        transport: service.cancel_transport("notes").unwrap(),
    };
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

async fn wait_done(state: &super::commands::QueryState, id: &str) -> super::commands::RunDto {
    for _ in 0..200 {
        let snap = super::commands::poll_run(state, id).unwrap();
        if !matches!(snap.status.as_str(), "running" | "cancel-requested") {
            return snap;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("执行没有结束");
}
