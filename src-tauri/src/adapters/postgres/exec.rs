//! PostgreSQL 的查询执行：simple_query 收行集（先 prepare 取列类型），错误位置按字符换算回用户语句。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::query::exec::{map_rewritten_char, reveal_rows, ErrorAt, Failure, RowSink, StatementValue};

/// PostgreSQL 的 position 从 1 开始、按字符计数。换成语句内从 0 开始的字符偏移；
/// 超出语句长度时落在语句末尾（执行时追加的 LIMIT 不在用户文本里）。
pub(crate) fn pg_error_char(position: u32, statement: &str) -> Option<usize> {
    if position == 0 {
        return None;
    }
    let count = statement.chars().count();
    Some(((position as usize) - 1).min(count))
}

/// 从 tokio-postgres 错误里取位置；只认原语句里的位置，内部生成语句的位置不对应用户文本。
fn pg_error_at(err: &tokio_postgres::Error, statement: &str) -> Option<ErrorAt> {
    match err.as_db_error()?.position()? {
        tokio_postgres::error::ErrorPosition::Original(position) => pg_error_char(*position, statement).map(ErrorAt::Char),
        tokio_postgres::error::ErrorPosition::Internal { .. } => None,
    }
}

/// sent 是实际发出的文本（可能加了行数上限），位置先在 sent 里换算，再映射回用户写的 statement。
fn pg_failure(err: &tokio_postgres::Error, sent: &str, statement: &str) -> Failure {
    let at = match pg_error_at(err, sent) {
        Some(ErrorAt::Char(offset)) => Some(ErrorAt::Char(map_rewritten_char(statement, sent, offset))),
        other => other,
    };
    Failure { message: crate::error_text::chain(err), at }
}

pub(in crate::adapters) async fn run_postgres_statements(client: &tokio_postgres::Client, statements: &[String]) -> Result<(), String> {
    for statement in statements {
        client.simple_query(statement).await.map_err(|err| crate::error_text::chain(&err))?;
    }
    Ok(())
}

pub(in crate::adapters) async fn postgres_current(client: &tokio_postgres::Client) -> Result<String, String> {
    let row = client.query_one("SELECT current_database()", &[]).await.map_err(|err| crate::error_text::chain(&err))?;
    row.try_get(0).map_err(|err| crate::error_text::chain(&err))
}

/// sent 是实际发出的文本（可能带上读取上限），sql 是用户写的那条：错误位置先在 sent 里换算，
/// 再映射回 sql，界面因此永远定位到草稿里自己的语句。
pub(in crate::adapters) async fn postgres_run(
    client: &tokio_postgres::Client,
    sent: &str,
    sql: &str,
    limit: usize,
    cancel: &Arc<AtomicBool>,
    sink: &dyn RowSink,
    index: usize,
) -> Result<StatementValue, Failure> {
    let prepared = match client.prepare(sent).await {
        Ok(statement) => {
            let columns = statement.columns().iter().map(|column| column.name().to_owned()).collect::<Vec<_>>();
            let types = statement.columns().iter().map(|column| column.type_().name().to_owned()).collect::<Vec<_>>();
            drop(statement);
            Some((columns, types))
        }
        Err(_) => None,
    };
    let messages = client.simple_query(sent).await.map_err(|err| pg_failure(&err, sent, sql))?;
    let mut columns = prepared.as_ref().map(|item| item.0.clone()).unwrap_or_default();
    let mut types = prepared.as_ref().map(|item| item.1.clone()).unwrap_or_default();
    let mut rows = Vec::new();
    let mut affected = None;
    let mut truncated = false;
    for message in messages {
        if cancel.load(Ordering::SeqCst) {
            return Err(Failure::from("已取消".to_owned()));
        }
        match message {
            tokio_postgres::SimpleQueryMessage::RowDescription(description) => {
                if columns.is_empty() {
                    columns = description.iter().map(|column| column.name().to_owned()).collect();
                }
            }
            tokio_postgres::SimpleQueryMessage::Row(row) => {
                if rows.len() >= limit {
                    truncated = true;
                    break;
                }
                let width = row.len();
                if columns.is_empty() {
                    columns = (0..width).map(|column| format!("col{}", column + 1)).collect();
                }
                if types.is_empty() {
                    types = vec!["类型未知".to_owned(); columns.len().max(width)];
                } else if types.len() < width {
                    types.resize(width, "类型未知".to_owned());
                }
                let cells = (0..width).map(|cell| row.get(cell).map(str::to_owned)).collect();
                rows.push(cells);
            }
            tokio_postgres::SimpleQueryMessage::CommandComplete(count) => affected = Some(count),
            _ => {}
        }
    }
    if columns.is_empty() {
        let value = StatementValue {
            columns,
            column_types: Vec::new(),
            rows,
            truncated: false,
            affected,
        };
        sink.publish(index, &value).await;
        return Ok(value);
    }
    if types.is_empty() {
        types = vec!["类型未知".to_owned(); columns.len()];
    }
    reveal_rows(sink, index, columns, types, rows, truncated, None, limit, cancel).await.map_err(Failure::from)
}
