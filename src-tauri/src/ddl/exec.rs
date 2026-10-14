use crate::connections::CatalogSession;

use super::introspect::{attach_reference, enrich, load, session_snapshot, table_of};
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
    let steps = run_steps(session, &plan.steps, snapshot.recorded_foreign_keys).await?;
    Ok(Receipt { blocked: Vec::new(), rule: plan.rule, steps, target: plan.target, rebuild: plan.rebuild })
}

async fn prepare_snapshot(session: &CatalogSession, request: &Request) -> Result<Snapshot, String> {
    if matches!(request.action, Action::SetForeignKeys { .. }) {
        return session_snapshot(session, request).await;
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

pub async fn run_steps(session: &CatalogSession, steps: &[Step], recorded_foreign_keys: Option<bool>) -> Result<Vec<StepReceipt>, String> {
    match session {
        CatalogSession::Scripted => Err("这条已打开的会话没有可查询的数据库通道。".to_owned()),
        CatalogSession::Sqlite { pool, .. } => run_sqlite(pool, steps, recorded_foreign_keys).await,
        CatalogSession::Mysql(pool) => run_mysql(pool, steps).await,
        CatalogSession::Postgres(client) => run_postgres(client, steps).await,
    }
}

async fn run_sqlite(pool: &sqlx::SqlitePool, steps: &[Step], recorded_foreign_keys: Option<bool>) -> Result<Vec<StepReceipt>, String> {
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
        let executed = if kind == StmtKind::Check {
            sqlx::query(sqlx::AssertSqlSafe(step.sql.clone()))
                .fetch_all(&mut *conn)
                .await
                .map(|rows| {
                    if rows.is_empty() {
                        Ok(())
                    } else {
                        Err(format!("foreign_key_check 发现 {} 处违规，不会把这次重建当成成功。", rows.len()))
                    }
                })
                .map_err(|err| err.to_string())
                .and_then(|item| item)
        } else {
            sqlx::query(sqlx::AssertSqlSafe(step.sql.clone()))
                .execute(&mut *conn)
                .await
                .map(|_| ())
                .map_err(|err| err.to_string())
        };
        match executed {
            Ok(()) => {
                if kind == StmtKind::Begin {
                    txn_from = Some(results.len());
                }
                if kind == StmtKind::Commit {
                    txn_from = None;
                }
                results.push(receipt(step, StepStatus::Ok, ""));
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
        restore_foreign_keys(&mut *conn, &mut results, recorded_foreign_keys).await;
    }
    Ok(results)
}

async fn restore_foreign_keys(conn: &mut sqlx::SqliteConnection, results: &mut Vec<StepReceipt>, recorded: Option<bool>) {
    let turned_off = results.iter().any(|step| step.status == StepStatus::Ok && is_fk_off(&step.sql));
    if !turned_off {
        return;
    }
    let value = if recorded.unwrap_or(false) { "ON" } else { "OFF" };
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
    } else {
        StmtKind::Other
    }
}

fn is_fk_off(sql: &str) -> bool {
    let text = sql.trim().trim_end_matches(';').trim().to_ascii_lowercase().replace(' ', "");
    text.starts_with("pragmaforeign_keys=") && text.ends_with("off")
}

fn sqlite_unknown(message: &str) -> bool {
    let text = message.to_ascii_lowercase();
    text.contains("pool closed") || text.contains("connection closed") || text.contains("broken pipe")
}

