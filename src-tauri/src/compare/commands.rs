use crate::application::tasks::{take_current, TaskBoard};
use crate::connections::commands::ConnectionsState;

use super::model::{DataReport, DataRequest, SchemaListRequest, StructureReport, StructureRequest};

#[tauri::command]
pub async fn compare_structure(state: tauri::State<'_, ConnectionsState>, request: StructureRequest) -> Result<StructureReport, String> {
    let tasks = TaskBoard::new();
    let token = tasks.begin("compare");
    take_current(crate::application::work::compare_structure(&state.service, &tasks, "compare", token, &request).await)
}

#[tauri::command]
pub async fn compare_data(state: tauri::State<'_, ConnectionsState>, request: DataRequest) -> Result<DataReport, String> {
    let tasks = TaskBoard::new();
    let token = tasks.begin("compare");
    take_current(crate::application::work::compare_data(&state.service, &tasks, "compare", token, &request).await)
}

#[tauri::command]
pub fn compare_cancel(id: String) {
    crate::compare::request_compare_cancel(&id);
}

#[tauri::command]
pub async fn compare_schemas(state: tauri::State<'_, ConnectionsState>, request: SchemaListRequest) -> Result<Vec<String>, String> {
    crate::application::work::compare_schemas(&state.service, &request.connection_id, &request.database).await
}
