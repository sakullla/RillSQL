//! 键空间读取与键操作：SCAN 分页（MATCH+游标+COUNT）、TYPE/PTTL、按类型读值、TTL 修改、删除/重命名。
//! 本模块只组装 Redis 命令。确认语义在 `require_delete` / `require_rename` 与应用服务里强制，不能只留在 Tauri command。

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
#[allow(dead_code)]
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

pub fn require_delete(keys: &[String], confirmed: bool) -> Result<(), String> {
    if keys.is_empty() {
        return Err("没有选择要删除的键。".to_owned());
    }
    if !confirmed {
        return Err(format!("删除 {} 个键属于写删操作，需要先在界面确认。", keys.len()));
    }
    Ok(())
}

pub fn require_rename(from: &str, to: &str, confirmed: bool) -> Result<(), String> {
    if from.is_empty() || to.is_empty() {
        return Err("重命名需要源键名和新键名。".to_owned());
    }
    if !confirmed {
        return Err("重命名键属于写删操作，需要先在界面确认。".to_owned());
    }
    Ok(())
}

pub fn require_ttl(seconds: Option<i64>) -> Result<(), String> {
    if let Some(seconds) = seconds {
        if seconds < 0 {
            return Err("TTL 秒数应为非负整数；要移除过期请用“永不过期”。".to_owned());
        }
    }
    Ok(())
}

/// 新建键时的初始值。空字符串对 String 合法，其余类型至少要有一个成员。
#[derive(Debug, Clone, PartialEq)]
pub enum RedisInitialValue {
    String(String),
    Hash { field: String, value: String },
    List(String),
    Set(String),
    ZSet { score: f64, member: String },
    Stream { id: String, fields: Vec<(String, String)> },
}

/// 类型化写操作。每一种都必须先过 `prepare_mutation` 的 confirmed 门禁，才能发到服务器。
#[derive(Debug, Clone, PartialEq)]
pub enum RedisMutation {
    CreateKey { key: String, ttl_secs: Option<i64>, initial: RedisInitialValue, overwrite: bool },
    SetString { key: String, value: String },
    HashSet { key: String, field: String, value: String },
    HashDel { key: String, fields: Vec<String> },
    ListPush { key: String, head: bool, value: String },
    ListSet { key: String, index: i64, value: String },
    ListRem { key: String, count: i64, value: String },
    SetAdd { key: String, member: String },
    SetRem { key: String, member: String },
    ZSetAdd { key: String, score: f64, member: String },
    ZSetRem { key: String, member: String },
    StreamAdd { key: String, id: String, fields: Vec<(String, String)> },
    StreamDel { key: String, id: String },
    SetTtl { key: String, seconds: Option<i64> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationStep {
    pub word: String,
    pub args: Vec<String>,
}

/// 已通过确认和参数校验的写计划。`exists_key` 有值时，执行前先 `EXISTS`，避免静默覆盖。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationPlan {
    pub exists_key: Option<String>,
    pub overwrite: bool,
    pub steps: Vec<MutationStep>,
}

#[derive(Debug)]
pub enum MutationFault {
    Io(redis::RedisError),
    Rejected(String),
}

impl std::fmt::Display for MutationFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Rejected(message) => f.write_str(message),
        }
    }
}

fn require_key(key: &str) -> Result<(), String> {
    if key.trim().is_empty() {
        return Err("需要键名。".to_owned());
    }
    Ok(())
}

fn finite_score(score: f64) -> Result<String, String> {
    if !score.is_finite() {
        return Err("分值必须是有限数字。".to_owned());
    }
    Ok(format!("{score}"))
}

fn push_step(steps: &mut Vec<MutationStep>, word: &str, args: Vec<String>) {
    steps.push(MutationStep { word: word.to_owned(), args });
}

fn expire_step(steps: &mut Vec<MutationStep>, key: &str, seconds: Option<i64>) -> Result<(), String> {
    require_ttl(seconds)?;
    if let Some(seconds) = seconds {
        push_step(steps, "EXPIRE", vec![key.to_owned(), seconds.to_string()]);
    }
    Ok(())
}

