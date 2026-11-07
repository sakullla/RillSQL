//! MongoDB IPC。JSON 校验、删除确认、会话和健康标记在应用服务里，这里只转发。

use super::docs::{CollectionSummary, DatabaseSummary, DocumentsPage};
use crate::application::mongo::{self, DeleteOutcome, InsertOutcome, ReadOutcome, ReplaceOutcome};
use crate::connections::commands::ConnectionsState;

#[tauri::command]
pub async fn mongo_list_databases(state: tauri::State<'_, ConnectionsState>, connection_id: String) -> Result<Vec<DatabaseSummary>, String> {
    mongo::list_databases(&state.service, &connection_id).await
}

#[tauri::command]
pub async fn mongo_list_collections(state: tauri::State<'_, ConnectionsState>, connection_id: String, database: String) -> Result<Vec<CollectionSummary>, String> {
    mongo::list_collections(&state.service, &connection_id, &database).await
}

#[tauri::command]
pub async fn mongo_find_documents(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    database: String,
    collection: String,
    skip: Option<u64>,
    limit: Option<u64>,
    filter_json: Option<String>,
) -> Result<DocumentsPage, String> {
    mongo::find_documents(&state.service, &connection_id, &database, &collection, skip, limit, filter_json).await
}

#[tauri::command]
pub async fn mongo_read_document(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    database: String,
    collection: String,
    id_json: String,
) -> Result<ReadOutcome, String> {
    mongo::read_document(&state.service, &connection_id, &database, &collection, &id_json).await
}

#[tauri::command]
pub async fn mongo_replace_document(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    database: String,
    collection: String,
    id_json: String,
    json: String,
) -> Result<ReplaceOutcome, String> {
    mongo::replace_document(&state.service, &connection_id, &database, &collection, &id_json, &json).await
}

#[tauri::command]
pub async fn mongo_insert_document(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    database: String,
    collection: String,
    json: String,
) -> Result<InsertOutcome, String> {
    mongo::insert_document(&state.service, &connection_id, &database, &collection, &json).await
}

#[tauri::command]
pub async fn mongo_delete_document(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    database: String,
    collection: String,
    id_json: String,
    confirmed: Option<bool>,
) -> Result<DeleteOutcome, String> {
    mongo::delete_document(&state.service, &connection_id, &database, &collection, &id_json, confirmed.unwrap_or(false)).await
}
