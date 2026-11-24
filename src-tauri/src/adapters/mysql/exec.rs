//! MySQL / MariaDB 的查询执行：整批借出一条连接（不发 COM_RESET_CONNECTION）、按需 USE 切库、
//! 会话级超时、逐行读取与类型名/单元文本、语法错误行号。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mysql_async::prelude::Queryable;

use crate::adapters::exec::to_hex;
use crate::adapters::SqlSyntax;
use crate::connections::CancelTransport;
use crate::query::exec::{chunk_rows, database_gate, refuse_message, DatabaseGate, ResultExtras, RowSink, StatementValue};

/// MySQL / MariaDB 的语法错误写在文本里："... near 'x' at line N"。取最后一处，行号从 1 开始。
pub(crate) fn mysql_error_line(message: &str) -> Option<usize> {
    let lower = message.to_ascii_lowercase();
    let at = lower.rfind("at line ")?;
    let digits: String = lower[at + "at line ".len()..].chars().take_while(|ch| ch.is_ascii_digit()).collect();
    digits.parse::<usize>().ok().filter(|line| *line > 0)
}

async fn checkout_mysql_pool(pool: &mysql_async::Pool, wait: Duration) -> Result<mysql_async::Conn, String> {
    match tokio::time::timeout(wait, pool.get_conn()).await {
        Ok(Ok(conn)) => Ok(conn),
        Ok(Err(err)) => Err(crate::error_text::chain(&err)),
        Err(_) => Err("建立查询执行连接超时".to_owned()),
    }
}

pub(in crate::adapters) async fn acquire_mysql_conn(exec_pool: &mysql_async::Pool) -> Result<mysql_async::Conn, String> {
    checkout_mysql_pool(exec_pool, Duration::from_secs(12)).await
}

pub(in crate::adapters) fn is_unsupported_timeout_var(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("unknown system variable")
        || lower.contains("1193")
        || lower.contains("max_execution_time")
        || lower.contains("max_statement_time")
}

pub(in crate::adapters) async fn align_mysql(conn: &mut mysql_async::Conn, engine: &str, database: &str) -> Result<(), String> {
    // DATABASE() returns one row containing SQL NULL until a schema is selected.
    let value: Option<Option<String>> = conn.query_first("SELECT DATABASE()").await.map_err(|err| crate::error_text::chain(&err))?;
    let current = value.flatten().unwrap_or_default();
    match database_gate(engine, &current, database) {
        DatabaseGate::Refuse => Err(refuse_message(&current, database)),
        DatabaseGate::Switch => {
            let statement = format!("USE {}", quote_mysql(database));
            conn.query_drop(statement).await.map_err(|err| crate::error_text::chain(&err))?;
            Ok(())
        }
        DatabaseGate::Ready => Ok(()),
    }
}

pub(in crate::adapters) async fn run_mysql_statements(conn: &mut mysql_async::Conn, statements: &[String]) -> Result<(), String> {
    for statement in statements {
        conn.query_drop(statement).await.map_err(|err| crate::error_text::chain(&err))?;
    }
    Ok(())
}

fn quote_mysql(name: &str) -> String {
    super::syntax::MYSQL_SYNTAX.quote_ident(name)
}

/// 语句是不是存储过程调用：CALL 要读完全部结果集，并取 SHOW WARNINGS。
fn is_call(sql: &str) -> bool {
    let (tokens, _) = crate::query::complete::tokens(sql);
    tokens.first().is_some_and(|token| token.word && !token.quoted && token.text.eq_ignore_ascii_case("CALL"))
}

/// 第一个结果集之后剩下的结果集（存储过程的多个 SELECT、最后的 OK 包）。
/// 每个带列的结果集最多收 limit 行；超出的行只跳过不保存。OK 包的影响行数记在 extras.affected。
async fn read_more_sets(
    result: &mut mysql_async::QueryResult<'_, '_, mysql_async::TextProtocol>,
    limit: usize,
    cancel: &Arc<AtomicBool>,
    extras: &mut ResultExtras,
) -> Result<(), String> {
    loop {
        if result.is_empty() {
            return Ok(());
        }
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消".to_owned());
        }
        let column_meta: Vec<(String, mysql_async::consts::ColumnType, u16)> = result
            .columns_ref()
            .iter()
            .map(|column| (column.name_str().into_owned(), column.column_type(), column.character_set()))
            .collect();
        if column_meta.is_empty() {
            extras.affected = Some(result.affected_rows());
            // 没有行的 OK 包：读一次就前进到下一个结果集。
            let _ = result.next().await.map_err(|err| crate::error_text::chain(&err))?;
            continue;
        }
        let mut value = StatementValue {
            columns: column_meta.iter().map(|column| column.0.clone()).collect(),
            column_types: column_meta.iter().map(|column| mysql_type_name(column.1, column.2).to_owned()).collect(),
            rows: Vec::new(),
            truncated: false,
            affected: None,
        };
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err("已取消".to_owned());
            }
            let Some(row) = result.next().await.map_err(|err| crate::error_text::chain(&err))? else {
                break;
            };
            if value.rows.len() >= limit {
                value.truncated = true;
                continue;
            }
            value.rows.push(
                (0..value.columns.len())
                    .map(|cell| mysql_cell(row.as_ref(cell).unwrap_or(&mysql_async::Value::NULL), column_meta[cell].1, column_meta[cell].2))
                    .collect(),
            );
        }
        extras.more.push(value);
    }
}

