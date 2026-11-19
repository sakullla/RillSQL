//! 诊断工具：慢日志、INFO 分节与关键指标、MEMORY STATS、客户端列表、大键扫描、Pub/Sub。
//! 解析部分是纯函数（单测覆盖）；网络部分按节点目标执行，集群下由调用方选节点或逐节点汇总。

use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;

use super::conn::{NodeTarget, RedisConn};
use super::keys::scan_args;

// ------------------------------------------------------------------ 慢日志

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlowEntry {
    pub id: i64,
    /// Unix 秒。
    pub at: i64,
    pub micros: i64,
    pub command: String,
    pub client: String,
    pub client_name: String,
    pub node: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlowSort {
    Duration,
    Time,
}

fn text(value: &redis::Value) -> String {
    match value {
        redis::Value::BulkString(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        redis::Value::SimpleString(text) => text.clone(),
        redis::Value::Int(number) => number.to_string(),
        redis::Value::Okay => "OK".to_owned(),
        redis::Value::Double(number) => number.to_string(),
        _ => String::new(),
    }
}

fn int(value: Option<&redis::Value>) -> i64 {
    match value {
        Some(redis::Value::Int(number)) => *number,
        Some(other) => text(other).parse().unwrap_or(0),
        None => 0,
    }
}

/// 解析 `SLOWLOG GET` 回复：[id, time, micros, [args...], client, name]（client/name 自 4.0 起）。
pub fn parse_slowlog(value: &redis::Value, node: &str) -> Vec<SlowEntry> {
    let redis::Value::Array(entries) = value else { return Vec::new() };
    entries
        .iter()
        .filter_map(|entry| {
            let redis::Value::Array(parts) = entry else { return None };
            let args = match parts.get(3) {
                Some(redis::Value::Array(args)) => args.iter().map(text).collect::<Vec<_>>().join(" "),
                _ => String::new(),
            };
            Some(SlowEntry {
                id: int(parts.first()),
                at: int(parts.get(1)),
                micros: int(parts.get(2)),
                command: args,
                client: parts.get(4).map(text).unwrap_or_default(),
                client_name: parts.get(5).map(text).unwrap_or_default(),
                node: node.to_owned(),
            })
        })
        .collect()
}

pub fn sort_slowlog(entries: &mut [SlowEntry], by: SlowSort, descending: bool) {
    entries.sort_by(|a, b| {
        let order = match by {
            SlowSort::Duration => a.micros.cmp(&b.micros).then(a.at.cmp(&b.at)),
            SlowSort::Time => a.at.cmp(&b.at).then(a.id.cmp(&b.id)),
        };
        if descending { order.reverse() } else { order }
    });
}

pub async fn slowlog(targets: &[NodeTarget], count: usize) -> Result<Vec<SlowEntry>, redis::RedisError> {
    let mut all = Vec::new();
    for target in targets {
        let mut conn = target.conn.clone();
        let value: redis::Value = redis::cmd("SLOWLOG").arg("GET").arg(count).query_async(&mut conn).await?;
        all.extend(parse_slowlog(&value, &target.addr));
    }
    sort_slowlog(&mut all, SlowSort::Duration, true);
    Ok(all)
}

/// 清空慢日志（危险操作，调用方必须先确认）。
pub async fn slowlog_reset(targets: &[NodeTarget]) -> Result<(), redis::RedisError> {
    for target in targets {
        let mut conn = target.conn.clone();
        let _: () = redis::cmd("SLOWLOG").arg("RESET").query_async(&mut conn).await?;
    }
    Ok(())
}

// ------------------------------------------------------------------ INFO

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InfoSection {
    pub name: String,
    pub items: Vec<(String, String)>,
}

/// 解析 INFO 文本：`# Section` 开新节，`k:v` 为条目。
pub fn parse_info(text: &str) -> Vec<InfoSection> {
    let mut sections: Vec<InfoSection> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('#') {
            sections.push(InfoSection { name: name.trim().to_owned(), items: Vec::new() });
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            if sections.is_empty() {
                sections.push(InfoSection { name: String::new(), items: Vec::new() });
            }
            if let Some(section) = sections.last_mut() {
                section.items.push((key.trim().to_owned(), value.trim().to_owned()));
            }
        }
    }
    sections
}

pub fn info_value<'a>(sections: &'a [InfoSection], key: &str) -> Option<&'a str> {
    sections.iter().flat_map(|section| section.items.iter()).find(|(name, _)| name == key).map(|(_, value)| value.as_str())
}

