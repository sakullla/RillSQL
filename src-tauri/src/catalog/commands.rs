use serde::{Deserialize, Serialize};

use crate::catalog::model::{kind_name, known_row_estimate, ConnStatus, Engine, KindReport, ObjectRow};
use crate::catalog::read::{self, ObjectWindow};
use crate::connections::commands::ConnectionsState;
use crate::connections::model::{EnvTag, ENV_NONE};
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
    pub signature: String,
    pub relation: String,
    pub materialized: bool,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KindReportDto {
    pub kind: &'static str,
    pub total: usize,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaDto {
    pub name: String,
    pub objects: Vec<ObjectDto>,
    pub total: usize,
    pub error: Option<String>,
    pub kind_reports: Vec<KindReportDto>,
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
    pub env_tone: String,
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
    pub kind_reports: Vec<KindReportDto>,
}

pub(crate) async fn load_catalog(
    service: &Service,
    page: usize,
    limits: &[ObjectLimit],
    connection_id: Option<&str>,
    outline: bool,
) -> Result<Vec<CatalogDto>, String> {
    let saved = service.list().await?;
    let tags = service.list_tags().await?;
    let mut catalogs = Vec::with_capacity(saved.len());
    for record in saved {
        if connection_id.is_some_and(|id| id != record.saved.id) {
            continue;
        }
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
            Some(session) if outline => read::read_catalog_outline(engine, session).await,
            Some(session) => read::read_catalog_session(engine, session, page, &windows).await,
            None => read::ConnectionBody {
                error: None,
                foreign_keys: None,
                databases: Vec::new(),
            },
        };
        let (env_label, env_tone) = describe_env(&tags, &record.saved.env);
        catalogs.push(CatalogDto {
            id: record.saved.id,
            name: record.saved.name,
            engine: engine.as_str().to_owned(),
            env: record.saved.env,
            env_label,
            env_tone,
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

pub(crate) async fn page_for_session(session: Option<&CatalogSession>, engine: Engine, database: &str, schema: &str, offset: usize, limit: usize) -> PageDto {
    let Some(session) = session else {
        return PageDto {
            objects: Vec::new(),
            total: 0,
            error: Some("这条连接还没有打开，无法继续读取对象".to_owned()),
            kind_reports: Vec::new(),
        };
    };
    let body = read::read_object_page(session, engine, database, schema, offset, limit).await;
    PageDto {
        objects: body.objects.iter().map(object_dto).collect(),
        total: body.total,
        error: body.error,
        kind_reports: body.kind_reports.iter().map(kind_report_dto).collect(),
    }
}

#[tauri::command]
pub async fn catalog_sessions(state: tauri::State<'_, ConnectionsState>) -> Result<Vec<String>, String> {
    Ok(state.service.open_session_ids())
}

#[tauri::command]
pub async fn catalog_refresh(
    state: tauri::State<'_, ConnectionsState>,
    limits: Vec<ObjectLimit>,
    page: Option<usize>,
    connection_id: Option<String>,
    outline: Option<bool>,
) -> Result<Vec<CatalogDto>, String> {
    load_catalog(&state.service, page.unwrap_or(0), &limits, connection_id.as_deref(), outline.unwrap_or(false)).await
}

#[tauri::command]
pub async fn catalog_definition(
    state: tauri::State<'_, ConnectionsState>,
    connection_id: String,
    database: String,
    schema: String,
    name: String,
    kind: String,
    signature: Option<String>,
    relation: Option<String>,
) -> Result<String, String> {
    let session = state
        .service
        .catalog_session(&connection_id)
        .ok_or_else(|| "这条连接还没有打开，无法读取定义".to_owned())?;
    let engine = saved_engine(&state, &connection_id).await?;
    read::read_definition(
        &session,
        engine,
        &database,
        &schema,
        &name,
        &kind,
        signature.as_deref().unwrap_or(""),
        relation.as_deref().unwrap_or(""),
    )
    .await
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
    let engine = saved_engine(&state, &connection_id).await?;
    Ok(page_for_session(session.as_ref(), engine, &database, &schema, offset, limit).await)
}

async fn saved_engine(state: &ConnectionsState, connection_id: &str) -> Result<Engine, String> {
    let saved = state.service.list().await?;
    saved
        .into_iter()
        .find(|item| item.saved.id == connection_id)
        .map(|item| map_engine(item.saved.engine))
        .ok_or_else(|| "找不到这条连接".to_owned())
}

fn describe_env(tags: &[EnvTag], id: &str) -> (String, String) {
    if id.is_empty() || id == ENV_NONE {
        return ("无标签".to_owned(), ENV_NONE.to_owned());
    }
    match tags.iter().find(|tag| tag.id == id) {
        Some(tag) => (tag.label.clone(), tag.tone.clone()),
        None => ("无标签".to_owned(), ENV_NONE.to_owned()),
    }
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
        kind_reports: schema.kind_reports.iter().map(kind_report_dto).collect(),
    }
}

fn object_dto(object: &ObjectRow) -> ObjectDto {
    ObjectDto {
        name: object.name.clone(),
        kind: kind_name(object.kind),
        estimated_rows: known_row_estimate(object.estimated_rows),
        stale: object.stale,
        signature: object.signature.clone(),
        relation: object.relation.clone(),
        materialized: object.materialized,
    }
}

fn kind_report_dto(report: &KindReport) -> KindReportDto {
    KindReportDto {
        kind: kind_name(report.kind),
        total: report.total,
        error: report.error.clone(),
    }
}

#[cfg(test)]
mod estimate_tests {
    use super::object_dto;
    use crate::catalog::model::ObjectRow;

    #[test]
    fn negative_estimate_is_omitted_from_the_catalog_payload() {
        let unknown = ObjectRow {
            name: "customers".to_owned(),
            kind: crate::catalog::model::ObjectKind::Table,
            estimated_rows: Some(-1),
            stale: false,
            signature: String::new(),
            relation: String::new(),
            materialized: false,
        };
        let dto = object_dto(&unknown);
        assert_eq!(dto.estimated_rows, None);
        assert!(serde_json::to_value(&dto).unwrap()["estimatedRows"].is_null());

        let empty = ObjectRow {
            estimated_rows: Some(0),
            ..unknown
        };
        assert_eq!(object_dto(&empty).estimated_rows, Some(0));
    }
}
