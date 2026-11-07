//! 目录、比较、关系图、导出、迁移和查询的类型化入口。
//! 这些方法使用普通 Rust 类型；进度用回调，不经过 Tauri 事件。

use std::sync::Arc;

use crate::catalog::commands::{self as catalog_commands, CatalogDto, ObjectLimit, PageDto};
use crate::catalog::{read_definition, Engine};
use crate::compare::{
    compare_is_cancelled, list_schemas, read_data, read_structure, request_compare_cancel, DataReport, DataRequest, StructureReport, StructureRequest,
};
use crate::connections::{Service, SessionStatus};
use crate::diagram::Diagram;
use crate::diagram::read_diagram as read_diagram_session;
use crate::dump::dump::{self, DumpDto, DumpProgress, DumpRequest};
use crate::migrate::migrate::{self, MigrateDto, MigrateProgress, MigrateRequest};
use crate::query::commands::{self as query_commands, RunDto, StartRequest};
use crate::query::{RenameRequest, SaveRequest, SavedQuery, SavedWrite};
use crate::query::QueryState;

use super::tasks::{TaskBoard, TaskOutcome, TaskToken};

#[allow(dead_code)]
pub async fn catalog_sessions(service: &Arc<Service>) -> Vec<SessionStatus> {
    Arc::clone(service).session_statuses().await
}

pub async fn catalog_refresh(service: &Service, limits: &[ObjectLimit], page: Option<usize>, connection_id: Option<&str>, outline: bool) -> Result<Vec<CatalogDto>, String> {
    catalog_commands::load_catalog(service, page.unwrap_or(0), limits, connection_id, outline).await
}

#[allow(dead_code)]
pub async fn catalog_page(service: &Service, connection_id: &str, database: &str, schema: &str, offset: usize, limit: usize) -> Result<PageDto, String> {
    let session = service.catalog_session(connection_id);
    let engine = saved_engine(service, connection_id).await?;
    Ok(catalog_commands::page_for_session(session.as_ref(), engine, database, schema, offset, limit).await)
}

#[allow(clippy::too_many_arguments, dead_code)]
pub async fn catalog_definition(
    service: &Service,
    connection_id: &str,
    database: &str,
    schema: &str,
    name: &str,
    kind: &str,
    signature: &str,
    relation: &str,
) -> Result<String, String> {
    let session = service.catalog_session(connection_id).ok_or_else(|| "这条连接还没有打开，无法读取定义。".to_owned())?;
    let engine = saved_engine(service, connection_id).await?;
    read_definition(&session, engine, database, schema, name, kind, signature, relation).await
}

async fn saved_engine(service: &Service, connection_id: &str) -> Result<Engine, String> {
    let saved = service.list().await?;
    saved.into_iter().find(|item| item.saved.id == connection_id).map(|item| item.saved.engine).ok_or_else(|| "找不到这条连接".to_owned())
}

pub async fn compare_structure(service: &Service, tasks: &TaskBoard, owner: &str, token: TaskToken, request: &StructureRequest) -> TaskOutcome<StructureReport> {
    let report = read_structure(service, request).await;
    tasks.accept(owner, token, report)
}

pub async fn compare_data(service: &Service, tasks: &TaskBoard, owner: &str, token: TaskToken, request: &DataRequest) -> TaskOutcome<DataReport> {
    let report = read_data(service, request).await;
    tasks.accept(owner, token, report)
}

pub async fn compare_schemas(service: &Service, connection_id: &str, database: &str) -> Result<Vec<String>, String> {
    list_schemas(service, connection_id, database).await
}

pub fn cancel_compare(tasks: &TaskBoard, owner: &str, id: &str) {
    tasks.cancel_owner_tasks(owner);
    request_compare_cancel(id);
}

pub fn compare_cancelled(id: &str) -> bool {
    compare_is_cancelled(id)
}

pub async fn read_diagram(service: &Service, connection_id: &str, database: &str, schema: &str) -> Result<Diagram, String> {
    let session = service.catalog_session(connection_id).ok_or_else(|| "这条连接还没有打开，无法读取关系图".to_owned())?;
    let engine = saved_engine(service, connection_id).await?;
    read_diagram_session(engine, &session, database, schema).await
}

