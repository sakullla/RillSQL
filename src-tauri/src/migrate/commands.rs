//! 跨库数据迁移的 IPC 入口：命令在模块内注册，进度经 `migrate_progress` 事件推送。

use super::migrate::{MigrateDto, MigrateRequest};
use crate::application::tasks::{take_current, TaskBoard};
use crate::connections::commands::ConnectionsState;

/// 把源表的数据流式搬到目标表。源与目标各用一条已打开的连接，不另开新连接。
pub async fn migrate_data(state: &ConnectionsState, request: MigrateRequest) -> Result<MigrateDto, String> {
    let tasks = TaskBoard::new();
    let token = tasks.begin("migrate");
    let outcome = crate::application::work::migrate_data(&state.service, &tasks, "migrate", token, &request, &|_payload| {}).await?;
    take_current(outcome)
}

/// 取消尚未完成的迁移；已经提交的批次保留，进度按已报告的计数呈现。
pub async fn migrate_cancel(id: String) -> Result<(), String> {
    super::migrate::request_cancel(&id);
    Ok(())
}