/// CALL 之后的 SHOW WARNINGS：每条一行“级别 代码：信息”。读不到时不报错，只是没有警告。
async fn read_warnings(conn: &mut mysql_async::Conn) -> Vec<String> {
    let rows: Vec<(String, u32, String)> = match conn.query("SHOW WARNINGS").await {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };
    rows.into_iter().map(|(level, code, message)| format!("{level} {code}: {message}")).collect()
}

/// 执行一条语句。返回第一个结果集（与流式发布的一致），其余结果集、最后的影响行数和警告放在 ResultExtras。
#[allow(clippy::too_many_arguments)]
pub(in crate::adapters) async fn mysql_run(
    conn: &mut mysql_async::Conn,
    transport: &CancelTransport,
    bounded: &str,
    limit: usize,
    cancel: &Arc<AtomicBool>,
    sink: &dyn RowSink,
    index: usize,
) -> Result<(StatementValue, ResultExtras), String> {
    // 握手包里的线程 id 就是 CONNECTION_ID()；不再为每条语句多走一次网络往返。
    transport.set_mysql_connection_id(u64::from(conn.id()));
    let call = is_call(bounded);
    let mut extras = ResultExtras::default();
    let value = {
        // 读取上限已由 LiveDriver 改写过，这里发出去的就是实际语句。
        let mut result = conn.query_iter(bounded.to_owned()).await.map_err(|err| crate::error_text::chain(&err))?;
        let (value, exhausted) = match mysql_first_set(&mut result, limit, cancel, sink, index).await {
            Ok(first) => first,
            Err(message) => {
                if cancel.load(Ordering::SeqCst) {
                    let _ = result.drop_result().await;
                }
                return Err(message);
            }
        };
        // 存储过程可能返回多个结果集：逐个收下，不再丢掉。
        if exhausted && call {
            if let Err(message) = read_more_sets(&mut result, limit, cancel, &mut extras).await {
                // 中途出错：已经收到的结果集保留，错误写进说明。
                extras.notes.push(message);
            }
        }
        let _ = result.drop_result().await;
        value
    };
    if call {
        extras.warnings = read_warnings(conn).await;
    }
    if call && value.columns.is_empty() && extras.more.is_empty() {
        extras.no_result_set = true;
    }
    Ok((value, extras))
}

/// 第一个结果集：带列时按块流式发布；返回值和“这个结果集是否已经读完”（读完才能继续读后面的结果集）。
async fn mysql_first_set(
    result: &mut mysql_async::QueryResult<'_, '_, mysql_async::TextProtocol>,
    limit: usize,
    cancel: &Arc<AtomicBool>,
    sink: &dyn RowSink,
    index: usize,
) -> Result<(StatementValue, bool), String> {
    let column_meta: Vec<(String, mysql_async::consts::ColumnType, u16)> = result
        .columns_ref()
        .iter()
        .map(|column| (column.name_str().into_owned(), column.column_type(), column.character_set()))
        .collect();
    let columns: Vec<String> = column_meta.iter().map(|column| column.0.clone()).collect();
    let column_types: Vec<String> = column_meta.iter().map(|column| mysql_type_name(column.1, column.2).to_owned()).collect();
    if columns.is_empty() {
        let affected = result.affected_rows();
        // OK 包没有行：读一次前进到下一个结果集（没有就结束），同一条连接才能执行下一条。
        let _ = result.next().await.map_err(|err| crate::error_text::chain(&err))?;
        let value = StatementValue {
            columns,
            column_types,
            rows: Vec::new(),
            truncated: false,
            affected: Some(affected),
        };
        sink.publish(index, &value).await;
        return Ok((value, true));
    }
    let mut rows = Vec::new();
    let mut truncated = false;
    let mut pending: Option<mysql_async::Row> = None;
    let step = chunk_rows(limit);
    loop {
        if cancel.load(Ordering::SeqCst) {
            // 调用方收掉剩余结果（drop_result），同一条连接才能执行下一条。
            return Err("已取消".to_owned());
        }
        let row = if let Some(row) = pending.take() {
            row
        } else {
            match result.next().await.map_err(|err| crate::error_text::chain(&err))? {
                Some(row) => row,
                None => break,
            }
        };
        if rows.len() >= limit {
            truncated = true;
            break;
        }
        let cells = (0..columns.len())
            .map(|cell| mysql_cell(row.as_ref(cell).unwrap_or(&mysql_async::Value::NULL), column_meta[cell].1, column_meta[cell].2))
            .collect();
        rows.push(cells);
        if rows.len() < limit && rows.len() % step == 0 {
            let peek = result.next().await.map_err(|err| crate::error_text::chain(&err))?;
            let partial = StatementValue {
                columns: columns.clone(),
                column_types: column_types.clone(),
                rows: std::mem::take(&mut rows),
                truncated: false,
                affected: None,
            };
            sink.publish(index, &partial).await;
            match peek {
                None => return Ok((partial, true)),
                Some(next) => {
                    sink.pause(cancel).await?;
                    rows = partial.rows;
                    pending = Some(next);
                }
            }
        }
    }
    let mut exhausted = true;
    if truncated {
        // LIMIT 已让服务器停住；再读一次只是收结束包，不是把剩余行读完。
        exhausted = result.next().await.map_err(|err| crate::error_text::chain(&err))?.is_none();
    }
    let value = StatementValue {
        columns,
        column_types,
        rows,
        truncated,
        affected: None,
    };
    sink.publish(index, &value).await;
    Ok((value, exhausted))
}