/// 确认门禁和参数校验。`confirmed == false` 时立刻拒绝，不产生任何 Redis 命令。
pub fn prepare_mutation(mutation: &RedisMutation, confirmed: bool) -> Result<MutationPlan, String> {
    if !confirmed {
        return Err("这个写操作需要先在界面确认。".to_owned());
    }
    let mut steps = Vec::new();
    let mut exists_key = None;
    let mut overwrite = false;
    match mutation {
        RedisMutation::CreateKey { key, ttl_secs, initial, overwrite: replace } => {
            require_key(key)?;
            require_ttl(*ttl_secs)?;
            exists_key = Some(key.clone());
            overwrite = *replace;
            match initial {
                RedisInitialValue::String(value) => {
                    if ttl_secs.is_none() {
                        push_step(&mut steps, "SET", vec![key.clone(), value.clone(), "KEEPTTL".into()]);
                    } else {
                        push_step(&mut steps, "SET", vec![key.clone(), value.clone()]);
                    }
                }
                RedisInitialValue::Hash { field, value } => {
                    if field.is_empty() {
                        return Err("Hash 需要字段名。".to_owned());
                    }
                    push_step(&mut steps, "HSET", vec![key.clone(), field.clone(), value.clone()]);
                }
                RedisInitialValue::List(value) => push_step(&mut steps, "RPUSH", vec![key.clone(), value.clone()]),
                RedisInitialValue::Set(member) => {
                    if member.is_empty() {
                        return Err("Set 需要成员。".to_owned());
                    }
                    push_step(&mut steps, "SADD", vec![key.clone(), member.clone()]);
                }
                RedisInitialValue::ZSet { score, member } => {
                    if member.is_empty() {
                        return Err("ZSet 需要成员。".to_owned());
                    }
                    let score = finite_score(*score)?;
                    push_step(&mut steps, "ZADD", vec![key.clone(), score, member.clone()]);
                }
                RedisInitialValue::Stream { id, fields } => {
                    if fields.is_empty() || fields.iter().any(|(field, _)| field.is_empty()) {
                        return Err("Stream 至少需要一对字段。".to_owned());
                    }
                    let mut args = vec![key.clone(), if id.is_empty() { "*".to_owned() } else { id.clone() }];
                    for (field, value) in fields {
                        args.push(field.clone());
                        args.push(value.clone());
                    }
                    push_step(&mut steps, "XADD", args);
                }
            }
            expire_step(&mut steps, key, *ttl_secs)?;
        }
        RedisMutation::SetString { key, value } => {
            require_key(key)?;
            push_step(&mut steps, "SET", vec![key.clone(), value.clone(), "KEEPTTL".into()]);
        }
        RedisMutation::HashSet { key, field, value } => {
            require_key(key)?;
            if field.is_empty() {
                return Err("Hash 需要字段名。".to_owned());
            }
            push_step(&mut steps, "HSET", vec![key.clone(), field.clone(), value.clone()]);
        }
        RedisMutation::HashDel { key, fields } => {
            require_key(key)?;
            if fields.is_empty() {
                return Err("没有要删除的字段。".to_owned());
            }
            let mut args = vec![key.clone()];
            args.extend(fields.iter().cloned());
            push_step(&mut steps, "HDEL", args);
        }
        RedisMutation::ListPush { key, head, value } => {
            require_key(key)?;
            push_step(&mut steps, if *head { "LPUSH" } else { "RPUSH" }, vec![key.clone(), value.clone()]);
        }
        RedisMutation::ListSet { key, index, value } => {
            require_key(key)?;
            push_step(&mut steps, "LSET", vec![key.clone(), index.to_string(), value.clone()]);
        }
        RedisMutation::ListRem { key, count, value } => {
            require_key(key)?;
            push_step(&mut steps, "LREM", vec![key.clone(), count.to_string(), value.clone()]);
        }
        RedisMutation::SetAdd { key, member } => {
            require_key(key)?;
            if member.is_empty() {
                return Err("Set 需要成员。".to_owned());
            }
            push_step(&mut steps, "SADD", vec![key.clone(), member.clone()]);
        }
        RedisMutation::SetRem { key, member } => {
            require_key(key)?;
            push_step(&mut steps, "SREM", vec![key.clone(), member.clone()]);
        }
        RedisMutation::ZSetAdd { key, score, member } => {
            require_key(key)?;
            if member.is_empty() {
                return Err("ZSet 需要成员。".to_owned());
            }
            let score = finite_score(*score)?;
            push_step(&mut steps, "ZADD", vec![key.clone(), score, member.clone()]);
        }
        RedisMutation::ZSetRem { key, member } => {
            require_key(key)?;
            push_step(&mut steps, "ZREM", vec![key.clone(), member.clone()]);
        }
        RedisMutation::StreamAdd { key, id, fields } => {
            require_key(key)?;
            if fields.is_empty() || fields.iter().any(|(field, _)| field.is_empty()) {
                return Err("Stream 至少需要一对字段。".to_owned());
            }
            let mut args = vec![key.clone(), if id.is_empty() { "*".to_owned() } else { id.clone() }];
            for (field, value) in fields {
                args.push(field.clone());
                args.push(value.clone());
            }
            push_step(&mut steps, "XADD", args);
        }
        RedisMutation::StreamDel { key, id } => {
            require_key(key)?;
            if id.is_empty() {
                return Err("Stream 需要消息 ID。".to_owned());
            }
            push_step(&mut steps, "XDEL", vec![key.clone(), id.clone()]);
        }
        RedisMutation::SetTtl { key, seconds } => {
            require_key(key)?;
            require_ttl(*seconds)?;
            let (word, amount) = ttl_command(*seconds);
            let mut args = vec![key.clone()];
            if word == "EXPIRE" {
                args.push(amount.to_string());
            }
            push_step(&mut steps, word, args);
        }
    }
    if steps.iter().any(|step| step.word.eq_ignore_ascii_case("KEYS")) {
        return Err("已禁用 KEYS：键空间浏览一律走 SCAN。".to_owned());
    }
    Ok(MutationPlan { exists_key, overwrite, steps })
}

