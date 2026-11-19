//! SQLite 的查询执行：在连接池借出的连接上逐条执行，进度回调轮询取消标志，结果分块交给调用方。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::TryStreamExt;
use sqlx::Column;
use sqlx::Row;
use sqlx::TypeInfo;
use sqlx::ValueRef;

use crate::adapters::exec::{looks_like_rows, to_hex};
use crate::query::exec::{chunk_rows, RowSink, StatementValue};

pub(in crate::adapters) async fn sqlite_run(
    pool: &sqlx::SqlitePool,
    sql: &str,
    limit: usize,
    cancel: &Arc<AtomicBool>,
    sink: &dyn RowSink,
    index: usize,
) -> Result<StatementValue, String> {
    let mut conn = pool.acquire().await.map_err(|err| crate::error_text::chain(&err))?;
    let watched = Arc::clone(cancel);
    conn.lock_handle()
        .await
        .map_err(|err| crate::error_text::chain(&err))?
        .set_progress_handler(64, move || !watched.load(Ordering::SeqCst));
    let outcome = sqlite_dispatch(&mut conn, sql, limit, cancel, sink, index).await;
    if let Ok(mut handle) = conn.lock_handle().await {
        handle.remove_progress_handler();
    }
    outcome
}

async fn sqlite_dispatch(
    conn: &mut sqlx::SqliteConnection,
    sql: &str,
    limit: usize,
    cancel: &Arc<AtomicBool>,
    sink: &dyn RowSink,
    index: usize,
) -> Result<StatementValue, String> {
    // 读取上限已由 LiveDriver 改写过，这里发出去的就是实际语句。
    let safe = sqlx::AssertSqlSafe(sql);
    if looks_like_rows(sql) {
        let mut stream = sqlx::query(safe).fetch(&mut *conn);
        let mut columns = Vec::new();
        let mut column_types = Vec::new();
        let mut rows = Vec::new();
        let mut truncated = false;
        let mut pending: Option<sqlx::sqlite::SqliteRow> = None;
        let step = chunk_rows(limit);
        loop {
            if cancel.load(Ordering::SeqCst) {
                drop(stream);
                return Err("已取消".to_owned());
            }
            let row = if let Some(row) = pending.take() {
                row
            } else {
                match stream.try_next().await.map_err(|err| crate::error_text::chain(&err))? {
                    Some(row) => row,
                    None => break,
                }
            };
            if columns.is_empty() {
                columns = row.columns().iter().map(|column| column.name().to_owned()).collect();
                column_types = sqlite_type_names(&row);
            }
            if rows.len() >= limit {
                truncated = true;
                break;
            }
            let width = columns.len();
            rows.push((0..width).map(|cell| sqlite_text(&row, cell)).collect());
            if rows.len() < limit && rows.len() % step == 0 {
                let peek = stream.try_next().await.map_err(|err| crate::error_text::chain(&err))?;
                let partial = StatementValue {
                    columns: columns.clone(),
                    column_types: column_types.clone(),
                    rows: std::mem::take(&mut rows),
                    truncated: false,
                    affected: None,
                };
                sink.publish(index, &partial).await;
                match peek {
                    None => {
                        drop(stream);
                        return Ok(partial);
                    }
                    Some(next) => {
                        sink.pause(cancel).await?;
                        rows = partial.rows;
                        pending = Some(next);
                    }
                }
            }
        }
        drop(stream);
        let value = StatementValue {
            columns,
            column_types,
            rows,
            truncated,
            affected: None,
        };
        sink.publish(index, &value).await;
        return Ok(value);
    }
    let done = sqlx::query(safe).execute(&mut *conn).await.map_err(|err| crate::error_text::chain(&err))?;
    let value = StatementValue {
        columns: Vec::new(),
        column_types: Vec::new(),
        rows: Vec::new(),
        truncated: false,
        affected: Some(done.rows_affected()),
    };
    sink.publish(index, &value).await;
    Ok(value)
}

fn sqlite_type_names(row: &sqlx::sqlite::SqliteRow) -> Vec<String> {
    row.columns()
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let declared = column.type_info().name();
            if declared.is_empty() || declared == "NULL" {
                sqlite_storage_name(row, index).unwrap_or_else(|| "类型未知".to_owned())
            } else {
                declared.to_owned()
            }
        })
        .collect()
}

fn sqlite_storage_name(row: &sqlx::sqlite::SqliteRow, index: usize) -> Option<String> {
    let raw = row.try_get_raw(index).ok()?;
    let name = raw.type_info().name().to_owned();
    drop(raw);
    if name.is_empty() || name == "NULL" { None } else { Some(name) }
}

fn sqlite_text(row: &sqlx::sqlite::SqliteRow, index: usize) -> Option<String> {
    let storage = {
        let raw = row.try_get_raw(index).ok()?;
        let name = raw.type_info().name().to_owned();
        drop(raw);
        name
    };
    match storage.as_str() {
        "NULL" => None,
        "INTEGER" | "INT4" | "BOOLEAN" => {
            if let Ok(value) = row.try_get::<Option<i64>, _>(index) {
                return value.map(|number| number.to_string());
            }
            row.try_get::<Option<String>, _>(index).ok().flatten()
        }
        // 浮点（含 avg()、带小数的表达式）：按 f64 读，不再落到文本解码而被当成 NULL。
        "REAL" | "FLOAT" | "DOUBLE" => {
            if let Ok(value) = row.try_get::<Option<f64>, _>(index) {
                return value.map(|number| number.to_string());
            }
            row.try_get::<Option<String>, _>(index).ok().flatten()
        }
        "BLOB" => row.try_get::<Option<Vec<u8>>, _>(index).ok().flatten().map(to_hex),
        _ => row
            .try_get::<Option<String>, _>(index)
            .ok()
            .flatten()
            // 声明类型与存储类不一致时（如 TEXT 列里存了数字），按实际值再读一次，不静默变成 NULL。
            .or_else(|| row.try_get_unchecked::<Option<String>, _>(index).ok().flatten()),
    }
}