/// 指标卡片：(i18n 键, 值)。命中率由 keyspace_hits / misses 计算。
pub fn key_metrics(sections: &[InfoSection]) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for (label, key) in [
        ("redis.info.version", "redis_version"),
        ("redis.info.role", "role"),
        ("redis.info.uptime", "uptime_in_days"),
        ("redis.info.clients", "connected_clients"),
        ("redis.info.memory", "used_memory_human"),
        ("redis.info.peak", "used_memory_peak_human"),
        ("redis.info.maxmemory", "maxmemory_human"),
        ("redis.info.fragmentation", "mem_fragmentation_ratio"),
        ("redis.info.ops", "instantaneous_ops_per_sec"),
        ("redis.info.commands", "total_commands_processed"),
        ("redis.info.expired", "expired_keys"),
        ("redis.info.evicted", "evicted_keys"),
    ] {
        if let Some(value) = info_value(sections, key) {
            out.push((label, value.to_owned()));
        }
    }
    let hits = info_value(sections, "keyspace_hits").and_then(|value| value.parse::<f64>().ok());
    let misses = info_value(sections, "keyspace_misses").and_then(|value| value.parse::<f64>().ok());
    if let (Some(hits), Some(misses)) = (hits, misses) {
        if hits + misses > 0. {
            out.push(("redis.info.hitRate", format!("{:.1}%", hits * 100. / (hits + misses))));
        }
    }
    out
}

pub async fn info(target: &NodeTarget) -> Result<Vec<InfoSection>, redis::RedisError> {
    let mut conn = target.conn.clone();
    let text: String = redis::cmd("INFO").query_async(&mut conn).await?;
    let mut sections = parse_info(&text);
    // MEMORY STATS 是可选的（老版本或被 ACL 禁用时跳过）。
    if let Ok(stats) = redis::cmd("MEMORY").arg("STATS").query_async::<redis::Value>(&mut conn).await {
        let items = flatten_stats(&stats, "");
        if !items.is_empty() {
            sections.push(InfoSection { name: "Memory Stats".to_owned(), items });
        }
    }
    Ok(sections)
}

/// MEMORY STATS 是嵌套的键值数组；嵌套项以 `父.子` 展平。
pub fn flatten_stats(value: &redis::Value, prefix: &str) -> Vec<(String, String)> {
    let pairs: Vec<(redis::Value, redis::Value)> = match value {
        redis::Value::Array(items) => items.chunks(2).filter(|pair| pair.len() == 2).map(|pair| (pair[0].clone(), pair[1].clone())).collect(),
        redis::Value::Map(pairs) => pairs.clone(),
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for (key, value) in pairs {
        let name = if prefix.is_empty() { text(&key) } else { format!("{prefix}.{}", text(&key)) };
        match value {
            redis::Value::Array(_) | redis::Value::Map(_) => out.extend(flatten_stats(&value, &name)),
            other => out.push((name, text(&other))),
        }
    }
    out
}

// ------------------------------------------------------------------ 客户端

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub id: String,
    pub addr: String,
    pub name: String,
    pub age: String,
    pub idle: String,
    pub db: String,
    pub cmd: String,
    pub flags: String,
    pub node: String,
}

/// 解析 `CLIENT LIST`：每行空格分隔的 `k=v`。
pub fn parse_clients(text: &str, node: &str) -> Vec<ClientInfo> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let field = |name: &str| {
                line.split_whitespace().find_map(|pair| pair.strip_prefix(name).and_then(|rest| rest.strip_prefix('='))).unwrap_or("").to_owned()
            };
            ClientInfo {
                id: field("id"),
                addr: field("addr"),
                name: field("name"),
                age: field("age"),
                idle: field("idle"),
                db: field("db"),
                cmd: field("cmd"),
                flags: field("flags"),
                node: node.to_owned(),
            }
        })
        .collect()
}

pub async fn clients(targets: &[NodeTarget]) -> Result<Vec<ClientInfo>, redis::RedisError> {
    let mut all = Vec::new();
    for target in targets {
        let mut conn = target.conn.clone();
        let text: String = redis::cmd("CLIENT").arg("LIST").query_async(&mut conn).await?;
        all.extend(parse_clients(&text, &target.addr));
    }
    Ok(all)
}

/// 断开一个客户端（危险操作，调用方必须先确认）。
pub async fn client_kill(target: &NodeTarget, id: &str) -> Result<i64, redis::RedisError> {
    let mut conn = target.conn.clone();
    redis::cmd("CLIENT").arg("KILL").arg("ID").arg(id).query_async(&mut conn).await
}

// ------------------------------------------------------------------ 大键扫描

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BigKey {
    pub key: String,
    pub key_type: String,
    /// 元素数（集合）或字节长度（字符串）。
    pub size: u64,
    /// MEMORY USAGE 字节数（开启时）。
    pub memory: Option<u64>,
    pub node: String,
}

impl BigKey {
    /// 排序依据：有内存用内存，否则用元素数/长度。
    fn weight(&self) -> u64 {
        self.memory.unwrap_or(self.size)
    }
}

impl Ord for BigKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // BinaryHeap 是大顶堆；TopN 要弹出最小的，所以反过来比较。
        other.weight().cmp(&self.weight()).then_with(|| self.key.cmp(&other.key))
    }
}

impl PartialOrd for BigKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// 只保留最大的 N 个。
#[derive(Debug, Default)]
pub struct TopN {
    limit: usize,
    heap: BinaryHeap<BigKey>,
}

