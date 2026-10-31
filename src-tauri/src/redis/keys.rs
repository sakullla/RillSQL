//! 键空间读取与键操作：SCAN 分页（MATCH+游标+COUNT）、TYPE/PTTL、按类型读值、TTL 修改、删除/重命名。
//! 本模块不做前端确认——confirmed 标记的语义在 commands 层强制；这里只提供命令组装与执行。

use serde::Serialize;

pub const DEFAULT_SCAN_COUNT: usize = 100;
/// SCAN 的 COUNT 提示上限：Redis 把它当提示值，过大的页直接拒绝避免一次拉全库。
pub const MAX_SCAN_COUNT: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedisKeyInfo {
    pub name: String,
    pub key_type: String,
    /// PTTL 毫秒：None=不过期，-2=键已不存在（读取瞬间被删）。
    pub ttl_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanPage {
    /// 下一页游标；0 表示遍历完成。
    pub cursor: u64,
    pub done: bool,
    pub keys: Vec<RedisKeyInfo>,
}

/// 按类型渲染的值载荷。前端 src/values/ 按这些形态分派查看器。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RedisValue {
    String { text: String },
    Hash { entries: Vec<KeyValuePair> },
    List { items: Vec<String> },
    Set { items: Vec<String> },
    ZSet { entries: Vec<ScoredMember> },
    Stream { messages: Vec<StreamMessage> },
    /// 键不存在或类型暂不支持结构化渲染。
    None {},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyValuePair {
    pub field: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoredMember {
    pub member: String,
    pub score: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamMessage {
    pub id: String,
    pub fields: Vec<KeyValuePair>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedisKeyDetail {
    pub key_type: String,
    pub ttl_ms: Option<i64>,
    pub value: RedisValue,
}

/// SCAN 参数组装（纯函数，供测试与命令组装共用）。游标 0=从头上开始；模式沿用 glob 语义（user:*）。
/// 全库没有一处调用 KEYS：分页只经这里的 SCAN。
pub fn scan_args(cursor: u64, pattern: Option<&str>, count: usize) -> Vec<String> {
    let mut args = vec![cursor.to_string()];
    if let Some(pattern) = pattern.filter(|item| !item.trim().is_empty()) {
        args.push("MATCH".to_owned());
        args.push(pattern.to_owned());
    }
    args.push("COUNT".to_owned());
    args.push(count.clamp(1, MAX_SCAN_COUNT).to_string());
    args
}

/// 值读取的命令形态（纯函数，供测试断言；执行在 read_value）。
/// zset 带 WITHSCORES、stream 用 XRANGE - + 全量；list 用 LRANGE 0 -1。
pub fn value_read_plan(key_type: &str) -> &'static [&'static str] {
    match key_type {
        "string" => &["GET"],
        "hash" => &["HGETALL"],
        "list" => &["LRANGE", "0", "-1"],
        "set" => &["SMEMBERS"],
        "zset" => &["ZRANGE", "0", "-1", "WITHSCORES"],
        "stream" => &["XRANGE", "-", "+"],
        _ => &[],
    }
}

/// TTL 修改命令组装（纯函数）：Some(n)=EXPIRE n 秒；None=PERSIST（移除过期）。
pub fn ttl_command(seconds: Option<i64>) -> (&'static str, i64) {
    match seconds {
        Some(secs) => ("EXPIRE", secs),
        None => ("PERSIST", 0),
    }
}

fn bulk_text(value: redis::Value) -> String {
    match value {
        redis::Value::BulkString(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        redis::Value::SimpleString(text) => text,
        redis::Value::Okay => "OK".to_owned(),
        redis::Value::Int(number) => number.to_string(),
        redis::Value::Nil => String::new(),
        other => format!("{other:?}"),
    }
}

fn flatten_pairs(values: Vec<redis::Value>) -> Vec<KeyValuePair> {
    let mut entries = Vec::with_capacity(values.len() / 2);
    let mut iter = values.into_iter();
    while let (Some(field), Some(value)) = (iter.next(), iter.next()) {
        entries.push(KeyValuePair {
            field: bulk_text(field),
            value: bulk_text(value),
        });
    }
    entries
}

/// SCAN 一页：返回下一游标与本页键的 TYPE/PTTL（一次 pipeline 批量读取）。
pub async fn scan_keys(
    conn: &mut redis::aio::MultiplexedConnection,
    cursor: u64,
    pattern: Option<&str>,
    count: usize,
) -> Result<ScanPage, redis::RedisError> {
    let mut scan = redis::cmd("SCAN");
    for arg in scan_args(cursor, pattern, count) {
        scan.arg(arg);
    }
    let (next, names): (u64, Vec<String>) = scan.query_async(conn).await?;
    if names.is_empty() {
        return Ok(ScanPage {
            cursor: next,
            done: next == 0,
            keys: Vec::new(),
        });
    }
    let mut pipe = redis::pipe();
    for name in &names {
        pipe.cmd("TYPE").arg(name).cmd("PTTL").arg(name);
    }
    let infos: Vec<redis::Value> = pipe.query_async(conn).await?;
    // pipeline 结果与命令顺序一致：每个键依次是 TYPE 的值与 PTTL 的整数。
    let mut keys = Vec::with_capacity(names.len());
    for (index, name) in names.iter().enumerate() {
        let key_type = infos.get(index * 2).cloned().map(bulk_text).unwrap_or_default();
        let ttl_ms = match infos.get(index * 2 + 1) {
            Some(redis::Value::Int(ms)) => Some(*ms),
            _ => None,
        };
        keys.push(RedisKeyInfo {
            name: name.clone(),
            key_type,
            ttl_ms,
        });
    }
    Ok(ScanPage {
        cursor: next,
        done: next == 0,
        keys,
    })
}

/// 读取单个键的类型、TTL 与按类型的值。
pub async fn read_key(conn: &mut redis::aio::MultiplexedConnection, key: &str) -> Result<RedisKeyDetail, redis::RedisError> {
    let key_type: String = redis::cmd("TYPE").arg(key).query_async(conn).await?;
    let ttl_ms: i64 = redis::cmd("PTTL").arg(key).query_async(conn).await?;
    let value = read_value(conn, key, &key_type).await?;
    Ok(RedisKeyDetail {
        key_type,
        ttl_ms: Some(ttl_ms),
        value,
    })
}

async fn read_value(
    conn: &mut redis::aio::MultiplexedConnection,
    key: &str,
    key_type: &str,
) -> Result<RedisValue, redis::RedisError> {
    let raw = match key_type {
        "string" => {
            let text: String = redis::cmd("GET").arg(key).query_async(conn).await?;
            return Ok(RedisValue::String { text });
        }
        "hash" => redis::cmd("HGETALL").arg(key).query_async::<Vec<redis::Value>>(conn).await?,
        "list" => redis::cmd("LRANGE").arg(key).arg(0).arg(-1).query_async::<Vec<redis::Value>>(conn).await?,
        "set" => redis::cmd("SMEMBERS").arg(key).query_async::<Vec<redis::Value>>(conn).await?,
        "zset" => redis::cmd("ZRANGE")
            .arg(key)
            .arg(0)
            .arg(-1)
            .arg("WITHSCORES")
            .query_async::<Vec<redis::Value>>(conn)
            .await?,
        "stream" => redis::cmd("XRANGE").arg(key).arg("-").arg("+").query_async::<Vec<redis::Value>>(conn).await?,
        _ => return Ok(RedisValue::None {}),
    };
    Ok(match key_type {
        "hash" => RedisValue::Hash { entries: flatten_pairs(raw) },
        "list" => RedisValue::List {
            items: raw.into_iter().map(bulk_text).collect(),
        },
        "set" => RedisValue::Set {
            items: raw.into_iter().map(bulk_text).collect(),
        },
        "zset" => {
            let entries = flatten_pairs(raw)
                .into_iter()
                .map(|pair| ScoredMember {
                    member: pair.field,
                    score: pair.value,
                })
                .collect();
            RedisValue::ZSet { entries }
        }
        "stream" => {
            let mut messages = Vec::with_capacity(raw.len());
            for entry in raw {
                let redis::Value::Array(parts) = entry else {
                    continue;
                };
                let mut parts = parts.into_iter();
                let id = bulk_text(parts.next().unwrap_or(redis::Value::Nil));
                let fields = match parts.next() {
                    Some(redis::Value::Array(field_values)) => flatten_pairs(field_values),
                    _ => Vec::new(),
                };
                messages.push(StreamMessage { id, fields });
            }
            RedisValue::Stream { messages }
        }
        _ => RedisValue::None {},
    })
}

/// 修改 TTL：Some(n)=EXPIRE；None=PERSIST。返回命令是否作用到键（0=键不存在）。
pub async fn set_ttl(
    conn: &mut redis::aio::MultiplexedConnection,
    key: &str,
    seconds: Option<i64>,
) -> Result<i64, redis::RedisError> {
    let (command, amount) = ttl_command(seconds);
    let mut cmd = redis::cmd(command);
    cmd.arg(key);
    if command == "EXPIRE" {
        cmd.arg(amount);
    }
    cmd.query_async(conn).await
}

/// 删除键（DEL）。确认语义由 commands 层强制。
pub async fn delete_keys(conn: &mut redis::aio::MultiplexedConnection, keys: &[String]) -> Result<i64, redis::RedisError> {
    let mut cmd = redis::cmd("DEL");
    for key in keys {
        cmd.arg(key);
    }
    cmd.query_async(conn).await
}

/// 重命名键（RENAME）。确认语义由 commands 层强制。
pub async fn rename_key(conn: &mut redis::aio::MultiplexedConnection, from: &str, to: &str) -> Result<(), redis::RedisError> {
    redis::cmd("RENAME").arg(from).arg(to).query_async(conn).await
}
