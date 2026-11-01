use sqlx::{Row, ValueRef};

use crate::connections::CatalogSession;

use super::introspect::{attach_reference, enrich, is_object_action, load, maintenance_note, object_snapshot, session_snapshot, table_of};
use super::model::{Action, Plan, Request, Snapshot, Step, StepReceipt, StepStatus};
use super::plan;

pub struct Receipt {
    pub blocked: Vec<String>,
    pub rule: String,
    pub steps: Vec<StepReceipt>,
    pub target: String,
    pub rebuild: bool,
}

pub async fn preview(session: &CatalogSession, request: &Request) -> Result<Plan, String> {
    let snapshot = prepare_snapshot(session, request).await?;
    Ok(plan::plan(&snapshot, &request.action))
}

pub async fn execute(session: &CatalogSession, request: &Request, confirmed_sql: &[String]) -> Result<Receipt, String> {
    let snapshot = prepare_snapshot(session, request).await?;
    let plan = plan::plan(&snapshot, &request.action);
    if !plan.blocked.is_empty() {
        return Ok(Receipt {
            blocked: plan.blocked,
            rule: plan.rule,
            steps: Vec::new(),
            target: plan.target,
            rebuild: false,
        });
    }
    let planned: Vec<String> = plan.steps.iter().map(|step| step.sql.clone()).collect();
    if planned != confirmed_sql {
        return Err("预览已过期：数据库里的结构或生成的 SQL 和确认时看到的不一致，没有执行。".to_owned());
    }
    // 重建的失败补偿恢复操作开始时读到的 live。SetForeignKeys 的语句本身就是目标值，不能再改回去。
    let restore = match &request.action {
        Action::SetForeignKeys { enabled } => Some(*enabled),
        _ => snapshot.live_foreign_keys,
    };
    let mut steps = run_steps(session, &plan.steps, restore).await?;
    // 维护动作（VACUUM/REINDEX）没有返回值，完成后补一句读到的结果，别让界面只看到“成功”。
    if !steps.is_empty() && steps.iter().all(|step| step.status == StepStatus::Ok) {
        if let Some(note) = maintenance_note(session, &request.action).await {
            if let Some(last) = steps.last_mut() {
                last.message = note;
            }
        }
    }
    Ok(Receipt { blocked: Vec::new(), rule: plan.rule, steps, target: plan.target, rebuild: plan.rebuild })
}

async fn prepare_snapshot(session: &CatalogSession, request: &Request) -> Result<Snapshot, String> {
    if matches!(request.action, Action::SetForeignKeys { .. }) {
        return session_snapshot(session, request).await;
    }
    if is_object_action(&request.action) {
        return object_snapshot(session, request).await;
    }
    if let Action::CreateTable { name, .. } = &request.action {
        return match load(session, request, name).await {
            Ok(mut snapshot) => {
                snapshot.object_exists = true;
                snapshot.name = name.clone();
                Ok(snapshot)
            }
            Err(message) if message.starts_with("找不到表或视图") => {
                let mut snapshot = Snapshot::bare(request.engine, &request.database, &request.schema, name);
                snapshot.kind = "missing".to_owned();
                snapshot.object_exists = false;
                snapshot.table_sql.clear();
                if let CatalogSession::Sqlite { foreign_keys, pool } = session {
                    snapshot.recorded_foreign_keys = *foreign_keys;
                    snapshot.sqlite_version = sqlx::query_scalar("SELECT sqlite_version()")
                        .fetch_one(pool)
                        .await
                        .map_err(|err| err.to_string())?;
                    let live: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(pool).await.map_err(|err| err.to_string())?;
                    snapshot.live_foreign_keys = Some(live != 0);
                }
                Ok(snapshot)
            }
            Err(message) => Err(message),
        };
    }
    let table = table_of(&request.action).ok_or_else(|| "没有目标表".to_owned())?;
    let mut snapshot = load(session, request, table).await?;
    enrich(session, &mut snapshot, &request.action).await?;
    attach_reference(session, request, &mut snapshot, &request.action).await?;
    Ok(snapshot)
}

pub async fn run_steps(session: &CatalogSession, steps: &[Step], restore_foreign_keys: Option<bool>) -> Result<Vec<StepReceipt>, String> {
    match session {
        // Redis 没有结构编辑：误入时拒绝执行而不是生成无意义语句。
        CatalogSession::Redis(_) => return Err("Redis 没有结构编辑。".to_owned()),
        CatalogSession::Mongo(_) => return Err("MongoDB 没有结构编辑。".to_owned()),
        CatalogSession::Scripted => Err("这个连接没有连着可查询的数据库。".to_owned()),
        CatalogSession::Sqlite { pool, .. } => run_sqlite(pool, steps, restore_foreign_keys).await,
        CatalogSession::Mysql(pool) => run_mysql(pool, steps).await,
        CatalogSession::Postgres(client) => run_postgres(client, steps).await,
        CatalogSession::Sqlserver(client) => run_sqlserver(client, steps).await,
    }
}

