use serde::{Deserialize, Serialize};

use crate::catalog::model::{ConnStatus, Engine, ObjectKind, ObjectRow};
use crate::catalog::read::{self, ObjectWindow};
use crate::connections::commands::ConnectionsState;
use crate::connections::{CatalogSession, Service};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectLimit {
    pub connection_id: String,
    pub database: String,
    pub schema: String,
    pub limit: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectDto {
    pub name: String,
    pub kind: &'static str,
    pub estimated_rows: Option<i64>,
    pub stale: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaDto {
    pub name: String,
    pub objects: Vec<ObjectDto>,
    pub total: usize,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseDto {
    pub name: String,
    pub schemas: Vec<SchemaDto>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogDto {
    pub id: String,
    pub name: String,
    pub engine: String,
    pub env: String,
    pub env_label: String,
    pub status: &'static str,
    pub default_database: Option<String>,
    pub foreign_keys: Option<bool>,
    pub error: Option<String>,
    pub databases: Vec<DatabaseDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageDto {
    pub objects: Vec<ObjectDto>,
    pub total: usize,
    pub error: Option<String>,
}

pub(crate) async fn load_catalog(service: &Service, page: usize, limits: &[ObjectLimit]) -> Result<Vec<CatalogDto>, String> {
    let saved = service.list().await?;
    let mut catalogs = Vec::with_capacity(saved.len());
    for record in saved {
        let engine = map_engine(record.saved.engine);
        let session = service.catalog_session(&record.saved.id);
        let connected = session.is_some();
        let windows: Vec<ObjectWindow> = limits
            .iter()
            .filter(|item| item.connection_id == record.saved.id)
            .map(|item| ObjectWindow {
                database: item.database.clone(),
                schema: item.schema.clone(),
                limit: item.limit,
            })
            .collect();
        let body = match &session {
            Some(session) => read::read_catalog_session(engine, session, page, &windows).await,
            None => read::ConnectionBody {
                error: None,
                foreign_keys: None,
                databases: Vec::new(),
            },
        };
        catalogs.push(CatalogDto {
            id: record.saved.id,
            name: record.saved.name,
            engine: engine.as_str().to_owned(),
            env: record.saved.env.as_str().to_owned(),
            env_label: record.saved.env.label_zh().to_owned(),
            status: if connected {
                ConnStatus::Connected
            } else {
                ConnStatus::Disconnected
            }
            .as_api(),
            default_database: record.saved.database,
            foreign_keys: if engine.shows_foreign_keys() { body.foreign_keys } else { None },
            error: body.error,
            databases: body.databases.into_iter().map(database_dto).collect(),
        });
    }
    Ok(catalogs)
}

pub(crate) async fn page_for_session(session: Option<&CatalogSession>, database: &str, schema: &str, offset: usize, limit: usize) -> PageDto {
    let Some(session) = session else {
        return PageDto {
            objects: Vec::new(),
            total: 0,
            error: Some("连接未打开，无法继续读取对象".to_owned()),
        };
    };
    let body = read::read_object_page(session, database, schema, offset, limit).await;
    PageDto {
        objects: body.objects.iter().map(object_dto).collect(),
        total: body.total,
        error: body.error,
    }
}

#[tauri::command]
pub async fn catalog_sessions(state: tauri::State<'_, ConnectionsState>) -> Result<Vec<String>, String> {
    Ok(state.service.open_session_ids())
}

#[tauri::command]
pub async fn catalog_refresh(state: tauri::State<'_, ConnectionsState>, limits: Vec<ObjectLimit>, page: Option<usize>) -> Result<Vec<CatalogDto>, String> {
    load_catalog(&state.service, page.unwrap_or(0), &limits).await
}

#[tauri::command]
pub async fn catalog_page(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    database: String,
    schema: String,
    offset: usize,
    limit: usize,
) -> Result<PageDto, String> {
    let session = state.service.catalog_session(&connection_id);
    Ok(page_for_session(session.as_ref(), &database, &schema, offset, limit).await)
}

fn map_engine(engine: crate::connections::model::Engine) -> Engine {
    match engine {
        crate::connections::model::Engine::Postgres => Engine::Postgres,
        crate::connections::model::Engine::Mysql => Engine::Mysql,
        crate::connections::model::Engine::Mariadb => Engine::Mariadb,
        crate::connections::model::Engine::Sqlite => Engine::Sqlite,
    }
}

trait StatusText {
    fn as_api(self) -> &'static str;
}

impl StatusText for ConnStatus {
    fn as_api(self) -> &'static str {
        match self {
            ConnStatus::Connected => "connected",
            ConnStatus::Disconnected => "disconnected",
            ConnStatus::Connecting => "connecting",
            ConnStatus::Error => "error",
            ConnStatus::Lost => "lost",
        }
    }
}

fn database_dto(database: crate::catalog::model::DatabaseNode) -> DatabaseDto {
    DatabaseDto {
        name: database.name,
        error: database.error,
        schemas: database.schemas.into_iter().map(schema_dto).collect(),
    }
}

fn schema_dto(schema: crate::catalog::model::SchemaNode) -> SchemaDto {
    SchemaDto {
        name: schema.name,
        total: schema.total,
        error: schema.error,
        objects: schema.objects.iter().map(object_dto).collect(),
    }
}

fn object_dto(object: &ObjectRow) -> ObjectDto {
    ObjectDto {
        name: object.name.clone(),
        kind: match object.kind {
            ObjectKind::Table => "table",
            ObjectKind::View => "view",
        },
        estimated_rows: object.estimated_rows,
        stale: object.stale,
    }
}
