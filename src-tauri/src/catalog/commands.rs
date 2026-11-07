use serde::{Deserialize, Serialize};

use crate::catalog::model::{kind_name, known_row_estimate, ConnStatus, Engine, KindReport, ObjectRow};
use crate::catalog::objects::{self, ExtrasBody, ExtrasList, ExtrasScope};
use crate::catalog::read::{self, ObjectWindow};
use crate::connections::commands::ConnectionsState;
use crate::connections::model::{EnvTag, ENV_NONE};
use crate::connections::{CatalogSession, Service, SessionCondition, SessionStatus};

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
    /// 只在 catalog_refresh 明确要求时返回；其余刷新为 null，不额外查询。
    pub extras: Option<ExtrasDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageDto {
    pub objects: Vec<ObjectDto>,
    pub total: usize,
    pub error: Option<String>,
    pub kind_reports: Vec<KindReportDto>,
}

/// 一节对象详情：条目与该节自己的失败原因。
#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExtrasListDto<T> {
    pub items: Vec<T>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionDto {
    pub name: String,
    pub default_version: String,
    pub installed_version: Option<String>,
    pub comment: String,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SequenceDto {
    pub schema: String,
    pub name: String,
    pub data_type: String,
    pub start_value: i64,
    pub min_value: i64,
    pub max_value: i64,
    pub increment: i64,
    pub cache_size: i64,
    pub cycle: bool,
    pub last_value: Option<i64>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EventDto {
    pub database: String,
    pub name: String,
    pub status: String,
    pub schedule: String,
    pub on_completion: String,
    pub definer: String,
    pub last_executed: Option<String>,
    /// 事件体原文（EVENT_DEFINITION），面板展开只读查看。
    pub definition: String,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PartitionItemDto {
    pub name: String,
    pub description: String,
    pub rows: Option<i64>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PartitionDto {
    pub database: String,
    pub table: String,
    pub method: String,
    pub expression: String,
    pub partitions: Vec<PartitionItemDto>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PragmaDto {
    pub journal_mode: Option<String>,
    pub user_version: Option<i64>,
    pub foreign_keys: Option<bool>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttachedDto {
    pub name: String,
    pub file: String,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExtrasDto {
    pub extensions: ExtrasListDto<ExtensionDto>,
    pub sequences: ExtrasListDto<SequenceDto>,
    pub events: ExtrasListDto<EventDto>,
    pub partitions: ExtrasListDto<PartitionDto>,
    pub pragmas: PragmaDto,
    pub attached: ExtrasListDto<AttachedDto>,
}

/// catalog_refresh 的 extras 参数：只给这一个连接、这一个库/schema 读对象详情。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtrasScopeParams {
    pub connection_id: String,
    pub database: String,
    pub schema: String,
}

pub(crate) async fn load_catalog(
    service: &Service,
    page: usize,
    limits: &[ObjectLimit],
    connection_id: Option<&str>,
    outline: bool,
) -> Result<Vec<CatalogDto>, String> {
    load_catalog_inner(service, page, limits, connection_id, outline, None).await
}

/// extras 只对匹配的那一个连接读取；其余连接的 extras 为 null。
pub(crate) async fn load_catalog_inner(
    service: &Service,
    page: usize,
    limits: &[ObjectLimit],
    connection_id: Option<&str>,
    outline: bool,
    extras: Option<&ExtrasScope>,
) -> Result<Vec<CatalogDto>, String> {
    let saved = service.list().await?;
    let tags = service.list_tags().await?;
    let mut catalogs = Vec::with_capacity(saved.len());
    for record in saved {
        if connection_id.is_some_and(|id| id != record.saved.id) {
            continue;
        }
        let engine = record.saved.engine;
        let condition = service.session_condition(&record.saved.id);
        // lost 会话不再读取目录：树上显示断开前读到的内容（R4）。
        let session = if matches!(condition, Some(SessionCondition::Connected)) {
            service.catalog_session(&record.saved.id)
        } else {
            None
        };
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
        let extras_body = match (&session, extras) {
            (Some(session), Some(scope)) if scope.connection_id == record.saved.id => {
                Some(objects::read_extras(session, engine, &scope.database, &scope.schema).await)
            }
            _ => None,
        };
        let (env_label, env_tone) = describe_env(&tags, &record.saved.env);
        let (status, error) = match condition {
            Some(SessionCondition::Lost { reason }) => (ConnStatus::Lost.as_api(), reason.or(body.error)),
            Some(SessionCondition::Connected) => (ConnStatus::Connected.as_api(), body.error),
            None => (ConnStatus::Disconnected.as_api(), body.error),
        };
        catalogs.push(CatalogDto {
            id: record.saved.id,
            name: record.saved.name,
            engine: engine.as_str().to_owned(),
            env: record.saved.env,
            env_label,
            env_tone,
            status,
            default_database: record.saved.database,
            foreign_keys: if engine.shows_foreign_keys() { body.foreign_keys } else { None },
            error,
            databases: body.databases.into_iter().map(database_dto).collect(),
            extras: extras_body.map(extras_dto),
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

pub async fn catalog_sessions(state: &ConnectionsState) -> Result<Vec<SessionStatus>, String> {
    Ok(std::sync::Arc::clone(&state.service).session_statuses().await)
}

pub async fn catalog_refresh(
    state: &ConnectionsState,
    limits: Vec<ObjectLimit>,
    page: Option<usize>,
    connection_id: Option<String>,
    outline: Option<bool>,
    extras: Option<ExtrasScopeParams>,
) -> Result<Vec<CatalogDto>, String> {
    let scope = extras.map(|item| ExtrasScope {
        connection_id: item.connection_id,
        database: item.database,
        schema: item.schema,
    });
    let page = page.unwrap_or(0);
    let connection_id = connection_id.as_deref();
    let outline = outline.unwrap_or(false);
    match scope {
        // 没要对象详情就走原路：不额外查询，返回体与改动前一致。
        None => load_catalog(&state.service, page, &limits, connection_id, outline).await,
        Some(scope) => {
            load_catalog_inner(&state.service, page, &limits, connection_id, outline, Some(&scope)).await
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn catalog_definition(
    state: &ConnectionsState,
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
    let engine = saved_engine(state, &connection_id).await?;
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

pub async fn catalog_page(
    state: &ConnectionsState,
    connection_id: String,
    database: String,
    schema: String,
    offset: usize,
    limit: usize,
) -> Result<PageDto, String> {
    let session = state.service.catalog_session(&connection_id);
    let engine = saved_engine(state, &connection_id).await?;
    Ok(page_for_session(session.as_ref(), engine, &database, &schema, offset, limit).await)
}

async fn saved_engine(state: &ConnectionsState, connection_id: &str) -> Result<Engine, String> {
    let saved = state.service.list().await?;
    saved
        .into_iter()
        .find(|item| item.saved.id == connection_id)
        .map(|item| item.saved.engine)
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

trait StatusText {
    #[allow(clippy::wrong_self_convention)]
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

fn extras_dto(body: ExtrasBody) -> ExtrasDto {
    ExtrasDto {
        extensions: extras_list(body.extensions, |item| ExtensionDto {
            name: item.name.clone(),
            default_version: item.default_version.clone(),
            installed_version: item.installed_version.clone(),
            comment: item.comment.clone(),
        }),
        sequences: extras_list(body.sequences, |item| SequenceDto {
            schema: item.schema.clone(),
            name: item.name.clone(),
            data_type: item.data_type.clone(),
            start_value: item.start_value,
            min_value: item.min_value,
            max_value: item.max_value,
            increment: item.increment,
            cache_size: item.cache_size,
            cycle: item.cycle,
            last_value: item.last_value,
        }),
        events: extras_list(body.events, |item| EventDto {
            database: item.database.clone(),
            name: item.name.clone(),
            status: item.status.clone(),
            schedule: item.schedule.clone(),
            on_completion: item.on_completion.clone(),
            definer: item.definer.clone(),
            last_executed: item.last_executed.clone(),
            definition: item.definition.clone(),
        }),
        partitions: extras_list(body.partitions, |item| PartitionDto {
            database: item.database.clone(),
            table: item.table.clone(),
            method: item.method.clone(),
            expression: item.expression.clone(),
            partitions: item
                .partitions
                .iter()
                .map(|part| PartitionItemDto {
                    name: part.name.clone(),
                    description: part.description.clone(),
                    rows: part.rows,
                })
                .collect(),
        }),
        pragmas: PragmaDto {
            journal_mode: body.pragmas.value.as_ref().and_then(|item| item.journal_mode.clone()),
            user_version: body.pragmas.value.as_ref().and_then(|item| item.user_version),
            foreign_keys: body.pragmas.value.as_ref().and_then(|item| item.foreign_keys),
            error: body.pragmas.error,
        },
        attached: extras_list(body.attached, |item| AttachedDto {
            name: item.name.clone(),
            file: item.file.clone(),
        }),
    }
}

fn extras_list<T, U>(list: ExtrasList<T>, map: impl Fn(&T) -> U) -> ExtrasListDto<U> {
    ExtrasListDto {
        items: list.items.iter().map(map).collect(),
        error: list.error,
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