pub async fn dump_database(
    service: &Service,
    tasks: &TaskBoard,
    owner: &str,
    token: TaskToken,
    request: &DumpRequest,
    progress: &(dyn Fn(DumpProgress) + Send + Sync),
) -> Result<TaskOutcome<DumpDto>, String> {
    let session = service.catalog_session(&request.connection_id).ok_or_else(|| "这条连接还没有打开，没有另开新的连接。".to_owned())?;
    let dto = dump::run_dump(&session, request, &|event| {
        if matches!(tasks.accept(owner, token, ()), TaskOutcome::Current { .. }) {
            progress(event);
        }
    })
    .await;
    Ok(tasks.accept(owner, token, dto))
}

pub fn cancel_dump(tasks: &TaskBoard, owner: &str, id: &str) {
    tasks.cancel_owner_tasks(owner);
    dump::request_cancel(id);
}

pub async fn migrate_data(
    service: &Service,
    tasks: &TaskBoard,
    owner: &str,
    token: TaskToken,
    request: &MigrateRequest,
    progress: &(dyn Fn(MigrateProgress) + Send + Sync),
) -> Result<TaskOutcome<MigrateDto>, String> {
    let source = service.catalog_session(&request.source.connection_id).ok_or_else(|| "源连接还没有打开，没有另开新的连接。".to_owned())?;
    let target = service.catalog_session(&request.target.connection_id).ok_or_else(|| "目标连接还没有打开，没有另开新的连接。".to_owned())?;
    let dto = migrate::run_migrate(&source, &target, request, &|event| {
        if matches!(tasks.accept(owner, token, ()), TaskOutcome::Current { .. }) {
            progress(event);
        }
    })
    .await;
    Ok(tasks.accept(owner, token, dto))
}

pub fn cancel_migrate(tasks: &TaskBoard, owner: &str, id: &str) {
    tasks.cancel_owner_tasks(owner);
    migrate::request_cancel(id);
}

#[allow(dead_code)]
pub async fn start_query(service: &Arc<Service>, query: &QueryState, tasks: &TaskBoard, owner: &str, request: StartRequest) -> Result<TaskOutcome<RunDto>, String> {
    let token = tasks.begin(owner);
    let run = query_commands::start_run(service, query, request).await?;
    Ok(tasks.accept(owner, token, run))
}

#[allow(dead_code)]
pub async fn poll_query(query: &QueryState, tasks: &TaskBoard, owner: &str, token: TaskToken, run_id: &str, continue_read: bool) -> Result<TaskOutcome<RunDto>, String> {
    let run = query_commands::poll_run(query, run_id, continue_read).await?;
    Ok(tasks.accept(owner, token, run))
}

#[allow(dead_code)]
pub fn release_query(query: &QueryState, tasks: &TaskBoard, owner: &str, run_id: &str) -> bool {
    tasks.supersede(owner);
    query_commands::release_run(query, run_id)
}

#[allow(dead_code)]
pub async fn cancel_query(service: &Arc<Service>, query: &QueryState, tasks: &TaskBoard, owner: &str, token: TaskToken, run_id: &str) -> Result<TaskOutcome<RunDto>, String> {
    let run = query_commands::request_cancel(service, query, run_id).await?;
    let outcome = tasks.accept(owner, token, run);
    tasks.cancel(owner, token.id);
    Ok(outcome)
}

#[allow(dead_code)]
pub async fn list_history(query: &QueryState) -> Result<Vec<crate::query::HistoryEntry>, String> {
    query.history().await?.list().await
}

#[allow(dead_code)]
pub async fn clear_history(query: &QueryState) -> Result<(), String> {
    query.history().await?.clear().await
}

#[allow(dead_code)]
pub async fn delete_history(query: &QueryState, id: &str) -> Result<(), String> {
    query.history().await?.delete(id).await
}

#[allow(dead_code)]
pub async fn list_saved(query: &QueryState) -> Result<Vec<SavedQuery>, String> {
    query.history().await?.list_saved().await
}

#[allow(dead_code)]
pub async fn save_saved(query: &QueryState, request: &SaveRequest) -> Result<SavedWrite, String> {
    query.history().await?.save_named(request).await
}

#[allow(dead_code)]
pub async fn rename_saved(query: &QueryState, request: &RenameRequest) -> Result<SavedQuery, String> {
    query.history().await?.rename_saved(request).await
}

#[allow(dead_code)]
pub async fn delete_saved(query: &QueryState, id: &str) -> Result<(), String> {
    query.history().await?.delete_saved(id).await
}