async fn run_sqlserver(client: &std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>, steps: &[Step]) -> Result<Vec<StepReceipt>, String> {
    let mut locked = client.lock().await;
    let mut results = Vec::new();
    let mut stop = false;
    for step in steps {
        if stop {
            results.push(receipt(step, StepStatus::Skipped, "前面的语句没有成功，这条没有执行。"));
            continue;
        }
        match locked.execute(step.sql.clone(), &[]).await {
            Ok(_) => results.push(receipt(step, StepStatus::Ok, "")),
            Err(tiberius::error::Error::Server(err)) => {
                stop = true;
                results.push(receipt(step, StepStatus::Failed, &err.to_string()));
            }
            Err(err) => {
                stop = true;
                results.push(receipt(step, StepStatus::Unknown, &err.to_string()));
            }
        }
    }
    Ok(results)
}

pub(crate) fn committed_foreign_keys(action: &Action, receipt: &Receipt) -> Option<bool> {
    let Action::SetForeignKeys { enabled } = action else {
        return None;
    };
    if !receipt.blocked.is_empty() {
        return None;
    }
    let applied = receipt.steps.iter().any(|step| step.status == StepStatus::Ok && pragma_foreign_keys_is(&step.sql, *enabled));
    applied.then_some(*enabled)
}

