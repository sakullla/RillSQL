//! 全库导出的 IPC 入口：命令在模块内注册，进度经 `dump_progress` 事件推送。

use super::dump::{DumpDto, DumpRequest};
use crate::connections::commands::ConnectionsState;

/// 把整库结构（可选数据）按依赖序导出为单个 SQL 文件。只读，不执行任何语句。
#[tauri::command]
pub async fn database_dump<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, ConnectionsState>,
    request: DumpRequest,
) -> Result<DumpDto, String> {
    use tauri::Emitter as _;
    crate::application::work::dump_database(&state.service, &request, &|payload| {
        let _ = app.emit("dump_progress", payload);
    })
    .await
}

/// 取消尚未完成的全库导出；未完成的 `.partial` 文件随取消删除。
#[tauri::command]
pub async fn database_dump_cancel(id: String) -> Result<(), String> {
    super::dump::request_cancel(&id);
    Ok(())
}
