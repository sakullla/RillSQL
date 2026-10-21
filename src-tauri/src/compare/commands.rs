use crate::connections::commands::ConnectionsState;

use super::model::{DataReport, DataRequest, StructureReport, StructureRequest};
use super::read::{self, read_data, read_structure};

#[tauri::command]
pub async fn compare_structure(state: tauri::State<'_, ConnectionsState>, request: StructureRequest) -> Result<StructureReport, String> {
    Ok(read_structure(&state.service, &request).await)
}

#[tauri::command]
pub async fn compare_data(state: tauri::State<'_, ConnectionsState>, request: DataRequest) -> Result<DataReport, String> {
    Ok(read_data(&state.service, &request).await)
}

#[tauri::command]
pub fn compare_cancel(id: String) {
    read::request_cancel(&id);
}
