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
    async fn run(&self, sql: &str, _limit: usize, _cancel: &Arc<AtomicBool>) -> exec::DriverReport {
        self.calls.lock().expect("calls").push(sql.to_owned());
        exec::DriverReport::unchanged(sql, self.steps.lock().expect("steps").remove(0))
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

/// 启动页“再次执行”的门：只有每条语句都明显只读才自动重放，其余一律按写入。
#[test]
fn clearly_read_only_is_conservative_about_hidden_writes() {
    for (engine, sql) in [
        ("postgres", "SELECT 1"),
        ("postgres", "select * from orders where note = 'delete me';"),
        ("postgres", "WITH a AS (SELECT 1 AS n) SELECT n FROM a"),
        ("postgres", "SELECT \"update\" FROM t -- insert later"),
        ("mysql", "SHOW TABLES; DESCRIBE t; EXPLAIN SELECT 1;"),
        ("sqlserver", "SELECT [delete] FROM t ORDER BY id DESC"),
        ("sqlserver", "SELECT TOP 10 a.id, b.[kill] FROM a LEFT JOIN b ON a.id = b.id WHERE a.n IS NOT NULL ORDER BY a.id"),
        ("mysql", "SELECT /*! SQL_NO_CACHE */ * FROM orders /* into nothing */"),
        ("mariadb", "SELECT /*M!100100 1 AS */ n FROM t"),
    ] {
        assert!(split::is_clearly_read_only(engine, sql), "{engine}: {sql} 应当是只读");
    }
    for (engine, sql) in [
        ("postgres", "WITH d AS (DELETE FROM orders WHERE id = 9 RETURNING *) SELECT * FROM d"),
        ("postgres", "WITH u AS (UPDATE t SET n = 1 RETURNING *) SELECT count(*) FROM u"),
        ("mysql", "WITH c AS (SELECT id FROM t) DELETE FROM t WHERE id IN (SELECT id FROM c)"),
        ("sqlserver", "EXEC dbo.purge_orders"),
        ("sqlserver", "EXECUTE dbo.purge_orders @days = 3"),
        ("sqlserver", "SELECT * INTO new_table FROM orders"),
        ("mysql", "RENAME TABLE a TO b"),
        ("postgres", "COMMENT ON TABLE t IS 'x'"),
        ("postgres", "EXPLAIN ANALYZE DELETE FROM t"),
        ("sqlite", "PRAGMA journal_mode = WAL"),
        ("mysql", "SET @x = 1"),
        ("postgres", "SELECT 1; DELETE FROM t;"),
        ("postgres", "SELECT * FROM t FOR UPDATE"),
        ("postgres", "SELECT 'unterminated"),
        ("postgres", "   -- only a comment"),
        // T-SQL 批次里无分号相接的写入/运维语句。
        ("sqlserver", "SELECT * FROM orders\nDBCC CHECKIDENT ('orders', RESEED, 0)"),
        ("sqlserver", "SELECT 1 KILL 53"),
        ("sqlserver", "SELECT 1 RESTORE DATABASE x FROM DISK='/tmp/x.bak'"),
        ("sqlserver", "SELECT 1\nBACKUP DATABASE x TO DISK='/tmp/x.bak'"),
        ("sqlserver", "SELECT 1 DENY SELECT ON t TO u"),
        ("sqlserver", "SELECT 1 SHUTDOWN"),
        ("sqlserver", "SELECT 1 RECONFIGURE"),
        ("sqlserver", "SELECT 1 CHECKPOINT"),
        ("sqlserver", "SELECT 1 DISABLE TRIGGER trg ON t"),
        ("sqlserver", "SELECT 1 ENABLE TRIGGER trg ON t"),
        ("sqlserver", "SELECT 1 RECEIVE TOP (1) * FROM q"),
        ("sqlserver", "SELECT 1 WAITFOR DELAY '00:00:05'"),
        // MySQL/MariaDB 会执行 /*! ... */ 里的代码。
        ("mysql", "SELECT * FROM orders /*!50000 INTO OUTFILE '/tmp/o.csv' */"),
        ("mysql", "SELECT * FROM orders /*! INTO OUTFILE '/tmp/o.csv' */"),
        ("mariadb", "SELECT * FROM orders /*!50000 INTO OUTFILE '/tmp/o.csv' */"),
        ("mariadb", "SELECT * FROM orders /*M!100100 INTO OUTFILE '/tmp/o.csv' */"),
    ] {
        assert!(!split::is_clearly_read_only(engine, sql), "{engine}: {sql} 应当按写入处理");
    }
}

/// 评审复现（A2 回归）：code_words 曾硬编码“单引号里反斜杠转义”，与 split_sql 的适配器词法不一致。
/// MySQL 双引号字符串里的 \" 让 INTO OUTFILE 被当成字符串内容，写文件语句被判只读并在启动页自动重放；
/// PG 则反过来把 'a\' 当成未闭合，SELECT INTO 被藏进字符串。现在 code_words 与 split_sql 同一规则，
/// 并且字符串里出现读法取决于设置的反斜杠时一律按写入处理。
#[test]
fn clearly_read_only_follows_adapter_backslash_rules() {
    for (engine, sql) in [
        ("mysql", "SELECT \"a\\\"\", 1 INTO OUTFILE \"/tmp/f\""),
        ("mariadb", "SELECT \"a\\\"\", 1 INTO OUTFILE \"/tmp/f\""),
        ("mysql", "SELECT 'a\\' INTO OUTFILE '/tmp/x' -- '"),
        ("mysql", "SELECT 'it\\'s' FROM t"),
        ("postgres", "SELECT 'a\\', 1 INTO t FROM x -- '"),
        ("postgres", "SELECT E'a\\', 1 FROM x -- ' INTO t"),
        ("postgres", "SELECT 'C:\\tmp' FROM t"),
    ] {
        assert!(!split::is_clearly_read_only(engine, sql), "{engine}: {sql} 应当按写入处理");
    }
    // 不认反斜杠的引擎、反引号标识符与双写引号不受影响。
    for (engine, sql) in [
        ("sqlite", "SELECT 'C:\\tmp' FROM t"),
        ("sqlserver", "SELECT 'C:\\tmp' FROM t"),
        ("mysql", "SELECT `a\\b` FROM t"),
        ("mysql", "SELECT 'it''s' FROM t"),
        ("postgres", "SELECT 'it''s' FROM t"),
    ] {
        assert!(split::is_clearly_read_only(engine, sql), "{engine}: {sql} 应当是只读");
    }
    // 词法一致：MySQL 的 \" 不结束双引号字符串，INTO 在代码里可见。
    let (words, _) = split::top_level_words("mysql", "SELECT \"a\\\"\", 1 INTO OUTFILE \"/tmp/f\"");
    assert!(words.iter().any(|(word, _, _)| word == "INTO"), "{words:?}");
    let (words, _) = split::top_level_words("postgres", "SELECT 'a\\', 1 INTO t FROM x -- '");
    assert!(words.iter().any(|(word, _, _)| word == "INTO"), "{words:?}");
    assert!(split::has_mode_dependent_escape("mysql", "SELECT 'a\\'''"));
    assert!(!split::has_mode_dependent_escape("mysql", "SELECT 1 -- a\\b"));
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

    let only_delimiter = split::split_sql("postgres", "DELIMITER $$");
    assert!(only_delimiter.pieces.is_empty());
    assert!(only_delimiter.notices[0].contains("不使用 DELIMITER"));
    let definer = "CREATE DEFINER=root PROCEDURE p()\nBEGIN\n  UPDATE t SET n = 1;\n  UPDATE t SET n = 2;\nEND";
    let kept = split::split_sql("mysql", definer);
    assert!(kept.refusal.is_none(), "{:?}", kept.refusal);
    assert_eq!(kept.pieces.len(), 1, "{:?}", kept.pieces.iter().map(|piece| &piece.text).collect::<Vec<_>>());
    assert!(kept.pieces[0].text.contains("UPDATE t SET n = 2;"));
    let host = "CREATE DEFINER=root@localhost FUNCTION f() BEGIN SELECT 1; SELECT 2; END";
    assert_eq!(split::split_sql("mariadb", host).pieces.len(), 1);
    let quoted = "CREATE DEFINER=`root`@`localhost` TRIGGER tr BEFORE INSERT ON t FOR EACH ROW BEGIN SET NEW.n = 1; END";
    assert_eq!(split::split_sql("mysql", quoted).pieces.len(), 1);
    let open_tail = "DELETE FROM t;\nSELECT 'open";
    assert!(split::statement_at("postgres", open_tail, open_tail.len()).is_none());
    assert_eq!(split::statement_at("postgres", open_tail, 0).expect("closed statement").text, "DELETE FROM t;");
}

#[tokio::test]
async fn sqlite_delimiter_without_statements_keeps_the_client_notice() {
    use super::commands::{start_run, QueryState, StartRequest};

    let dir = std::env::temp_dir().join(format!("rillsql-delim-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let service = Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")));
    let state = QueryState::new(dir.join("history.sqlite"));
    let err = start_run(
        &service,
        &state,
        StartRequest {
            connection_id: "notes".to_owned(),
            connection_name: "本地笔记".to_owned(),
            database: "main".to_owned(),
            engine: "sqlite".to_owned(),
            sql: "DELIMITER $$".to_owned(),
            limit: 100,
            timeout_ms: None,
        },
    )
    .await
    .expect_err("只有客户端指令");
    assert!(err.contains("不使用 DELIMITER"), "{err}");
    assert!(!err.contains("只有空白或注释"), "{err}");
    let blank = start_run(
        &service,
        &state,
        StartRequest {
            connection_id: "notes".to_owned(),
            connection_name: "本地笔记".to_owned(),
            database: "main".to_owned(),
            engine: "sqlite".to_owned(),
            sql: "  -- note\n".to_owned(),
            limit: 100,
            timeout_ms: None,
        },
    )
    .await
    .expect_err("空白");
    assert!(blank.contains("只有空白或注释"), "{blank}");
    let _ = std::fs::remove_dir_all(&dir);
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

/// 事务控制语句的断连不是“语句被拒绝”：COMMIT/ROLLBACK 可能已在服务器执行，必须保留未知。
#[test]
fn disconnected_transaction_controls_are_unknown_not_ordinary_errors() {
    for sql in [
        "COMMIT",
        "COMMIT;",
        "  -- note\nROLLBACK",
        "ROLLBACK TRANSACTION",
        "SAVEPOINT sp1",
        "RELEASE SAVEPOINT sp1",
        "BEGIN",
        "START TRANSACTION",
        "END",
    ] {
        let stop = classify_failure(sql, "connection closed".to_owned(), false);
        assert!(matches!(stop, StatementStop::Unknown { .. }), "{sql} 断连应保留未知，实际 {stop:?}");
        assert!(split::is_transaction_control(sql), "{sql} 应识别为事务控制语句");
    }
    for sql in ["SELECT 1", "SHOW TABLES", "UPDATE t SET n = 1", "INSERT INTO t VALUES (1)"] {
        assert!(!split::is_transaction_control(sql), "{sql} 不是事务控制语句");
    }
    // 只读语句与普通语法错误仍然按普通失败处理，不能扩大未知范围。
    let read = classify_failure("SELECT 1", "connection closed".to_owned(), false);
    assert!(matches!(read, StatementStop::Error { .. }));
    let syntax = classify_failure("COMMIT", "You have an error in your SQL syntax".to_owned(), false);
    assert!(matches!(syntax, StatementStop::Error { .. }));
}

#[test]
fn timeout_statements_match_each_engine_and_none_disables_them() {
    assert_eq!(
        exec::timeout_statements("postgres", Some(30_000)),
        vec!["SET statement_timeout = 30000".to_owned()]
    );
    assert_eq!(
        exec::timeout_statements("mysql", Some(30_000)),
        vec!["SET SESSION MAX_EXECUTION_TIME = 30000".to_owned()]
    );
    assert_eq!(
        exec::timeout_statements("mariadb", Some(30_000)),
        vec!["SET SESSION max_statement_time = 30".to_owned()]
    );
    assert!(exec::timeout_statements("sqlite", Some(30_000)).is_empty());
    assert!(exec::timeout_statements("postgres", None).is_empty());
    assert!(exec::timeout_statements("postgres", Some(0)).is_empty());
    assert_eq!(
        exec::timeout_reset_statements("postgres"),
        vec!["SET statement_timeout = 0".to_owned()]
    );
    assert_eq!(
        exec::timeout_reset_statements("mysql"),
        vec!["SET SESSION MAX_EXECUTION_TIME = 0".to_owned()]
    );
    assert_eq!(
        exec::timeout_reset_statements("mariadb"),
        vec!["SET SESSION max_statement_time = 0".to_owned()]
    );
    assert!(exec::timeout_reset_statements("sqlite").is_empty());
}

#[test]
fn statement_timeouts_report_as_error_instead_of_unknown() {
    for message in [
        "canceling statement due to statement timeout",
        "Query execution was interrupted, maximum statement execution time exceeded",
        "Query execution was interrupted (max_statement_time exceeded)",
        "max_execution_time exceeded",
    ] {
        let stop = classify_failure("UPDATE t SET n = 1", message.to_owned(), false);
        match stop {
            StatementStop::Error { message: text, .. } => assert!(text.starts_with("执行超时，语句已停止。"), "{text}"),
            other => panic!("语句超时不能落到 {other:?}：{message}"),
        }
    }
    let unknown = classify_failure("UPDATE t SET n = 1", "connection reset by peer".to_owned(), false);
    assert!(matches!(unknown, StatementStop::Unknown { .. }));
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
    let report = run_batch(&parts, 100, &flag, false, &driver, None).await;
    assert_eq!(report.status, "unknown");
    assert_eq!(report.calls, 2);
    assert_eq!(report.statements[0].status, "ok");
    assert_eq!(report.statements[1].status, "unknown");
    assert_eq!(report.statements[2].status, "skipped");
    assert_eq!(driver.calls.lock().expect("calls").iter().filter(|sql| sql.starts_with("UPDATE")).count(), 1);
}

#[tokio::test]
async fn a_statement_error_skips_every_later_statement() {
    let driver = Scripted {
        steps: Mutex::new(vec![
            Ok(ok_count()),
            Err(StatementStop::Error { message: "no such table: missing".to_owned(), at: None, code: None }),
            Ok(ok_count()),
        ]),
        calls: Mutex::new(Vec::new()),
    };
    let flag = Arc::new(AtomicBool::new(false));
    let parts = vec![piece("INSERT INTO t VALUES (1)"), piece("SELECT * FROM missing"), piece("DELETE FROM t")];
    let report = run_batch(&parts, 100, &flag, false, &driver, None).await;
    assert_eq!(report.status, "error");
    assert_eq!(report.calls, 2, "失败之后不再发送");
    assert_eq!(report.statements[0].status, "ok");
    assert_eq!(report.statements[1].status, "error");
    assert_eq!(report.statements[1].message, "no such table: missing");
    assert_eq!(report.statements[2].status, "skipped");
    assert_eq!(driver.calls.lock().expect("calls").len(), 2);
}

/// 关系型连接的协调器：方言来自适配器，测试不伪造事务语义。
fn coordinator(engine: crate::connections::model::Engine) -> crate::connections::transaction::TransactionCoordinator {
    use crate::connections::transaction::TransactionCoordinator;
    TransactionCoordinator::new("notes", crate::adapters::adapter(engine).transaction())
}

fn scripted(steps: Vec<Result<StatementValue, StatementStop>>) -> Scripted {
    Scripted {
        steps: Mutex::new(steps),
        calls: Mutex::new(Vec::new()),
    }
}

#[tokio::test]
async fn handwritten_controls_share_the_connection_transaction() {
    use crate::connections::model::Engine;
    use crate::transaction::{TransactionEffect, TransactionState};

    let transactions = coordinator(Engine::Postgres);
    let driver = scripted(vec![Ok(ok_count()), Ok(ok_count()), Ok(ok_count())]);
    let flag = Arc::new(AtomicBool::new(false));
    let parts = vec![piece("BEGIN"), piece("UPDATE item SET n = 1"), piece("COMMIT")];
    let report = run_batch(&parts, 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.status, "done");
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::Began));
    assert_eq!(report.statements[1].transaction, Some(TransactionEffect::StillActive));
    assert_eq!(report.statements[2].transaction, Some(TransactionEffect::Committed));
    assert_eq!(report.transaction_effect, Some(TransactionEffect::Committed));
    // 批次携带的是协调器里的同一份共享快照，不是这一批自己的副本。
    let snapshot = report.transaction.expect("关系型批次要带事务快照");
    assert_eq!(snapshot.state, TransactionState::Idle);
    assert_eq!(Some(snapshot), transactions.snapshot(), "批次快照与协调器共享");
    assert_eq!(driver.calls.lock().expect("calls").as_slice(), ["BEGIN", "UPDATE item SET n = 1", "COMMIT"]);
}

#[tokio::test]
async fn refusals_never_reach_the_database() {
    use crate::connections::model::Engine;

    let flag = Arc::new(AtomicBool::new(false));
    // 没有活动事务时提交：明确失败，不伪造成功结果（R3）。
    let transactions = coordinator(Engine::Mysql);
    let driver = scripted(vec![Ok(ok_count())]);
    let report = run_batch(&[piece("COMMIT")], 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.status, "error");
    assert_eq!(report.calls, 0);
    assert_eq!(report.statements[0].status, "error");
    assert!(report.statements[0].message.contains("没有活动事务"), "{}", report.statements[0].message);
    assert!(report.statements[0].sent_sql.is_empty());
    assert!(driver.calls.lock().expect("calls").is_empty(), "预检拒绝的控制语句不能下发");

    // 活动事务里再次开始：PostgreSQL 只会忽略，界面必须引导到保存点（R3）。
    let transactions = coordinator(Engine::Postgres);
    transactions
        .acknowledge(
            &crate::transaction::ControlStatement::Begin { mode: None },
            crate::transaction::ControlOutcome::Confirmed,
        )
        .unwrap();
    let driver = scripted(vec![Ok(ok_count())]);
    let report = run_batch(&[piece("BEGIN")], 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.statements[0].status, "error");
    assert!(report.statements[0].message.contains("保存点"), "{}", report.statements[0].message);
    assert!(driver.calls.lock().expect("calls").is_empty());
}