const MYSQL_BINARY_CHARSET: u16 = 63;

fn mysql_type_name(kind: mysql_async::consts::ColumnType, charset: u16) -> &'static str {
    use mysql_async::consts::ColumnType::*;
    let binary = charset == MYSQL_BINARY_CHARSET;
    match kind {
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "DECIMAL",
        MYSQL_TYPE_TINY => "TINYINT",
        MYSQL_TYPE_SHORT => "SMALLINT",
        MYSQL_TYPE_LONG => "INT",
        MYSQL_TYPE_FLOAT => "FLOAT",
        MYSQL_TYPE_DOUBLE => "DOUBLE",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "TIMESTAMP",
        MYSQL_TYPE_LONGLONG => "BIGINT",
        MYSQL_TYPE_INT24 => "MEDIUMINT",
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "DATE",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "TIME",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "DATETIME",
        MYSQL_TYPE_YEAR => "YEAR",
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING => {
            if binary {
                "VARBINARY"
            } else {
                "VARCHAR"
            }
        }
        MYSQL_TYPE_BIT => "BIT",
        MYSQL_TYPE_JSON => "JSON",
        MYSQL_TYPE_ENUM => "ENUM",
        MYSQL_TYPE_SET => "SET",
        MYSQL_TYPE_TINY_BLOB => {
            if binary {
                "TINYBLOB"
            } else {
                "TINYTEXT"
            }
        }
        MYSQL_TYPE_BLOB => {
            if binary {
                "BLOB"
            } else {
                "TEXT"
            }
        }
        MYSQL_TYPE_MEDIUM_BLOB => {
            if binary {
                "MEDIUMBLOB"
            } else {
                "MEDIUMTEXT"
            }
        }
        MYSQL_TYPE_LONG_BLOB => {
            if binary {
                "LONGBLOB"
            } else {
                "LONGTEXT"
            }
        }
        MYSQL_TYPE_STRING => {
            if binary {
                "BINARY"
            } else {
                "CHAR"
            }
        }
        MYSQL_TYPE_GEOMETRY => "GEOMETRY",
        MYSQL_TYPE_NULL => "NULL",
        MYSQL_TYPE_VECTOR => "VECTOR",
        MYSQL_TYPE_UNKNOWN | MYSQL_TYPE_TYPED_ARRAY => "类型未知",
    }
}

