//! 任务 id 与递增代次。视图只接受当前代次；取消优先于迟到结果。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskToken {
    pub id: u64,
    pub generation: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TaskOutcome<T> {
    Current(T),
    Stale { id: u64, generation: u64 },
    Cancelled { id: u64 },
}

struct Slot {
    generation: u64,
    cancelled: HashMap<u64, bool>,
}

pub struct TaskBoard {
    next_id: AtomicU64,
    owners: Mutex<HashMap<String, Slot>>,
}

impl TaskBoard {
    pub fn new() -> Self {
        Self { next_id: AtomicU64::new(1), owners: Mutex::new(HashMap::new()) }
    }

    pub fn begin(&self, owner: &str) -> TaskToken {
        let mut owners = self.owners.lock().expect("tasks");
        let slot = owners.entry(owner.to_owned()).or_insert_with(|| Slot { generation: 1, cancelled: HashMap::new() });
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let generation = slot.generation;
        slot.cancelled.insert(id, false);
        TaskToken { id, generation }
    }

    /// 关闭或切换标签。旧代次结果不能再写入。
    pub fn supersede(&self, owner: &str) -> u64 {
        let mut owners = self.owners.lock().expect("tasks");
        let slot = owners.entry(owner.to_owned()).or_insert_with(|| Slot { generation: 1, cancelled: HashMap::new() });
        slot.generation = slot.generation.saturating_add(1);
        slot.generation
    }

    /// 取消这个所有者当前的全部任务。代次不变，所以结果记为已取消而不是过期。
    pub fn cancel_owner_tasks(&self, owner: &str) {
        let mut owners = self.owners.lock().expect("tasks");
        if let Some(slot) = owners.get_mut(owner) {
            for flag in slot.cancelled.values_mut() {
                *flag = true;
            }
        }
    }

    pub fn cancel(&self, owner: &str, id: u64) -> bool {
        let mut owners = self.owners.lock().expect("tasks");
        let Some(slot) = owners.get_mut(owner) else {
            return false;
        };
        match slot.cancelled.get_mut(&id) {
            Some(flag) => {
                *flag = true;
                true
            }
            None => false,
        }
    }

    pub fn accept<T>(&self, owner: &str, token: TaskToken, value: T) -> TaskOutcome<T> {
        let owners = self.owners.lock().expect("tasks");
        let Some(slot) = owners.get(owner) else {
            return TaskOutcome::Stale { id: token.id, generation: token.generation };
        };
        if slot.cancelled.get(&token.id).copied().unwrap_or(false) {
            return TaskOutcome::Cancelled { id: token.id };
        }
        if slot.generation != token.generation {
            return TaskOutcome::Stale { id: token.id, generation: token.generation };
        }
        TaskOutcome::Current(value)
    }
}

impl Default for TaskBoard {
    fn default() -> Self {
        Self::new()
    }
}

/// 把会堵住调用线程的文件或数据库工作交给阻塞线程池。
pub async fn run_blocking<T, F>(work: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work).await.map_err(|err| format!("后台任务失败：{err}"))?
}