#[tokio::test]
async fn a_failed_statement_aborts_the_postgres_transaction_and_skips_the_rest() {
    use crate::connections::model::Engine;
    use crate::transaction::{ControlOutcome, ControlStatement, TransactionEffect, TransactionState};

    let transactions = coordinator(Engine::Postgres);
    transactions.acknowledge(&ControlStatement::Begin { mode: None }, ControlOutcome::Confirmed).unwrap();
    let driver = scripted(vec![
        Ok(ok_count()),
        Err(StatementStop::Error { message: "relation \"missing\" does not exist".to_owned(), at: None, code: Some("42P01".to_owned()) }),
        Ok(ok_count()),
    ]);
    let flag = Arc::new(AtomicBool::new(false));
    let parts = vec![piece("UPDATE item SET n = 1"), piece("UPDATE missing SET n = 1"), piece("SELECT 1")];
    let report = run_batch(&parts, 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.status, "error");
    assert_eq!(report.statements[1].status, "error");
    assert_eq!(report.statements[1].transaction, Some(TransactionEffect::Aborted));
    assert!(report.statements[1].message.contains("只能回滚"), "{}", report.statements[1].message);
    assert_eq!(report.statements[2].status, "skipped");
    assert_eq!(report.statements[2].message, "事务已中止，未执行");
    assert_eq!(report.calls, 2, "中止之后不再发送");
    assert_eq!(transactions.snapshot().unwrap().state, TransactionState::Aborted);
}

#[tokio::test]
async fn mysql_and_mariadb_ddl_report_the_implicit_commit_and_keep_running() {
    use crate::connections::model::Engine;
    use crate::transaction::{ControlOutcome, ControlStatement, TransactionEffect, TransactionState};

    for engine in [Engine::Mysql, Engine::Mariadb] {
        let transactions = coordinator(engine);
        transactions.acknowledge(&ControlStatement::Begin { mode: None }, ControlOutcome::Confirmed).unwrap();
        let driver = scripted(vec![Ok(ok_count()), Ok(ok_count())]);
        let flag = Arc::new(AtomicBool::new(false));
        let parts = vec![piece("ALTER TABLE item ADD COLUMN note TEXT"), piece("INSERT INTO item (n) VALUES (1)")];
        let report = run_batch(&parts, 100, &flag, false, &driver, Some(&transactions)).await;
        assert_eq!(report.status, "done", "隐式提交不阻断这一批的后续语句");
        assert_eq!(report.statements[0].status, "ok");
        assert_eq!(report.statements[0].transaction, Some(TransactionEffect::ImplicitCommit), "{engine:?}");
        assert!(report.statements[0].message.contains("隐式提交"), "{}", report.statements[0].message);
        // 隐式提交之后这条语句又回到自动提交：不再宣称事务仍然活动。
        assert_eq!(report.statements[1].transaction, Some(TransactionEffect::Unchanged));
        assert_eq!(report.transaction_effect, Some(TransactionEffect::ImplicitCommit));
        assert_eq!(transactions.snapshot().unwrap().state, TransactionState::Idle);
    }
}

#[tokio::test]
async fn a_second_begin_on_mysql_reports_the_implicit_commit_instead_of_nesting() {
    use crate::connections::model::Engine;
    use crate::transaction::{ControlOutcome, ControlStatement, TransactionEffect, TransactionState};

    let transactions = coordinator(Engine::Mysql);
    transactions.acknowledge(&ControlStatement::Begin { mode: None }, ControlOutcome::Confirmed).unwrap();
    let driver = scripted(vec![Ok(ok_count())]);
    let flag = Arc::new(AtomicBool::new(false));
    let report = run_batch(&[piece("BEGIN")], 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.statements[0].status, "ok");
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::ImplicitCommit));
    assert!(report.statements[0].message.contains("隐式提交"), "{}", report.statements[0].message);
    let snapshot = transactions.snapshot().unwrap();
    assert_eq!(snapshot.state, TransactionState::Active, "新事务已经活动，不伪造嵌套层级");
    assert_eq!(snapshot.depth, Some(1));
}

#[tokio::test]
async fn an_unclassified_mysql_statement_marks_the_transaction_unknown_and_stops() {
    use crate::connections::model::Engine;
    use crate::transaction::{ControlOutcome, ControlStatement, TransactionEffect, TransactionState};

    let transactions = coordinator(Engine::Mysql);
    transactions.acknowledge(&ControlStatement::Begin { mode: None }, ControlOutcome::Confirmed).unwrap();
    let driver = scripted(vec![Ok(ok_count()), Ok(ok_count())]);
    let flag = Arc::new(AtomicBool::new(false));
    let report = run_batch(&[piece("CALL rotate_notes()"), piece("SELECT 1")], 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.statements[0].status, "ok", "语句本身成功");
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::Unknown));
    assert!(report.statements[0].message.contains("无法确认"), "{}", report.statements[0].message);
    assert_eq!(report.statements[1].status, "skipped");
    assert_eq!(report.status, "unknown");
    assert_eq!(report.calls, 1);
    assert_eq!(transactions.snapshot().unwrap().state, TransactionState::Unknown);
}

#[tokio::test]
async fn an_unclassified_statement_in_autocommit_leaves_no_unknown_transaction() {
    use crate::connections::model::Engine;
    use crate::transaction::TransactionEffect;

    // 自动提交下没有事务可被隐式提交或丢确认：不能因为语句未分类就留下未知状态。
    let transactions = coordinator(Engine::Mysql);
    let driver = scripted(vec![Ok(ok_count())]);
    let flag = Arc::new(AtomicBool::new(false));
    let report = run_batch(&[piece("CALL rotate_notes()")], 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.status, "done");
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::Unchanged));
    assert!(report.statements[0].message.is_empty(), "{}", report.statements[0].message);
    assert_eq!(report.transaction_effect, None);
    assert!(transactions.snapshot().unwrap().unresolved.is_none());
}

#[tokio::test]
async fn a_cancelled_statement_keeps_a_usable_mysql_transaction() {
    use crate::connections::model::Engine;
    use crate::transaction::{ControlOutcome, ControlStatement, TransactionEffect, TransactionState};

    let transactions = coordinator(Engine::Mysql);
    transactions.acknowledge(&ControlStatement::Begin { mode: None }, ControlOutcome::Confirmed).unwrap();
    let driver = scripted(vec![Err(StatementStop::Cancelled { message: "Query execution was interrupted".to_owned() })]);
    let flag = Arc::new(AtomicBool::new(false));
    let report = run_batch(&[piece("UPDATE item SET n = 1"), piece("DELETE FROM item")], 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.status, "cancelled");
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::StillActive), "取消不等于回滚");
    assert_eq!(report.statements[1].status, "skipped");
    assert_eq!(transactions.snapshot().unwrap().state, TransactionState::Active);

    // PostgreSQL 的取消会让事务中止：同样按数据库真实状态报告，而不是替用户回滚。
    let transactions = coordinator(Engine::Postgres);
    transactions.acknowledge(&ControlStatement::Begin { mode: None }, ControlOutcome::Confirmed).unwrap();
    let driver = scripted(vec![Err(StatementStop::Cancelled {
        message: "canceling statement due to user request".to_owned(),
    })]);
    let report = run_batch(&[piece("SELECT pg_sleep(10)")], 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::Aborted));
    assert_eq!(transactions.snapshot().unwrap().state, TransactionState::Aborted, "取消后的中止由数据库决定，不自动回滚");
}

#[tokio::test]
async fn a_disconnect_in_a_transaction_keeps_the_result_unknown() {
    use crate::connections::model::Engine;
    use crate::transaction::{ControlOutcome, ControlStatement, TransactionEffect, TransactionState, UnknownReason};

    let transactions = coordinator(Engine::Postgres);
    transactions.acknowledge(&ControlStatement::Begin { mode: None }, ControlOutcome::Confirmed).unwrap();
    let driver = scripted(vec![Err(StatementStop::Unknown {
        message: "连接中断，无法确认写入是否完成。不会自动重放。connection reset by peer".to_owned(),
    })]);
    let flag = Arc::new(AtomicBool::new(false));
    let report = run_batch(&[piece("UPDATE item SET n = 1"), piece("SELECT 1")], 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.status, "unknown");
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::Unknown));
    assert_eq!(report.statements[1].status, "skipped");
    assert_eq!(report.calls, 1);
    let snapshot = transactions.snapshot().unwrap();
    assert_eq!(snapshot.state, TransactionState::Unknown);
    assert_eq!(snapshot.unresolved.expect("旧事务未知记录").reason, UnknownReason::ConnectionLost);
}

#[tokio::test]
async fn sqlserver_native_savepoints_are_registered_and_keep_the_transaction_active() {
    use crate::connections::model::Engine;
    use crate::transaction::{SavepointName, TransactionEffect, TransactionState};

    let transactions = coordinator(Engine::Sqlserver);
    let driver = scripted(vec![Ok(ok_count()), Ok(ok_count()), Ok(ok_count()), Ok(ok_count())]);
    let flag = Arc::new(AtomicBool::new(false));
    let parts = vec![piece("BEGIN TRAN"), piece("SAVE TRAN sp1"), piece("UPDATE item SET n = 2"), piece("ROLLBACK TRAN sp1")];
    let report = run_batch(&parts, 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.status, "done", "{:?}", report.statements);
    // T-SQL 手写保存点按方言映射成同一条控制语句，保存点因此被登记而不是当成普通语句。
    assert_eq!(
        driver.calls.lock().expect("calls").as_slice(),
        ["BEGIN TRANSACTION", "SAVE TRANSACTION [sp1]", "UPDATE item SET n = 2", "ROLLBACK TRANSACTION [sp1]"]
    );
    assert_eq!(report.statements[1].transaction, Some(TransactionEffect::StillActive));
    assert_eq!(report.statements[3].transaction, Some(TransactionEffect::StillActive));
    let snapshot = transactions.snapshot().unwrap();
    assert_eq!(snapshot.state, TransactionState::Active, "回滚到保存点不结束外层事务");
    assert_eq!(snapshot.savepoints, vec![SavepointName::parse("sp1").unwrap()]);

    // 不带名字的 ROLLBACK TRANSACTION 才是整体回滚。
    let driver = scripted(vec![Ok(ok_count())]);
    let report = run_batch(&[piece("ROLLBACK TRANSACTION")], 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::RolledBack));
    assert_eq!(driver.calls.lock().expect("calls").as_slice(), ["ROLLBACK TRANSACTION"]);
    assert_eq!(transactions.snapshot().unwrap().state, TransactionState::Idle);
}

#[tokio::test]
async fn the_sqlserver_probe_reconciles_an_unconfirmed_control_without_losing_the_unknown() {
    use crate::connections::model::Engine;
    use crate::transaction::{ControlOutcome, ControlStatement, TransactionEffect, TransactionState, UnknownReason};

    let transactions = coordinator(Engine::Sqlserver);
    transactions.acknowledge(&ControlStatement::Begin { mode: None }, ControlOutcome::Confirmed).unwrap();
    transactions
        .acknowledge(
            &ControlStatement::Commit { chain: false },
            ControlOutcome::Unconfirmed(UnknownReason::ConnectionLost),
        )
        .unwrap();
    assert_eq!(transactions.snapshot().unwrap().state, TransactionState::Unknown);

    // 探针确认数据库已经没有活动事务，随后两条普通语句在自动提交下执行。
    let probe_rows = StatementValue {
        columns: vec!["trancount".to_owned(), "state".to_owned()],
        column_types: vec!["int".to_owned(), "int".to_owned()],
        rows: vec![vec![Some("0".to_owned()), Some("0".to_owned())]],
        truncated: false,
        affected: None,
    };
    let driver = scripted(vec![Ok(probe_rows), Ok(ok_count()), Ok(ok_count())]);
    let flag = Arc::new(AtomicBool::new(false));
    let report = run_batch(&[piece("SELECT 1"), piece("SELECT 2")], 100, &flag, false, &driver, Some(&transactions)).await;
    let calls = driver.calls.lock().expect("calls").clone();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(calls[0].contains("XACT_STATE"), "未知之后先核对状态：{calls:?}");
    assert_eq!(report.status, "done");
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::Unchanged));
    let snapshot = transactions.snapshot().unwrap();
    assert_eq!(snapshot.state, TransactionState::Idle, "探针确认没有活动事务");
    assert_eq!(
        snapshot.unresolved.expect("旧事务结果仍然未知").reason,
        UnknownReason::ConnectionLost,
        "核对当前状态不等于确认旧事务的结局"
    );
}

/// P1 复现：无活动事务时执行无法确认形态的手写控制语句，界面不能停在 Idle。
#[tokio::test]
async fn ambiguous_control_statements_do_not_leave_the_snapshot_idle() {
    use crate::connections::model::Engine;
    use crate::transaction::{TransactionEffect, TransactionState, UnknownReason};

    for (engine, sql, follow_up) in [
        (Engine::Mysql, "START TRANSACTION READ ONLY", "COMMIT"),
        (Engine::Postgres, "BEGIN READ WRITE", "ROLLBACK"),
    ] {
        let transactions = coordinator(engine);
        let driver = scripted(vec![Ok(ok_count())]);
        let flag = Arc::new(AtomicBool::new(false));
        let report = run_batch(&[piece(sql)], 100, &flag, false, &driver, Some(&transactions)).await;
        assert_eq!(report.statements[0].status, "ok", "{engine:?}");
        assert_eq!(report.statements[0].transaction, Some(TransactionEffect::Unknown), "{engine:?}");
        assert_eq!(report.status, "unknown");
        let snapshot = transactions.snapshot().unwrap();
        assert_eq!(snapshot.state, TransactionState::Unknown, "{engine:?} 不能停在 Idle：数据库里可能已经有事务");
        assert_eq!(snapshot.unresolved.expect("未知记录").reason, UnknownReason::UnrecognizedEffect);

        // 后续普通语句不再被当成“无事务”的自动提交结果。
        let driver = scripted(vec![Ok(ok_count())]);
        let report = run_batch(&[piece("INSERT INTO item (n) VALUES (1)")], 100, &flag, false, &driver, Some(&transactions)).await;
        assert_eq!(report.statements[0].transaction, Some(TransactionEffect::Unknown), "{engine:?}");

        // 结束语句不再被本地预检拒绝：它必须真的发给数据库。
        let driver = scripted(vec![Ok(ok_count())]);
        let report = run_batch(&[piece(follow_up)], 100, &flag, false, &driver, Some(&transactions)).await;
        assert_eq!(driver.calls.lock().expect("calls").as_slice(), [follow_up], "{engine:?}");
        assert_eq!(report.statements[0].status, "unknown", "{engine:?}");
        assert_eq!(transactions.snapshot().unwrap().state, TransactionState::Idle, "结束语句已执行，结局仍未知");
    }
}

/// T-SQL 的 `BEGIN TRAN <name>` 不在可确认形态内：这一批按未知登记，下一批开始前由
/// `@@TRANCOUNT` 核对，之后 COMMIT 才能如实结束事务。
#[tokio::test]
async fn sqlserver_named_begin_is_unknown_until_the_depth_probe_confirms_it() {
    use crate::connections::model::Engine;
    use crate::transaction::{TransactionEffect, TransactionState, UnknownReason};

    let transactions = coordinator(Engine::Sqlserver);
    let driver = scripted(vec![Ok(ok_count())]);
    let flag = Arc::new(AtomicBool::new(false));
    let report = run_batch(&[piece("BEGIN TRAN sp")], 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::Unknown));
    assert_eq!(driver.calls.lock().expect("calls").as_slice(), ["BEGIN TRAN sp"], "这一批不追加探针，先按未知登记");
    let snapshot = transactions.snapshot().unwrap();
    assert_eq!(snapshot.state, TransactionState::Unknown, "不能停在 Idle：数据库里可能已经有事务");
    assert_eq!(snapshot.depth, None, "层数不可信");
    let record = snapshot.unresolved.expect("必须留下可核对的未知记录");
    assert_eq!(record.reason, UnknownReason::UnrecognizedEffect);

    // 下一批开始前核对：探针确认事务活动（T-SQL 的 BEGIN TRAN <name> 确实开始了事务）。
    let probe_active = StatementValue {
        columns: vec!["trancount".to_owned(), "state".to_owned()],
        column_types: vec!["int".to_owned(), "int".to_owned()],
        rows: vec![vec![Some("1".to_owned()), Some("1".to_owned())]],
        truncated: false,
        affected: None,
    };
    let driver = scripted(vec![Ok(probe_active), Ok(ok_count())]);
    let report = run_batch(&[piece("COMMIT TRAN")], 100, &flag, false, &driver, Some(&transactions)).await;
    let calls = driver.calls.lock().expect("calls").clone();
    assert!(calls.len() == 2 && calls[0].contains("XACT_STATE"), "COMMIT 之前先核对状态：{calls:?}");
    assert_eq!(calls[1], "COMMIT TRANSACTION", "探针确认后 COMMIT 真的发给数据库");
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::Committed));
    let snapshot = transactions.snapshot().unwrap();
    assert_eq!(snapshot.state, TransactionState::Idle);
    assert_eq!(snapshot.depth, Some(0));
    assert!(snapshot.unresolved.is_none(), "确认过的未知记录随提交关闭");
}

