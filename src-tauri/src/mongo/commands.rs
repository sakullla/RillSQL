//! MongoDB 专项 IPC（R12）：库/集合清单、文档分页（JSON 过滤带定位错误）、文档 CRUD。
//! 删除按 confirmed 标记强制：没有界面确认就拒绝且不发任何命令；编辑/新增先做 JSON 校验再落库。

use std::sync::Arc;

use mongodb::bson::{Bson, Document};

use super::docs::{self, CollectionSummary, DatabaseSummary, DocumentsPage};
use crate::connections::commands::ConnectionsState;
use crate::connections::SessionHealth;

/// 取 MongoDB 会话并执行；网络类错误回落到共享健康句柄（R4 探活）。
async fn with_mongo<T, F, Fut>(state: &tauri::State<'_, ConnectionsState>, connection_id: &str, run: F) -> Result<T, String>
where
    F: FnOnce(mongodb::Client, Arc<SessionHealth>) -> Fut,
    Fut: std::future::Future<Output = Result<T, mongodb::error::Error>>,
{
    let (client, health) = state
        .service
        .mongo_session(connection_id)
        .ok_or_else(|| "这条连接还没有打开，无法浏览集合文档。".to_owned())?;
    let outcome = run(client, health.clone()).await;
    if let Err(err) = &outcome {
        if matches!(
            &*err.kind,
            mongodb::error::ErrorKind::Io(_)
                | mongodb::error::ErrorKind::ServerSelection { .. }
                | mongodb::error::ErrorKind::ConnectionPoolCleared { .. }
        ) {
            health.note_disconnect();
        }
    }
    outcome.map_err(|err| err.to_string())
}

/// 库列表（含集合数），排除 admin/config/local。
#[tauri::command]
pub async fn mongo_list_databases(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
) -> Result<Vec<DatabaseSummary>, String> {
    let (client, _health) = state
        .service
        .mongo_session(&connection_id)
        .ok_or_else(|| "这条连接还没有打开，无法浏览集合文档。".to_owned())?;
    docs::list_databases(&client).await
}

/// 集合列表 + estimatedDocumentCount 计数。
#[tauri::command]
pub async fn mongo_list_collections(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    database: String,
) -> Result<Vec<CollectionSummary>, String> {
    let database = database.trim().to_owned();
    if database.is_empty() {
        return Err("需要数据库名。".to_owned());
    }
    let (client, _health) = state
        .service
        .mongo_session(&connection_id)
        .ok_or_else(|| "这条连接还没有打开，无法浏览集合文档。".to_owned())?;
    docs::list_collections(&client, &database).await
}

/// 文档分页：skip/limit + 可选 JSON 过滤。过滤语法错误在解析阶段就返回定位信息，不发查询。
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
    let database = database.trim().to_owned();
    let collection = collection.trim().to_owned();
    if database.is_empty() || collection.is_empty() {
        return Err("需要数据库名和集合名。".to_owned());
    }
    let filter = match filter_json.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => docs::parse_document("过滤", text)?,
        None => Document::new(),
    };
    let skip = skip.unwrap_or(0);
    let limit = limit.unwrap_or(docs::DEFAULT_PAGE_LIMIT).clamp(1, 500);
    with_mongo(&state, &connection_id, move |client, _| async move {
        docs::find_documents(&client, &database, &collection, skip, limit, filter).await
    })
    .await
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadOutcome {
    pub json: String,
}

/// 读单个文档（按 _id 扩展 JSON 定位），返回整档 relaxed JSON 文本。
#[tauri::command]
pub async fn mongo_read_document(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    database: String,
    collection: String,
    id_json: String,
) -> Result<ReadOutcome, String> {
    let (database, collection) = require_target(&database, &collection)?;
    let id = docs::parse_id(&id_json)?;
    let json = with_mongo(&state, &connection_id, move |client, _| async move {
        docs::read_document(&client, &database, &collection, id).await
    })
    .await?
    .ok_or_else(|| "没有找到这个文档（可能已被删除）。".to_owned())?;
    Ok(ReadOutcome { json })
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceOutcome {
    pub matched: i64,
    pub modified: i64,
}

/// 编辑文档：整体 JSON 覆盖。JSON 校验通过才执行 replace_one（校验失败不发任何命令）。
#[tauri::command]
pub async fn mongo_replace_document(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    database: String,
    collection: String,
    id_json: String,
    json: String,
) -> Result<ReplaceOutcome, String> {
    let (database, collection) = require_target(&database, &collection)?;
    let id = docs::parse_id(&id_json)?;
    let replacement = docs::parse_document("文档", &json)?;
    let outcome = with_mongo(&state, &connection_id, move |client, _| async move {
        client
            .database(&database)
            .collection::<Document>(&collection)
            .replace_one(docs::id_filter(id), replacement)
            .await
    })
    .await?;
    Ok(ReplaceOutcome {
        matched: i64::try_from(outcome.matched_count).unwrap_or(i64::MAX),
        modified: i64::try_from(outcome.modified_count).unwrap_or(i64::MAX),
    })
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertOutcome {
    pub inserted_id_json: String,
}

/// 新增文档：JSON 校验通过才执行 insert_one，返回服务器生成的 _id。
#[tauri::command]
pub async fn mongo_insert_document(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    database: String,
    collection: String,
    json: String,
) -> Result<InsertOutcome, String> {
    let (database, collection) = require_target(&database, &collection)?;
    let document = docs::parse_document("文档", &json)?;
    let inserted = with_mongo(&state, &connection_id, move |client, _| async move {
        client
            .database(&database)
            .collection::<Document>(&collection)
            .insert_one(document)
            .await
    })
    .await?;
    Ok(InsertOutcome {
        inserted_id_json: docs::value_to_json(&inserted.inserted_id),
    })
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteOutcome {
    pub deleted: i64,
}

/// 删除文档（按 _id）。confirmed=false（界面没有确认）时拒绝且不发 delete。
#[tauri::command]
pub async fn mongo_delete_document(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    database: String,
    collection: String,
    id_json: String,
    confirmed: Option<bool>,
) -> Result<DeleteOutcome, String> {
    let (database, collection) = require_target(&database, &collection)?;
    let id: Bson = docs::parse_id(&id_json)?;
    if !confirmed.unwrap_or(false) {
        return Err("删除文档属于写删操作，需要先在界面确认。".to_owned());
    }
    let outcome = with_mongo(&state, &connection_id, move |client, _| async move {
        client
            .database(&database)
            .collection::<Document>(&collection)
            .delete_one(docs::id_filter(id))
            .await
    })
    .await?;
    let deleted = i64::try_from(outcome.deleted_count).unwrap_or(i64::MAX);
    Ok(DeleteOutcome { deleted })
}

fn require_target(database: &str, collection: &str) -> Result<(String, String), String> {
    let database = database.trim();
    let collection = collection.trim();
    if database.is_empty() {
        return Err("需要数据库名。".to_owned());
    }
    if collection.is_empty() {
        return Err("需要集合名。".to_owned());
    }
    Ok((database.to_owned(), collection.to_owned()))
}
