use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::connections::commands::ConnectionsState;

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

fn default_preview_bytes() -> u64 {
    1024
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
    /// 大字段预览的截断上限（字节）；缺省 1024，来自设置 preview_kb。
    #[serde(default = "default_preview_bytes")]
    pub preview_bytes: u64,
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

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueArg {
    pub column: String,
    pub value: Option<String>,
    #[serde(default)]
    pub resolved: bool,
}

#[derive(Clone, Debug, Deserialize)]
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
    /// 这些值读出时的大字段预览上限；写回前的信封判断按它比较，而不是另找一个默认值。
    #[serde(default = "default_preview_bytes")]
    pub preview_bytes: u64,
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

pub async fn grid_describe(state: &ConnectionsState, request: TableRef) -> Result<SchemaDto, String> {
    crate::application::grid::describe(&state.service, &request).await
}

pub async fn grid_page(state: &ConnectionsState, request: PageRequest) -> Result<PageDto, String> {
    crate::application::grid::page(&state.service, &request).await
}

pub async fn grid_count(state: &ConnectionsState, request: CountRequest) -> Result<CountDto, String> {
    crate::application::grid::count(&state.service, &request).await
}

pub async fn grid_commit(state: &ConnectionsState, request: CommitRequest) -> Result<CommitDto, String> {
    // 现有界面只在预览确认后才调用这条 command。新调用方走应用服务并显式传 confirmed。
    crate::application::grid::commit(&state.service, &request, true).await
}

pub async fn grid_export(state: &ConnectionsState, request: ExportRequest) -> Result<ExportDto, String> {
    crate::application::grid::export(&state.service, &request).await
}

pub async fn grid_export_cancel(id: String) -> Result<(), String> {
    super::request_grid_export_cancel(&id);
    Ok(())
}