/// P3：T-SQL 过程调用可能在内部改变 `@@TRANCOUNT`，成功后层数必须由探针重新核对。
#[tokio::test]
async fn sqlserver_procedure_calls_verify_the_nesting_depth() {
    use crate::connections::model::Engine;
    use crate::transaction::{TransactionEffect, TransactionState};

    let probe_depth_two = StatementValue {
        columns: vec!["trancount".to_owned(), "state".to_owned()],
        column_types: vec!["int".to_owned(), "int".to_owned()],
        rows: vec![vec![Some("2".to_owned()), Some("1".to_owned())]],
        truncated: false,
        affected: None,
    };
    let transactions = coordinator(Engine::Sqlserver);
    let driver = scripted(vec![Ok(ok_count()), Ok(ok_count()), Ok(probe_depth_two)]);
    let flag = Arc::new(AtomicBool::new(false));
    let report = run_batch(&[piece("BEGIN TRAN"), piece("EXEC dbo.rotate_notes")], 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.status, "done", "{:?}", report.statements);
    assert_eq!(report.statements[1].transaction, Some(TransactionEffect::StillActive));
    let calls = driver.calls.lock().expect("calls").clone();
    assert_eq!(calls[0], "BEGIN TRANSACTION");
    assert_eq!(calls[1], "EXEC dbo.rotate_notes");
    assert!(calls[2].contains("XACT_STATE"), "过程调用后必须核对层数：{calls:?}");
    let snapshot = transactions.snapshot().unwrap();
    assert_eq!(snapshot.state, TransactionState::Active);
    assert_eq!(snapshot.depth, Some(2), "过程内部的 BEGIN 由探针发现");

    // 一次 COMMIT 只减少一层：不能因为旧层数把事务报成已结束。
    let driver = scripted(vec![Ok(ok_count())]);
    let report = run_batch(&[piece("COMMIT TRAN")], 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::StillActive));
    let snapshot = transactions.snapshot().unwrap();
    assert_eq!(snapshot.state, TransactionState::Active);
    assert_eq!(snapshot.depth, Some(1));
}

#[tokio::test]
async fn an_unconfirmed_commit_is_reported_unknown_and_not_replayed() {
    use crate::connections::model::Engine;
    use crate::transaction::{ControlOutcome, ControlStatement, TransactionEffect, TransactionState};

    let transactions = coordinator(Engine::Mysql);
    transactions.acknowledge(&ControlStatement::Begin { mode: None }, ControlOutcome::Confirmed).unwrap();
    let driver = scripted(vec![Err(StatementStop::Unknown { message: "连接中断，无法确认写入是否完成。".to_owned() })]);
    let flag = Arc::new(AtomicBool::new(false));
    let report = run_batch(&[piece("COMMIT"), piece("SELECT 1")], 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.statements[0].status, "unknown");
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::Unknown));
    assert!(report.statements[0].message.contains("没有重放"), "{}", report.statements[0].message);
    assert_eq!(report.statements[1].status, "skipped");
    assert_eq!(driver.calls.lock().expect("calls").len(), 1, "控制语句没有重放");
    assert_eq!(transactions.snapshot().unwrap().state, TransactionState::Unknown);
}

#[tokio::test]
async fn handwritten_savepoints_keep_the_outer_transaction_active() {
    use crate::connections::model::Engine;
    use crate::transaction::{ControlOutcome, ControlStatement, SavepointName, TransactionEffect, TransactionState};

    let transactions = coordinator(Engine::Postgres);
    transactions.acknowledge(&ControlStatement::Begin { mode: None }, ControlOutcome::Confirmed).unwrap();
    let driver = scripted(vec![Ok(ok_count()), Ok(ok_count()), Ok(ok_count())]);
    let flag = Arc::new(AtomicBool::new(false));
    let parts = vec![piece("SAVEPOINT sp1"), piece("UPDATE item SET n = 2"), piece("ROLLBACK TO SAVEPOINT sp1")];
    let report = run_batch(&parts, 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.status, "done");
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::StillActive));
    assert_eq!(report.statements[2].transaction, Some(TransactionEffect::StillActive));
    let snapshot = transactions.snapshot().unwrap();
    assert_eq!(snapshot.state, TransactionState::Active, "回滚到保存点不结束外层事务");
    assert_eq!(snapshot.savepoints, vec![SavepointName::parse("sp1").unwrap()]);
    assert_eq!(
        driver.calls.lock().expect("calls").as_slice(),
        ["SAVEPOINT \"sp1\"", "UPDATE item SET n = 2", "ROLLBACK TO SAVEPOINT \"sp1\""]
    );
}

#[tokio::test]
async fn dialect_error_mappings_decide_the_transaction_effect() {
    use crate::connections::model::Engine;
    use crate::transaction::{ControlOutcome, ControlStatement, TransactionEffect, TransactionState};

    // SQL Server 的不可提交事务（3931）：只能整体回滚，不能继续执行普通语句。
    let sqlserver = coordinator(Engine::Sqlserver);
    sqlserver.acknowledge(&ControlStatement::Begin { mode: None }, ControlOutcome::Confirmed).unwrap();
    let driver = scripted(vec![Err(StatementStop::Error {
        message: "The current transaction cannot be committed and cannot support operations that write to the log file".to_owned(),
        at: None,
        code: Some("3931".to_owned()),
    })]);
    let flag = Arc::new(AtomicBool::new(false));
    let report = run_batch(&[piece("UPDATE item SET n = 1")], 100, &flag, false, &driver, Some(&sqlserver)).await;
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::Aborted));
    assert_eq!(sqlserver.snapshot().unwrap().state, TransactionState::Aborted);

    // MySQL 死锁（1213）由服务器回滚整个事务：不继续宣称活动，也不重放这条写入。
    let mysql = coordinator(Engine::Mysql);
    mysql.acknowledge(&ControlStatement::Begin { mode: None }, ControlOutcome::Confirmed).unwrap();
    let driver = scripted(vec![Err(StatementStop::Error {
        message: "Deadlock found when trying to get lock; try restarting transaction".to_owned(),
        at: None,
        code: Some("1213".to_owned()),
    })]);
    let report = run_batch(&[piece("UPDATE item SET n = 1"), piece("SELECT 1")], 100, &flag, false, &driver, Some(&mysql)).await;
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::Unknown));
    assert_eq!(report.status, "error", "语句本身失败；事务结局另行报告为未知");
    assert_eq!(report.statements[1].status, "skipped");
    assert_eq!(report.statements[1].message, "事务状态未知，未执行");
    assert_eq!(mysql.snapshot().unwrap().state, TransactionState::Unknown);
}

#[tokio::test]
async fn a_rollback_after_a_lost_commit_ends_the_transaction_but_keeps_the_result_unknown() {
    use crate::connections::model::Engine;
    use crate::transaction::{ControlOutcome, ControlStatement, TransactionEffect, TransactionState, UnknownReason};

    let transactions = coordinator(Engine::Mysql);
    transactions.acknowledge(&ControlStatement::Begin { mode: None }, ControlOutcome::Confirmed).unwrap();
    // 丢失结果的 COMMIT：旧事务结局无法确认。
    transactions
        .acknowledge(
            &ControlStatement::Commit { chain: false },
            ControlOutcome::Unconfirmed(UnknownReason::ControlUnconfirmed),
        )
        .unwrap();
    let driver = scripted(vec![Ok(ok_count())]);
    let flag = Arc::new(AtomicBool::new(false));
    let report = run_batch(&[piece("ROLLBACK"), piece("SELECT 1")], 100, &flag, false, &driver, Some(&transactions)).await;
    assert_eq!(report.statements[0].status, "unknown", "回滚确实执行了，但旧事务结局仍不能宣称");
    assert_eq!(report.statements[0].transaction, Some(TransactionEffect::Unknown));
    assert!(report.statements[0].message.contains("仍无法确认"), "{}", report.statements[0].message);
    assert_eq!(report.statements[1].status, "skipped");
    assert_eq!(transactions.snapshot().unwrap().state, TransactionState::Idle, "回滚之后新会话没有活动事务");
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
    // MySQL 与 PostgreSQL 的取消实现位于各自适配器的 Canceller。
    let mysql_cancel = std::fs::read_to_string(root.join("src/adapters/mysql/connect.rs")).unwrap();
    let postgres_cancel = std::fs::read_to_string(root.join("src/adapters/postgres/connect.rs")).unwrap();
    // 执行器与各引擎的执行驱动（A4 起在适配器里）都只用已打开的会话。
    let exec = ["src/query/exec.rs", "src/adapters/exec.rs", "src/adapters/sqlite/exec.rs", "src/adapters/mysql/exec.rs", "src/adapters/postgres/exec.rs", "src/adapters/sqlserver/exec.rs"]
        .iter()
        .map(|path| std::fs::read_to_string(root.join(path)).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(mysql_cancel.contains("Pool::new"));
    assert!(postgres_cancel.contains("cancel_query"));
    assert!(!postgres_cancel.contains("cancel_query_raw"));
    assert!(!exec.contains("open_sqlite"));
    assert!(!exec.contains("open_mysql"));
    assert!(!exec.contains("open_postgres"));
    assert!(!exec.contains("Pool::new"));
    let route = std::fs::read_to_string(root.join("src/connections/route.rs")).unwrap();
    assert!(route.contains("verify_hostname"));
    assert!(route.contains("不会改用 localhost，也不会关闭主机名验证"));
}

#[test]
fn a_late_result_stays_on_its_own_tab_instead_of_the_active_one() {
    use crate::application::tasks::TaskBoard;
    use crate::query::assist::RunScope;
    use crate::query::commands::{RunDto, StatementDto};
    use crate::query::desk::{BeginRun, Desk, Target, Ticket};

    let tasks = TaskBoard::new();
    let mut desk = Desk::new();
    let target = Target {
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: "main".to_owned(),
        engine: "sqlite".to_owned(),
        schema: String::new(),
        environment: String::new(),
        server_version: None,
    };
    desk.import_tab("a", "select 1", Some(target.clone()));
    desk.import_tab("b", "select 2", Some(target));
    desk.focus_tab(Some("a"));
    let BeginRun::Ready(prepared) = desk.begin_run(&tasks, RunScope::Script, None) else {
        panic!("应开始执行 select 1");
    };
    let ticket = Ticket { tab_id: prepared.tab_id.clone(), token: prepared.token, version: prepared.version };
    desk.focus_tab(Some("b"));

    let run = RunDto {
        id: "run-a".to_owned(),
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: "main".to_owned(),
        engine: "sqlite".to_owned(),
        status: "done".to_owned(),
        cancel: String::new(),
        cancel_message: String::new(),
        via_ssh: false,
        fresh_channel: false,
        statements: vec![StatementDto {
            sql: "select 1".to_owned(),
            sent_sql: None,
            rewrite: None,
            status: "ok".to_owned(),
            message: String::new(),
            columns: vec!["one".to_owned()],
            column_types: vec!["integer".to_owned()],
            rows: vec![vec![Some("1".to_owned())]],
            truncated: false,
            affected: None,
            elapsed_ms: None,
            error_at: None,
            error_code: None,
            transaction: None,
            result_set: None,
        }],
        transaction: None,
        started_at: String::new(),
        elapsed_ms: 1,
    };
    desk.finish_polled(&tasks, &ticket, Ok(run), "已取消");

    assert_eq!(desk.active_id(), Some("b"));
    assert!(desk.results().is_empty(), "活动查询的结果不能被另一个标签的迟到结果覆盖");
    desk.focus_tab(Some("a"));
    let results = desk.results();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].rows[0][0].as_deref(), Some("1"));
}

