//! 跨库数据迁移的 IPC 入口：命令在模块内注册，进度经 `migrate_progress` 事件推送。

use super::migrate::{MigrateDto, MigrateRequest};
use crate::connections::commands::ConnectionsState;

/// 把源表的数据流式搬到目标表。源与目标各用一条已打开的连接，不另开新连接。
#[tauri::command]
pub async fn migrate_data<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, ConnectionsState>,
    request: MigrateRequest,
) -> Result<MigrateDto, String> {
    use tauri::Emitter as _;
    crate::application::work::migrate_data(&state.service, &request, &|payload| {
        let _ = app.emit("migrate_progress", payload);
    })
    .await
}

/// 取消尚未完成的迁移；已经提交的批次保留，进度按已报告的计数呈现。
#[tauri::command]
pub async fn migrate_cancel(id: String) -> Result<(), String> {
    super::migrate::request_cancel(&id);
    Ok(())
}
