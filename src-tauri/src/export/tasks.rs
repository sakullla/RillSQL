//! 任务中心状态。取消后保持已取消和实际范围，不能再显示为运行中或全部成功。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Running,
    Done,
    Failed,
    Cancelled,
    Blocked,
    Partial,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskView {
    pub id: String,
    pub kind: &'static str,
    pub phase: Phase,
    pub done_rows: u64,
    pub detail: String,
    pub path: Option<String>,
    pub success: bool,
}

impl TaskView {
    pub fn running(id: impl Into<String>, kind: &'static str, detail: impl Into<String>) -> Self {
        Self { id: id.into(), kind, phase: Phase::Running, done_rows: 0, detail: detail.into(), path: None, success: false }
    }

    pub fn finish(id: impl Into<String>, kind: &'static str, phase: Phase, done_rows: u64, detail: impl Into<String>, path: Option<String>) -> Self {
        let success = phase == Phase::Done;
        Self { id: id.into(), kind, phase, done_rows, detail: detail.into(), path, success }
    }

    pub fn label_key(&self) -> &'static str {
        match self.phase {
            Phase::Running => "export.task.running",
            Phase::Done => "export.task.done",
            Phase::Failed => "export.task.failed",
            Phase::Cancelled => "export.task.cancelled",
            Phase::Blocked => "export.task.blocked",
            Phase::Partial => "export.task.partial",
        }
    }

    pub fn running_now(&self) -> bool {
        self.phase == Phase::Running
    }
}

#[derive(Clone, Debug, Default)]
pub struct TaskLog {
    pub tasks: Vec<TaskView>,
}

impl TaskLog {
    pub fn push(&mut self, task: TaskView) {
        if let Some(slot) = self.tasks.iter_mut().find(|item| item.id == task.id) {
            *slot = task;
        } else {
            self.tasks.push(task);
        }
    }

    pub fn running(&self) -> bool {
        self.tasks.iter().any(TaskView::running_now)
    }

    pub fn clear_finished(&mut self) {
        self.tasks.retain(TaskView::running_now);
    }
}

/// 代次已取消时，迟到的成功结果也不能改回完成。
pub fn prefer_cancel(mut task: TaskView, cancelled: bool) -> TaskView {
    if cancelled && task.phase != Phase::Cancelled {
        task.detail = format!("已取消。实际完成范围 {} 行。{}", task.done_rows, task.detail);
        task.phase = Phase::Cancelled;
        task.success = false;
        task.path = None;
    }
    task
}
