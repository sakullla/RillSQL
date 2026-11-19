//! 模板批量操作：删除（UNLINK）与设置/移除过期（EXPIRE/PERSIST）。
//! 先预览数量与样例，确认后分批执行，可取消，报告进度与失败数。集群下按槽分组，同槽多键一条命令。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::cluster;
use super::conn::RedisConn;
use super::keyspace;

/// 一批的键数。
pub const BATCH_CHUNK: usize = 500;
/// 预览里展示的样例数。
pub const PREVIEW_SAMPLES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchOp {
    Unlink,
    /// Some(秒) = EXPIRE；None = PERSIST。
    Ttl(Option<i64>),
}

impl BatchOp {
    pub fn word(self) -> &'static str {
        match self {
            Self::Unlink => "UNLINK",
            Self::Ttl(Some(_)) => "EXPIRE",
            Self::Ttl(None) => "PERSIST",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchPreview {
    pub count: usize,
    pub samples: Vec<String>,
    pub command: String,
}

/// 预览：总数、均匀抽取的样例与命令摘要。
pub fn preview(keys: &[String], op: BatchOp) -> BatchPreview {
    let command = match op {
        BatchOp::Ttl(Some(seconds)) => format!("{} … {seconds} ({})", op.word(), keys.len()),
        _ => format!("{} … ({})", op.word(), keys.len()),
    };
    BatchPreview { count: keys.len(), samples: keyspace::sample(keys, PREVIEW_SAMPLES), command }
}

/// 执行前置检查：没有键或未确认都拒绝；负 TTL 拒绝。
pub fn require_batch(keys: &[String], op: BatchOp, confirmed: bool) -> Result<(), String> {
    if keys.is_empty() {
        return Err("没有要处理的键。".to_owned());
    }
    if let BatchOp::Ttl(Some(seconds)) = op {
        if seconds < 0 {
            return Err("TTL 秒数应为非负整数。".to_owned());
        }
    }
    if !confirmed {
        return Err(format!("批量处理 {} 个键属于写删操作，需要先在界面确认。", keys.len()));
    }
    Ok(())
}

/// 分批：单机按固定块；集群先按槽分组，再把每组切块（同槽才能一条命令多键）。
pub fn plan_chunks(keys: &[String], cluster_mode: bool, chunk: usize) -> Vec<Vec<String>> {
    let chunk = chunk.max(1);
    if !cluster_mode {
        return keys.chunks(chunk).map(<[String]>::to_vec).collect();
    }
    cluster::group_by_slot(keys).into_iter().flat_map(|(_, group)| group.chunks(chunk).map(<[String]>::to_vec).collect::<Vec<_>>()).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BatchProgress {
    pub done: usize,
    pub failed: usize,
    pub total: usize,
    pub cancelled: bool,
    pub finished: bool,
}

/// 分批执行。UNLINK 一批一条命令；EXPIRE/PERSIST 单键命令，单机用 pipeline，集群逐键（自动路由）。
pub async fn run_batch(
    mut conn: RedisConn,
    keys: Vec<String>,
    op: BatchOp,
    confirmed: bool,
    cancel: Arc<AtomicBool>,
    send: impl Fn(BatchProgress) -> bool,
) -> Result<BatchProgress, String> {
    require_batch(&keys, op, confirmed)?;
    let cluster_mode = conn.is_cluster();
    let mut progress = BatchProgress { total: keys.len(), ..BatchProgress::default() };
    for chunk in plan_chunks(&keys, cluster_mode, BATCH_CHUNK) {
        if cancel.load(Ordering::SeqCst) {
            progress.cancelled = true;
            break;
        }
        let size = chunk.len();
        let ok = match op {
            BatchOp::Unlink => {
                let mut cmd = redis::cmd("UNLINK");
                for key in &chunk {
                    cmd.arg(key);
                }
                cmd.query_async::<i64>(&mut conn).await.map(|_| size)
            }
            BatchOp::Ttl(seconds) => {
                if cluster_mode {
                    let mut ok = 0;
                    for key in &chunk {
                        if ttl_cmd(key, seconds).query_async::<i64>(&mut conn).await.is_ok() {
                            ok += 1;
                        }
                    }
                    Ok(ok)
                } else {
                    let mut pipe = redis::pipe();
                    for key in &chunk {
                        pipe.add_command(ttl_cmd(key, seconds));
                    }
                    pipe.query_async::<Vec<redis::Value>>(&mut conn)
                        .await
                        .map(|values| values.iter().filter(|value| !matches!(value, redis::Value::ServerError(_))).count())
                }
            }
        };
        match ok {
            Ok(applied) => {
                progress.done += applied;
                progress.failed += size - applied.min(size);
            }
            Err(_) => progress.failed += size,
        }
        if !send(progress) {
            progress.cancelled = true;
            break;
        }
    }
    progress.finished = true;
    send(progress);
    Ok(progress)
}

fn ttl_cmd(key: &str, seconds: Option<i64>) -> redis::Cmd {
    match seconds {
        Some(seconds) => {
            let mut cmd = redis::cmd("EXPIRE");
            cmd.arg(key).arg(seconds);
            cmd
        }
        None => {
            let mut cmd = redis::cmd("PERSIST");
            cmd.arg(key);
            cmd
        }
    }
}
