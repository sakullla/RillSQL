//! 全库导出的 IPC 入口：命令在模块内注册，进度经 `dump_progress` 事件推送。

use super::dump::{DumpDto, DumpRequest};
use crate::application::tasks::{take_current, TaskBoard};
use crate::connections::commands::ConnectionsState;

/// 把整库结构（可选数据）按依赖序导出为单个 SQL 文件。只读，不执行任何语句。
pub async fn database_dump(state: &ConnectionsState, request: DumpRequest) -> Result<DumpDto, String> {
    let tasks = TaskBoard::new();
    let token = tasks.begin("dump");
    let outcome = crate::application::work::dump_database(&state.service, &tasks, "dump", token, &request, &|_payload| {}).await?;
    take_current(outcome)
}

/// 取消尚未完成的全库导出；未完成的 `.partial` 文件随取消删除。
pub async fn database_dump_cancel(id: String) -> Result<(), String> {
    super::dump::request_cancel(&id);
    Ok(())
}