impl TopN {
    pub fn new(limit: usize) -> Self {
        Self { limit: limit.max(1), heap: BinaryHeap::new() }
    }

    pub fn push(&mut self, item: BigKey) {
        self.heap.push(item);
        if self.heap.len() > self.limit {
            self.heap.pop();
        }
    }

    /// 由大到小。
    pub fn sorted(&self) -> Vec<BigKey> {
        let mut items: Vec<BigKey> = self.heap.iter().cloned().collect();
        items.sort_by(|a, b| b.weight().cmp(&a.weight()).then_with(|| a.key.cmp(&b.key)));
        items
    }
}

/// 类型对应的长度命令（大键扫描用；字符串取字节数）。
pub fn size_command(key_type: &str) -> Option<&'static str> {
    super::keys::length_command(key_type)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BigKeyEvent {
    Progress { scanned: u64, total: Option<u64>, top: Vec<BigKey> },
    Finished { scanned: u64, top: Vec<BigKey>, cancelled: bool },
    Failed(String),
}

/// 后台扫描全部键，按类型取长度（可选 MEMORY USAGE），保留 TopN。可取消。
pub async fn big_keys(targets: Vec<NodeTarget>, top: usize, with_memory: bool, cancel: Arc<AtomicBool>, send: impl Fn(BigKeyEvent) -> bool) {
    let total = super::scan::dbsize_total(&targets).await;
    let mut best = TopN::new(top);
    let mut scanned = 0u64;
    for target in &targets {
        let mut conn = target.conn.clone();
        let mut cursor = 0u64;
        loop {
            if cancel.load(Ordering::SeqCst) {
                send(BigKeyEvent::Finished { scanned, top: best.sorted(), cancelled: true });
                return;
            }
            let mut scan = redis::cmd("SCAN");
            for arg in scan_args(cursor, None, 500) {
                scan.arg(arg);
            }
            let (next, names): (u64, Vec<String>) = match scan.query_async(&mut conn).await {
                Ok(page) => page,
                Err(err) => {
                    send(BigKeyEvent::Failed(crate::error_text::chain(&err)));
                    return;
                }
            };
            if !names.is_empty() {
                match measure(&mut conn, &names, with_memory, &target.addr).await {
                    Ok(items) => items.into_iter().for_each(|item| best.push(item)),
                    Err(err) => {
                        send(BigKeyEvent::Failed(crate::error_text::chain(&err)));
                        return;
                    }
                }
                scanned += names.len() as u64;
                if !send(BigKeyEvent::Progress { scanned, total, top: best.sorted() }) {
                    return;
                }
            }
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
    }
    send(BigKeyEvent::Finished { scanned, top: best.sorted(), cancelled: false });
}

async fn measure(conn: &mut RedisConn, names: &[String], with_memory: bool, node: &str) -> Result<Vec<BigKey>, redis::RedisError> {
    let mut pipe = redis::pipe();
    for name in names {
        pipe.cmd("TYPE").arg(name);
    }
    let types: Vec<String> = pipe.query_async(conn).await?;
    let mut pipe = redis::pipe();
    for (name, key_type) in names.iter().zip(&types) {
        match size_command(key_type) {
            Some(word) => pipe.cmd(word).arg(name),
            None => pipe.cmd("EXISTS").arg(name),
        };
        if with_memory {
            pipe.cmd("MEMORY").arg("USAGE").arg(name).arg("SAMPLES").arg(5);
        }
    }
    let values: Vec<redis::Value> = pipe.query_async(conn).await?;
    let stride = if with_memory { 2 } else { 1 };
    Ok(names
        .iter()
        .zip(types)
        .enumerate()
        .map(|(index, (name, key_type))| {
            let size = u64::try_from(int(values.get(index * stride))).unwrap_or(0);
            let memory = with_memory.then(|| u64::try_from(int(values.get(index * stride + 1))).unwrap_or(0));
            BigKey { key: name.clone(), key_type, size, memory, node: node.to_owned() }
        })
        .collect())
}

/// 抽样 MEMORY USAGE（模板内存估算）。读不到的记为 -1。
pub async fn memory_usage(conn: &mut RedisConn, keys: &[String]) -> Vec<i64> {
    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        let bytes: Option<i64> = redis::cmd("MEMORY").arg("USAGE").arg(key).arg("SAMPLES").arg(5).query_async(conn).await.ok().flatten();
        out.push(bytes.unwrap_or(-1));
    }
    out
}

// ------------------------------------------------------------------ Pub/Sub

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PubSubMessage {
    pub channel: String,
    pub pattern: Option<String>,
    pub payload: String,
}

/// 订阅目标：带 glob 字符的按模式订阅（PSUBSCRIBE），其余按频道。
pub fn is_pattern(channel: &str) -> bool {
    channel.contains(['*', '?', '['])
}

/// 发布一条消息，返回收到的订阅者数。
pub async fn publish(conn: &mut RedisConn, channel: &str, message: &str) -> Result<i64, redis::RedisError> {
    redis::cmd("PUBLISH").arg(channel).arg(message).query_async(conn).await
}
