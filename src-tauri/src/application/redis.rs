//! Redis 能力的类型化入口。确认、会话和健康标记不依赖 Tauri command。

use std::sync::Arc;

use crate::connections::{Service, SessionHealth};
use serde::Serialize;

use crate::redis::exec::{self, RespReply};
use crate::redis::keys::{self, RedisKeyDetail, RedisMutation, ScanPage};

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

async fn with_redis<T, F, Fut>(service: &Service, connection_id: &str, run: F) -> Result<T, String>
where
    F: FnOnce(Arc<tokio::sync::Mutex<redis::aio::MultiplexedConnection>>, Arc<SessionHealth>) -> Fut,
    Fut: std::future::Future<Output = Result<T, redis::RedisError>>,
{
    let (conn, health) = service.redis_session(connection_id).ok_or_else(|| "这条连接还没有打开，无法浏览键空间。".to_owned())?;
    let outcome = run(conn, health.clone()).await;
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
    with_redis(service, connection_id, move |conn, _| async move {
        keys::scan_keys(&mut *conn.lock().await, cursor, pattern.as_deref(), count).await
    })
    .await
}

pub async fn read_key(service: &Service, connection_id: &str, key: String) -> Result<RedisKeyDetail, String> {
    with_redis(service, connection_id, move |conn, _| async move { keys::read_key(&mut *conn.lock().await, &key).await }).await
}

pub async fn set_ttl(service: &Service, connection_id: &str, key: String, seconds: Option<i64>, confirmed: bool) -> Result<TtlOutcome, String> {
    let applied = mutate(service, connection_id, RedisMutation::SetTtl { key, seconds }, confirmed).await?;
    Ok(TtlOutcome { applied })
}

/// 类型化写入口。未确认时在取会话之前拒绝，不会向 Redis 发送写命令。
pub async fn mutate(service: &Service, connection_id: &str, mutation: RedisMutation, confirmed: bool) -> Result<i64, String> {
    keys::prepare_mutation(&mutation, confirmed)?;
    let (conn, health) = service.redis_session(connection_id).ok_or_else(|| "这条连接还没有打开，无法浏览键空间。".to_owned())?;
    let outcome = keys::apply_mutation(&mut *conn.lock().await, &mutation, confirmed).await;
    if let Err(keys::MutationFault::Io(err)) = &outcome {
        if matches!(err.kind(), redis::ErrorKind::IoError) {
            health.note_disconnect();
        }
    }
    outcome.map_err(|err| err.to_string())
}

pub async fn delete_keys(service: &Service, connection_id: &str, keys: Vec<String>, confirmed: bool) -> Result<DeleteOutcome, String> {
    keys::require_delete(&keys, confirmed)?;
    let deleted = with_redis(service, connection_id, move |conn, _| async move {
        keys::delete_keys(&mut *conn.lock().await, &keys).await
    })
    .await?;
    Ok(DeleteOutcome { deleted })
}

pub async fn rename_key(service: &Service, connection_id: &str, from: String, to: String, confirmed: bool) -> Result<RenameOutcome, String> {
    keys::require_rename(&from, &to, confirmed)?;
    with_redis(service, connection_id, move |conn, _| async move {
        keys::rename_key(&mut *conn.lock().await, &from, &to).await
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
    let command_word = word.clone();
    let reply = with_redis(service, connection_id, move |conn, _| async move {
        exec::run_command(&mut *conn.lock().await, &command_word, &args).await
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
    with_redis(service, connection_id, move |conn, _| async move {
        let mut guard = conn.lock().await;
        let info: String = redis::cmd("INFO").arg("keyspace").query_async(&mut *guard).await.unwrap_or_default();
        Ok(parse_info_keyspace(&info))
    })
    .await
}

/// 切换 Redis 会话当前逻辑库（SELECT db），并返回最新的键空间统计。
pub async fn select_db(service: &Service, connection_id: &str, db: u8) -> Result<Vec<KeyspaceDb>, String> {
    with_redis(service, connection_id, move |conn, _| async move {
        let mut guard = conn.lock().await;
        let _: () = redis::cmd("SELECT").arg(db).query_async(&mut *guard).await?;
        let info: String = redis::cmd("INFO").arg("keyspace").query_async(&mut *guard).await.unwrap_or_default();
        Ok(parse_info_keyspace(&info))
    })
    .await
}
