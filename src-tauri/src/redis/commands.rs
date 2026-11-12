//! Redis IPC。确认、会话和健康标记在应用服务里，这里只转发。

use super::keys::ScanPage;
use crate::application::redis::{self, DeleteOutcome, ExecOutcome, RenameOutcome, TtlOutcome};
use crate::connections::commands::ConnectionsState;
use crate::redis::keys::RedisKeyDetail;

pub async fn redis_scan_keys(
    state: &ConnectionsState,
    connection_id: String,
    cursor: Option<u64>,
    pattern: Option<String>,
    count: Option<usize>,
) -> Result<ScanPage, String> {
    redis::scan_keys(&state.service, &connection_id, cursor, pattern, count).await
}

pub async fn redis_read_key(state: &ConnectionsState, connection_id: String, key: String) -> Result<RedisKeyDetail, String> {
    redis::read_key(&state.service, &connection_id, key).await
}

pub async fn redis_set_ttl(
    state: &ConnectionsState,
    connection_id: String,
    key: String,
    seconds: Option<i64>,
    confirmed: Option<bool>,
) -> Result<TtlOutcome, String> {
    redis::set_ttl(&state.service, &connection_id, key, seconds, confirmed.unwrap_or(false)).await
}

pub async fn redis_delete_keys(
    state: &ConnectionsState,
    connection_id: String,
    keys: Vec<String>,
    confirmed: Option<bool>,
) -> Result<DeleteOutcome, String> {
    redis::delete_keys(&state.service, &connection_id, keys, confirmed.unwrap_or(false)).await
}

pub async fn redis_rename_key(
    state: &ConnectionsState,
    connection_id: String,
    from: String,
    to: String,
    confirmed: Option<bool>,
) -> Result<RenameOutcome, String> {
    redis::rename_key(&state.service, &connection_id, from, to, confirmed.unwrap_or(false)).await
}

pub async fn redis_exec(
    state: &ConnectionsState,
    connection_id: String,
    command: String,
    args: Vec<String>,
    confirmed: Option<bool>,
) -> Result<ExecOutcome, String> {
    redis::exec(&state.service, &connection_id, command, args, confirmed.unwrap_or(false)).await
}
