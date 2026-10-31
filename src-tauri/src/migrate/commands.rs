//! 跨库数据迁移的 IPC 入口：命令在模块内注册，进度经 `migrate_progress` 事件推送。

use super::migrate::{run_migrate, MigrateDto, MigrateRequest};
use crate::connections::commands::ConnectionsState;

/// 把源表的数据流式搬到目标表。源与目标各用一条已打开的连接，不另开新连接。
#[tauri::command]
pub async fn migrate_data<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, ConnectionsState>,
    request: MigrateRequest,
) -> Result<MigrateDto, String> {
    let source = state
        .service
        .catalog_session(&request.source.connection_id)
        .ok_or_else(|| "源连接还没有打开，没有另开新的连接。".to_owned())?;
    let target = state
        .service
        .catalog_session(&request.target.connection_id)
        .ok_or_else(|| "目标连接还没有打开，没有另开新的连接。".to_owned())?;
    use tauri::Emitter as _;
    Ok(run_migrate(&source, &target, &request, &|payload| {
        let _ = app.emit("migrate_progress", payload);
    })
    .await)
}

/// 取消尚未完成的迁移；已经提交的批次保留，进度按已报告的计数呈现。
#[tauri::command]
pub async fn migrate_cancel(id: String) -> Result<(), String> {
    super::migrate::request_cancel(&id);
    Ok(())
}