/// 确认条上展示的命令摘要。
pub fn mutation_preview(mutation: &RedisMutation) -> String {
    match mutation {
        RedisMutation::CreateKey { key, ttl_secs, initial, overwrite } => {
            let ttl = ttl_secs.map(|seconds| format!(" EXPIRE {seconds}")).unwrap_or_default();
            let flag = if *overwrite { " OVERWRITE" } else { "" };
            format!("CREATE {key} {}{ttl}{flag}", initial_preview(initial))
        }
        RedisMutation::SetString { key, value } => format!("SET {key} {} KEEPTTL", clip(value)),
        RedisMutation::HashSet { key, field, value } => format!("HSET {key} {field} {}", clip(value)),
        RedisMutation::HashDel { key, fields } => format!("HDEL {key} {}", fields.join(" ")),
        RedisMutation::ListPush { key, head, value } => format!("{} {key} {}", if *head { "LPUSH" } else { "RPUSH" }, clip(value)),
        RedisMutation::ListSet { key, index, value } => format!("LSET {key} {index} {}", clip(value)),
        RedisMutation::ListRem { key, count, value } => format!("LREM {key} {count} {}", clip(value)),
        RedisMutation::SetAdd { key, member } => format!("SADD {key} {}", clip(member)),
        RedisMutation::SetRem { key, member } => format!("SREM {key} {}", clip(member)),
        RedisMutation::ZSetAdd { key, score, member } => format!("ZADD {key} {score} {}", clip(member)),
        RedisMutation::ZSetRem { key, member } => format!("ZREM {key} {}", clip(member)),
        RedisMutation::StreamAdd { key, id, fields } => {
            let id = if id.is_empty() { "*" } else { id.as_str() };
            format!("XADD {key} {id} {}", fields.iter().map(|(field, _)| field.as_str()).collect::<Vec<_>>().join(" "))
        }
        RedisMutation::StreamDel { key, id } => format!("XDEL {key} {id}"),
        RedisMutation::SetTtl { key, seconds } => match seconds {
            Some(seconds) => format!("EXPIRE {key} {seconds}"),
            None => format!("PERSIST {key}"),
        },
    }
}

fn initial_preview(initial: &RedisInitialValue) -> String {
    match initial {
        RedisInitialValue::String(value) => format!("SET {}", clip(value)),
        RedisInitialValue::Hash { field, value } => format!("HSET {field} {}", clip(value)),
        RedisInitialValue::List(value) => format!("RPUSH {}", clip(value)),
        RedisInitialValue::Set(member) => format!("SADD {}", clip(member)),
        RedisInitialValue::ZSet { score, member } => format!("ZADD {score} {}", clip(member)),
        RedisInitialValue::Stream { id, fields } => format!("XADD {} {}", if id.is_empty() { "*" } else { id }, fields.len()),
    }
}

fn clip(text: &str) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() > 80 {
        format!("{}…", flat.chars().take(80).collect::<String>())
    } else {
        flat
    }
}

/// 执行已经通过 `prepare_mutation` 的写计划。未确认、非法分值和已存在且未声明覆盖的键都不会写入。
pub async fn apply_mutation(
    conn: &mut redis::aio::MultiplexedConnection,
    mutation: &RedisMutation,
    confirmed: bool,
) -> Result<i64, MutationFault> {
    let plan = prepare_mutation(mutation, confirmed).map_err(MutationFault::Rejected)?;
    if let RedisMutation::SetTtl { key, seconds } = mutation {
        return set_ttl(conn, key, *seconds).await.map_err(MutationFault::Io);
    }
    if let Some(key) = &plan.exists_key {
        let exists: i64 = redis::cmd("EXISTS").arg(key).query_async(conn).await.map_err(MutationFault::Io)?;
        if exists > 0 && !plan.overwrite {
            return Err(MutationFault::Rejected(format!("键 {key} 已存在。确认覆盖后才会写入。")));
        }
    }
    let mut applied = 1i64;
    for step in &plan.steps {
        if step.word.eq_ignore_ascii_case("KEYS") {
            return Err(MutationFault::Rejected("已禁用 KEYS：键空间浏览一律走 SCAN。".to_owned()));
        }
        let mut cmd = redis::cmd(step.word.as_str());
        for arg in &step.args {
            cmd.arg(arg);
        }
        let value: redis::Value = cmd.query_async(conn).await.map_err(MutationFault::Io)?;
        if let redis::Value::Int(number) = value {
            applied = number;
        }
    }
    Ok(applied)
}

/// 删除键（DEL）。调用方必须先通过 `require_delete`。
pub async fn delete_keys(conn: &mut redis::aio::MultiplexedConnection, keys: &[String]) -> Result<i64, redis::RedisError> {
    let mut cmd = redis::cmd("DEL");
    for key in keys {
        cmd.arg(key);
    }
    cmd.query_async(conn).await
}

/// 重命名键（RENAME）。调用方必须先通过 `require_rename`。
pub async fn rename_key(conn: &mut redis::aio::MultiplexedConnection, from: &str, to: &str) -> Result<(), redis::RedisError> {
    redis::cmd("RENAME").arg(from).arg(to).query_async(conn).await
}
