//! Redis 查询台：在查询编辑器里逐行执行 Redis 命令，结果按结构化行集返回（KEYS 自动改成游标 SCAN）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::query::exec::{reveal_rows, RowSink, StatementValue};

fn tokenize_redis_line(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in line.trim().trim_end_matches(';').trim().chars() {
        match quote {
            Some(mark) if ch == mark => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn redis_scalar_text(value: &redis::Value) -> Option<String> {
    match value {
        redis::Value::Nil => None,
        redis::Value::Int(n) => Some(n.to_string()),
        redis::Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        redis::Value::SimpleString(s) => Some(s.clone()),
        redis::Value::Okay => Some("OK".to_owned()),
        redis::Value::Boolean(b) => Some(b.to_string()),
        redis::Value::Double(f) => Some(f.to_string()),
        other => Some(format!("{other:?}")),
    }
}

pub(in crate::adapters) async fn redis_query_run(
    conn: &Arc<tokio::sync::Mutex<super::conn::RedisConn>>,
    script: &str,
    limit: usize,
    cancel: &AtomicBool,
    sink: &dyn RowSink,
    index: usize,
) -> Result<StatementValue, String> {
    let cap = limit.max(1);
    let lines: Vec<&str> = script
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with("--"))
        .collect();
    if lines.is_empty() {
        return Err("请输入要执行的 Redis 命令。".to_owned());
    }

    let mut last_result: Option<StatementValue> = None;
    for line in lines {
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消".to_owned());
        }
        let tokens = tokenize_redis_line(line);
        if tokens.is_empty() {
            continue;
        }
        let head_upper = tokens[0].to_ascii_uppercase();

        // KEYS pattern 在查询台中自动转为安全的游标增量 SCAN（最多返回 cap 条，绝不阻塞服务端）。
        if head_upper == "KEYS" {
            let pattern = tokens.get(1).map(String::as_str).unwrap_or("*");
            let mut cursor = 0u64;
            let mut collected: Vec<String> = Vec::new();
            let mut truncated = false;
            let mut rounds = 0usize;
            {
                let mut guard = conn.lock().await;
                loop {
                    if cancel.load(Ordering::SeqCst) {
                        return Err("已取消".to_owned());
                    }
                    let mut cmd = redis::cmd("SCAN");
                    cmd.arg(cursor);
                    if !pattern.is_empty() && pattern != "*" {
                        cmd.arg("MATCH").arg(pattern);
                    }
                    cmd.arg("COUNT").arg(200usize);
                    let (next_cursor, batch): (u64, Vec<String>) = cmd
                        .query_async(&mut *guard)
                        .await
                        .map_err(|err| err.to_string())?;
                    for key in batch {
                        if collected.len() >= cap {
                            truncated = true;
                            break;
                        }
                        if !collected.contains(&key) {
                            collected.push(key);
                        }
                    }
                    cursor = next_cursor;
                    rounds += 1;
                    if cursor == 0 || truncated || rounds >= 50 {
                        if cursor != 0 {
                            truncated = true;
                        }
                        break;
                    }
                }
            }
            let rows: Vec<Vec<Option<String>>> = collected
                .into_iter()
                .enumerate()
                .map(|(ix, key)| vec![Some((ix + 1).to_string()), Some(key)])
                .collect();
            last_result = Some(
                reveal_rows(
                    sink,
                    index,
                    vec!["#".to_owned(), "key".to_owned()],
                    vec!["bigint".to_owned(), "varchar".to_owned()],
                    rows,
                    truncated,
                    None,
                    cap,
                    cancel,
                )
                .await?,
            );
            continue;
        }

        let value: redis::Value = {
            let mut guard = conn.lock().await;
            let mut cmd = redis::cmd(&tokens[0]);
            for arg in &tokens[1..] {
                cmd.arg(arg);
            }
            cmd.query_async(&mut *guard).await.map_err(|err| err.to_string())?
        };

        let (columns, column_types, mut rows) = match (head_upper.as_str(), value) {
            ("HGETALL", redis::Value::Array(items)) => {
                let mut rows = Vec::new();
                for pair in items.chunks(2) {
                    let field = pair.first().and_then(redis_scalar_text);
                    let val = pair.get(1).and_then(redis_scalar_text);
                    rows.push(vec![field, val]);
                }
                (vec!["field".to_owned(), "value".to_owned()], vec!["varchar".to_owned(), "text".to_owned()], rows)
            }
            (_, redis::Value::Map(pairs)) => {
                let rows = pairs
                    .iter()
                    .map(|(k, v)| vec![redis_scalar_text(k), redis_scalar_text(v)])
                    .collect();
                (vec!["field".to_owned(), "value".to_owned()], vec!["varchar".to_owned(), "text".to_owned()], rows)
            }
            ("INFO", redis::Value::BulkString(bytes)) => {
                let text = String::from_utf8_lossy(&bytes);
                let mut section = "server".to_owned();
                let mut rows = Vec::new();
                for info_line in text.lines() {
                    let trimmed = info_line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Some(sec) = trimmed.strip_prefix('#') {
                        section = sec.trim().to_owned();
                        continue;
                    }
                    if let Some((k, v)) = trimmed.split_once(':') {
                        rows.push(vec![Some(section.clone()), Some(k.trim().to_owned()), Some(v.trim().to_owned())]);
                    }
                }
                (
                    vec!["section".to_owned(), "metric".to_owned(), "value".to_owned()],
                    vec!["varchar".to_owned(), "varchar".to_owned(), "varchar".to_owned()],
                    rows,
                )
            }
            (_, redis::Value::Array(items)) | (_, redis::Value::Set(items)) => {
                let rows = items
                    .iter()
                    .enumerate()
                    .map(|(ix, item)| vec![Some((ix + 1).to_string()), redis_scalar_text(item)])
                    .collect();
                (vec!["#".to_owned(), "value".to_owned()], vec!["bigint".to_owned(), "text".to_owned()], rows)
            }
            (_, redis::Value::Int(n)) => (
                vec!["result".to_owned()],
                vec!["bigint".to_owned()],
                vec![vec![Some(n.to_string())]],
            ),
            (_, scalar) => (
                vec!["result".to_owned()],
                vec!["text".to_owned()],
                vec![vec![redis_scalar_text(&scalar)]],
            ),
        };

        let truncated = rows.len() > cap;
        if truncated {
            rows.truncate(cap);
        }
        last_result = Some(reveal_rows(sink, index, columns, column_types, rows, truncated, None, cap, cancel).await?);
    }

    last_result.ok_or_else(|| "请输入要执行的 Redis 命令。".to_owned())
}