#[test]
fn query_commands_are_appended_without_dropping_catalog_or_grid() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let query = std::fs::read_to_string(root.join("src/app/views/query/mod.rs")).unwrap();
    for name in ["query::commands::start_run(", "query::commands::poll_run(", "query::commands::request_cancel("] {
        assert!(query.contains(name), "{name}");
    }
    let history = std::fs::read_to_string(root.join("src/app/views/history/mod.rs")).unwrap();
    for name in ["fn load_runs(", "fn load_saved(", "fn save_named(", "fn rename_saved(", "fn delete_saved(", "fn delete_run(", "fn clear_runs("] {
        assert!(history.contains(name), "{name}");
    }
    let grid = std::fs::read_to_string(root.join("src/app/views/grid/mod.rs")).unwrap();
    for name in ["application::grid::describe(", "application::grid::page_described(", "application::grid::count(", "application::grid::commit("] {
        assert!(grid.contains(name), "{name}");
    }
    let compare = std::fs::read_to_string(root.join("src/app/views/compare/mod.rs")).unwrap();
    assert!(compare.contains("application::work::catalog_refresh("));
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
async fn history_delete_removes_one_entry_and_stays_idempotent() {
    let dir = std::env::temp_dir().join(format!("rillsql-history-delete-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = history::HistoryStore::open(&dir.join("query-history.sqlite")).await.unwrap();
    for (id, started_at) in [("h1", "2026-10-14T10:00:00Z"), ("h2", "2026-10-15T10:00:00Z"), ("h3", "2026-10-16T10:00:00Z")] {
        store
            .insert(&HistoryDraft {
                id: id.to_owned(),
                sql: format!("SELECT '{id}'"),
                connection_id: "notes".to_owned(),
                connection_name: "笔记".to_owned(),
                database_name: "main".to_owned(),
                engine: "sqlite".to_owned(),
                started_at: started_at.to_owned(),
                summary: "执行中".to_owned(),
            })
            .await
            .unwrap();
    }
    store.delete("h2").await.unwrap();
    let left = store.list().await.unwrap();
    assert_eq!(left.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), vec!["h3", "h1"]);
    assert_eq!(left.iter().map(|item| item.sql.clone()).collect::<Vec<_>>(), vec!["SELECT 'h3'", "SELECT 'h1'"]);
    store.delete("h2").await.unwrap();
    store.delete("missing").await.unwrap();
    assert_eq!(store.list().await.unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn history_cap_keeps_only_the_latest_entries() {
    let dir = std::env::temp_dir().join(format!("rillsql-history-cap-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = history::HistoryStore::open_with_cap(&dir.join("query-history.sqlite"), 5).await.unwrap();
    for index in 0..8 {
        store
            .insert(&HistoryDraft {
                id: format!("h{index}"),
                sql: format!("SELECT {index}"),
                connection_id: "notes".to_owned(),
                connection_name: "笔记".to_owned(),
                database_name: "main".to_owned(),
                engine: "sqlite".to_owned(),
                started_at: format!("2026-10-{:02}T10:00:00Z", index + 1),
                summary: "执行中".to_owned(),
            })
            .await
            .unwrap();
    }
    let listed = store.list().await.unwrap();
    assert_eq!(listed.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), vec!["h7", "h6", "h5", "h4", "h3"]);
    let _ = std::fs::remove_dir_all(&dir);
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
            timeout_ms: None,
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
    let check = exec::LiveDriver::attach(session, "sqlite", "main", service.cancel_transport("notes").unwrap(), None)
        .await
        .unwrap();
    let rows = check.run("SELECT id, name FROM t", 10, &Arc::new(AtomicBool::new(false))).await.outcome.unwrap();
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
            timeout_ms: None,
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

/// 手写事务、跨批次保留与共享快照：真实 SQLite 文件上的完整流程检查（R2、R3、R5）。
#[tokio::test]
async fn a_handwritten_transaction_survives_batches_and_reports_the_shared_snapshot() {
    use crate::connections::Service;
    use crate::transaction::{TransactionEffect, TransactionState};
    use super::commands::{start_run, QueryState, StartRequest};

    let dir = std::env::temp_dir().join(format!("rillsql-query-tx-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let database = dir.join("notes.sqlite");
    let setup = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY)").execute(&setup).await.unwrap();
    setup.close().await;

    let service = Arc::new(Service::new_for_test(dir.join("connections.sqlite")));
    service.open_sqlite_for_test("notes", "笔记", &database).await.unwrap();
    let state = QueryState::new(dir.join("query-history.sqlite"));
    let request = |sql: &str| StartRequest {
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: "main".to_owned(),
        engine: "sqlite".to_owned(),
        sql: sql.to_owned(),
        limit: 50,
        timeout_ms: None,
    };
    let count = |service: Arc<Service>| async move {
        let session = service.catalog_session("notes").unwrap();
        let check = exec::LiveDriver::attach(session, "sqlite", "main", service.cancel_transport("notes").unwrap(), None)
            .await
            .unwrap();
        let rows = check.run("SELECT count(*) FROM t", 10, &Arc::new(AtomicBool::new(false))).await.outcome.unwrap();
        rows.rows[0][0].clone()
    };

    let started = start_run(&service, &state, request("BEGIN;\nINSERT INTO t (id) VALUES (7);")).await.unwrap();
    let opened = wait_done(&state, &started.id).await;
    assert_eq!(opened.status, "done", "{:?}", opened.statements);
    assert_eq!(opened.statements[0].transaction, Some(TransactionEffect::Began));
    assert_eq!(opened.statements[1].transaction, Some(TransactionEffect::StillActive));
    let report = opened.transaction.expect("关系型查询要带事务快照");
    assert_eq!(report.snapshot.state, TransactionState::Active);
    assert_eq!(report.effect, Some(TransactionEffect::StillActive));
    // 同一连接的业务标签与查询标签看到的是同一份快照。
    assert_eq!(service.transaction_snapshot("notes"), Some(report.snapshot.clone()));
    assert_eq!(count(Arc::clone(&service)).await.as_deref(), Some("0"), "未提交的写入在元数据连接上不可见");

    // 新批次沿用同一个活动事务：手写 ROLLBACK 在同一会话里结束它。
    let started = start_run(&service, &state, request("ROLLBACK;")).await.unwrap();
    let rolled = wait_done(&state, &started.id).await;
    assert_eq!(rolled.statements[0].transaction, Some(TransactionEffect::RolledBack));
    let snapshot = rolled.transaction.expect("回滚后的快照").snapshot;
    assert_eq!(snapshot.state, TransactionState::Idle);
    assert_eq!(count(Arc::clone(&service)).await.as_deref(), Some("0"), "回滚撤销了此前未提交的修改");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tsql_split_keeps_top_level_begin_end_blocks_together() {
    // 顶层 BEGIN...END 是一条语句：块内的分号不结束语句。
    let batch = split::split_sql("sqlserver", "BEGIN\n  UPDATE t SET n = 1;\n  UPDATE u SET m = 2;\nEND\nSELECT 1;");
    assert_eq!(batch.pieces.len(), 2, "{batch:?}");
    assert!(batch.pieces[0].text.starts_with("BEGIN"));
    assert!(batch.pieces[0].text.trim_end().ends_with("END"));
    assert_eq!(batch.pieces[1].text.trim_end_matches(';'), "SELECT 1");

    // 嵌套块与 CREATE PROCEDURE 体同规则。
    let nested = split::split_sql("sqlserver", "BEGIN\n  BEGIN\n    SELECT 1;\n  END\n  SELECT 2;\nEND;");
    assert_eq!(nested.pieces.len(), 1, "{nested:?}");

    // 块内的字符串分号不参与计数。
    let quoted = split::split_sql("sqlserver", "BEGIN\n  SELECT 'a;b';\nEND;");
    assert_eq!(quoted.pieces.len(), 1, "{quoted:?}");

    // BEGIN TRAN 是事务语句，不是块：分号照常切分。
    let transaction = split::split_sql("sqlserver", "BEGIN TRAN;\nUPDATE t SET n = 1;\nCOMMIT;");
    assert_eq!(transaction.pieces.len(), 3, "{transaction:?}");
    let spelled = split::split_sql("sqlserver", "BEGIN TRANSACTION;\nSELECT 1;");
    assert_eq!(spelled.pieces.len(), 2, "{spelled:?}");

    // 未闭合的 BEGIN 块整批拒绝发送。
    let unclosed = split::split_sql("sqlserver", "SELECT 1;\nBEGIN\n  SELECT 2;");
    assert_eq!(unclosed.pieces.len(), 1);
    assert_eq!(unclosed.refusal.as_deref(), Some("BEGIN 块未闭合，没有发送。"));

    // DELIMITER 是 mysql 客户端指令，T-SQL 同样跳过并提示。
    let delimiter = split::split_sql("sqlserver", "DELIMITER //\nSELECT 1;");
    assert_eq!(delimiter.pieces.len(), 1);
    assert!(delimiter.notices.iter().any(|item| item.contains("SQL Server 不使用 DELIMITER")));
}

#[test]
fn sqlserver_cancel_uses_kill_spid_statement() {
    assert_eq!(cancel::kill_sql(57), "KILL 57");
    assert_eq!(cancel::kill_sql(0), "KILL 0");
}

#[test]
fn sqlserver_result_cap_uses_top_without_touching_with_statements() {
    assert_eq!(exec::cap_result_sql("sqlserver", "SELECT * FROM t", 10), "SELECT TOP (11) * FROM t");
    assert_eq!(exec::cap_result_sql("sqlserver", "SELECT * FROM t;", 10), "SELECT TOP (11) * FROM t;");
    assert_eq!(exec::cap_result_sql("sqlserver", "select * from t", 10), "select TOP (11) * from t");
    assert_eq!(exec::cap_result_sql("sqlserver", "SELECT * FROM t OFFSET 5 ROWS FETCH NEXT 1000 ROWS ONLY", 10), "SELECT * FROM t OFFSET 5 ROWS FETCH NEXT 11 ROWS ONLY");
    assert_eq!(exec::cap_result_sql("sqlserver", "SELECT * FROM t FETCH NEXT 5 ROWS ONLY", 10), "SELECT * FROM t FETCH NEXT 5 ROWS ONLY");
    assert_eq!(exec::cap_result_sql("sqlserver", "SELECT TOP 1000 * FROM t", 10), "SELECT TOP 11 * FROM t");
    assert_eq!(exec::cap_result_sql("sqlserver", "SELECT TOP (5) * FROM t", 10), "SELECT TOP (5) * FROM t");
    // WITH 开头的语句不能盲目改写外层，保持原样。
    assert_eq!(exec::cap_result_sql("sqlserver", "WITH c AS (SELECT * FROM t) SELECT * FROM c", 10), "WITH c AS (SELECT * FROM t) SELECT * FROM c");
    assert_eq!(exec::cap_result_sql("sqlserver", "UPDATE t SET n = 1", 10), "UPDATE t SET n = 1");
}

#[test]
fn sqlserver_set_and_showplan_statements_route_through_the_row_path() {
    // sp_executesql（execute 路径）里的 SET 只作用于内层批次；SET 语句必须走 simple_query 才会留在会话上。
    assert!(crate::adapters::sqlserver::exec::is_showplan_statement("SET SHOWPLAN_ALL ON"));
    assert!(crate::adapters::sqlserver::exec::is_showplan_statement("set showplan_all off"));
    assert!(crate::adapters::sqlserver::exec::is_showplan_statement("  SET SHOWPLAN_ALL ON;\nSELECT 1;\nSET SHOWPLAN_ALL OFF"));
    assert!(!crate::adapters::sqlserver::exec::is_showplan_statement("SELECT 1"));
    assert!(!crate::adapters::sqlserver::exec::is_showplan_statement("SET NOCOUNT ON"));
    // 路由口径：SET 开头（会话设置）与 SELECT 族都走收行路径，写语句走 execute 拿受影响行数。
    assert_eq!(split::first_keyword("SET NOCOUNT ON"), "SET");
    assert_eq!(split::first_keyword("SET SHOWPLAN_ALL ON"), "SET");
    assert_eq!(split::first_keyword("SELECT * FROM t"), "SELECT");
    assert_eq!(split::first_keyword("UPDATE t SET n = 1"), "UPDATE");
}

#[test]
fn sqlserver_showplan_batch_survives_the_real_split_and_routes_each_piece() {
    // 真实批次（不经 synthetic 拦截）：整批按分号切成三条语句，每条都走收行路径，
    // 中间的 SELECT 在 SHOWPLAN_ALL 下返回计划行集而不是执行语句。
    let batch = crate::adapters::sqlserver::exec::showplan_batch("SELECT * FROM [dbo].[orders]");
    let report = split::split_sql("sqlserver", &batch);
    assert_eq!(report.pieces.len(), 3, "{report:?}");
    assert_eq!(report.pieces[0].text.trim_end_matches(';'), "SET SHOWPLAN_ALL ON");
    assert_eq!(report.pieces[1].text.trim_end_matches(';'), "SELECT * FROM [dbo].[orders]");
    assert_eq!(report.pieces[2].text.trim_end_matches(';'), "SET SHOWPLAN_ALL OFF");
    for piece in &report.pieces {
        let first = piece.text.split_whitespace().next().unwrap_or("");
        assert!(first.eq_ignore_ascii_case("SET") || first.eq_ignore_ascii_case("SELECT"), "每条语句都应走 simple_query 收行路径：{}", piece.text);
    }
}

#[test]
fn sqlserver_showplan_result_rows_are_ordered_parents_first() {
    // 乱序返回的 SHOWPLAN_ALL 行集按 NodeId 排成父先子后；非 SHOWPLAN 形状原样返回。
    let columns: Vec<String> = ["StmtId", "NodeId", "Parent", "PhysicalOp", "LogicalOp", "Argument", "EstimateRows"]
        .iter()
        .map(|item| item.to_string())
        .collect();
    let rows = vec![
        vec![Some("1".to_owned()), Some("4".to_owned()), Some("3".to_owned()), Some("Nested Loops".to_owned()), Some("".to_owned()), Some("".to_owned()), Some("10".to_owned())],
        vec![Some("1".to_owned()), Some("1".to_owned()), Some("0".to_owned()), Some("".to_owned()), Some("".to_owned()), Some("SELECT".to_owned()), Some("1".to_owned())],
        vec![Some("1".to_owned()), Some("3".to_owned()), Some("1".to_owned()), Some("Sort".to_owned()), Some("".to_owned()), Some("".to_owned()), Some("3".to_owned())],
        vec![Some("1".to_owned()), Some("2".to_owned()), Some("1".to_owned()), Some("Clustered Index Scan".to_owned()), Some("".to_owned()), Some("".to_owned()), Some("3".to_owned())],
    ];
    let ordered = crate::adapters::sqlserver::exec::order_showplan_results(&columns, rows);
    let node_ids: Vec<&str> = ordered.iter().map(|row| row[1].as_deref().unwrap_or("")).collect();
    assert_eq!(node_ids, vec!["1", "2", "3", "4"]);
    // NodeId 非数字或父缺失（层次不完整）时按原序返回，不猜。
    let plain = vec![Some("a".to_owned()), Some("b".to_owned())];
    let kept = crate::adapters::sqlserver::exec::order_showplan_results(&columns, vec![plain.clone()]);
    assert_eq!(kept, vec![plain]);
    // 普通结果集恰好带 NodeId+Parent 数字列（无 SHOWPLAN 专属列）时不重排、保持原序。
    let ordinary_columns: Vec<String> = ["NodeId", "Parent", "value"].iter().map(|item| item.to_string()).collect();
    let ordinary = vec![
        vec![Some("2".to_owned()), Some("1".to_owned()), Some("second".to_owned())],
        vec![Some("1".to_owned()), Some("0".to_owned()), Some("first".to_owned())],
    ];
    let unchanged = crate::adapters::sqlserver::exec::order_showplan_results(&ordinary_columns, ordinary.clone());
    assert_eq!(unchanged, ordinary, "非 SHOWPLAN 形状不得静默重排");
    let ordinary_columns: Vec<String> = ["id", "name"].iter().map(|item| item.to_string()).collect();
    let untouched = crate::adapters::sqlserver::exec::order_showplan_results(&ordinary_columns, vec![vec![Some("1".to_owned()), Some("x".to_owned())]]);
    assert_eq!(untouched.len(), 1);
}

#[test]
fn sqlserver_showplan_batch_wraps_the_statement_without_executing_it() {
    // SHOWPLAN 批次：SET 开、原语句、SET 关。原语句保持一行，不做改写。
    let batch = crate::adapters::sqlserver::exec::showplan_batch("SELECT * FROM [dbo].[orders]");
    assert_eq!(batch, "SET SHOWPLAN_ALL ON;\nSELECT * FROM [dbo].[orders];\nSET SHOWPLAN_ALL OFF");
    assert!(batch.starts_with("SET SHOWPLAN_ALL ON"));
    assert!(batch.ends_with("SET SHOWPLAN_ALL OFF"));
    assert!(batch.contains("\nSELECT * FROM [dbo].[orders];\n"));
}

#[test]
fn showplan_rows_build_an_outline_by_parent() {
    // SHOWPLAN_ALL 行集按 Parent/NodeId 层次化：根在 0 层，子算子逐层加深。
    let rows = vec![
        (1, 0, "SELECT".to_owned(), "0.03".to_owned()),
        (2, 1, "Clustered Index Scan".to_owned(), "0.02".to_owned()),
        (3, 1, "Sort".to_owned(), "0.01".to_owned()),
        (4, 3, "Nested Loops".to_owned(), "0.005".to_owned()),
    ];
    let outline = crate::adapters::sqlserver::exec::showplan_outline(&rows);
    assert_eq!(outline.len(), 4);
    assert_eq!(outline[0], (0, "SELECT".to_owned(), "0.03".to_owned()));
    assert_eq!(outline[1], (1, "Clustered Index Scan".to_owned(), "0.02".to_owned()));
    assert_eq!(outline[2], (1, "Sort".to_owned(), "0.01".to_owned()));
    assert_eq!(outline[3], (2, "Nested Loops".to_owned(), "0.005".to_owned()));
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
    // 执行器在 query/exec.rs，会话一侧与各引擎驱动在适配器里（A4）；按同一份源码文本检查。
    let exec = ["src/query/exec.rs", "src/adapters/exec.rs", "src/adapters/mysql/exec.rs", "src/adapters/postgres/exec.rs"]
        .iter()
        .map(|path| std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let mysql_run = exec.split("async fn mysql_run").nth(1).unwrap();
    let mysql_run = mysql_run.split("fn mysql_cell").next().unwrap();
    assert!(!mysql_run.contains("get_conn"), "每条语句不能重新借连接");
    assert_eq!(exec.matches("get_conn").count(), 1);
    assert!(exec.contains("reset_connection(false)"));
    let drivers = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/adapters/mysql/connect.rs")).unwrap();
    assert!(drivers.contains("with_reset_connection(false)"), "池归还时不能把这条会话重置掉");
    assert!(drivers.contains("with_inactive_connection_ttl"), "空闲时必须留着这一条已打开的连接");
    assert!(!exec.contains("while result.next().await.map_err(|err| err.to_string())?.is_some() {}"));
    let truncated = mysql_run.split("if truncated").nth(1).expect("mysql truncated");
    assert!(!truncated.contains("while"), "截断之后不能把剩余行读完");
    let postgres = exec.split("async fn postgres_run").nth(1).unwrap();
    let postgres = postgres.split("pub fn clamp_limit").next().unwrap();
    // 读取上限只在 LiveDriver 里改写一次；驱动收到什么就发什么，不会第二次改写。
    assert!(!postgres.contains("cap_result_sql"), "驱动不再自己改写");
    assert!(postgres.contains("prepare(sent)"));
    assert!(postgres.contains("simple_query(sent)"));
    let live = exec.split("impl StatementDriver for LiveDriver").nth(1).unwrap();
    assert!(live.contains("cap_result_sql_detail(&self.engine, sql, limit)"), "改写点只剩一处，并且实际语句会随结果返回");
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
    let driver = exec::LiveDriver::attach(CatalogSession::Mysql(pool.clone()), "mysql", "app", CancelTransport::None, None)
        .await
        .unwrap_or_else(|err| panic!("{err} queries={:?} failures={:?}", stub.queries.lock().unwrap(), stub.failures.lock().unwrap()));
    let flag = Arc::new(AtomicBool::new(false));
    let report = run_batch(
        &[piece("SET @keep = 'stay'"), piece("SELECT @keep"), piece("SELECT CONNECTION_ID()")],
        10,
        &flag,
        false,
        &driver,
        None,
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
    assert!(queries.iter().any(|sql| sql == "USE app"), "{queries:?}");
    assert_eq!(stub.resets.load(Ordering::SeqCst), 0, "归还连接时执行了 COM_RESET_CONNECTION queries={queries:?} failures={failures:?}");
    assert_eq!(stub.accepts.load(Ordering::SeqCst), 1, "执行为同一批另开了连接 queries={queries:?} failures={failures:?}");
    assert_eq!(kept.as_ref().map(Option::as_deref), Some(Some("stay")), "queries={queries:?} failures={failures:?}");
    assert_eq!(same.flatten(), first_id, "queries={queries:?}");
    drop(again);
    assert_eq!(stub.resets.load(Ordering::SeqCst), 0);
    assert_eq!(stub.accepts.load(Ordering::SeqCst), 1);

    let driver = exec::LiveDriver::attach(CatalogSession::Mysql(pool.clone()), "mariadb", "app", CancelTransport::None, Some(1_500))
        .await
        .unwrap();
    let limited = run_batch(&[piece("SELECT * FROM big")], 2, &flag, false, &driver, None).await;
    assert_eq!(limited.statements[0].rows.len(), 2, "{:?}", limited.statements);
    assert!(limited.statements[0].truncated);
    assert_eq!(stub.rows_sent.load(Ordering::SeqCst), 3);
    assert!(!stub.uncapped.load(Ordering::SeqCst));
    let queries = stub.queries.lock().expect("queries").clone();
    assert!(queries.iter().any(|sql| sql.to_ascii_uppercase().contains("LIMIT 3")), "{queries:?}");
    // attach 时下发会话级超时（MariaDB 单位是秒），整批结束后 finish 复位，不泄漏到目录连接。
    assert!(queries.iter().any(|sql| sql.contains("max_statement_time = 1.5")), "{queries:?}");
    driver.finish().await.unwrap();
    drop(driver);
    let queries = stub.queries.lock().expect("queries").clone();
    assert!(queries.iter().any(|sql| sql.contains("max_statement_time = 0")), "{queries:?}");
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
    let mut database: Option<String> = None;
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
                database = None;
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
                    write_text_rows(&mut stream, &mut reply, std::slice::from_ref(&database)).await?;
                } else if upper.starts_with("USE") {
                    database = Some(sql[3..].trim().trim_matches('`').to_owned());
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
        timeout_ms: None,
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

#[test]
fn tds_value_formatters_cover_numeric_uuid_and_temporal_types() {
    use crate::adapters::sqlserver::exec::{tds_datetime_offset_text, tds_naive_date_text, tds_naive_datetime_text, tds_naive_time_text, tds_numeric_text, tds_uuid_text};
    // decimal / money：定点文本保留小数位。
    let money = tiberius::numeric::Numeric::new_with_scale(12850, 2);
    assert_eq!(tds_numeric_text(money), "128.50");
    // uniqueidentifier。
    let id = uuid::Uuid::parse_str("0E984725-C51C-4BF4-9960-E1C80E27ABA0").unwrap();
    assert_eq!(tds_uuid_text(id), "0e984725-c51c-4bf4-9960-e1c80e27aba0");
    // datetime / datetime2 / smalldatetime。
    let stamp = chrono::NaiveDate::from_ymd_opt(2024, 1, 2).unwrap().and_hms_opt(15, 4, 5).unwrap();
    assert_eq!(tds_naive_datetime_text(stamp), "2024-01-02 15:04:05");
    let with_nanos = chrono::NaiveDate::from_ymd_opt(2024, 1, 2)
        .unwrap()
        .and_hms_nano_opt(15, 4, 5, 123_456_000)
        .unwrap();
    assert_eq!(tds_naive_datetime_text(with_nanos), "2024-01-02 15:04:05.123456");
    // date / time(n)。
    assert_eq!(tds_naive_date_text(chrono::NaiveDate::from_ymd_opt(2024, 1, 2).unwrap()), "2024-01-02");
    assert_eq!(tds_naive_time_text(chrono::NaiveTime::from_hms_opt(15, 4, 5).unwrap()), "15:04:05");
    // datetimeoffset（RFC3339）。
    let offset = chrono::DateTime::parse_from_rfc3339("2024-01-02T15:04:05+08:00").unwrap().with_timezone(&chrono::Utc);
    assert_eq!(tds_datetime_offset_text(offset), "2024-01-02T07:04:05+00:00");
}

#[test]
fn tds_type_names_map_temporal_numeric_and_xml_columns() {
    use crate::adapters::sqlserver::exec::tds_type_name;
    use tiberius::ColumnType;
    assert_eq!(tds_type_name(ColumnType::Datetime2), "datetime");
    assert_eq!(tds_type_name(ColumnType::Daten), "date");
    assert_eq!(tds_type_name(ColumnType::Timen), "time");
    assert_eq!(tds_type_name(ColumnType::DatetimeOffsetn), "datetimeoffset");
    assert_eq!(tds_type_name(ColumnType::Decimaln), "decimal");
    assert_eq!(tds_type_name(ColumnType::Money), "decimal");
    assert_eq!(tds_type_name(ColumnType::Guid), "uniqueidentifier");
    assert_eq!(tds_type_name(ColumnType::Xml), "xml");
}

#[test]
fn tsql_split_handles_bracket_identifiers_try_catch_and_go_batches() {
    // 方括号标识符：[end]/[case]/[begin] 不参与块计数；]] 是转义的 ]。
    let brackets = split::split_sql("sqlserver", "SELECT [end], [case], [a]]b] FROM t; SELECT 1;");
    assert_eq!(brackets.pieces.len(), 2, "{brackets:?}");
    assert!(brackets.pieces[0].text.contains("[a]]b]"));
    // 未闭合的方括号整批拒绝。
    let unclosed = split::split_sql("sqlserver", "SELECT [name FROM t;");
    assert_eq!(unclosed.refusal.as_deref(), Some("方括号标识符没有闭合，没有发送。"));
    // 方括号标识符里的 -- 不是注释（与 TS 镜像同序）：标识符完整保留、正常切分。
    let dashed = split::split_sql("sqlserver", "SELECT [a--b] FROM t; SELECT 1;");
    assert_eq!(dashed.pieces.len(), 2, "{dashed:?}");
    assert!(dashed.pieces[0].text.contains("[a--b]"), "{dashed:?}");
    assert!(dashed.refusal.is_none(), "{dashed:?}");
    // 方括号里的 TOP/LIMIT 不当关键字：行数上限照常改写为 TOP。
    assert_eq!(
        exec::cap_result_sql("sqlserver", "SELECT [limit] FROM t", 10),
        "SELECT TOP (11) [limit] FROM t"
    );

    // TRY/CATCH 是一条语句：END TRY 后的分号不切分，END CATCH 才收束。
    let try_catch = split::split_sql("sqlserver", "BEGIN TRY\n  SELECT 1;\nEND TRY\nBEGIN CATCH\n  SELECT 2;\nEND CATCH\nSELECT 3;");
    assert_eq!(try_catch.pieces.len(), 2, "{try_catch:?}");
    assert!(try_catch.pieces[0].text.contains("BEGIN TRY"));
    assert!(try_catch.pieces[0].text.trim_end().ends_with("END CATCH"));
    // END TRY 后面带分号的写法同样不提前切分。
    let semicolon = split::split_sql("sqlserver", "BEGIN TRY\n  SELECT 1;\nEND TRY;\nBEGIN CATCH\n  SELECT 2;\nEND CATCH;");
    assert_eq!(semicolon.pieces.len(), 1, "{semicolon:?}");

    // GO 是批次分隔：不进语句文本，前后语句各自成条；GO n 计数形式同样跳过。
    let batches = split::split_sql("sqlserver", "SELECT 1;\nGO\nSELECT 2\nGO 5\nSELECT 3;");
    assert_eq!(batches.pieces.len(), 3, "{batches:?}");
    assert!(batches.pieces.iter().all(|piece| !piece.text.trim_start().eq_ignore_ascii_case("GO") && !piece.text.contains("GO")));
    // 行中的 GO 只是普通词（如表名前缀），不切分。
    let mid_line = split::split_sql("sqlserver", "SELECT 1 FROM gopher WHERE x = 1;");
    assert_eq!(mid_line.pieces.len(), 1, "{mid_line:?}");
    // 语句之间裸 GO 且无分号：GO 前的语句在 GO 行收束。
    let no_semi = split::split_sql("sqlserver", "UPDATE t SET n = 1\nGO\nUPDATE u SET m = 2\nGO");
    assert_eq!(no_semi.pieces.len(), 2, "{no_semi:?}");
}

struct Pausing {
    paused: Mutex<Duration>,
}

#[async_trait::async_trait]
impl StatementDriver for Pausing {
    async fn run(&self, sql: &str, _limit: usize, _cancel: &Arc<AtomicBool>) -> exec::DriverReport {
        let started = std::time::Instant::now();
        tokio::time::sleep(Duration::from_millis(60)).await;
        // 调度繁忙时 sleep 会晚于期限返回，暂停时长必须包含实际等待。
        *self.paused.lock().expect("paused") += started.elapsed();
        exec::DriverReport::unchanged(sql, Ok(ok_count()))
    }

    fn paused(&self) -> Duration {
        *self.paused.lock().expect("paused")
    }
}

#[tokio::test]
async fn each_statement_reports_its_own_elapsed_time_and_error_position() {
    let driver = Scripted {
        steps: Mutex::new(vec![
            Ok(ok_count()),
            Err(StatementStop::Error { message: "syntax error".to_owned(), at: Some(exec::ErrorAt::Line(2)), code: Some("42601".to_owned()) }),
            Ok(ok_count()),
        ]),
        calls: Mutex::new(Vec::new()),
    };
    let flag = Arc::new(AtomicBool::new(false));
    let parts = vec![piece("INSERT INTO t VALUES (1)"), piece("SELECT\nnope"), piece("DELETE FROM t")];
    let report = run_batch(&parts, 100, &flag, false, &driver, None).await;
    assert!(report.statements[0].elapsed_ms.is_some_and(|ms| ms >= 1), "执行过的语句至少 1 ms");
    assert!(report.statements[0].error_at.is_none());
    assert_eq!(report.statements[1].status, "error");
    assert!(report.statements[1].elapsed_ms.is_some(), "失败的语句也执行过");
    assert_eq!(report.statements[1].error_at, Some(exec::ErrorAt::Line(2)));
    assert_eq!(report.statements[1].error_code.as_deref(), Some("42601"), "驱动给出的错误码随语句报告带出");
    assert_eq!(report.statements[0].error_code, None);
    assert_eq!(report.statements[2].status, "skipped");
    assert_eq!(report.statements[2].elapsed_ms, None, "没有执行的语句没有耗时");

    let cancelled = Scripted {
        steps: Mutex::new(vec![Err(StatementStop::Cancelled { message: "canceling statement due to user request".to_owned() })]),
        calls: Mutex::new(Vec::new()),
    };
    let report = run_batch(&[piece("SELECT pg_sleep(10)")], 100, &flag, false, &cancelled, None).await;
    assert_eq!(report.statements[0].status, "cancelled");
    assert_eq!(report.statements[0].elapsed_ms, None, "取消的语句只标已取消，不给耗时");

    let waiting = Pausing { paused: Mutex::new(Duration::ZERO) };
    let report = run_batch(&[piece("SELECT 1")], 100, &flag, false, &waiting, None).await;
    let spent = report.statements[0].elapsed_ms.expect("执行过");
    assert!(spent < 50, "等待继续读取的时间不算进耗时：{spent} ms");
}

#[test]
fn error_positions_are_parsed_per_driver() {
    // PostgreSQL：position 从 1 开始按字符计数，换成语句内的字符偏移；越界停在末尾。
    assert_eq!(crate::adapters::postgres::exec::pg_error_char(8, "SELECT nope FROM t"), Some(7));
    assert_eq!(crate::adapters::postgres::exec::pg_error_char(1, "SELECT"), Some(0));
    assert_eq!(crate::adapters::postgres::exec::pg_error_char(3, "选择 x"), Some(2), "按字符而不是字节");
    assert_eq!(crate::adapters::postgres::exec::pg_error_char(99, "SELECT"), Some(6));
    assert_eq!(crate::adapters::postgres::exec::pg_error_char(0, "SELECT"), None);
    // MySQL / MariaDB：从文本里取 "at line N"。
    let mysql = "ERROR 1064 (42000): You have an error in your SQL syntax; check the manual that corresponds to your MySQL server version for the right syntax to use near 'FORM t' at line 2";
    assert_eq!(crate::adapters::mysql::exec::mysql_error_line(mysql), Some(2));
    assert_eq!(crate::adapters::mysql::exec::mysql_error_line("Table 'shop.t' doesn't exist"), None);
    assert_eq!(crate::adapters::mysql::exec::mysql_error_line("near 'x' at line 0"), None);
    // SQL Server：服务器错误的行号，0 表示没有。
    assert_eq!(crate::adapters::sqlserver::exec::sqlserver_error_line(3), Some(3));
    assert_eq!(crate::adapters::sqlserver::exec::sqlserver_error_line(0), None);
    // 错误发生在存储过程/触发器/函数里时，行号属于模块定义，不给位置，退回整条语句标错。
    assert_eq!(crate::adapters::sqlserver::exec::sqlserver_error_at(3, ""), Some(exec::ErrorAt::Line(3)));
    assert_eq!(crate::adapters::sqlserver::exec::sqlserver_error_at(0, ""), None);
    assert_eq!(crate::adapters::sqlserver::exec::sqlserver_error_at(12, "dbo.audit_orders"), None, "模块内的行号不映射到用户语句");
    assert_eq!(crate::adapters::sqlserver::exec::sqlserver_error_at(1, "trg_orders"), None);
    // 执行前追加的 LIMIT 不影响原文里的位置。
    let original = "SELECT nope FROM t";
    let sent = "SELECT nope FROM t LIMIT 101";
    assert_eq!(exec::map_rewritten_char(original, sent, 7), 7);
    assert_eq!(exec::map_rewritten_char(original, sent, 25), original.chars().count());
    let original = "SELECT a FROM t LIMIT 5000 OFFSET bad";
    let sent = "SELECT a FROM t LIMIT 101 OFFSET bad";
    assert_eq!(exec::map_rewritten_char(original, sent, sent.find("bad").unwrap()), original.find("bad").unwrap());
}

#[tokio::test]
async fn real_sqlite_statements_carry_elapsed_time_and_no_error_position() {
    use super::commands::{request_cancel, start_run, QueryState};

    let dir = std::env::temp_dir().join(format!("rillsql-elapsed-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let service = notes_service(&dir).await;
    let state = QueryState::new(dir.join("history.sqlite"));
    let started = start_run(&service, &state, run_request("INSERT INTO t (id, name) VALUES (1, 'a');\nSELECT id, name FROM t;\nSELECT nope FROM t;", "main"))
        .await
        .unwrap();
    assert!(started.statements.iter().all(|statement| statement.elapsed_ms.is_none()), "还没执行时没有耗时");
    let finished = wait_done(&state, &started.id).await;
    assert_eq!(finished.statements[1].status, "ok");
    for statement in &finished.statements {
        assert!(statement.elapsed_ms.is_some_and(|ms| ms > 0), "{:?}", statement.elapsed_ms);
    }
    assert_eq!(finished.statements[2].status, "error");
    assert_eq!(finished.statements[2].error_at, None, "SQLite 不给位置");

    let slow = start_run(
        &service,
        &state,
        run_request("WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 5000000) SELECT sum(x) FROM c", "main"),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    request_cancel(&service, &state, &slow.id).await.unwrap();
    let ended = wait_done(&state, &slow.id).await;
    assert_eq!(ended.status, "cancelled");
    assert_eq!(ended.statements[0].status, "cancelled");
    assert_eq!(ended.statements[0].elapsed_ms, None, "取消的语句不给耗时");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn desk_maps_statement_errors_to_document_offsets_and_lines() {
    use crate::application::tasks::TaskBoard;
    use crate::query::assist::RunScope;
    use crate::query::commands::{RunDto, StatementDto};
    use crate::query::desk::{error_mark, BeginRun, Desk, ErrorMark, Target, Ticket};

    // 纯换算：字符偏移 → 字节偏移；行号加上语句起点所在行，且不越过语句最后一行；没有位置就整条语句。
    let draft = "SELECT 1;\nSELECT 'é', nope\n  FROM t;";
    let start = draft.find("SELECT 'é'").unwrap();
    let range = (start, draft.len());
    assert_eq!(error_mark(draft, range, Some(exec::ErrorAt::Char(12))), ErrorMark::Offset(draft.find("nope").unwrap()));
    assert_eq!(error_mark(draft, range, Some(exec::ErrorAt::Line(2))), ErrorMark::Line(2));
    assert_eq!(error_mark(draft, range, Some(exec::ErrorAt::Line(9))), ErrorMark::Line(2), "不越过语句最后一行");
    assert_eq!(error_mark(draft, range, None), ErrorMark::Statement);

    let tasks = TaskBoard::new();
    let mut desk = Desk::new();
    let target = Target {
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: "main".to_owned(),
        engine: "sqlite".to_owned(),
        schema: String::new(),
        environment: String::new(),
        server_version: None,
    };
    desk.import_tab("a", draft, Some(target));
    desk.focus_tab(Some("a"));
    let BeginRun::Ready(prepared) = desk.begin_run(&tasks, RunScope::Script, None) else {
        panic!("应开始执行");
    };
    let ticket = Ticket { tab_id: prepared.tab_id.clone(), token: prepared.token, version: prepared.version };
    let statement = |sql: &str, status: &str, at: Option<exec::ErrorAt>| StatementDto {
        sql: sql.to_owned(),
        sent_sql: None,
        rewrite: None,
        status: status.to_owned(),
        message: String::new(),
        columns: Vec::new(),
        column_types: Vec::new(),
        rows: Vec::new(),
        truncated: false,
        affected: None,
        elapsed_ms: Some(3),
        error_at: at,
        error_code: None,
        transaction: None,
        result_set: None,
    };
    let run = RunDto {
        id: "run".to_owned(),
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: "main".to_owned(),
        engine: "sqlite".to_owned(),
        status: "error".to_owned(),
        cancel: String::new(),
        cancel_message: String::new(),
        via_ssh: false,
        fresh_channel: false,
        statements: vec![
            statement("SELECT 1;", "ok", None),
            statement("SELECT 'é', nope\n  FROM t;", "error", Some(exec::ErrorAt::Line(2))),
        ],
        transaction: None,
        started_at: String::new(),
        elapsed_ms: 6,
    };
    desk.finish_polled(&tasks, &ticket, Ok(run), "已取消");
    let results = desk.results();
    assert_eq!(results[0].range, Some((0, "SELECT 1;".len())));
    assert_eq!(results[0].error_mark, None, "成功的语句没有错误标记");
    assert_eq!(results[0].elapsed_ms, Some(3));
    assert_eq!(results[1].range, Some(range));
    assert_eq!(results[1].error_mark, Some(ErrorMark::Line(2)));
}

#[test]
fn desk_marks_whole_statement_when_sent_sql_differs_from_draft() {
    use crate::application::tasks::TaskBoard;
    use crate::query::assist::RunScope;
    use crate::query::commands::{RunDto, StatementDto};
    use crate::query::desk::{BeginRun, Desk, ErrorMark, PreparedRun, Target, Ticket};
    use std::collections::HashMap;

    let statement = |sql: &str, status: &str, at: Option<exec::ErrorAt>, truncated: bool| StatementDto {
        sql: sql.to_owned(),
        sent_sql: None,
        rewrite: None,
        status: status.to_owned(),
        message: String::new(),
        columns: Vec::new(),
        column_types: Vec::new(),
        rows: Vec::new(),
        truncated,
        affected: None,
        elapsed_ms: Some(2),
        error_at: at,
        error_code: None,
        transaction: None,
        result_set: None,
    };
    let run = |status: &str, statements: Vec<StatementDto>| RunDto {
        id: "run".to_owned(),
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: "main".to_owned(),
        engine: "sqlite".to_owned(),
        status: status.to_owned(),
        cancel: String::new(),
        cancel_message: String::new(),
        via_ssh: false,
        fresh_channel: false,
        statements,
        transaction: None,
        started_at: String::new(),
        elapsed_ms: 4,
    };
    let ticket = |prepared: &PreparedRun| Ticket { tab_id: prepared.tab_id.clone(), token: prepared.token, version: prepared.version };
    let target = Target {
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: "main".to_owned(),
        engine: "sqlite".to_owned(),
        schema: String::new(),
        environment: String::new(),
        server_version: None,
    };
    let tasks = TaskBoard::new();
    let values: HashMap<String, String> = [("id".to_owned(), "42".to_owned())].into_iter().collect();

    // 变量替换后再执行：送出的文本和草稿不同，但条数一致，按序号对应，失败语句整条标错。
    let draft = "SELECT 1;\nSELECT *\n  FROM t WHERE id = :id;";
    let mut desk = Desk::new();
    desk.import_tab("a", draft, Some(target.clone()));
    desk.focus_tab(Some("a"));
    assert!(matches!(desk.begin_run(&tasks, RunScope::Script, None), BeginRun::Variables(_)), "有变量时先确认");
    let sent = desk.confirm_variables(&values).expect("替换后的文本");
    assert_ne!(sent, draft);
    let BeginRun::Ready(prepared) = desk.begin_run(&tasks, RunScope::Script, Some(sent.clone())) else {
        panic!("确认变量后应开始执行");
    };
    let pieces = crate::query::split::split_sql("sqlite", &sent).pieces;
    assert_eq!(pieces.len(), 2);
    desk.finish_polled(
        &tasks,
        &ticket(&prepared),
        Ok(run(
            "error",
            vec![
                statement(&pieces[0].text, "ok", None, false),
                statement(&pieces[1].text, "error", Some(exec::ErrorAt::Line(2)), false),
            ],
        )),
        "已取消",
    );
    let results = desk.results();
    assert_eq!(results[0].range, Some((0, "SELECT 1;".len())), "没被替换的语句照常对上");
    assert_eq!(results[0].error_mark, None);
    let second = draft.find("SELECT *").unwrap();
    assert_eq!(results[1].range, Some((second, draft.len())), "替换过的语句按序号对回草稿");
    assert_eq!(results[1].error_mark, Some(ErrorMark::Statement), "送出的文本变了，位置不可信，整条语句标错");

    // 加载更多：重放上一条结果的文本，对应它在草稿里的范围；失败时同样整条标错。
    let draft = "\n\nSELECT *\n  FROM t WHERE id = :id;";
    let mut desk = Desk::new();
    desk.import_tab("b", draft, Some(target.clone()));
    desk.focus_tab(Some("b"));
    assert!(matches!(desk.begin_run(&tasks, RunScope::Script, None), BeginRun::Variables(_)));
    let sent = desk.confirm_variables(&values).expect("替换后的文本");
    let BeginRun::Ready(prepared) = desk.begin_run(&tasks, RunScope::Script, Some(sent.clone())) else {
        panic!("确认变量后应开始执行");
    };
    let piece = crate::query::split::split_sql("sqlite", &sent).pieces.remove(0).text;
    desk.finish_polled(&tasks, &ticket(&prepared), Ok(run("ok", vec![statement(&piece, "ok", None, true)])), "已取消");
    let start = draft.find("SELECT").unwrap();
    assert_eq!(desk.results()[0].range, Some((start, draft.len())));
    let BeginRun::Ready(prepared) = desk.begin_run_limited(&tasks, RunScope::Current, Some(piece.clone()), 200) else {
        panic!("加载更多应开始执行");
    };
    desk.finish_polled(
        &tasks,
        &ticket(&prepared),
        Ok(run("error", vec![statement(&piece, "error", Some(exec::ErrorAt::Line(2)), false)])),
        "已取消",
    );
    let results = desk.results();
    assert_eq!(results[0].range, Some((start, draft.len())), "加载更多的结果仍对回草稿");
    assert_eq!(results[0].error_mark, Some(ErrorMark::Statement));

    // 未替换的加载更多：送出的就是草稿里那条，驱动给的位置照常换算。
    let draft = "SELECT 1;\nSELECT nope\n  FROM t;";
    let mut desk = Desk::new();
    desk.import_tab("c", draft, Some(target));
    desk.focus_tab(Some("c"));
    let start = draft.find("SELECT nope").unwrap();
    desk.set_selection(start, start, start);
    let BeginRun::Ready(prepared) = desk.begin_run(&tasks, RunScope::Current, None) else {
        panic!("应开始执行");
    };
    let piece = "SELECT nope\n  FROM t;";
    desk.finish_polled(&tasks, &ticket(&prepared), Ok(run("ok", vec![statement(piece, "ok", None, true)])), "已取消");
    assert_eq!(desk.results()[0].range, Some((start, draft.len())));
    desk.set_selection(0, 0, 0);
    let BeginRun::Ready(prepared) = desk.begin_run_limited(&tasks, RunScope::Current, Some(piece.to_owned()), 200) else {
        panic!("加载更多应开始执行");
    };
    desk.finish_polled(
        &tasks,
        &ticket(&prepared),
        Ok(run("error", vec![statement(piece, "error", Some(exec::ErrorAt::Line(2)), false)])),
        "已取消",
    );
    let results = desk.results();
    assert_eq!(results[0].range, Some((start, draft.len())), "光标移走后加载更多仍按上次的范围");
    assert_eq!(results[0].error_mark, Some(ErrorMark::Line(2)), "文本一致时按驱动位置标行");
}

/// 测试替身：像真实驱动一样先做读取上限改写，再把实际发出的语句和结果一起交回。
struct Capping {
    engine: &'static str,
}

#[async_trait::async_trait]
impl StatementDriver for Capping {
    async fn run(&self, sql: &str, limit: usize, _cancel: &Arc<AtomicBool>) -> exec::DriverReport {
        let capped = exec::cap_result_sql_detail(self.engine, sql, limit);
        // 自动追加的 LIMIT 接在残缺语句后面时，服务端报的就是这条错误。
        let outcome = if sql.trim_end().ends_with("WHERE") {
            Err(StatementStop::Error { message: "near \"LIMIT\": syntax error".to_owned(), at: None, code: None })
        } else {
            Ok(ok_count())
        };
        exec::DriverReport { sent: capped, outcome, failure: None, extras: Default::default() }
    }
}

#[test]
fn auto_limit_rewrite_reports_what_it_appended_or_replaced() {
    // 追加 LIMIT：改写规则和读取上限不变，只多一份可以解释的说明。
    let appended = exec::cap_result_sql_detail("postgres", "SELECT * FROM logs WHERE", 100);
    assert_eq!(appended.sql, "SELECT * FROM logs WHERE LIMIT 101");
    assert_eq!(appended.sql, exec::cap_result_sql("postgres", "SELECT * FROM logs WHERE", 100), "规则与 cap_result_sql 一致");
    let rewrite = appended.rewrite.expect("自动追加 LIMIT 要留下来源");
    assert_eq!(rewrite.kind, "limit");
    assert_eq!(rewrite.at, "SELECT * FROM logs WHERE".len(), "插入点是原语句末尾");
    assert_eq!(rewrite.replaced, 0);
    assert_eq!(rewrite.added, " LIMIT 101");
    assert_eq!(rewrite.inserted_len(), rewrite.added.len());

    // 替换数字：只认被替换数字的起点，后面 OFFSET 之类原样保留。
    let replaced = exec::cap_result_sql_detail("postgres", "SELECT a FROM t LIMIT 5000 OFFSET 0", 100);
    assert_eq!(replaced.sql, "SELECT a FROM t LIMIT 101 OFFSET 0");
    let rewrite = replaced.rewrite.expect("改小读取上限也是自动改写");
    assert_eq!(rewrite.kind, "limit");
    assert_eq!(rewrite.at, "SELECT a FROM t LIMIT ".len());
    assert_eq!(rewrite.replaced, 4);
    assert_eq!(rewrite.added, "101");

    // 手写 LIMIT 且不超过上限：一个字都不改，也不附说明。
    let untouched = exec::cap_result_sql_detail("postgres", "SELECT * FROM t LIMIT 10", 100);
    assert_eq!(untouched.sql, "SELECT * FROM t LIMIT 10");
    assert!(untouched.rewrite.is_none(), "手写 LIMIT 不附加改写说明");

    // 读取上限仍是 1..=10000，探测用的是 cap + 1。
    let ceiling = exec::clamp_limit(50_000);
    assert_eq!(ceiling, 10_000);
    assert_eq!(exec::cap_result_sql_detail("postgres", "SELECT 1", ceiling).sql, "SELECT 1 LIMIT 10001");

    // SQL Server 走 TOP，追加在第一个 SELECT 之后。
    let top = exec::cap_result_sql_detail("sqlserver", "SELECT [limit] FROM t", 10);
    assert_eq!(top.sql, "SELECT TOP (11) [limit] FROM t");
    let rewrite = top.rewrite.expect("TOP 也是自动改写");
    assert_eq!(rewrite.kind, "top");
    assert_eq!(rewrite.at, "SELECT".len());
    assert_eq!(rewrite.added, " TOP (11)");
    // 已有 TOP：只换数字。
    let top_replaced = exec::cap_result_sql_detail("sqlserver", "SELECT TOP (5000) id FROM t", 10);
    assert_eq!(top_replaced.sql, "SELECT TOP (11) id FROM t");
    assert_eq!(top_replaced.rewrite.expect("换数字").kind, "top");

    // FETCH NEXT 已有：只改行数。
    let fetch = exec::cap_result_sql_detail("postgres", "SELECT * FROM t OFFSET 0 ROWS FETCH NEXT 5000 ROWS ONLY", 100);
    assert_eq!(fetch.sql, "SELECT * FROM t OFFSET 0 ROWS FETCH NEXT 101 ROWS ONLY");
    assert_eq!(fetch.rewrite.expect("FETCH 行数改写").kind, "fetch");

    // 不会返回行的语句不改写。
    let write = exec::cap_result_sql_detail("postgres", "UPDATE t SET n = 1", 100);
    assert_eq!(write.sql, "UPDATE t SET n = 1");
    assert!(write.rewrite.is_none());
}

#[tokio::test]
async fn batch_carries_the_statement_it_sent_and_the_rewrite_source() {
    let flag = Arc::new(AtomicBool::new(false));
    let driver = Capping { engine: "sqlite" };
    let parts = vec![piece("SELECT * FROM t"), piece("SELECT * FROM logs WHERE"), piece("SELECT 3")];
    let report = run_batch(&parts, 100, &flag, false, &driver, None).await;
    // 成功语句：原文、实际语句、改写来源三者都能对上。
    assert_eq!(report.statements[0].status, "ok");
    assert_eq!(report.statements[0].sql, "SELECT * FROM t");
    assert_eq!(report.statements[0].sent_sql, "SELECT * FROM t LIMIT 101");
    let rewrite = report.statements[0].rewrite.clone().expect("自动追加 LIMIT");
    assert_eq!(rewrite.at, "SELECT * FROM t".len());
    // 失败语句：错误原文照旧，实际发出的语句仍然带回来。
    assert_eq!(report.statements[1].status, "error");
    assert_eq!(report.statements[1].sql, "SELECT * FROM logs WHERE");
    assert_eq!(report.statements[1].sent_sql, "SELECT * FROM logs WHERE LIMIT 101");
    assert!(report.statements[1].rewrite.is_some(), "失败的那条也要说明被改写过");
    assert!(report.statements[1].message.contains("near"), "{:?}", report.statements[1].message);
    // 没有执行的语句：没有实际语句，也没有改写。
    assert_eq!(report.statements[2].status, "skipped");
    assert!(report.statements[2].sent_sql.is_empty());
    assert!(report.statements[2].rewrite.is_none());
}

#[tokio::test]
async fn hand_written_limit_keeps_the_sent_sql_and_adds_no_note() {
    let flag = Arc::new(AtomicBool::new(false));
    let driver = Capping { engine: "sqlite" };
    let report = run_batch(&[piece("SELECT * FROM t LIMIT 10")], 100, &flag, false, &driver, None).await;
    assert_eq!(report.statements[0].status, "ok");
    assert!(report.statements[0].rewrite.is_none(), "手写 LIMIT 不附加改写说明");
    assert_eq!(report.statements[0].sent_sql, "SELECT * FROM t LIMIT 10");
}

/// 自动改写的位置映射回草稿：插入点落在原语句边界上，定位和错误标记用的是同一段范围。
#[test]
fn rewritten_error_maps_back_to_the_draft_statement_boundary() {
    use crate::application::tasks::TaskBoard;
    use crate::query::assist::RunScope;
    use crate::query::commands::{RunDto, StatementDto};
    use crate::query::desk::{BeginRun, Desk, ErrorMark, Target, Ticket};

    let draft = "SELECT 1;\nSELECT * FROM logs WHERE;";
    let second = "SELECT * FROM logs WHERE;";
    let start = draft.find("SELECT * FROM logs").unwrap();
    let rewrite = exec::AutoRewrite { kind: "limit".to_owned(), at: "SELECT * FROM logs WHERE".len(), replaced: 0, added: " LIMIT 101".to_owned() };
    let statement = |status: &str, at: Option<exec::ErrorAt>| StatementDto {
        sql: second.to_owned(),
        sent_sql: Some("SELECT * FROM logs WHERE LIMIT 101;".to_owned()),
        rewrite: Some(rewrite.clone()),
        status: status.to_owned(),
        message: if status == "error" { "near \"LIMIT\": syntax error".to_owned() } else { String::new() },
        columns: Vec::new(),
        column_types: Vec::new(),
        rows: Vec::new(),
        truncated: false,
        affected: None,
        elapsed_ms: Some(2),
        error_at: at,
        error_code: None,
        transaction: None,
        result_set: None,
    };
    let run = |statements: Vec<StatementDto>| RunDto {
        id: "run".to_owned(),
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: "main".to_owned(),
        engine: "sqlite".to_owned(),
        status: "error".to_owned(),
        cancel: String::new(),
        cancel_message: String::new(),
        via_ssh: false,
        fresh_channel: false,
        statements,
        transaction: None,
        started_at: String::new(),
        elapsed_ms: 3,
    };
    let target = Target {
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: "main".to_owned(),
        engine: "sqlite".to_owned(),
        schema: String::new(),
        environment: String::new(),
        server_version: None,
    };
    let tasks = TaskBoard::new();
    let mut desk = Desk::new();
    desk.import_tab("a", draft, Some(target));
    desk.focus_tab(Some("a"));
    let BeginRun::Ready(prepared) = desk.begin_run(&tasks, RunScope::Script, None) else {
        panic!("应开始执行");
    };
    let ticket = Ticket { tab_id: prepared.tab_id.clone(), token: prepared.token, version: prepared.version };
    // SQLite 不给错误位置：整条语句标错，但仍然能指出自动插入的边界。
    desk.finish_polled(&tasks, &ticket, Ok(run(vec![statement("ok", None), statement("error", None)])), "已取消");
    let results = desk.results();
    assert_eq!(results[1].sql, second, "展示和定位都用草稿里的原文");
    assert_eq!(results[1].sent_sql.as_deref(), Some("SELECT * FROM logs WHERE LIMIT 101;"), "同时带回实际执行的语句");
    assert_eq!(results[1].range, Some((start, draft.len())), "落在草稿里这条语句上");
    assert_eq!(results[1].rewrite_anchor, Some(start + "SELECT * FROM logs WHERE".len()), "改写位置映射到原草稿的插入边界");
    assert_eq!(results[1].error_mark, Some(ErrorMark::Statement), "驱动没给位置时不伪造精确位置");
    let anchor = results[1].rewrite_anchor.unwrap();
    assert_eq!(&draft[anchor..], ";", "边界正好在语句末尾的分号之前");

    // 驱动给了位置（行号）时照常换算，改写位置不受影响。
    let BeginRun::Ready(prepared) = desk.begin_run(&tasks, RunScope::Script, None) else {
        panic!("应再次执行");
    };
    let ticket = Ticket { tab_id: prepared.tab_id.clone(), token: prepared.token, version: prepared.version };
    desk.finish_polled(&tasks, &ticket, Ok(run(vec![statement("ok", None), statement("error", Some(exec::ErrorAt::Line(1)))])), "已取消");
    let results = desk.results();
    assert_eq!(results[1].error_mark, Some(ErrorMark::Line(1)));
    assert_eq!(results[1].rewrite_anchor, Some(start + "SELECT * FROM logs WHERE".len()));
}

/// 没有可查询的会话时一条语句都没发出去：报告里的"实际语句"必须是原输入，
/// 不能带上一处从未执行的改写（否则界面会给没发出去的语句显示追加的 LIMIT）。
#[tokio::test]
async fn scripted_session_reports_no_rewrite_because_nothing_was_sent() {
    use crate::connections::{CancelTransport, CatalogSession};

    let driver = exec::LiveDriver::attach(CatalogSession::Scripted, "sqlite", "main", CancelTransport::None, None)
        .await
        .expect("脚本会话不需要真实连接");
    let flag = Arc::new(AtomicBool::new(false));
    let report = driver.run("SELECT * FROM logs", 2, &flag).await;
    assert!(report.outcome.is_err(), "没有可查询的会话必须如实报错");
    assert_eq!(report.sent.sql, "SELECT * FROM logs", "没发出去的语句原样带回");
    assert_eq!(report.sent.rewrite, None, "没发出去的语句不带改写信息");
}

/// 过期的执行不能把它的改写信息写到更新的结果上。
#[test]
fn a_stale_run_cannot_write_rewrite_metadata_onto_newer_results() {
    use crate::application::tasks::TaskBoard;
    use crate::query::assist::RunScope;
    use crate::query::commands::{RunDto, StatementDto};
    use crate::query::desk::{BeginRun, Desk, Target, Ticket};

    let draft = "SELECT * FROM logs WHERE;";
    let rewrite = |added: &str| exec::AutoRewrite { kind: "limit".to_owned(), at: draft.len() - 1, replaced: 0, added: added.to_owned() };
    let statement = |sent: &str, added: &str| StatementDto {
        sql: draft.to_owned(),
        sent_sql: Some(sent.to_owned()),
        rewrite: Some(rewrite(added)),
        status: "error".to_owned(),
        message: "near \"LIMIT\": syntax error".to_owned(),
        columns: Vec::new(),
        column_types: Vec::new(),
        rows: Vec::new(),
        truncated: false,
        affected: None,
        elapsed_ms: Some(1),
        error_at: None,
        error_code: None,
        transaction: None,
        result_set: None,
    };
    let run = |sql: &str, sent: &str, added: &str| RunDto {
        id: sql.to_owned(),
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: "main".to_owned(),
        engine: "sqlite".to_owned(),
        status: "error".to_owned(),
        cancel: String::new(),
        cancel_message: String::new(),
        via_ssh: false,
        fresh_channel: false,
        statements: vec![statement(sent, added)],
        transaction: None,
        started_at: String::new(),
        elapsed_ms: 1,
    };
    let target = Target {
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: "main".to_owned(),
        engine: "sqlite".to_owned(),
        schema: String::new(),
        environment: String::new(),
        server_version: None,
    };
    let tasks = TaskBoard::new();
    let mut desk = Desk::new();
    desk.import_tab("a", draft, Some(target));
    desk.focus_tab(Some("a"));
    let BeginRun::Ready(first) = desk.begin_run(&tasks, RunScope::Script, None) else {
        panic!("应开始执行");
    };
    let first_ticket = Ticket { tab_id: first.tab_id.clone(), token: first.token, version: first.version };
    desk.finish_polled(&tasks, &first_ticket, Ok(run("run-1", "SELECT * FROM logs WHERE LIMIT 101;", " LIMIT 101")), "已取消");
    assert_eq!(desk.results()[0].sent_sql.as_deref(), Some("SELECT * FROM logs WHERE LIMIT 101;"));

    let BeginRun::Ready(second) = desk.begin_run(&tasks, RunScope::Script, None) else {
        panic!("应再次执行");
    };
    let second_ticket = Ticket { tab_id: second.tab_id.clone(), token: second.token, version: second.version };
    desk.finish_polled(&tasks, &second_ticket, Ok(run("run-2", "SELECT * FROM logs WHERE LIMIT 201;", " LIMIT 201")), "已取消");
    // 旧的执行结果晚到：不能把 LIMIT 101 的说明写回新结果。
    desk.finish_polled(&tasks, &first_ticket, Ok(run("run-1", "SELECT * FROM logs WHERE LIMIT 101;", " LIMIT 101")), "已取消");
    let results = desk.results();
    assert_eq!(results[0].sent_sql.as_deref(), Some("SELECT * FROM logs WHERE LIMIT 201;"), "过期结果不覆盖新结果");
    assert_eq!(results[0].rewrite.as_ref().map(|rewrite| rewrite.added.as_str()), Some(" LIMIT 201"));
}

/// 同一段长错误在消息页只记一次：整次执行的失败和语句记录说同一件事时不重复堆。
#[test]
fn the_same_long_error_is_logged_once_in_messages() {
    use crate::application::tasks::TaskBoard;
    use crate::query::desk::{Desk, StatementView, Target};

    let target = Target {
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: "main".to_owned(),
        engine: "sqlite".to_owned(),
        schema: String::new(),
        environment: String::new(),
        server_version: None,
    };
    let tasks = TaskBoard::new();
    let mut desk = Desk::new();
    desk.import_tab("a", "select nope;", Some(target));
    desk.focus_tab(Some("a"));
    // 语句记录里已经有这段错误。
    desk.plant_run(vec![StatementView {
        sql: "select nope;".to_owned(),
        sent_sql: None,
        rewrite: None,
        rewrite_anchor: None,
        status: "error".to_owned(),
        message: "no such column: nope".to_owned(),
        columns: Vec::new(),
        rows: Vec::new(),
        truncated: false,
        affected: None,
        elapsed_ms: Some(1),
        error_at: None,
        error_code: None,
        range: None,
        error_mark: None,
        result_set: None,
    }]);
    let ticket = desk.issue(&tasks).expect("ticket");
    assert!(desk.stop_run("a", ticket.token, "no such column: nope"));
    assert_eq!(desk.log().len(), 1, "同一段长错误不堆两条：{:?}", desk.log());
    assert_eq!(desk.log()[0].message, "no such column: nope");

    // 不同的整体失败照常单独记一条。
    let mut desk = Desk::new();
    desk.import_tab("b", "select 1;", Some(Target {
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: "main".to_owned(),
        engine: "sqlite".to_owned(),
        schema: String::new(),
        environment: String::new(),
        server_version: None,
    }));
    desk.focus_tab(Some("b"));
    desk.plant_run(vec![StatementView {
        sql: "select 1;".to_owned(),
        sent_sql: None,
        rewrite: None,
        rewrite_anchor: None,
        status: "ok".to_owned(),
        message: String::new(),
        columns: vec!["n".to_owned()],
        rows: vec![vec![Some("1".to_owned())]],
        truncated: false,
        affected: None,
        elapsed_ms: Some(1),
        error_at: None,
        error_code: None,
        range: None,
        error_mark: None,
        result_set: None,
    }]);
    let ticket = desk.issue(&tasks).expect("ticket");
    assert!(desk.stop_run("b", ticket.token, "连接中断，无法确认写入是否完成。"));
    assert_eq!(desk.log().len(), 2, "不同的失败仍然要记下来：{:?}", desk.log());
}

/// 回归：SQLite 的浮点结果（avg()、带小数的表达式、REAL 列）在查询结果里是数字，不是 NULL；
/// TEXT 列里存的数字也按实际值读出来。
#[tokio::test]
async fn sqlite_query_results_keep_real_values() {
    use crate::connections::Service;

    let dir = std::env::temp_dir().join(format!("rillsql-query-real-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let database = dir.join("numbers.sqlite");
    let setup = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&database).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, price REAL, label TEXT)").execute(&setup).await.unwrap();
    sqlx::query("INSERT INTO t VALUES (1, 1.5, 'a'), (2, 2.5, 42)").execute(&setup).await.unwrap();
    setup.close().await;

    let service = Arc::new(Service::new_for_test(dir.join("connections.sqlite")));
    service.open_sqlite_for_test("numbers", "数字", &database).await.unwrap();
    let session = service.catalog_session("numbers").unwrap();
    let driver = exec::LiveDriver::attach(session, "sqlite", "main", service.cancel_transport("numbers").unwrap(), None).await.unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let rows = driver.run("SELECT avg(price), price * 2, price, label FROM t ORDER BY id", 10, &cancel).await.outcome.unwrap();
    assert_eq!(rows.rows[0][0], Some("2".to_owned()), "avg() 是浮点结果：{:?}", rows.rows);
    let rows = driver.run("SELECT price * 2, price, label FROM t ORDER BY id", 10, &cancel).await.outcome.unwrap();
    assert_eq!(rows.rows[0][0], Some("3".to_owned()));
    assert_eq!(rows.rows[1][1], Some("2.5".to_owned()));
    assert_eq!(rows.rows[1][2], Some("42".to_owned()), "TEXT 列里的数字按实际值读出");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 回归：中文输入法的全角分号“；”在字符串与注释之外按语句结束符处理；
/// 语句文本里不带它，自动追加的 LIMIT 也不会接在它后面（否则数据库报 near "；" 语法错误）。
#[test]
fn fullwidth_semicolon_ends_a_statement_outside_strings() {
    let sql = "SELECT * FROM thread_items WHERE turn_id='01a1；89'；\nSELECT 1;";
    let report = split::split_sql("sqlite", sql);
    assert!(report.refusal.is_none(), "{report:?}");
    let texts: Vec<&str> = report.pieces.iter().map(|piece| piece.text.as_str()).collect();
    assert_eq!(texts, vec!["SELECT * FROM thread_items WHERE turn_id='01a1；89'", "SELECT 1;"], "字符串里的全角分号原样保留");
    // 片段区间仍指向编辑器里的原文，定位出错位置不偏移。
    let first = &report.pieces[0];
    assert_eq!(&sql[first.start..first.end], first.text);

    let capped = exec::cap_result_sql("sqlite", &report.pieces[0].text, 100);
    assert_eq!(capped, "SELECT * FROM thread_items WHERE turn_id='01a1；89' LIMIT 101");

    // 只有一个全角分号的“空语句”不算一条语句；注释里的全角分号不切分。
    assert!(split::split_sql("sqlite", "；").pieces.is_empty());
    let commented = split::split_sql("postgres", "SELECT 1 -- 说明；\n+ 1；");
    assert_eq!(commented.pieces.len(), 1, "{commented:?}");
    assert_eq!(commented.pieces[0].text, "SELECT 1 -- 说明；\n+ 1");
}

/// 分隔符因引擎而异：SQL Server 的 GO（大小写、次数、行尾注释、多写的分号）是批次分隔，
/// 不进语句文本，读取上限用 TOP；MySQL 的 DELIMITER 自定义结束符下，过程体里的分号（含全角）不切分。
#[test]
fn delimiters_follow_each_engine() {
    let tsql = "SELECT * FROM orders\ngo\nSELECT name FROM customers\nGO 2\nSELECT 1 -- 第三批\nGO -- 结束\nSELECT 2；\nGO;\n";
    let report = split::split_sql("sqlserver", tsql);
    assert!(report.refusal.is_none(), "{report:?}");
    let texts: Vec<&str> = report.pieces.iter().map(|piece| piece.text.as_str()).collect();
    assert_eq!(texts, vec!["SELECT * FROM orders", "SELECT name FROM customers", "SELECT 1 -- 第三批", "SELECT 2"]);
    assert!(texts.iter().all(|text| !text.to_ascii_uppercase().contains("\nGO")), "GO 不进语句");
    assert_eq!(exec::cap_result_sql("sqlserver", texts[0], 100), "SELECT TOP (101) * FROM orders", "SQL Server 用 TOP 而不是 LIMIT");
    // 单独一行的 GO 才是分隔：列名或字符串里的 go 不切分。
    let inline = split::split_sql("sqlserver", "SELECT go FROM t WHERE note = 'go'\nGO");
    assert_eq!(inline.pieces.len(), 1);
    assert_eq!(inline.pieces[0].text, "SELECT go FROM t WHERE note = 'go'");

    let mysql = "DELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2； END$$\nDELIMITER ;\nSELECT 3；SELECT 4;";
    let report = split::split_sql("mysql", mysql);
    assert!(report.refusal.is_none(), "{report:?}");
    let texts: Vec<&str> = report.pieces.iter().map(|piece| piece.text.as_str()).collect();
    assert_eq!(texts.len(), 3, "{texts:?}");
    assert!(texts[0].starts_with("CREATE PROCEDURE") && texts[0].contains("SELECT 2；"), "自定义结束符下过程体整体保留：{texts:?}");
    assert_eq!(texts[1], "SELECT 3");
    assert_eq!(texts[2], "SELECT 4;");
}

#[test]
fn redis_query_desk_completion_plans_commands_and_keys() {
    use super::assist::{CatalogObject, CompletionScope};
    let scope = CompletionScope {
        engine: "redis".into(),
        provider: true,
        objects: vec![
            CatalogObject { schema: String::new(), name: "workshop:ranking:1".into(), kind: "key".into(), comment: String::new() },
            CatalogObject { schema: String::new(), name: "user:profile:42".into(), kind: "key".into(), comment: String::new() },
        ],
        ..Default::default()
    };
    let cmd_plan = super::complete::plan(&scope, "hge", 3, crate::i18n::Locale::ZhCn);
    assert!(cmd_plan.candidates.iter().any(|c| c.label == "HGETALL"));

    let key_plan = super::complete::plan(&scope, "HGETALL work", 12, crate::i18n::Locale::ZhCn);
    assert_eq!(key_plan.candidates.len(), 1);
    assert_eq!(key_plan.candidates[0].insert, "workshop:ranking:1");
}

#[test]
fn comment_stripping_follows_each_dialect_lexis_for_quotes() {
    // PG dollar-quote 里的 -- 与 /* 是字符串内容，不是注释：后面的代码不能被吞掉。
    assert_eq!(
        split::strip_sql_comments("postgres", "SELECT $$a -- b$$ AS x FROM t -- tail"),
        "SELECT $$a -- b$$ AS x FROM t  "
    );
    assert_eq!(
        split::strip_sql_comments("postgres", "SELECT $q$/* x */$q$ FROM t /* c */ WHERE 1"),
        "SELECT $q$/* x */$q$ FROM t   WHERE 1"
    );
    // MySQL 字符串里的反斜杠转义引号：'a\' -- b' 是一个字符串，不会从中间开出注释。
    assert_eq!(
        split::strip_sql_comments("mysql", "SELECT 'a\\' -- b' FROM t # tail"),
        "SELECT 'a\\' -- b' FROM t  "
    );
    // PG 不认反斜杠转义：'a\' 在这里已经闭合，后面的 -- 才是注释。
    assert_eq!(split::strip_sql_comments("postgres", "SELECT 'a\\' -- b"), "SELECT 'a\\'  ");
    // T-SQL 方括号标识符里的 -- 是名字的一部分。
    assert_eq!(split::strip_sql_comments("sqlserver", "SELECT [a--b] FROM t"), "SELECT [a--b] FROM t");
    // 规整：引号外的空白与注释折成一个空格，引号里的空白原样保留。
    assert_eq!(
        split::normalize_sql_code("postgres", "SELECT  id,\n  \"full  name\" -- 说明\nFROM   \"my  table\"\n"),
        "SELECT id, \"full  name\" FROM \"my  table\""
    );
    assert_eq!(split::normalize_sql_code("mysql", "SELECT 'x  y'\t/* a */\n FROM t"), "SELECT 'x  y' FROM t");
}

#[test]
fn single_table_proof_reads_quoted_names_and_dialect_strings() {
    use crate::app::views::grid::prove_single_table;
    // 引号标识符可以带空格：整体读入，不在空白处截断成另一个名字。
    let proof = prove_single_table("postgres", "SELECT * FROM \"my  table\" -- 注释\nWHERE id = 1").unwrap();
    assert_eq!(proof.table, "my  table");
    let proof = prove_single_table("postgres", "SELECT * FROM \"sales\".\"Order Items\"").unwrap();
    assert_eq!((proof.schema.as_deref(), proof.table.as_str()), (Some("sales"), "Order Items"));
    let proof = prove_single_table("sqlserver", "SELECT * FROM [dbo].[a]]b]").unwrap();
    assert_eq!((proof.schema.as_deref(), proof.table.as_str()), (Some("dbo"), "a]b"));
    // dollar-quote 里的注释标记不能把后面的 JOIN 藏起来。
    assert_eq!(prove_single_table("postgres", "SELECT $$ -- $$ AS x FROM a JOIN b ON true"), Err("grid.readonly.join"));
    // 字符串里的反斜杠在 MySQL NO_BACKSLASH_ESCAPES/ANSI_QUOTES 与 PG E'…' 下读法不同：一律不证明，说明原因。
    assert_eq!(prove_single_table("mysql", "SELECT 'it\\'s -- ' AS x FROM a JOIN b ON 1"), Err("grid.readonly.ambiguousEscape"));
    assert_eq!(prove_single_table("mysql", "SELECT 'it\\'s' AS x FROM orders"), Err("grid.readonly.ambiguousEscape"));
    // 评审复现：默认读法证明为 orders，NO_BACKSLASH_ESCAPES 服务器却看到 JOIN。
    assert_eq!(prove_single_table("mysql", "SELECT 'a\\' FROM a JOIN b -- ' FROM orders"), Err("grid.readonly.ambiguousEscape"));
    assert_eq!(prove_single_table("postgres", "SELECT E'a\\', 1 FROM a JOIN b -- ' FROM orders"), Err("grid.readonly.ambiguousEscape"));
    // 没有歧义的写法照常证明：双写引号、反引号里的反斜杠、不认反斜杠的引擎。
    assert!(prove_single_table("mysql", "SELECT 'it''s' AS x FROM orders").is_ok());
    assert!(prove_single_table("mysql", "SELECT `a\\b` FROM orders").is_ok());
    assert!(prove_single_table("sqlite", "SELECT 'C:\\tmp' AS p FROM orders").is_ok());
    assert!(prove_single_table("sqlserver", "SELECT 'C:\\tmp' AS p FROM orders").is_ok());
    // 切分同样按 MySQL 的反斜杠转义：字符串里的分号不切开语句；PG 不认反斜杠转义。
    assert_eq!(split::split_sql("mysql", "SELECT 'a\\'; b' FROM t; SELECT 2").pieces.len(), 2);
    assert_eq!(split::split_sql("postgres", "SELECT 'a\\'; SELECT 2").pieces.len(), 2);
}

/// 执行计划按目标连接的服务器版本选格式：MySQL 8.0.16 起用 TREE，更低或未知用传统 EXPLAIN；
/// 不能看计划的语句在本地拒绝，不发送；版本只更新绑定同一连接的标签。
#[test]
fn explain_uses_the_target_server_version_and_refuses_ddl_locally() {
    use crate::query::desk::{Desk, Target};
    let target = |connection: &str| Target {
        connection_id: connection.to_owned(),
        connection_name: connection.to_owned(),
        database: "shop".to_owned(),
        engine: "mysql".to_owned(),
        schema: String::new(),
        environment: String::new(),
        server_version: None,
    };
    let mut desk = Desk::new();
    desk.import_tab("a", "SELECT * FROM orders", Some(target("m8")));
    desk.import_tab("b", "SELECT * FROM orders", Some(target("m5")));
    desk.focus_tab(Some("a"));
    assert_eq!(desk.explain_active().unwrap(), "EXPLAIN SELECT * FROM orders", "版本未知时用传统格式");
    desk.set_server_version("m8", Some("8.0.36".to_owned()));
    desk.set_server_version("m5", Some("5.7.44-log".to_owned()));
    assert_eq!(desk.target().and_then(|target| target.server_version).as_deref(), Some("8.0.36"));
    assert_eq!(desk.explain_active().unwrap(), "EXPLAIN FORMAT=TREE SELECT * FROM orders");
    desk.focus_tab(Some("b"));
    assert_eq!(desk.explain_active().unwrap(), "EXPLAIN SELECT * FROM orders", "5.7 没有 TREE 格式");
    desk.set_server_version("m5", Some("8.0.15".to_owned()));
    assert_eq!(desk.explain_active().unwrap(), "EXPLAIN SELECT * FROM orders", "8.0.15 仍是传统格式");
    desk.set_server_version("m5", Some("8.0.16".to_owned()));
    assert_eq!(desk.explain_active().unwrap(), "EXPLAIN FORMAT=TREE SELECT * FROM orders");
    for ddl in ["CREATE TABLE t (id int)", "ALTER TABLE t ADD c int", "DROP TABLE t", "TRUNCATE TABLE t", "GRANT ALL ON *.* TO u", "SET NAMES utf8mb4"] {
        desk.set_draft(ddl);
        let reason = desk.explain_active().expect_err(ddl);
        assert!(reason.contains(&split::first_keyword(ddl)), "{ddl}: {reason}");
    }
    // MySQL 家族：DESC/DESCRIBE 跟着语句时就是 EXPLAIN，改写成 EXPLAIN 原样发送，不再包一层；
    // DESC 表名是查看表结构，本地拒绝并说明。
    desk.set_draft("DESC SELECT * FROM orders");
    assert_eq!(desk.explain_active().unwrap(), "EXPLAIN SELECT * FROM orders");
    desk.set_draft("DESC orders");
    let reason = desk.explain_active().expect_err("DESC 表名");
    assert!(reason.contains("DESC SELECT"), "{reason}");
    // CTAS 和 DECLARE 在 MySQL 不能 EXPLAIN，本地拒绝，不发送。
    for refused in ["CREATE TABLE t AS SELECT * FROM orders", "DECLARE c CURSOR FOR SELECT 1"] {
        desk.set_draft(refused);
        assert!(desk.explain_active().is_err(), "{refused}");
    }
}

/// R14：一条语句返回多个结果集时每个结果集一页；后续结果集沿用这条语句的草稿范围，后面的语句不错位。
#[test]
fn extra_result_sets_share_their_statement_range() {
    use crate::application::tasks::TaskBoard;
    use crate::query::assist::RunScope;
    use crate::query::commands::{RunDto, StatementDto};
    use crate::query::desk::{BeginRun, Desk, Target, Ticket};

    let tasks = TaskBoard::new();
    let mut desk = Desk::new();
    let target = Target {
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: "main".to_owned(),
        engine: "sqlite".to_owned(),
        schema: String::new(),
        environment: String::new(),
        server_version: None,
    };
    let draft = "select 1;\nselect 2";
    let pieces = crate::query::split::split_sql("sqlite", draft).pieces;
    let (one, two) = (pieces[0].text.clone(), pieces[1].text.clone());
    desk.import_tab("a", draft, Some(target));
    desk.focus_tab(Some("a"));
    let BeginRun::Ready(prepared) = desk.begin_run(&tasks, RunScope::Script, None) else {
        panic!("应开始执行");
    };
    let ticket = Ticket { tab_id: prepared.tab_id.clone(), token: prepared.token, version: prepared.version };
    let dto = |sql: &str, value: &str, set: Option<(usize, usize)>, message: &str| StatementDto {
        sql: sql.to_owned(),
        sent_sql: None,
        rewrite: None,
        status: "ok".to_owned(),
        message: message.to_owned(),
        columns: vec!["v".to_owned()],
        column_types: vec!["integer".to_owned()],
        rows: vec![vec![Some(value.to_owned())]],
        truncated: false,
        affected: None,
        elapsed_ms: Some(1),
        error_at: None,
        error_code: None,
        transaction: None,
        result_set: set,
    };
    let run = RunDto {
        id: "run-a".to_owned(),
        connection_id: "notes".to_owned(),
        connection_name: "笔记".to_owned(),
        database: "main".to_owned(),
        engine: "sqlite".to_owned(),
        status: "done".to_owned(),
        cancel: String::new(),
        cancel_message: String::new(),
        via_ssh: false,
        fresh_channel: false,
        statements: vec![
            dto(&one, "a", Some((0, 2)), ""),
            dto(&one, "b", Some((1, 2)), "警告（1 条）：Warning 1642: demo"),
            dto(&two, "c", None, ""),
        ],
        transaction: None,
        started_at: String::new(),
        elapsed_ms: 1,
    };
    desk.finish_polled(&tasks, &ticket, Ok(run), "已取消");
    let results = desk.results();
    assert_eq!(results.len(), 3, "两个结果集各一页，加上第二条语句");
    let first = draft.find("select 1").unwrap();
    let second = draft.find("select 2").unwrap();
    assert_eq!(results[0].range, Some((first, first + one.len())));
    assert_eq!(results[1].range, results[0].range, "后续结果集沿用同一条语句的范围");
    assert_eq!(results[2].range, Some((second, draft.len())), "后面的语句不错位");
    assert_eq!(results[1].rows[0][0].as_deref(), Some("b"));
    let log = desk.log();
    assert!(log.iter().any(|entry| entry.message.contains("警告")), "消息页带警告：{log:?}");
}

/// 存储过程中途失败（SELECT 之后 SIGNAL）：驱动带回已经读到的结果集。
struct FailingCall;

#[async_trait::async_trait]
impl StatementDriver for FailingCall {
    async fn run(&self, sql: &str, _limit: usize, _cancel: &Arc<AtomicBool>) -> exec::DriverReport {
        let set = |name: &str, value: &str| exec::StatementValue {
            columns: vec![name.to_owned()],
            column_types: vec!["INT".to_owned()],
            rows: vec![vec![Some(value.to_owned())]],
            truncated: false,
            affected: None,
        };
        if !sql.starts_with("CALL") {
            return exec::DriverReport::unchanged(sql, Ok(ok_count()));
        }
        let message = "ERROR 1644 (45000): order check failed".to_owned();
        let extras = exec::ResultExtras { partial: Some(set("first", "1")), more: vec![set("second", "2")], ..Default::default() };
        exec::DriverReport {
            sent: exec::CappedSql::unchanged(sql),
            outcome: Err(StatementStop::Error { message: message.clone(), at: None, code: Some("1644".to_owned()) }),
            failure: Some(exec::Failure { message, at: None, code: Some("1644".to_owned()) }),
            extras,
        }
    }
}

/// R14：过程中途失败时语句按失败登记（批次失败、后续语句不执行、错误码与原文保留），
/// 已经读到的结果集照样保留在报告里。
#[tokio::test]
async fn a_procedure_failing_midway_is_an_error_that_keeps_its_result_sets() {
    let flag = Arc::new(AtomicBool::new(false));
    let parts = vec![piece("CALL shop.check_orders()"), piece("SELECT 2")];
    let report = run_batch(&parts, 100, &flag, false, &FailingCall, None).await;
    assert_eq!(report.status, "error", "批次按失败结束");
    let call = &report.statements[0];
    assert_eq!(call.status, "error");
    assert!(call.message.contains("order check failed"), "{}", call.message);
    assert_eq!(call.error_code.as_deref(), Some("1644"));
    assert_eq!(call.columns, vec!["first".to_owned()], "第一个结果集保留");
    assert_eq!(call.rows, vec![vec![Some("1".to_owned())]]);
    assert_eq!(call.extras.more.len(), 1, "其余已读到的结果集保留");
    assert!(call.extras.partial.is_none(), "partial 已经并入语句本身");
    assert_eq!(report.statements[1].status, "skipped", "失败之后的语句不执行");
}

// ---------------------------------------------------------------- 执行计划画布（R11）：JOIN 建模与节点类型

mod plan_join_model {
    use crate::query::plan::{self, NodeKind, Plan, PlanNode};

    fn cols(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn row(cells: &[Option<&str>]) -> Vec<Option<String>> {
        cells.iter().map(|cell| cell.map(str::to_owned)).collect()
    }

    fn count(plan: &Plan, kind: NodeKind) -> usize {
        plan.lines().iter().filter(|line| line.node.node_kind() == kind).count()
    }

    fn edges(plan: &Plan) -> usize {
        fn walk(node: &PlanNode) -> usize {
            node.children.len() + node.children.iter().map(walk).sum::<usize>()
        }
        plan.roots.iter().map(walk).sum()
    }

    const SQL: &str = "EXPLAIN select c.name, o.total, l.qty from shop.orders o join shop.customers c on c.id = o.customer_id join shop.order_lines AS l on l.order_id = o.id";

    /// 传统表格格式：同一 (id, select_type) 块的三张表构成左深 Nested loop：2 个 JOIN、3 个表节点、4 条边，标题是表名加别名。
    #[test]
    fn tabular_three_table_join_becomes_left_deep_nested_loops() {
        let columns = cols(&["id", "select_type", "table", "partitions", "type", "possible_keys", "key", "key_len", "ref", "rows", "filtered", "Extra"]);
        let rows = vec![
            row(&[Some("1"), Some("SIMPLE"), Some("o"), None, Some("ALL"), Some("PRIMARY"), None, None, None, Some("3"), Some("33.33"), Some("Using where")]),
            row(&[Some("1"), Some("SIMPLE"), Some("c"), None, Some("eq_ref"), Some("PRIMARY"), Some("PRIMARY"), Some("4"), Some("shop.o.customer_id"), Some("1"), Some("100.00"), None]),
            row(&[Some("1"), Some("SIMPLE"), Some("l"), None, Some("ref"), Some("PRIMARY"), Some("PRIMARY"), Some("4"), Some("shop.o.id"), Some("3"), Some("100.00"), None]),
        ];
        let plan = plan::parse("mysql", SQL, &columns, &rows).expect("plan");
        assert_eq!(plan.roots.len(), 1, "一棵树，画布上一定有连线");
        assert_eq!(count(&plan, NodeKind::Join), 2);
        assert_eq!(count(&plan, NodeKind::Table), 3);
        assert_eq!(edges(&plan), 4);

        let root = &plan.roots[0];
        assert_eq!(root.op, "Nested loop");
        assert_eq!(root.children[1].title(), "order_lines l", "右输入是最后连接的表");
        let inner = &root.children[0];
        assert_eq!(inner.node_kind(), NodeKind::Join);
        let titles: Vec<String> = inner.children.iter().map(PlanNode::title).collect();
        assert_eq!(titles, vec!["orders o", "customers c"]);
        assert_eq!(inner.children[0].access_type.as_deref(), Some("ALL"));
        assert_eq!(inner.rows, Some(1.), "JOIN 产出行数：3 × 33.33% × 1");
        assert_eq!(root.rows, Some(3.));
        let lines = plan.lines();
        let depths: Vec<usize> = lines.iter().map(|line| line.depth).collect();
        assert_eq!(depths, vec![0, 1, 2, 2, 1]);
    }

    /// 哈希连接（MySQL 8.0.18+ 表格格式的 `Using join buffer (hash join)`）显示为 Hash join。
    #[test]
    fn tabular_join_buffer_hash_join_is_named() {
        let columns = cols(&["id", "select_type", "table", "type", "key", "rows", "filtered", "Extra"]);
        let rows = vec![
            row(&[Some("1"), Some("SIMPLE"), Some("a"), Some("ALL"), None, Some("10"), Some("100.00"), None]),
            row(&[Some("1"), Some("SIMPLE"), Some("b"), Some("ALL"), None, Some("20"), Some("10.00"), Some("Using where; Using join buffer (hash join)")]),
        ];
        let plan = plan::parse("mysql", "EXPLAIN select * from a join b on a.x = b.x", &columns, &rows).expect("plan");
        assert_eq!(plan.roots[0].op, "Hash join");
        assert_eq!(plan.roots[0].rows, Some(20.));
    }

    /// JSON：nested_loop 保留成层级（左深 JOIN），产出行数与累计代价取右表的 rows_produced_per_join / prefix_cost。
    #[test]
    fn json_nested_loop_keeps_the_join_hierarchy() {
        let json = r#"{"query_block": {"select_id": 1, "cost_info": {"query_cost": "1.45"}, "nested_loop": [
            {"table": {"table_name": "o", "access_type": "ALL", "rows_examined_per_scan": 3, "rows_produced_per_join": 1, "filtered": "33.33",
                       "cost_info": {"read_cost": "0.45", "eval_cost": "0.10", "prefix_cost": "0.55"}}},
            {"table": {"table_name": "c", "access_type": "eq_ref", "key": "PRIMARY", "rows_examined_per_scan": 1, "rows_produced_per_join": 1, "filtered": "100.00",
                       "cost_info": {"read_cost": "0.25", "eval_cost": "0.10", "prefix_cost": "0.90"}}},
            {"table": {"table_name": "l", "access_type": "ref", "key": "PRIMARY", "rows_examined_per_scan": 3, "rows_produced_per_join": 3, "filtered": "100.00",
                       "using_join_buffer": "hash join", "cost_info": {"read_cost": "0.25", "eval_cost": "0.30", "prefix_cost": "1.45"}}}]}}"#;
        let plan = plan::parse("mysql", &SQL.replace("EXPLAIN", "EXPLAIN FORMAT=JSON"), &cols(&["EXPLAIN"]), &[vec![Some(json.to_owned())]]).expect("plan");
        let ops: Vec<String> = plan.lines().iter().map(|line| line.node.title()).collect();
        assert_eq!(ops, vec!["Query block #1", "Hash join", "Nested loop", "orders o", "customers c", "order_lines l"]);
        let depths: Vec<usize> = plan.lines().iter().map(|line| line.depth).collect();
        assert_eq!(depths, vec![0, 1, 2, 3, 3, 2]);
        let lines = plan.lines();
        assert_eq!((lines[1].node.rows, lines[1].node.cost), (Some(3.), Some(1.45)));
        assert_eq!((lines[2].node.rows, lines[2].node.cost), (Some(1.), Some(0.9)));
        assert_eq!(count(&plan, NodeKind::Join), 2);
        assert_eq!(edges(&plan), 5);
    }

    /// MariaDB 的 block-nl-join 包装拆开，连接方式（BNLH）落在 JOIN 节点上。
    #[test]
    fn mariadb_block_nl_join_unwraps_into_a_join_node() {
        let json = r#"{"query_block": {"select_id": 1, "nested_loop": [
            {"table": {"table_name": "a", "access_type": "ALL", "rows": 10, "filtered": 100}},
            {"block-nl-join": {"table": {"table_name": "b", "access_type": "hash_ALL", "rows": 20, "filtered": 100}, "buffer_type": "flat", "join_type": "BNLH"}}]}}"#;
        let plan = plan::parse("mariadb", "EXPLAIN FORMAT=JSON select * from a join b", &cols(&["EXPLAIN"]), &[vec![Some(json.to_owned())]]).expect("plan");
        let lines = plan.lines();
        assert_eq!(lines[1].node.op, "Hash join");
        assert_eq!(lines[3].node.title(), "b");
        assert!(lines.iter().all(|line| line.node.op != "block-nl-join"));
    }

    /// 只有一张表的计划不造 JOIN：一个表节点，没有边。
    #[test]
    fn single_table_plan_is_one_table_node() {
        let columns = cols(&["id", "select_type", "table", "type", "key", "rows", "filtered", "Extra"]);
        let rows = vec![row(&[Some("1"), Some("SIMPLE"), Some("o"), Some("const"), Some("PRIMARY"), Some("1"), Some("100.00"), None])];
        let plan = plan::parse("mysql", "EXPLAIN select * from orders o where id = 1", &columns, &rows).expect("plan");
        assert_eq!(plan.lines().len(), 1);
        assert_eq!(plan.roots[0].node_kind(), NodeKind::Table);
        assert_eq!(plan.roots[0].title(), "orders o");
        assert_eq!(edges(&plan), 0);
    }

    /// `->` 缩进树：缩进回退到比第一层还浅时不丢行（以前在这里提前结束循环）。
    #[test]
    fn arrow_tree_keeps_rows_after_indentation_falls_back() {
        let tree = "    -> Sort: o.total  (cost=2 rows=3)\n        -> Table scan on o  (cost=1 rows=3)\n-> Select #2 (subquery in condition; run only once)\n    -> Index lookup on c using PRIMARY (id=1)  (cost=0.35 rows=1)\n  -> Filter: (l.qty > 1)  (cost=0.5 rows=1)\n      -> Table scan on l  (cost=0.5 rows=3)";
        let plan = plan::parse("mysql", "EXPLAIN FORMAT=TREE select", &cols(&["EXPLAIN"]), &[vec![Some(tree.to_owned())]]).expect("plan");
        let lines = plan.lines();
        assert_eq!(lines.len(), 6, "六行全部保留");
        assert_eq!(plan.roots.len(), 2);
        assert_eq!(plan.roots[1].node_kind(), NodeKind::Subquery);
        assert_eq!(plan.roots[1].children.len(), 2, "回退后浅一些的行挂到最近的上层");
        let depths: Vec<usize> = lines.iter().map(|line| line.depth).collect();
        assert_eq!(depths, vec![0, 1, 0, 1, 1, 2]);
        assert_eq!(lines[1].node.title(), "o");
        assert_eq!(lines[1].node.node_kind(), NodeKind::Table);
    }
}

/// 外壳把已有查询标签改到另一条连接或另一个库（首页、对象树、历史再次打开）：编辑器跟上，
/// 不停在第一次导入时的旧库上；外壳给的库为空时不清掉已经选好的库。
#[test]
fn desk_follows_the_shell_target_for_an_existing_tab() {
    let mut desk = crate::query::desk::Desk::new();
    let target = |connection: &str, database: &str| crate::query::desk::Target {
        connection_id: connection.to_owned(),
        connection_name: connection.to_owned(),
        database: database.to_owned(),
        engine: "mysql".to_owned(),
        schema: String::new(),
        environment: String::new(),
        server_version: None,
    };
    desk.import_tab("a", "select 1", Some(target("dev", "shop")));
    desk.focus_tab(Some("a"));
    assert!(!desk.follow_target("a", Some(target("dev", "shop"))), "相同目标不算变化");
    assert!(desk.follow_target("a", Some(target("dev", "crm"))));
    assert_eq!(desk.target().map(|t| t.database), Some("crm".to_owned()));
    assert!(!desk.follow_target("a", Some(target("dev", ""))), "外壳没给库时保留已选的库");
    assert_eq!(desk.target().map(|t| t.database), Some("crm".to_owned()));
    assert!(desk.follow_target("a", Some(target("uat", ""))), "换连接时一定跟上");
    let now = desk.target().unwrap();
    assert_eq!((now.connection_id.as_str(), now.database.as_str()), ("uat", ""));
    assert!(!desk.follow_target("a", None), "外壳没有目标时不动");
}
