use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::connections::commands::ConnectionsState;

use super::live::{self, ExportMode, ExportSpec, Mutation, MutationKind};
use super::store::{Cell, Filter, FilterOp, Sort};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableRef {
    pub connection_id: String,
    pub database: String,
    pub schema: String,
    pub table: String,
    pub engine: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterArg {
    pub column: String,
    pub op: String,
    pub value: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SortArg {
    pub column: String,
    pub direction: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageRequest {
    #[serde(flatten)]
    pub table: TableRef,
    pub filters: Vec<FilterArg>,
    pub sort: Option<SortArg>,
    pub offset: u64,
    pub limit: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CountRequest {
    #[serde(flatten)]
    pub table: TableRef,
    pub filters: Vec<FilterArg>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueArg {
    pub column: String,
    pub value: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeArg {
    pub id: String,
    pub kind: String,
    pub sets: Vec<ValueArg>,
    pub locator: Vec<ValueArg>,
    pub values: Vec<ValueArg>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitRequest {
    #[serde(flatten)]
    pub table: TableRef,
    pub changes: Vec<ChangeArg>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    pub id: String,
    pub mode: String,
    #[serde(flatten)]
    pub table: TableRef,
    pub filters: Vec<FilterArg>,
    pub sort: Option<SortArg>,
    pub limit: u64,
    pub truncated: bool,
    pub statement: String,
    pub acknowledged: bool,
    pub path: String,
    pub delimiter: String,
    pub null_token: String,
    pub bom: bool,
    pub header: bool,
    #[serde(default)]
    pub format: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDto {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub primary: bool,
    pub unique: bool,
    pub auto: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaDto {
    pub engine: String,
    pub database: String,
    pub schema: String,
    pub name: String,
    pub kind: String,
    pub columns: Vec<ColumnDto>,
    pub key: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowDto {
    pub id: String,
    pub cells: BTreeMap<String, Option<String>>,
    pub located: bool,
    pub locator_columns: Vec<String>,
    pub locator_values: Vec<Option<String>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageDto {
    pub rows: Vec<RowDto>,
    pub truncated: bool,
    pub sql: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CountDto {
    pub matched: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepDto {
    pub id: String,
    pub sql: String,
    pub label: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedDto {
    pub id: String,
    pub sql: String,
    pub label: String,
    pub message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitDto {
    pub applied: Vec<StepDto>,
    pub failed: Option<FailedDto>,
    pub pending_ids: Vec<String>,
    pub rolled_back: bool,
    pub rule: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportDto {
    pub status: String,
    pub success: bool,
    pub written_rows: u64,
    pub path: Option<String>,
    pub residual_path: Option<String>,
    pub message: String,
    pub partial: bool,
}

fn cell_of(value: Option<String>) -> Cell {
    match value {
        Some(text) => Cell::Text(text),
        None => Cell::Null,
    }
}

fn cell_dto(cell: &Cell) -> Option<String> {
    match cell {
        Cell::Null => None,
        Cell::Text(text) => Some(text.clone()),
    }
}

fn filters_of(filters: &[FilterArg]) -> Result<Vec<Filter>, String> {
    filters
        .iter()
        .map(|filter| {
            Ok(Filter {
                column: filter.column.clone(),
                op: parse_op(&filter.op)?,
                value: filter.value.clone(),
            })
        })
        .collect()
}

fn parse_op(op: &str) -> Result<FilterOp, String> {
    Ok(match op {
        "eq" => FilterOp::Eq,
        "ne" => FilterOp::Ne,
        "contains" => FilterOp::Contains,
        "starts" => FilterOp::Starts,
        "gt" => FilterOp::Gt,
        "lt" => FilterOp::Lt,
        "null" => FilterOp::Null,
        "notnull" => FilterOp::NotNull,
        "empty" => FilterOp::Empty,
        other => return Err(format!("未知筛选 {other}")),
    })
}

fn sort_of(sort: Option<&SortArg>) -> Option<Sort> {
    sort.map(|sort| Sort { column: sort.column.clone(), descending: sort.direction == "desc" })
}

fn engine_name(engine: super::store::Engine) -> &'static str {
    match engine {
        super::store::Engine::Postgres => "postgres",
        super::store::Engine::Mysql => "mysql",
        super::store::Engine::Mariadb => "mariadb",
        super::store::Engine::Sqlite => "sqlite",
    }
}

fn schema_dto(schema: &super::store::TableSchema, database: &str) -> SchemaDto {
    SchemaDto {
        engine: engine_name(schema.engine).to_owned(),
        database: database.to_owned(),
        schema: schema.schema.clone(),
        name: schema.name.clone(),
        kind: schema.kind.to_owned(),
        key: schema.key.clone(),
        columns: schema
            .columns
            .iter()
            .map(|column| ColumnDto {
                name: column.name.clone(),
                type_name: column.type_name.clone(),
                nullable: column.nullable,
                primary: column.primary,
                unique: column.unique,
                auto: column.auto,
            })
            .collect(),
    }
}

fn require_session(state: &ConnectionsState, id: &str) -> Result<crate::connections::CatalogSession, String> {
    state.service.catalog_session(id).ok_or_else(|| "这条连接还没有打开，没有另开新的连接。".to_owned())
}

#[tauri::command]
pub async fn grid_describe(state: tauri::State<'_, ConnectionsState>, request: TableRef) -> Result<SchemaDto, String> {
    let session = require_session(&state, &request.connection_id)?;
    let schema = live::describe(&session, &request.database, &request.schema, &request.table, &request.engine).await?;
    Ok(schema_dto(&schema, &request.database))
}

#[tauri::command]
pub async fn grid_page(state: tauri::State<'_, ConnectionsState>, request: PageRequest) -> Result<PageDto, String> {
    let session = require_session(&state, &request.table.connection_id)?;
    let filters = filters_of(&request.filters)?;
    let sort = sort_of(request.sort.as_ref());
    let page = live::query_page(
        &session,
        &request.table.database,
        &request.table.schema,
        &request.table.table,
        &request.table.engine,
        &filters,
        sort.as_ref(),
        request.offset,
        request.limit,
    )
    .await?;
    Ok(PageDto {
        truncated: page.truncated,
        sql: page.sql,
        rows: page
            .rows
            .into_iter()
            .map(|row| RowDto {
                id: row.id,
                cells: row.cells.iter().map(|(name, cell)| (name.clone(), cell_dto(cell))).collect(),
                located: row.located,
                locator_columns: row.locator.iter().map(|(name, _)| name.clone()).collect(),
                locator_values: row.locator.iter().map(|(_, cell)| cell_dto(cell)).collect(),
            })
            .collect(),
    })
}

#[tauri::command]
pub async fn grid_count(state: tauri::State<'_, ConnectionsState>, request: CountRequest) -> Result<CountDto, String> {
    let session = require_session(&state, &request.table.connection_id)?;
    let filters = filters_of(&request.filters)?;
    let matched = live::count_rows(&session, &request.table.database, &request.table.schema, &request.table.table, &request.table.engine, &filters).await?;
    Ok(CountDto { matched })
}

#[tauri::command]
pub async fn grid_commit(state: tauri::State<'_, ConnectionsState>, request: CommitRequest) -> Result<CommitDto, String> {
    let session = require_session(&state, &request.table.connection_id)?;
    let changes = request
        .changes
        .iter()
        .map(|change| {
            let kind = match change.kind.as_str() {
                "update" => MutationKind::Update,
                "insert" => MutationKind::Insert,
                "delete" => MutationKind::Delete,
                other => return Err(format!("未知变更 {other}")),
            };
            Ok(Mutation {
                id: change.id.clone(),
                kind,
                sets: change.sets.iter().map(|item| (item.column.clone(), cell_of(item.value.clone()))).collect(),
                locator: change.locator.iter().map(|item| (item.column.clone(), cell_of(item.value.clone()))).collect(),
                values: change.values.iter().map(|item| (item.column.clone(), cell_of(item.value.clone()))).collect(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let report = live::commit(&session, &request.table.database, &request.table.schema, &request.table.table, &request.table.engine, &changes).await?;
    Ok(CommitDto {
        applied: report
            .applied
            .into_iter()
            .map(|step| StepDto { label: step_label(&step.id), id: step.id, sql: step.sql })
            .collect(),
        failed: report.failed.map(|step| FailedDto {
            label: step_label(&step.id),
            id: step.id,
            sql: step.sql,
            message: step.message,
        }),
        pending_ids: report.pending_ids,
        rolled_back: report.rolled_back,
        rule: "按顺序逐条提交，遇到错误即停止；已成功的语句不会自动撤销。".to_owned(),
    })
}

fn step_label(id: &str) -> String {
    if id.starts_with("u:") {
        format!("修改 {}", &id[2..])
    } else if id.starts_with("d:") {
        format!("删除 {}", &id[2..])
    } else {
        "新增 1 行".to_owned()
    }
}

#[tauri::command]
pub async fn grid_export(state: tauri::State<'_, ConnectionsState>, request: ExportRequest) -> Result<ExportDto, String> {
    let session = require_session(&state, &request.table.connection_id)?;
    let mode = match request.mode.as_str() {
        "table-loaded" => ExportMode::TableLoaded,
        "table-full" => ExportMode::TableFull,
        "query" => ExportMode::Query,
        other => return Err(format!("未知导出范围 {other}")),
    };
    let spec = ExportSpec {
        id: request.id,
        mode,
        database: request.table.database,
        schema: request.table.schema,
        table: request.table.table,
        engine_hint: request.table.engine,
        filters: filters_of(&request.filters)?,
        sort: sort_of(request.sort.as_ref()),
        limit: request.limit,
        truncated: request.truncated,
        statement: request.statement,
        acknowledged: request.acknowledged,
        path: request.path,
        delimiter: request.delimiter,
        null_token: request.null_token,
        bom: request.bom,
        header: request.header,
        format: request.format,
        fail_after_rows: None,
        cancel_after_rows: None,
    };
    let outcome = live::run_export(&session, &spec).await;
    Ok(ExportDto {
        status: outcome.status.to_owned(),
        success: outcome.success,
        written_rows: outcome.written_rows,
        path: outcome.path,
        residual_path: outcome.residual_path,
        message: outcome.message,
        partial: outcome.partial,
    })
}

#[tauri::command]
pub async fn grid_export_cancel(id: String) -> Result<(), String> {
    live::request_cancel(&id);
    Ok(())
}
