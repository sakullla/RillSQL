use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::connections::commands::ConnectionsState;

use super::live::{self, ExportMode, ExportSpec};
use super::store::{Cell, Filter, FilterOp, Sort};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableRef {
    pub connection_id: String,
    pub database: String,
    pub schema: String,
    pub table: String,
    pub engine: String,
    /// 只有补全需要外键，默认关闭以免每次打开表都多查一次。
    #[serde(default)]
    pub include_foreign_keys: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterArg {
    pub column: String,
    pub op: String,
    pub value: Option<String>,
    #[serde(default)]
    pub second: Option<String>,
    #[serde(default)]
    pub values: Vec<String>,
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
    /// 整表关键字搜索（R3），与筛选 AND 组合；空表示没有搜索。
    #[serde(default)]
    pub search: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CountRequest {
    #[serde(flatten)]
    pub table: TableRef,
    pub filters: Vec<FilterArg>,
    /// 与 grid_page 相同的关键字，保证计数与分页读到同一组行。
    #[serde(default)]
    pub search: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueArg {
    pub column: String,
    pub value: Option<String>,
    #[serde(default)]
    pub resolved: bool,
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
    /// 整表关键字（R3）。全表远程导出携带当前网格关键字，与界面读到同一组行。
    #[serde(default)]
    pub search: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreign_keys: Option<Vec<ForeignKeyDto>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyDto {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
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
                second: filter.second.clone(),
                values: filter.values.clone(),
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
        "between" => FilterOp::Between,
        "in" => FilterOp::In,
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
        super::store::Engine::Sqlserver => "sqlserver",
        super::store::Engine::Redis => "redis",
        super::store::Engine::Mongodb => "mongodb",
    }
}

fn schema_dto(schema: &super::store::TableSchema, database: &str, foreign_keys: Option<Vec<super::live::ForeignKey>>) -> SchemaDto {
    SchemaDto {
        engine: engine_name(schema.engine).to_owned(),
        database: database.to_owned(),
        schema: schema.schema.clone(),
        name: schema.name.clone(),
        kind: schema.kind.to_owned(),
        key: schema.key.clone(),
        foreign_keys: foreign_keys.map(|list| {
            list.into_iter()
                .map(|item| ForeignKeyDto {
                    name: item.name,
                    columns: item.columns,
                    ref_schema: item.ref_schema,
                    ref_table: item.ref_table,
                    ref_columns: item.ref_columns,
                })
                .collect()
        }),
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
    let foreign_keys = if request.include_foreign_keys {
        // 外键失败只让这一项为空，不影响表结构本身。
        Some(live::foreign_keys(&session, &request.database, &request.schema, &request.table, &request.engine).await.unwrap_or_default())
    } else {
        None
    };
    Ok(schema_dto(&schema, &request.database, foreign_keys))
}

#[tauri::command]
pub async fn grid_page(state: tauri::State<'_, ConnectionsState>, request: PageRequest) -> Result<PageDto, String> {
    let session = require_session(&state, &request.table.connection_id)?;
    let filters = filters_of(&request.filters)?;
    let sort = sort_of(request.sort.as_ref());
    let page = live::query_page_search(
        &session,
        &request.table.database,
        &request.table.schema,
        &request.table.table,
        &request.table.engine,
        &filters,
        sort.as_ref(),
        request.search.as_deref(),
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
    let matched = live::count_rows_search(&session, &request.table.database, &request.table.schema, &request.table.table, &request.table.engine, &filters, request.search.as_deref()).await?;
    Ok(CountDto { matched })
}

#[tauri::command]
pub async fn grid_commit(state: tauri::State<'_, ConnectionsState>, request: CommitRequest) -> Result<CommitDto, String> {
    // 现有界面只在预览确认后才调用这条 command。新调用方走应用服务并显式传 confirmed。
    crate::application::grid::commit(&state.service, &request, true).await
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
        search: request.search.clone(),
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
