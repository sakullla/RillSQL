//! Redis 能力的类型化入口。确认、会话和健康标记不依赖 Tauri command。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::connections::{Service, SessionHealth};
use serde::Serialize;

use crate::adapters::redis::batch::{self, BatchOp, BatchProgress};
use crate::adapters::redis::conn::{NodeTarget, RedisConn, RedisHandle, Topology};
use crate::adapters::redis::diag::{self, BigKeyEvent, ClientInfo, InfoSection, PubSubMessage, SlowEntry};
use crate::adapters::redis::exec::{self, RespReply};
use crate::adapters::redis::keys::{self, RedisKeyDetail, RedisMutation, ScanPage, ValuePage};
use crate::adapters::redis::keyspace::KeyspacePrefs;
use crate::adapters::redis::scan::{self, ScanEvent, ScanOptions, ScanResume};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TtlOutcome {
    pub applied: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteOutcome {
    pub deleted: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameOutcome {
    pub renamed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyspaceDb {
    pub db: u8,
    pub keys: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecOutcome {
    pub command: String,
    pub reply: RespReply,
}

const NOT_OPEN: &str = "这条连接还没有打开，无法浏览键空间。";

/// IO 错误记为疑似断线，交给探活。
fn note_io(health: &SessionHealth, err: &redis::RedisError) {
    if matches!(err.kind(), redis::ErrorKind::IoError) {
        health.note_disconnect();
    }
}

fn handle(service: &Service, connection_id: &str) -> Result<(RedisHandle, Arc<SessionHealth>), String> {
    service.redis_handle(connection_id).ok_or_else(|| NOT_OPEN.to_owned())
}

/// 诊断目标：指定节点（集群），或全部节点（空串）。
async fn targets(handle: &RedisHandle, health: &SessionHealth, node: &str) -> Result<Vec<NodeTarget>, String> {
    let outcome = if node.trim().is_empty() { handle.node_targets().await } else { handle.node_target(node).await.map(|target| vec![target]) };
    outcome.map_err(|err| {
        note_io(health, &err);
        crate::error_text::chain(&err)
    })
}

async fn with_redis<T, F, Fut>(service: &Service, connection_id: &str, run: F) -> Result<T, String>
where
    F: FnOnce(RedisConn, Arc<SessionHealth>) -> Fut,
    Fut: std::future::Future<Output = Result<T, redis::RedisError>>,
{
    let (handle, health) = service.redis_handle(connection_id).ok_or_else(|| NOT_OPEN.to_owned())?;
    let outcome = run(handle.conn(), health.clone()).await;
    if let Err(err) = &outcome {
        if matches!(err.kind(), redis::ErrorKind::IoError) {
            health.note_disconnect();
        }
    }
    outcome.map_err(|err| err.to_string())
}

pub async fn scan_keys(service: &Service, connection_id: &str, cursor: Option<u64>, pattern: Option<String>, count: Option<usize>) -> Result<ScanPage, String> {
    let cursor = cursor.unwrap_or(0);
    let count = count.unwrap_or(keys::DEFAULT_SCAN_COUNT);
    with_redis(service, connection_id, move |mut conn, _| async move {
        keys::scan_keys(&mut conn, cursor, pattern.as_deref(), count).await
    })
    .await
}

pub async fn read_key(service: &Service, connection_id: &str, key: String) -> Result<RedisKeyDetail, String> {
    with_redis(service, connection_id, move |mut conn, _| async move { keys::read_key(&mut conn, &key).await }).await
}

pub async fn set_ttl(service: &Service, connection_id: &str, key: String, seconds: Option<i64>, confirmed: bool) -> Result<TtlOutcome, String> {
    let applied = mutate(service, connection_id, RedisMutation::SetTtl { key, seconds }, confirmed).await?;
    Ok(TtlOutcome { applied })
}

/// 类型化写入口。未确认时在取会话之前拒绝，不会向 Redis 发送写命令。
pub async fn mutate(service: &Service, connection_id: &str, mutation: RedisMutation, confirmed: bool) -> Result<i64, String> {
    keys::prepare_mutation(&mutation, confirmed)?;
    let (handle, health) = handle(service, connection_id)?;
    let outcome = keys::apply_mutation(&mut handle.conn(), &mutation, confirmed).await;
    if let Err(keys::MutationFault::Io(err)) = &outcome {
        if matches!(err.kind(), redis::ErrorKind::IoError) {
            health.note_disconnect();
        }
    }
    outcome.map_err(|err| err.to_string())
}

/// 带核对的写回：确认之后、写入之前再读一次当前值，指纹与读取时不一致就不写（返回 `Ok(None)`）。
/// 确认框开着的时候别人改了值，这里会拦下；剩下的窗口只有这一次读与写之间的一次往返。
pub async fn mutate_unchanged(
    service: &Service,
    connection_id: &str,
    mutation: RedisMutation,
    probe: keys::ValueProbe,
    expected: u64,
    confirmed: bool,
) -> Result<Option<i64>, String> {
    keys::prepare_mutation(&mutation, confirmed)?;
    let current = probe_value(service, connection_id, probe).await?;
    if keys::value_fingerprint(current.as_deref()) != expected {
        return Ok(None);
    }
    mutate(service, connection_id, mutation, confirmed).await.map(Some)
}

pub async fn delete_keys(service: &Service, connection_id: &str, keys: Vec<String>, confirmed: bool) -> Result<DeleteOutcome, String> {
    keys::require_delete(&keys, confirmed)?;
    let deleted = with_redis(service, connection_id, move |mut conn, _| async move {
        keys::delete_keys(&mut conn, &keys).await
    })
    .await?;
    Ok(DeleteOutcome { deleted })
}

pub async fn rename_key(service: &Service, connection_id: &str, from: String, to: String, confirmed: bool) -> Result<RenameOutcome, String> {
    keys::require_rename(&from, &to, confirmed)?;
    with_redis(service, connection_id, move |mut conn, _| async move {
        keys::rename_key(&mut conn, &from, &to).await
    })
    .await?;
    Ok(RenameOutcome { renamed: true })
}

pub async fn exec(service: &Service, connection_id: &str, command: String, args: Vec<String>, confirmed: bool) -> Result<ExecOutcome, String> {
    let word = command.trim().to_string();
    if word.is_empty() {
        return Err("请输入要执行的命令。".to_owned());
    }
    exec::guard_invocation(&word, &args, confirmed)?;
    if service.redis_handle(connection_id).is_some_and(|(handle, _)| handle.is_cluster()) {
        let (command, expanded) = exec::expand_command(&word, &args);
        if let Some(message) = exec::crossslot_message(&command, &expanded) {
            return Err(message);
        }
    }
    let command_word = word.clone();
    let reply = with_redis(service, connection_id, move |mut conn, _| async move {
        exec::run_command(&mut conn, &command_word, &args).await
    })
    .await?;
    Ok(ExecOutcome { command: word, reply })
}

/// 解析 `INFO keyspace` 输出里的 `dbN:keys=K,...` 行。
pub fn parse_info_keyspace(info: &str) -> Vec<KeyspaceDb> {
    let mut list = Vec::new();
    for line in info.lines() {
        let trimmed = line.trim();
        let Some((db_part, stats)) = trimmed.split_once(':') else { continue };
        let Some(num) = db_part.strip_prefix("db") else { continue };
        let Ok(db) = num.parse::<u8>() else { continue };
        let mut count = 0u64;
        for pair in stats.split(',') {
            if let Some(val) = pair.trim().strip_prefix("keys=") {
                if let Ok(parsed) = val.parse::<u64>() {
                    count = parsed;
                }
            }
        }
        list.push(KeyspaceDb { db, keys: count });
    }
    list.sort_by_key(|item| item.db);
    list
}

/// 读取实例各逻辑库（db0–db15）的键数量统计。
pub async fn list_keyspaces(service: &Service, connection_id: &str) -> Result<Vec<KeyspaceDb>, String> {
    let (handle, health) = handle(service, connection_id)?;
    if handle.is_cluster() {
        // 集群只有 0 号库：各主节点 DBSIZE 之和。
        let targets = targets(&handle, &health, "").await?;
        let keys = scan::dbsize_total(&targets).await.unwrap_or(0);
        return Ok(vec![KeyspaceDb { db: 0, keys }]);
    }
    with_redis(service, connection_id, move |mut conn, _| async move {
        let info: String = redis::cmd("INFO").arg("keyspace").query_async(&mut conn).await.unwrap_or_default();
        Ok(parse_info_keyspace(&info))
    })
    .await
}

/// 切换 Redis 会话当前逻辑库（SELECT db），并返回最新的键空间统计。
pub async fn select_db(service: &Service, connection_id: &str, db: u8) -> Result<Vec<KeyspaceDb>, String> {
    if service.redis_handle(connection_id).is_some_and(|(handle, _)| handle.is_cluster()) {
        if db != 0 {
            return Err("集群只有 0 号库。".to_owned());
        }
        return list_keyspaces(service, connection_id).await;
    }
    with_redis(service, connection_id, move |mut conn, _| async move {
        let _: () = redis::cmd("SELECT").arg(db).query_async(&mut conn).await?;
        let info: String = redis::cmd("INFO").arg("keyspace").query_async(&mut conn).await.unwrap_or_default();
        Ok(parse_info_keyspace(&info))
    })
    .await
}

// ------------------------------------------------------------------ 拓扑与偏好

/// 当前拓扑（集群时重读节点表）。
pub async fn topology(service: &Service, connection_id: &str) -> Result<Topology, String> {
    let (handle, health) = handle(service, connection_id)?;
    handle.refresh_topology().await.map_err(|err| {
        note_io(&health, &err);
        crate::error_text::chain(&err)
    })
}

/// 本连接的键空间偏好（分隔符、模板规则、扫描参数）。读不到时用默认。
pub async fn load_prefs(service: &Service, connection_id: &str) -> KeyspacePrefs {
    let text = service.connection_prefs(connection_id, KeyspacePrefs::NS).await.ok().flatten();
    KeyspacePrefs::from_json(text.as_deref())
}

pub async fn save_prefs(service: &Service, connection_id: &str, prefs: &KeyspacePrefs) -> Result<(), String> {
    service.save_connection_prefs(connection_id, KeyspacePrefs::NS, &prefs.to_json()).await
}

/// 各主节点的键数（集群节点页）。
pub async fn node_key_counts(service: &Service, connection_id: &str) -> Result<Vec<(String, u64)>, String> {
    let (handle, health) = handle(service, connection_id)?;
    let targets = targets(&handle, &health, "").await?;
    let mut out = Vec::new();
    for target in targets {
        let mut conn = target.conn.clone();
        let size: u64 = redis::cmd("DBSIZE").query_async(&mut conn).await.unwrap_or(0);
        out.push((target.addr, size));
    }
    Ok(out)
}

// ------------------------------------------------------------------ 扫描与值

/// 后台全量扫描：逐节点连续 SCAN，事件经 `send` 交回；`send` 返回 false 表示接收方已不在，扫描就此停下。
pub async fn scan_keyspace(
    service: &Service,
    connection_id: &str,
    resume: ScanResume,
    options: ScanOptions,
    cancel: Arc<AtomicBool>,
    send: impl Fn(ScanEvent) -> bool,
) {
    let (handle, health) = match handle(service, connection_id) {
        Ok(found) => found,
        Err(message) => {
            send(ScanEvent::Failed(message));
            send(ScanEvent::Finished { resume, cancelled: false, capped: false });
            return;
        }
    };
    let targets = match targets(&handle, &health, "").await {
        Ok(targets) => targets,
        Err(message) => {
            send(ScanEvent::Failed(message));
            send(ScanEvent::Finished { resume, cancelled: false, capped: false });
            return;
        }
    };
    scan::scan_keyspace(targets, resume, options, cancel, send).await;
}

/// 读当前值（写回前核对是否被别人改过）。
pub async fn probe_value(service: &Service, connection_id: &str, probe: keys::ValueProbe) -> Result<Option<String>, String> {
    with_redis(service, connection_id, move |mut conn, _| async move { keys::probe_value(&mut conn, &probe).await }).await
}

/// 大字符串「加载全部」：GET 整个值。
pub async fn read_full_string(service: &Service, connection_id: &str, key: String) -> Result<String, String> {
    with_redis(service, connection_id, move |mut conn, _| async move { keys::read_full_string(&mut conn, &key).await }).await
}

/// 读下一页值。
pub async fn read_page(service: &Service, connection_id: &str, key: String, key_type: String, token: Option<String>) -> Result<ValuePage, String> {
    with_redis(service, connection_id, move |mut conn, _| async move {
        keys::read_page(&mut conn, &key, &key_type, token.as_deref(), keys::VALUE_PAGE_SIZE).await
    })
    .await
}

/// 抽样 MEMORY USAGE。
pub async fn memory_samples(service: &Service, connection_id: &str, keys: Vec<String>) -> Result<Vec<i64>, String> {
    let (handle, _) = handle(service, connection_id)?;
    Ok(diag::memory_usage(&mut handle.conn(), &keys).await)
}

/// 一次模板批量操作：发起时的逻辑库、键与操作。
#[derive(Debug, Clone)]
pub struct BatchRequest {
    pub db: u8,
    pub keys: Vec<String>,
    pub op: BatchOp,
}

/// 模板批量操作（UNLINK / EXPIRE / PERSIST）。未确认时在取会话之前拒绝。
///
/// `db` 是发起时的逻辑库：批量操作在一条固定于该库的独占连接上执行，
/// 不受主连接上随后发生的 SELECT（切库、控制台）影响。
pub async fn batch(
    service: &Service,
    connection_id: &str,
    request: BatchRequest,
    confirmed: bool,
    cancel: Arc<AtomicBool>,
    send: impl Fn(BatchProgress) -> bool,
) -> Result<BatchProgress, String> {
    let BatchRequest { db, keys, op } = request;
    batch::require_batch(&keys, op, confirmed)?;
    let (handle, health) = handle(service, connection_id)?;
    let conn = handle.pinned(i64::from(db)).await.map_err(|err| {
        note_io(&health, &err);
        crate::error_text::chain(&err)
    })?;
    batch::run_batch(conn, keys, op, confirmed, cancel, send).await
}

// ------------------------------------------------------------------ 诊断

pub async fn slowlog(service: &Service, connection_id: &str, node: String, count: usize) -> Result<Vec<SlowEntry>, String> {
    let (handle, health) = handle(service, connection_id)?;
    let targets = targets(&handle, &health, &node).await?;
    diag::slowlog(&targets, count).await.map_err(|err| crate::error_text::chain(&err))
}

pub async fn slowlog_reset(service: &Service, connection_id: &str, node: String, confirmed: bool) -> Result<(), String> {
    exec::guard_invocation("SLOWLOG RESET", &[], confirmed)?;
    let (handle, health) = handle(service, connection_id)?;
    let targets = targets(&handle, &health, &node).await?;
    diag::slowlog_reset(&targets).await.map_err(|err| crate::error_text::chain(&err))
}

/// 单个节点的 INFO（空串 = 单机或第一个主节点）。
pub async fn info(service: &Service, connection_id: &str, node: String) -> Result<Vec<InfoSection>, String> {
    let (handle, health) = handle(service, connection_id)?;
    let targets = targets(&handle, &health, &node).await?;
    let target = targets.first().ok_or_else(|| "没有可读取的节点。".to_owned())?;
    diag::info(target).await.map_err(|err| crate::error_text::chain(&err))
}

pub async fn clients(service: &Service, connection_id: &str, node: String) -> Result<Vec<ClientInfo>, String> {
    let (handle, health) = handle(service, connection_id)?;
    let targets = targets(&handle, &health, &node).await?;
    diag::clients(&targets).await.map_err(|err| crate::error_text::chain(&err))
}

pub async fn client_kill(service: &Service, connection_id: &str, node: String, client_id: String, confirmed: bool) -> Result<i64, String> {
    exec::guard_invocation("CLIENT KILL", &[], confirmed)?;
    let (handle, health) = handle(service, connection_id)?;
    let targets = targets(&handle, &health, &node).await?;
    let target = targets.first().ok_or_else(|| "没有可操作的节点。".to_owned())?;
    diag::client_kill(target, &client_id).await.map_err(|err| crate::error_text::chain(&err))
}

pub async fn big_keys(
    service: &Service,
    connection_id: &str,
    top: usize,
    with_memory: bool,
    cancel: Arc<AtomicBool>,
    send: impl Fn(BigKeyEvent) -> bool,
) {
    let found = match handle(service, connection_id) {
        Ok((handle, health)) => targets(&handle, &health, "").await,
        Err(message) => Err(message),
    };
    match found {
        Ok(targets) => diag::big_keys(targets, top, with_memory, cancel, send).await,
        Err(message) => {
            send(BigKeyEvent::Failed(message));
        }
    }
}

pub async fn publish(service: &Service, connection_id: &str, channel: String, message: String) -> Result<i64, String> {
    with_redis(service, connection_id, move |mut conn, _| async move { diag::publish(&mut conn, &channel, &message).await }).await
}

/// Pub/Sub：专用连接订阅频道/模式，直到取消或接收方不在。订阅成功后先发一条空频道的就绪事件。
pub async fn subscribe(
    service: &Service,
    connection_id: &str,
    channels: Vec<String>,
    cancel: Arc<AtomicBool>,
    send: impl Fn(Result<PubSubMessage, String>) -> bool,
) {
    use futures::StreamExt as _;
    let (handle, _) = match handle(service, connection_id) {
        Ok(found) => found,
        Err(message) => {
            send(Err(message));
            return;
        }
    };
    let mut pubsub = match handle.pubsub().await {
        Ok(pubsub) => pubsub,
        Err(err) => {
            send(Err(crate::error_text::chain(&err)));
            return;
        }
    };
    for channel in &channels {
        let outcome = if diag::is_pattern(channel) { pubsub.psubscribe(channel).await } else { pubsub.subscribe(channel).await };
        if let Err(err) = outcome {
            send(Err(crate::error_text::chain(&err)));
            return;
        }
    }
    if !send(Ok(PubSubMessage { channel: String::new(), pattern: None, payload: String::new() })) {
        return;
    }
    let mut stream = pubsub.on_message();
    loop {
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        match tokio::time::timeout(std::time::Duration::from_millis(250), stream.next()).await {
            Ok(Some(msg)) => {
                let payload: String = msg.get_payload().unwrap_or_default();
                let pattern: Option<String> = msg.get_pattern().ok();
                if !send(Ok(PubSubMessage { channel: msg.get_channel_name().to_owned(), pattern, payload })) {
                    return;
                }
            }
            Ok(None) => {
                send(Err("订阅连接已断开。".to_owned()));
                return;
            }
            Err(_) => {}
        }
    }
}
