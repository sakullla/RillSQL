//! 后台全量扫描：逐节点连续 SCAN（不用 KEYS）直到读完、取消或达到内存索引上限。
//! 每批键带 TYPE/PTTL（同节点 pipeline），经通道交给界面；进度为已读取数 / DBSIZE（集群为各主节点之和）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::conn::NodeTarget;
use super::keys::{scan_args, RedisKeyInfo};

/// 续扫位置：每个节点的游标，None 表示该节点已读完。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanResume {
    pub cursors: Vec<Option<u64>>,
}

impl ScanResume {
    pub fn fresh(nodes: usize) -> Self {
        Self { cursors: vec![Some(0); nodes] }
    }

    pub fn finished(&self) -> bool {
        self.cursors.iter().all(Option::is_none)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanEvent {
    /// 各节点 DBSIZE 之和（读不到时为 None）。
    Total(Option<u64>),
    Batch(Vec<RedisKeyInfo>),
    /// 结束：读完、被取消，或达到上限（capped）。
    Finished { resume: ScanResume, cancelled: bool, capped: bool },
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub pattern: Option<String>,
    pub count: usize,
    /// 本轮最多再读取多少个键（索引上限减去已读）。
    pub budget: usize,
}

/// 读各节点 DBSIZE 之和。
pub async fn dbsize_total(targets: &[NodeTarget]) -> Option<u64> {
    let mut total = 0u64;
    for target in targets {
        let mut conn = target.conn.clone();
        let size: u64 = redis::cmd("DBSIZE").query_async(&mut conn).await.ok()?;
        total += size;
    }
    Some(total)
}

/// 连续扫描。`resume` 的节点数必须与 `targets` 一致（不一致时从头扫）。
pub async fn scan_keyspace(
    targets: Vec<NodeTarget>,
    resume: ScanResume,
    options: ScanOptions,
    cancel: Arc<AtomicBool>,
    send: impl Fn(ScanEvent) -> bool,
) {
    let mut resume = if resume.cursors.len() == targets.len() { resume } else { ScanResume::fresh(targets.len()) };
    send(ScanEvent::Total(dbsize_total(&targets).await));
    let mut read = 0usize;
    for (slot, target) in targets.iter().enumerate() {
        let mut conn = target.conn.clone();
        while let Some(cursor) = resume.cursors[slot] {
            if cancel.load(Ordering::SeqCst) {
                send(ScanEvent::Finished { resume, cancelled: true, capped: false });
                return;
            }
            if read >= options.budget {
                send(ScanEvent::Finished { resume, cancelled: false, capped: true });
                return;
            }
            let mut scan = redis::cmd("SCAN");
            for arg in scan_args(cursor, options.pattern.as_deref(), options.count) {
                scan.arg(arg);
            }
            let page: Result<(u64, Vec<String>), _> = scan.query_async(&mut conn).await;
            let (next, names) = match page {
                Ok(page) => page,
                Err(err) => {
                    send(ScanEvent::Failed(crate::error_text::chain(&err)));
                    send(ScanEvent::Finished { resume, cancelled: false, capped: false });
                    return;
                }
            };
            resume.cursors[slot] = if next == 0 { None } else { Some(next) };
            if names.is_empty() {
                continue;
            }
            match describe(&mut conn, names).await {
                Ok(keys) => {
                    read += keys.len();
                    if !send(ScanEvent::Batch(keys)) {
                        return;
                    }
                }
                Err(err) => {
                    send(ScanEvent::Failed(crate::error_text::chain(&err)));
                    send(ScanEvent::Finished { resume, cancelled: false, capped: false });
                    return;
                }
            }
        }
    }
    send(ScanEvent::Finished { resume, cancelled: false, capped: false });
}

/// 同一节点上的一批键：pipeline 读 TYPE 与 PTTL。
async fn describe(conn: &mut super::conn::RedisConn, names: Vec<String>) -> Result<Vec<RedisKeyInfo>, redis::RedisError> {
    let mut pipe = redis::pipe();
    for name in &names {
        pipe.cmd("TYPE").arg(name).cmd("PTTL").arg(name);
    }
    let infos: Vec<redis::Value> = pipe.query_async(conn).await?;
    Ok(names
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            let key_type = match infos.get(index * 2) {
                Some(redis::Value::SimpleString(text)) => text.clone(),
                Some(redis::Value::BulkString(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
                Some(redis::Value::Okay) => "OK".to_owned(),
                _ => String::new(),
            };
            let ttl_ms = match infos.get(index * 2 + 1) {
                Some(redis::Value::Int(ms)) => Some(*ms),
                _ => None,
            };
            RedisKeyInfo { name, key_type, ttl_ms }
        })
        .collect())
}
