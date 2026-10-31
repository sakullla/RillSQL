//! 迁移执行器的测试：SQLite 真库做端到端（不依赖 docker），纯函数做矩阵判定。

use super::*;
use crate::connections::Service;
use std::path::Path;

/// 建库文件并执行建表/造数语句。会话驱动不建文件，先按 dump 测试的方式把库落出来。
async fn create_sqlite(file: &Path, setup: &[&str]) {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(file).create_if_missing(true))
        .await
        .unwrap();
    for sql in setup {
        sqlx::query(sqlx::AssertSqlSafe((*sql).to_owned())).execute(&pool).await.unwrap();
    }
    pool.close().await;
}

/// 一对源/目标会话：两个独立的 sqlite 文件，模拟两条连接（跨引擎场景共用同一执行器通路）。
async fn two_sessions(dir: &Path) -> (CatalogSession, CatalogSession) {
    let service = Service::new_for_test(dir.join("connections.sqlite"));
    service.open_sqlite_for_test("src", "旧库", &dir.join("src.sqlite")).await.expect("打开源会话");
    service.open_sqlite_for_test("dst", "新库", &dir.join("dst.sqlite")).await.expect("打开目标会话");
    let source = service.catalog_session("src").expect("源会话");
    let target = service.catalog_session("dst").expect("目标会话");
    (source, target)
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rillsql-migrate-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ref_of(connection_id: &str, table: &str) -> MigrateTableRef {
    MigrateTableRef {
        connection_id: connection_id.to_owned(),
        engine: "sqlite".to_owned(),
        database: String::new(),
        schema: String::new(),
        table: table.to_owned(),
    }
}

fn request(id: &str, source: MigrateTableRef, target: MigrateTableRef, mapping: &[(&str, &str)]) -> MigrateRequest {
    MigrateRequest {
        id: id.to_owned(),
        source,
        target,
        mapping: mapping
            .iter()
            .map(|(source, target)| ColumnPair { source: (*source).to_owned(), target: (*target).to_owned() })
            .collect(),
        options: MigrateOptions::default(),
    }
}

async fn text_cell(session: &CatalogSession, sql: &str) -> String {
    match fetch_cells(session, sql, 1).await.expect(sql).pop().and_then(|row| row.into_iter().next()) {
        Some(Cell::Text(text)) => text,
        _ => panic!("没有读到值：{sql}"),
    }
}

/// PG→MySQL 的验收在 docker 侧核验；这里用 SQLite 真库覆盖同一执行器通路：
/// 行数一致、值正确（含引号/二进制/空值）、冲突计数、单行失败继续、取消保留进度。
#[tokio::test]
async fn migrates_rows_with_quotes_binary_and_nulls_between_real_databases() {
    let dir = temp_dir("e2e");
    create_sqlite(
        &dir.join("src.sqlite"),
        &[
            "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT, avatar BLOB, score REAL)",
            "INSERT INTO people (id, name, avatar, score) VALUES (1, '林''夏', x'DEADbeef', 1.5), (2, '周衡', NULL, NULL), (3, '', x'', -2.25)",
        ],
    )
    .await;
    create_sqlite(&dir.join("dst.sqlite"), &["CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT, avatar BLOB, score REAL)"]).await;
    let (source, target) = two_sessions(&dir).await;

    let events: Mutex<Vec<MigrateProgress>> = Mutex::new(Vec::new());
    let outcome = run_migrate(
        &source,
        &target,
        &request("m1", ref_of("src", "people"), ref_of("dst", "people"), &[]),
        &|payload| events.lock().expect("events").push(payload),
    )
    .await;
    assert_eq!(outcome.status, "done", "{}", outcome.message);
    assert!(outcome.success);
    assert_eq!((outcome.inserted, outcome.skipped, outcome.failed), (3, 0, 0), "{}", outcome.message);
    assert_eq!(text_cell(&target, "SELECT COUNT(*) FROM people").await, "3");
    assert_eq!(text_cell(&target, "SELECT name FROM people WHERE id = 1").await, "林'夏");
    assert_eq!(text_cell(&target, "SELECT lower(hex(avatar)) FROM people WHERE id = 1").await, "deadbeef");
    assert_eq!(text_cell(&target, "SELECT lower(hex(avatar)) FROM people WHERE id = 3").await, "");
    assert_eq!(text_cell(&target, "SELECT score FROM people WHERE id = 3").await, "-2.25");
    // 空串与空值不混：空串迁移后仍是空串，二进制空值仍是空值。
    assert_eq!(text_cell(&target, "SELECT name FROM people WHERE id = 3").await, "");
    assert_eq!(text_cell(&target, "SELECT avatar IS NULL FROM people WHERE id = 2").await, "1");
    assert!(events.lock().expect("events").iter().any(|event| event.inserted == 3));
    drop(source);
    drop(target);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn conflict_rows_are_skipped_and_the_rest_migrate() {
    let dir = temp_dir("conflict");
    create_sqlite(
        &dir.join("src.sqlite"),
        &[
            "CREATE TABLE items (id INTEGER PRIMARY KEY, code TEXT UNIQUE)",
            "INSERT INTO items (id, code) VALUES (1, 'A'), (2, 'B'), (3, 'C')",
        ],
    )
    .await;
    // 预置主键冲突（id=1）与唯一键冲突（code='B'）：这两种都计入跳过。
    create_sqlite(
        &dir.join("dst.sqlite"),
        &[
            "CREATE TABLE items (id INTEGER PRIMARY KEY, code TEXT UNIQUE)",
            "INSERT INTO items (id, code) VALUES (1, 'Z'), (9, 'B')",
        ],
    )
    .await;
    let (source, target) = two_sessions(&dir).await;

    let outcome = run_migrate(&source, &target, &request("m2", ref_of("src", "items"), ref_of("dst", "items"), &[]), &|_| {}).await;
    assert_eq!(outcome.status, "done", "{}", outcome.message);
    assert_eq!((outcome.inserted, outcome.skipped, outcome.failed), (1, 2, 0), "{}", outcome.message);
    // 冲突行不覆盖目标已有值。
    assert_eq!(text_cell(&target, "SELECT code FROM items WHERE id = 1").await, "Z");
    assert_eq!(text_cell(&target, "SELECT COUNT(*) FROM items").await, "3");
    drop(source);
    drop(target);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn single_row_failures_are_recorded_and_migration_continues() {
    let dir = temp_dir("fail");
    // id=2 超长会被 CHECK 拒；id=3 的空值进 NOT NULL 列被拒；其余行必须照常写入。
    create_sqlite(
        &dir.join("src.sqlite"),
        &[
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, code TEXT, owner TEXT)",
            "INSERT INTO notes (id, code, owner) VALUES (1, 'ab', '林夏'), (2, 'TOOLONG', '周衡'), (3, 'cd', NULL), (4, NULL, '何清')",
        ],
    )
    .await;
    create_sqlite(
        &dir.join("dst.sqlite"),
        &["CREATE TABLE notes (id INTEGER PRIMARY KEY, code TEXT CHECK (length(code) <= 3), owner TEXT NOT NULL)"],
    )
    .await;
    let (source, target) = two_sessions(&dir).await;

    let outcome = run_migrate(&source, &target, &request("m3", ref_of("src", "notes"), ref_of("dst", "notes"), &[]), &|_| {}).await;
    assert_eq!(outcome.status, "done", "{}", outcome.message);
    assert_eq!((outcome.inserted, outcome.skipped, outcome.failed), (2, 0, 2), "{}", outcome.message);
    assert!(outcome.failed_ids.contains(&"2".to_owned()), "{:?}", outcome.failed_ids);
    assert!(outcome.failed_ids.contains(&"3".to_owned()), "{:?}", outcome.failed_ids);
    assert_eq!(outcome.first_errors.len(), 2, "{:?}", outcome.first_errors);
    assert!(outcome.first_errors.iter().any(|item| item.starts_with("2：")), "{:?}", outcome.first_errors);
    assert_eq!(text_cell(&target, "SELECT COUNT(*) FROM notes").await, "2");
    assert_eq!(text_cell(&target, "SELECT owner FROM notes WHERE id = 1").await, "林夏");
    drop(source);
    drop(target);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cross_family_literals_follow_the_target_column_type() {
    let dir = temp_dir("family");
    // 源列全是 TEXT，目标列是 INTEGER/BOOLEAN：字面量必须按目标类型落（与导入同策略）。
    create_sqlite(
        &dir.join("src.sqlite"),
        &[
            "CREATE TABLE flags (id INTEGER PRIMARY KEY, num TEXT, on_off TEXT)",
            "INSERT INTO flags (id, num, on_off) VALUES (1, '42', 't'), (2, '-7', 'false')",
        ],
    )
    .await;
    create_sqlite(&dir.join("dst.sqlite"), &["CREATE TABLE flags (id INTEGER PRIMARY KEY, num INTEGER, on_off BOOLEAN)"]).await;
    let (source, target) = two_sessions(&dir).await;

    let outcome = run_migrate(
        &source,
        &target,
        &request("m4", ref_of("src", "flags"), ref_of("dst", "flags"), &[("num", "num"), ("on_off", "on_off")]),
        &|_| {},
    )
    .await;
    assert_eq!(outcome.status, "done", "{}", outcome.message);
    // 文本 → 数值/布尔是“尽力转换”，按矩阵给出警告但不阻断。
    assert_eq!(outcome.warnings.len(), 2, "{:?}", outcome.warnings);
    assert_eq!(text_cell(&target, "SELECT typeof(num) FROM flags WHERE id = 1").await, "integer");
    assert_eq!(text_cell(&target, "SELECT num FROM flags WHERE id = 2").await, "-7");
    assert_eq!(text_cell(&target, "SELECT on_off FROM flags WHERE id = 1").await, "1");
    drop(source);
    drop(target);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cancel_stops_and_keeps_the_reported_progress() {
    let dir = temp_dir("cancel");
    let mut inserts = Vec::new();
    for id in 1..=12 {
        inserts.push(format!("({id}, 'm{id}')"));
    }
    create_sqlite(
        &dir.join("src.sqlite"),
        &[
            "CREATE TABLE logs (id INTEGER PRIMARY KEY, message TEXT)",
            Box::leak(format!("INSERT INTO logs (id, message) VALUES {}", inserts.join(", ")).into_boxed_str()),
        ],
    )
    .await;
    create_sqlite(&dir.join("dst.sqlite"), &["CREATE TABLE logs (id INTEGER PRIMARY KEY, message TEXT)"]).await;
    let (source, target) = two_sessions(&dir).await;

    // 第一批提交后取消：在进度回调里触发，确定性地停在已提交的批次上。
    let cancel_at = Mutex::new(false);
    let outcome = {
        let cancel_at = &cancel_at;
        run_migrate(
            &source,
            &target,
            &MigrateRequest {
                id: "m5".to_owned(),
                source: ref_of("src", "logs"),
                target: ref_of("dst", "logs"),
                mapping: Vec::new(),
                options: MigrateOptions { batch_rows: 5 },
            },
            &|payload| {
                let mut armed = cancel_at.lock().unwrap();
                if payload.inserted >= 5 && !*armed {
                    *armed = true;
                    request_cancel("m5");
                }
            },
        )
        .await
    };
    assert_eq!(outcome.status, "cancelled", "{}", outcome.message);
    assert!(!outcome.success);
    assert_eq!(outcome.inserted, 5, "{}", outcome.message);
    assert_eq!(text_cell(&target, "SELECT COUNT(*) FROM logs").await, "5");
    drop(source);
    drop(target);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn bad_mappings_and_missing_tables_are_blocked_before_any_write() {
    let dir = temp_dir("block");
    create_sqlite(&dir.join("src.sqlite"), &["CREATE TABLE a (id INTEGER PRIMARY KEY, name TEXT)"]).await;
    create_sqlite(
        &dir.join("dst.sqlite"),
        &["CREATE TABLE b (id INTEGER PRIMARY KEY, label TEXT)", "CREATE VIEW v_b AS SELECT * FROM b"],
    )
    .await;
    let (source, target) = two_sessions(&dir).await;

    let unknown_column =
        run_migrate(&source, &target, &request("m6", ref_of("src", "a"), ref_of("dst", "b"), &[("name", "missing")]), &|_| {}).await;
    assert_eq!(unknown_column.status, "blocked");
    assert!(unknown_column.message.contains("missing"), "{}", unknown_column.message);

    let duplicate_target = run_migrate(
        &source,
        &target,
        &request("m7", ref_of("src", "a"), ref_of("dst", "b"), &[("id", "id"), ("name", "ID")]),
        &|_| {},
    )
    .await;
    assert_eq!(duplicate_target.status, "blocked");
    assert!(duplicate_target.message.contains("两个源列"), "{}", duplicate_target.message);

    let no_target_table = run_migrate(&source, &target, &request("m8", ref_of("src", "a"), ref_of("dst", "nope"), &[]), &|_| {}).await;
    assert_eq!(no_target_table.status, "blocked");
    assert!(no_target_table.message.contains("读取目标表失败"), "{}", no_target_table.message);

    // 目标视图不能作为迁移目标。
    let view_target = run_migrate(&source, &target, &request("m9", ref_of("src", "a"), ref_of("dst", "v_b"), &[]), &|_| {}).await;
    assert_eq!(view_target.status, "blocked");
    assert!(view_target.message.contains("视图"), "{}", view_target.message);
    drop(source);
    drop(target);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn unmapped_required_target_columns_get_a_warning_not_a_block() {
    let dir = temp_dir("warn");
    create_sqlite(
        &dir.join("src.sqlite"),
        &["CREATE TABLE a (id INTEGER PRIMARY KEY, name TEXT)", "INSERT INTO a (id, name) VALUES (1, 'x')"],
    )
    .await;
    create_sqlite(&dir.join("dst.sqlite"), &["CREATE TABLE b (id INTEGER PRIMARY KEY, label TEXT NOT NULL)"]).await;
    let (source, target) = two_sessions(&dir).await;

    // 只映射 id：目标 label 不可为空且未映射 → 警告；没有默认值时行会计入失败而不是被阻断。
    let outcome = run_migrate(&source, &target, &request("m10", ref_of("src", "a"), ref_of("dst", "b"), &[("id", "id")]), &|_| {}).await;
    assert_eq!(outcome.status, "done", "{}", outcome.message);
    assert!(outcome.warnings.iter().any(|item| item.contains("label")), "{:?}", outcome.warnings);
    assert_eq!(outcome.failed, 1, "{}", outcome.message);
    assert_eq!(text_cell(&target, "SELECT COUNT(*) FROM b").await, "0");
    drop(source);
    drop(target);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pair_note_flags_lossy_pairs_and_accepts_safe_ones() {
    assert_eq!(pair_note("integer", "bigint"), None);
    assert_eq!(pair_note("text", "text"), None);
    assert_eq!(pair_note("bytea", "blob"), None);
    assert_eq!(pair_note("timestamp", "timestamp"), None);
    // varchar 带限长：提示超长会计入失败，不会静默截断。
    let limited = pair_note("text", "varchar(50)").expect("限长提示");
    assert!(limited.contains("不会被截断"), "{limited}");
    let decimal_to_int = pair_note("numeric", "integer").expect("截断提示");
    assert!(decimal_to_int.contains("截断"), "{decimal_to_int}");
    let text_to_int = pair_note("text", "integer").expect("数字提示");
    assert!(text_to_int.contains("数字"), "{text_to_int}");
    let text_to_binary = pair_note("text", "blob").expect("十六进制提示");
    assert!(text_to_binary.contains("十六进制"), "{text_to_binary}");
    let datetime_to_date = pair_note("timestamp", "date").expect("时间丢失提示");
    assert!(datetime_to_date.contains("时间部分"), "{datetime_to_date}");
    let geometry = pair_note("geometry", "text").expect("几何提示");
    assert!(geometry.contains("几何"), "{geometry}");
    let unknown = pair_note("类型未知", "text").expect("未知提示");
    assert!(unknown.contains("类型未知"), "{unknown}");
}

#[test]
fn conflict_errors_are_classified_across_engine_wordings() {
    assert!(is_conflict_error("UNIQUE constraint failed: items.id"));
    assert!(is_conflict_error("Duplicate entry '1' for key 'items.PRIMARY'"));
    assert!(is_conflict_error(r#"duplicate key value violates unique constraint "items_pkey""#));
    assert!(!is_conflict_error("no such table: items"));
    assert!(!is_conflict_error("FOREIGN KEY constraint failed"));
    assert!(!is_conflict_error("CHECK constraint failed: length(code) <= 3"));
}

#[test]
fn row_identity_prefers_mapped_key_values() {
    let cells = vec![Cell::Text("7".to_owned()), Cell::Null, Cell::Text("x".to_owned())];
    assert_eq!(row_identity(&[0], 3, &cells), "7");
    assert_eq!(row_identity(&[0, 1], 3, &cells), "7/NULL");
    assert_eq!(row_identity(&[], 3, &cells), "行#3");
}

#[test]
fn batch_insert_uses_the_target_engine_and_column_types() {
    let columns = vec![
        Column { name: "id".to_owned(), type_name: "integer".to_owned(), nullable: false, primary: true, unique: false, auto: false },
        Column { name: "body".to_owned(), type_name: "blob".to_owned(), nullable: true, primary: false, unique: false, auto: false },
    ];
    let rows = vec![
        vec![Cell::Text("1".to_owned()), Cell::Text("0AFF".to_owned())],
        vec![Cell::Text("2".to_owned()), Cell::Null],
    ];
    let mysql = batch_insert(Engine::Mysql, "`app`.`docs`", &columns, &rows);
    assert_eq!(mysql, "INSERT INTO `app`.`docs` (`id`, `body`) VALUES (1, X'0aff'), (2, NULL)");
    let postgres = batch_insert(Engine::Postgres, "\"public\".\"docs\"", &columns, &rows);
    assert_eq!(postgres, "INSERT INTO \"public\".\"docs\" (\"id\", \"body\") VALUES (1, '\\x0aff'), (2, NULL)");
    assert_eq!(batch_insert(Engine::Sqlite, "\"docs\"", &columns, &[]), "");
}

#[test]
fn request_cancel_flags_a_running_job() {
    let flag = begin_job("migrate-cancel-test");
    assert!(!flag.load(Ordering::Relaxed));
    request_cancel("migrate-cancel-test");
    assert!(flag.load(Ordering::Relaxed));
    finish_job("migrate-cancel-test");
}
