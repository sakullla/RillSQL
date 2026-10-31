//! Redis 专项 IPC（R11）：SCAN 分页/值读取/TTL/删除/重命名/命令执行。
//! 写删类命令的确认语义在这里强制：confirmed=false 时拒绝执行且不发任何命令。

use std::sync::Arc;

use serde::Serialize;

use super::exec::{self, RespReply};
use super::keys::{self, RedisKeyDetail, ScanPage};
use crate::connections::commands::ConnectionsState;
use crate::connections::SessionHealth;

/// 取 Redis 连接并执行；网络类错误回落到共享健康句柄（R4 探活）。
async fn with_redis<T, F, Fut>(state: &tauri::State<'_, ConnectionsState>, connection_id: &str, run: F) -> Result<T, String>
where
    F: FnOnce(Arc<tokio::sync::Mutex<redis::aio::MultiplexedConnection>>, Arc<SessionHealth>) -> Fut,
    Fut: std::future::Future<Output = Result<T, redis::RedisError>>,
{
    let (conn, health) = state
        .service
        .redis_session(connection_id)
        .ok_or_else(|| "这条连接还没有打开，无法浏览键空间。".to_owned())?;
    let outcome = run(conn, health.clone()).await;
    if let Err(err) = &outcome {
        if matches!(err.kind(), redis::ErrorKind::IoError) {
            health.note_disconnect();
        }
    }
    outcome.map_err(|err| err.to_string())
}

/// 键空间 SCAN 分页：MATCH 模式过滤 + 游标 + COUNT（禁 KEYS）。
#[tauri::command]
pub async fn redis_scan_keys(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    cursor: Option<u64>,
    pattern: Option<String>,
    count: Option<usize>,
) -> Result<ScanPage, String> {
    let cursor = cursor.unwrap_or(0);
    let count = count.unwrap_or(keys::DEFAULT_SCAN_COUNT);
    with_redis(&state, &connection_id, move |conn, _| async move {
        keys::scan_keys(&mut *conn.lock().await, cursor, pattern.as_deref(), count).await
    })
    .await
}

/// 读取单个键的类型、TTL 与按类型的值。
#[tauri::command]
pub async fn redis_read_key(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    key: String,
) -> Result<RedisKeyDetail, String> {
    with_redis(&state, &connection_id, move |conn, _| async move {
        keys::read_key(&mut *conn.lock().await, &key).await
    })
    .await
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TtlOutcome {
    /// 1=已设置；0=键不存在。
    pub applied: i64,
}

/// 修改 TTL：seconds=Some(n) 设置 n 秒过期；None 移除过期（PERSIST）。
#[tauri::command]
pub async fn redis_set_ttl(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    key: String,
    seconds: Option<i64>,
) -> Result<TtlOutcome, String> {
    if let Some(seconds) = seconds {
        if seconds < 0 {
            return Err("TTL 秒数应为非负整数；要移除过期请用“永不过期”。".to_owned());
        }
    }
    let applied = with_redis(&state, &connection_id, move |conn, _| async move {
        keys::set_ttl(&mut *conn.lock().await, &key, seconds).await
    })
    .await?;
    Ok(TtlOutcome { applied })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteOutcome {
    pub deleted: i64,
}

/// 删除键。confirmed=false（前端没有确认）时拒绝且不发 DEL。
#[tauri::command]
pub async fn redis_delete_keys(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    keys: Vec<String>,
    confirmed: Option<bool>,
) -> Result<DeleteOutcome, String> {
    if keys.is_empty() {
        return Err("没有选择要删除的键。".to_owned());
    }
    if !confirmed.unwrap_or(false) {
        return Err(format!("删除 {} 个键属于写删操作，需要先在界面确认。", keys.len()));
    }
    let deleted = with_redis(&state, &connection_id, move |conn, _| async move {
        keys::delete_keys(&mut *conn.lock().await, &keys).await
    })
    .await?;
    Ok(DeleteOutcome { deleted })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameOutcome {
    pub renamed: bool,
}

/// 重命名键。confirmed=false 时拒绝且不发 RENAME。
#[tauri::command]
pub async fn redis_rename_key(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    from: String,
    to: String,
    confirmed: Option<bool>,
) -> Result<RenameOutcome, String> {
    if from.is_empty() || to.is_empty() {
        return Err("重命名需要源键名和新键名。".to_owned());
    }
    if !confirmed.unwrap_or(false) {
        return Err("重命名键属于写删操作，需要先在界面确认。".to_owned());
    }
    with_redis(&state, &connection_id, move |conn, _| async move {
        keys::rename_key(&mut *conn.lock().await, &from, &to).await
    })
    .await?;
    Ok(RenameOutcome { renamed: true })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecOutcome {
    pub command: String,
    pub reply: RespReply,
}

/// 命令执行通道：任意命令 + 参数。KEYS 无条件禁用；写删类必须 confirmed=true。
#[tauri::command]
pub async fn redis_exec(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    command: String,
    args: Vec<String>,
    confirmed: Option<bool>,
) -> Result<ExecOutcome, String> {
    let word = command.trim().to_string();
    if word.is_empty() {
        return Err("请输入要执行的命令。".to_owned());
    }
    let confirmed = confirmed.unwrap_or(false);
    exec::guard_command(&word, confirmed)?;
    let command_word = word.clone();
    let reply = with_redis(&state, &connection_id, move |conn, _| async move {
        exec::run_command(&mut *conn.lock().await, &command_word, &args).await
    })
    .await?;
    Ok(ExecOutcome { command: word, reply })
}