fn mysql_cell(value: &mysql_async::Value, kind: mysql_async::consts::ColumnType, charset: u16) -> Option<String> {
    use mysql_async::consts::ColumnType::*;
    if matches!(kind, MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL) {
        return match value {
            mysql_async::Value::NULL => None,
            mysql_async::Value::Bytes(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
            mysql_async::Value::Int(number) => Some(number.to_string()),
            mysql_async::Value::UInt(number) => Some(number.to_string()),
            mysql_async::Value::Float(_) | mysql_async::Value::Double(_) => None,
            mysql_async::Value::Date(_, _, _, _, _, _, _) | mysql_async::Value::Time(_, _, _, _, _, _) => None,
        };
    }
    let binary = charset == MYSQL_BINARY_CHARSET
        && matches!(
            kind,
            MYSQL_TYPE_TINY_BLOB
                | MYSQL_TYPE_BLOB
                | MYSQL_TYPE_MEDIUM_BLOB
                | MYSQL_TYPE_LONG_BLOB
                | MYSQL_TYPE_STRING
                | MYSQL_TYPE_VARCHAR
                | MYSQL_TYPE_VAR_STRING
        );
    match value {
        mysql_async::Value::NULL => None,
        mysql_async::Value::Bytes(bytes) => {
            if binary {
                Some(to_hex(bytes.clone()))
            } else {
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
        }
        mysql_async::Value::Int(number) => Some(number.to_string()),
        mysql_async::Value::UInt(number) => Some(number.to_string()),
        mysql_async::Value::Float(number) => Some(number.to_string()),
        mysql_async::Value::Double(number) => Some(number.to_string()),
        mysql_async::Value::Date(year, month, day, hour, minute, second, micros) => Some(mysql_date_text(kind, *year, *month, *day, *hour, *minute, *second, *micros)),
        mysql_async::Value::Time(negative, days, hour, minute, second, micros) => {
            Some(format!("{}{days} {hour:02}:{minute:02}:{second:02}.{micros:06}", if *negative { "-" } else { "" }))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn mysql_date_text(kind: mysql_async::consts::ColumnType, year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8, micros: u32) -> String {
    use mysql_async::consts::ColumnType::*;
    let date = format!("{year:04}-{month:02}-{day:02}");
    let time = if micros == 0 {
        format!("{hour:02}:{minute:02}:{second:02}")
    } else {
        format!("{hour:02}:{minute:02}:{second:02}.{micros:06}")
    };
    match kind {
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => date,
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => time,
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 | MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => format!("{date} {time}"),
        _ if hour == 0 && minute == 0 && second == 0 && micros == 0 => date,
        _ => format!("{date} {time}"),
    }
}

#[cfg(test)]
mod mysql_value_tests {
    use super::{mysql_cell, mysql_type_name};
    use mysql_async::consts::ColumnType::*;

    #[test]
    fn charset_splits_text_from_binary() {
        let text = mysql_async::Value::Bytes("你好".as_bytes().to_vec());
        let binary = mysql_async::Value::Bytes(vec![0xff, 0x00, 0x41]);
        assert_eq!(mysql_type_name(MYSQL_TYPE_BLOB, 45), "TEXT");
        assert_eq!(mysql_cell(&text, MYSQL_TYPE_BLOB, 45).as_deref(), Some("你好"));
        assert_eq!(mysql_type_name(MYSQL_TYPE_TINY_BLOB, 33), "TINYTEXT");
        assert_eq!(mysql_type_name(MYSQL_TYPE_MEDIUM_BLOB, 45), "MEDIUMTEXT");
        assert_eq!(mysql_type_name(MYSQL_TYPE_LONG_BLOB, 45), "LONGTEXT");
        assert_eq!(mysql_type_name(MYSQL_TYPE_BLOB, 63), "BLOB");
        assert_eq!(mysql_type_name(MYSQL_TYPE_TINY_BLOB, 63), "TINYBLOB");
        assert_eq!(mysql_type_name(MYSQL_TYPE_MEDIUM_BLOB, 63), "MEDIUMBLOB");
        assert_eq!(mysql_type_name(MYSQL_TYPE_LONG_BLOB, 63), "LONGBLOB");
        assert_eq!(mysql_cell(&binary, MYSQL_TYPE_BLOB, 63).as_deref(), Some("ff0041"));
        assert_eq!(mysql_type_name(MYSQL_TYPE_VAR_STRING, 63), "VARBINARY");
        assert_eq!(mysql_type_name(MYSQL_TYPE_VARCHAR, 63), "VARBINARY");
        assert_eq!(mysql_type_name(MYSQL_TYPE_STRING, 63), "BINARY");
        assert_eq!(mysql_cell(&binary, MYSQL_TYPE_VAR_STRING, 63).as_deref(), Some("ff0041"));
        assert_eq!(mysql_cell(&binary, MYSQL_TYPE_STRING, 63).as_deref(), Some("ff0041"));
        assert_eq!(mysql_type_name(MYSQL_TYPE_VAR_STRING, 45), "VARCHAR");
        assert_eq!(mysql_type_name(MYSQL_TYPE_STRING, 45), "CHAR");
        assert_eq!(mysql_cell(&mysql_async::Value::Bytes(b"abc".to_vec()), MYSQL_TYPE_VAR_STRING, 45).as_deref(), Some("abc"));
        assert_eq!(mysql_cell(&mysql_async::Value::Bytes(b"12.50".to_vec()), MYSQL_TYPE_NEWDECIMAL, 63).as_deref(), Some("12.50"));
        assert_eq!(mysql_cell(&mysql_async::Value::Date(2024, 1, 2, 0, 0, 0, 0), MYSQL_TYPE_DATE, 63).as_deref(), Some("2024-01-02"));
    }
}
