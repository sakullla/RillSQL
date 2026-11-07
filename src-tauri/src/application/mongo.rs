//! MongoDB 能力的类型化入口。JSON 校验和删除确认在接触数据库之前完成。

use std::sync::Arc;

use mongodb::bson::{Bson, Document};
use serde::Serialize;

use crate::connections::{Service, SessionHealth};
use crate::mongo::docs::{self, CollectionSummary, DatabaseSummary, DocumentsPage};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadOutcome {
    pub json: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceOutcome {
    pub matched: i64,
    pub modified: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertOutcome {
    pub inserted_id_json: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteOutcome {
    pub deleted: i64,
}

async fn with_mongo<T, F, Fut>(service: &Service, connection_id: &str, run: F) -> Result<T, String>
where
    F: FnOnce(mongodb::Client, Arc<SessionHealth>) -> Fut,
    Fut: std::future::Future<Output = Result<T, mongodb::error::Error>>,
{
    let (client, health) = service.mongo_session(connection_id).ok_or_else(|| "这条连接还没有打开，无法浏览集合文档。".to_owned())?;
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

pub async fn list_databases(service: &Service, connection_id: &str) -> Result<Vec<DatabaseSummary>, String> {
    let (client, _health) = service.mongo_session(connection_id).ok_or_else(|| "这条连接还没有打开，无法浏览集合文档。".to_owned())?;
    docs::list_databases(&client).await
}

pub async fn list_collections(service: &Service, connection_id: &str, database: &str) -> Result<Vec<CollectionSummary>, String> {
    let database = database.trim().to_owned();
    if database.is_empty() {
        return Err("需要数据库名。".to_owned());
    }
    let (client, _health) = service.mongo_session(connection_id).ok_or_else(|| "这条连接还没有打开，无法浏览集合文档。".to_owned())?;
    docs::list_collections(&client, &database).await
}

pub async fn find_documents(
    service: &Service,
    connection_id: &str,
    database: &str,
    collection: &str,
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
    with_mongo(service, connection_id, move |client, _| async move {
        docs::find_documents(&client, &database, &collection, skip, limit, filter).await
    })
    .await
}

pub async fn read_document(service: &Service, connection_id: &str, database: &str, collection: &str, id_json: &str) -> Result<ReadOutcome, String> {
    let (database, collection) = require_target(database, collection)?;
    let id = docs::parse_id(id_json)?;
    let json = with_mongo(service, connection_id, move |client, _| async move {
        docs::read_document(&client, &database, &collection, id).await
    })
    .await?
    .ok_or_else(|| "没有找到这个文档（可能已被删除）。".to_owned())?;
    Ok(ReadOutcome { json })
}

pub async fn replace_document(service: &Service, connection_id: &str, database: &str, collection: &str, id_json: &str, json: &str) -> Result<ReplaceOutcome, String> {
    let (database, collection) = require_target(database, collection)?;
    let id = docs::parse_id(id_json)?;
    let replacement = docs::parse_document("文档", json)?;
    let outcome = with_mongo(service, connection_id, move |client, _| async move {
        client.database(&database).collection::<Document>(&collection).replace_one(docs::id_filter(id), replacement).await
    })
    .await?;
    Ok(ReplaceOutcome {
        matched: i64::try_from(outcome.matched_count).unwrap_or(i64::MAX),
        modified: i64::try_from(outcome.modified_count).unwrap_or(i64::MAX),
    })
}

pub async fn insert_document(service: &Service, connection_id: &str, database: &str, collection: &str, json: &str) -> Result<InsertOutcome, String> {
    let (database, collection) = require_target(database, collection)?;
    let document = docs::parse_document("文档", json)?;
    let inserted = with_mongo(service, connection_id, move |client, _| async move {
        client.database(&database).collection::<Document>(&collection).insert_one(document).await
    })
    .await?;
    Ok(InsertOutcome { inserted_id_json: docs::value_to_json(&inserted.inserted_id) })
}

pub async fn delete_document(service: &Service, connection_id: &str, database: &str, collection: &str, id_json: &str, confirmed: bool) -> Result<DeleteOutcome, String> {
    let (database, collection) = require_target(database, collection)?;
    let id: Bson = docs::parse_id(id_json)?;
    docs::require_delete(confirmed)?;
    let outcome = with_mongo(service, connection_id, move |client, _| async move {
        client.database(&database).collection::<Document>(&collection).delete_one(docs::id_filter(id)).await
    })
    .await?;
    Ok(DeleteOutcome { deleted: i64::try_from(outcome.deleted_count).unwrap_or(i64::MAX) })
}