async fn run_sqlite(pool: &sqlx::SqlitePool, steps: &[Step], restore: Option<bool>) -> Result<Vec<StepReceipt>, String> {
    let mut conn = pool.acquire().await.map_err(|err| err.to_string())?;
    let mut results = Vec::new();
    let mut stop = false;
    let mut unknown = false;
    let mut txn_from: Option<usize> = None;
    for step in steps {
        if stop {
            results.push(receipt(step, StepStatus::Skipped, "前面的语句没有成功，这条没有执行。"));
            continue;
        }
        let kind = statement_kind(&step.sql);
        let executed: Result<Option<String>, String> = match kind {
            StmtKind::Check => sqlx::query(sqlx::AssertSqlSafe(step.sql.clone()))
                .fetch_all(&mut *conn)
                .await
                .map(|rows| {
                    if rows.is_empty() {
                        Ok(None)
                    } else {
                        Err(format!("foreign_key_check 发现 {} 处违规，不会把这次重建当成成功。", rows.len()))
                    }
                })
                .map_err(|err| err.to_string())
                .and_then(|item| item),
            // PRAGMA 可能带回一行（journal_mode 就会），用 fetch_all 接住并当成结果反馈。
            StmtKind::Probe => sqlx::query(sqlx::AssertSqlSafe(step.sql.clone()))
                .fetch_all(&mut *conn)
                .await
                .map(|rows| Ok::<Option<String>, String>(probe_note(&rows)))
                .map_err(|err| err.to_string())
                .and_then(|item| item),
            _ => sqlx::query(sqlx::AssertSqlSafe(step.sql.clone()))
                .execute(&mut *conn)
                .await
                .map(|_| Ok::<Option<String>, String>(None))
                .map_err(|err| err.to_string())
                .and_then(|item| item),
        };
        match executed {
            Ok(note) => {
                if kind == StmtKind::Begin {
                    txn_from = Some(results.len());
                }
                if kind == StmtKind::Commit {
                    txn_from = None;
                }
                results.push(receipt(step, StepStatus::Ok, note.as_deref().unwrap_or("")));
            }
            Err(message) if sqlite_unknown(&message) => {
                unknown = true;
                stop = true;
                results.push(receipt(step, StepStatus::Unknown, &message));
            }
            Err(message) => {
                stop = true;
                results.push(receipt(step, StepStatus::Failed, &message));
                if let Some(start) = txn_from {
                    match sqlx::query(sqlx::AssertSqlSafe("ROLLBACK".to_owned())).execute(&mut *conn).await {
                        Ok(_) => {
                            for item in results.iter_mut().skip(start) {
                                if item.status == StepStatus::Ok {
                                    item.status = StepStatus::RolledBack;
                                    item.message = "重建事务已回滚，这条语句没有保留。".to_owned();
                                }
                            }
                            txn_from = None;
                        }
                        Err(err) => {
                            unknown = true;
                            for item in results.iter_mut().skip(start) {
                                if item.status == StepStatus::Ok {
                                    item.status = StepStatus::Unknown;
                                    item.message = format!("回滚重建事务失败，结果未知：{err}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if !unknown {
        restore_foreign_keys(&mut *conn, &mut results, restore).await;
    }
    Ok(results)
}

async fn restore_foreign_keys(conn: &mut sqlx::SqliteConnection, results: &mut Vec<StepReceipt>, restore: Option<bool>) {
    let turned_off = results.iter().any(|step| step.status == StepStatus::Ok && is_fk_off(&step.sql));
    if !turned_off {
        return;
    }
    let value = if restore.unwrap_or(false) { "ON" } else { "OFF" };
    let sql = format!("PRAGMA foreign_keys = {value};");
    if results.iter().any(|step| step.status == StepStatus::Ok && step.sql.trim().eq_ignore_ascii_case(sql.trim())) {
        return;
    }
    let outcome = sqlx::query(sqlx::AssertSqlSafe(sql.clone())).execute(&mut *conn).await;
    if let Some(step) = results.iter_mut().rev().find(|step| step.sql.trim().eq_ignore_ascii_case(sql.trim())) {
        match outcome {
            Ok(_) => {
                step.status = StepStatus::Ok;
                step.message = "已恢复会话原来的 foreign_keys。".to_owned();
            }
            Err(err) => {
                step.status = StepStatus::Unknown;
                step.message = format!("恢复 foreign_keys 失败，结果未知：{err}");
            }
        }
        return;
    }
    results.push(StepReceipt {
        sql,
        label: "恢复会话原来的 foreign_keys".to_owned(),
        status: if outcome.is_ok() { StepStatus::Ok } else { StepStatus::Unknown },
        message: match &outcome {
            Ok(_) => "已恢复会话原来的 foreign_keys。".to_owned(),
            Err(err) => format!("恢复 foreign_keys 失败，结果未知：{err}"),
        },
    });
}

async fn run_mysql(pool: &mysql_async::Pool, steps: &[Step]) -> Result<Vec<StepReceipt>, String> {
    let mut conn = pool.get_conn().await.map_err(|err| err.to_string())?;
    let mut results = Vec::new();
    let mut stop = false;
    for step in steps {
        if stop {
            results.push(receipt(step, StepStatus::Skipped, "前面的语句没有成功，这条没有执行。"));
            continue;
        }
        match mysql_async::prelude::Queryable::query_drop(&mut conn, step.sql.clone()).await {
            Ok(()) => results.push(receipt(step, StepStatus::Ok, "")),
            Err(mysql_async::Error::Server(err)) => {
                stop = true;
                results.push(receipt(step, StepStatus::Failed, &err.to_string()));
            }
            Err(err) => {
                stop = true;
                results.push(receipt(step, StepStatus::Unknown, &err.to_string()));
            }
        }
    }
    Ok(results)
}

async fn run_postgres(client: &tokio_postgres::Client, steps: &[Step]) -> Result<Vec<StepReceipt>, String> {
    let mut results = Vec::new();
    let mut stop = false;
    for step in steps {
        if stop {
            results.push(receipt(step, StepStatus::Skipped, "前面的语句没有成功，这条没有执行。"));
            continue;
        }
        match client.batch_execute(&step.sql).await {
            Ok(()) => results.push(receipt(step, StepStatus::Ok, "")),
            Err(err) if err.as_db_error().is_some() => {
                stop = true;
                results.push(receipt(step, StepStatus::Failed, &err.to_string()));
            }
            Err(err) => {
                stop = true;
                results.push(receipt(step, StepStatus::Unknown, &err.to_string()));
            }
        }
    }
    Ok(results)
}

fn receipt(step: &Step, status: StepStatus, message: &str) -> StepReceipt {
    StepReceipt { sql: step.sql.clone(), label: step.label.clone(), status, message: message.to_owned() }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StmtKind {
    Begin,
    Commit,
    Check,
    /// 会带回结果的 PRAGMA：execute 会因为“返回了结果”报错，必须用 fetch_all。
    Probe,
    Other,
}

fn statement_kind(sql: &str) -> StmtKind {
    let text = sql.trim().trim_end_matches(';').trim().to_ascii_lowercase();
    if text == "begin" || text.starts_with("begin ") {
        StmtKind::Begin
    } else if text == "commit" {
        StmtKind::Commit
    } else if text.starts_with("pragma foreign_key_check") {
        StmtKind::Check
    } else if text.starts_with("pragma ") {
        StmtKind::Probe
    } else {
        StmtKind::Other
    }
}

/// PRAGMA 的回显就是结果反馈（例如 journal_mode 会回显新模式）。
fn probe_note(rows: &[sqlx::sqlite::SqliteRow]) -> Option<String> {
    let row = rows.first()?;
    let raw = row.try_get_raw(0).ok()?;
    if raw.is_null() {
        return None;
    }
    if let Ok(text) = row.try_get::<String, _>(0) {
        return Some(format!("数据库返回：{text}"));
    }
    if let Ok(number) = row.try_get::<i64, _>(0) {
        return Some(format!("数据库返回：{number}"));
    }
    if let Ok(number) = row.try_get::<f64, _>(0) {
        return Some(format!("数据库返回：{number}"));
    }
    None
}

fn is_fk_off(sql: &str) -> bool {
    pragma_foreign_keys_is(sql, false)
}

fn pragma_foreign_keys_is(sql: &str, enabled: bool) -> bool {
    let text = sql.trim().trim_end_matches(';').trim().to_ascii_lowercase().replace(' ', "");
    let want = if enabled { "pragmaforeign_keys=on" } else { "pragmaforeign_keys=off" };
    text == want
}

fn sqlite_unknown(message: &str) -> bool {
    let text = message.to_ascii_lowercase();
    text.contains("pool closed") || text.contains("connection closed") || text.contains("broken pipe")
}

