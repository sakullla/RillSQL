use serde::{Deserialize, Serialize};

use crate::connections::commands::ConnectionsState;

use super::exec::{self, Receipt};
use super::introspect::load;
use super::model::{Action, Engine, Plan, Request, Snapshot, Step, StepStatus};
use super::plan::display_sql;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DescribeBody {
    pub connection_id: String,
    pub database: String,
    pub schema: String,
    pub table: String,
    pub engine: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewBody {
    pub connection_id: String,
    pub database: String,
    pub schema: String,
    pub engine: String,
    pub action: Action,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteBody {
    pub connection_id: String,
    pub database: String,
    pub schema: String,
    pub engine: String,
    pub action: Action,
    pub confirmed_sql: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDto {
    name: String,
    type_name: String,
    nullable: bool,
    default_sql: String,
    primary: bool,
    auto: bool,
    generated: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexColumnDto {
    name: String,
    descending: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexDto {
    name: String,
    unique: bool,
    columns: Vec<IndexColumnDto>,
    protected: bool,
    reason: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyDto {
    name: String,
    columns: Vec<String>,
    ref_schema: String,
    ref_table: String,
    ref_columns: Vec<String>,
    on_delete: String,
    on_update: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingDto {
    name: String,
    table: String,
    schema: String,
    columns: Vec<String>,
    ref_columns: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DescribeDto {
    engine: String,
    database: String,
    schema: String,
    name: String,
    kind: String,
    storage: Option<String>,
    sqlite_version: String,
    foreign_keys: Option<bool>,
    recorded_foreign_keys: Option<bool>,
    row_count: Option<i64>,
    columns: Vec<ColumnDto>,
    indexes: Vec<IndexDto>,
    foreign_key_list: Vec<ForeignKeyDto>,
    incoming: Vec<IncomingDto>,
    display_sql: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskDto {
    level: String,
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepDto {
    sql: String,
    label: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDto {
    blocked: Vec<String>,
    risks: Vec<RiskDto>,
    steps: Vec<StepDto>,
    rule: String,
    rebuild: bool,
    target: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptStepDto {
    sql: String,
    label: String,
    status: StepStatus,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptDto {
    blocked: Vec<String>,
    rule: String,
    steps: Vec<ReceiptStepDto>,
    target: String,
    rebuild: bool,
}

fn request_from(database: String, schema: String, engine: &str, action: Action) -> Result<Request, String> {
    Ok(Request { database, schema, engine: Engine::parse(engine)?, action })
}

fn require_session(state: &ConnectionsState, id: &str) -> Result<crate::connections::CatalogSession, String> {
    state.service.catalog_session(id).ok_or_else(|| "这条连接还没有打开，没有另开新的连接。".to_owned())
}

fn describe_dto(snapshot: &Snapshot) -> DescribeDto {
    DescribeDto {
        engine: snapshot.engine.as_str().to_owned(),
        database: snapshot.database.clone(),
        schema: snapshot.schema.clone(),
        name: snapshot.name.clone(),
        kind: snapshot.kind.clone(),
        storage: snapshot.storage.clone(),
        sqlite_version: snapshot.sqlite_version.clone(),
        foreign_keys: snapshot.live_foreign_keys,
        recorded_foreign_keys: snapshot.recorded_foreign_keys,
        row_count: snapshot.row_count,
        columns: snapshot
            .columns
            .iter()
            .map(|column| ColumnDto {
                name: column.name.clone(),
                type_name: column.type_name.clone(),
                nullable: column.nullable,
                default_sql: column.default_sql.clone().unwrap_or_default(),
                primary: column.primary,
                auto: column.auto,
                generated: column.generated,
            })
            .collect(),
        indexes: snapshot
            .indexes
            .iter()
            .map(|index| IndexDto {
                name: index.name.clone(),
                unique: index.unique,
                protected: index.protected,
                reason: index.reason.clone(),
                columns: index.columns.iter().map(|column| IndexColumnDto { name: column.name.clone(), descending: column.descending }).collect(),
            })
            .collect(),
        foreign_key_list: snapshot.foreign_keys.iter().map(fk_dto).collect(),
        incoming: snapshot
            .incoming
            .iter()
            .map(|item| IncomingDto {
                name: item.name.clone(),
                table: item.table.clone(),
                schema: item.schema.clone(),
                columns: item.columns.clone(),
                ref_columns: item.ref_columns.clone(),
            })
            .collect(),
        display_sql: display_sql(snapshot),
    }
}

fn fk_dto(key: &super::model::ForeignKey) -> ForeignKeyDto {
    ForeignKeyDto {
        name: key.name.clone(),
        columns: key.columns.clone(),
        ref_schema: key.ref_schema.clone(),
        ref_table: key.ref_table.clone(),
        ref_columns: key.ref_columns.clone(),
        on_delete: key.on_delete.clone(),
        on_update: key.on_update.clone(),
    }
}

fn plan_dto(plan: Plan) -> PlanDto {
    PlanDto {
        blocked: plan.blocked,
        risks: plan.risks.into_iter().map(|risk| RiskDto { level: risk.level, text: risk.text }).collect(),
        steps: plan.steps.into_iter().map(step_dto).collect(),
        rule: plan.rule,
        rebuild: plan.rebuild,
        target: plan.target,
    }
}

fn step_dto(step: Step) -> StepDto {
    StepDto { sql: step.sql, label: step.label }
}

fn receipt_dto(receipt: Receipt) -> ReceiptDto {
    ReceiptDto {
        blocked: receipt.blocked,
        rule: receipt.rule,
        target: receipt.target,
        rebuild: receipt.rebuild,
        steps: receipt
            .steps
            .into_iter()
            .map(|step| ReceiptStepDto { sql: step.sql, label: step.label, status: step.status, message: step.message })
            .collect(),
    }
}

#[tauri::command]
pub async fn ddl_describe(state: tauri::State<'_, ConnectionsState>, request: DescribeBody) -> Result<DescribeDto, String> {
    let session = require_session(&state, &request.connection_id)?;
    let loaded = request_from(request.database, request.schema, &request.engine, Action::DropTable { table: request.table.clone() })?;
    let snapshot = load(&session, &loaded, &request.table).await?;
    Ok(describe_dto(&snapshot))
}

#[tauri::command]
pub async fn ddl_preview(state: tauri::State<'_, ConnectionsState>, request: PreviewBody) -> Result<PlanDto, String> {
    let session = require_session(&state, &request.connection_id)?;
    let loaded = request_from(request.database, request.schema, &request.engine, request.action)?;
    Ok(plan_dto(exec::preview(&session, &loaded).await?))
}

pub(crate) async fn execute_saved(
    service: &crate::connections::Service,
    connection_id: &str,
    request: &Request,
    confirmed_sql: &[String],
) -> Result<Receipt, String> {
    let session = service.catalog_session(connection_id).ok_or_else(|| "这条连接还没有打开，没有另开新的连接。".to_owned())?;
    let receipt = exec::execute(&session, request, confirmed_sql).await?;
    if let Some(enabled) = exec::committed_foreign_keys(&request.action, &receipt) {
        service.set_catalog_foreign_keys(connection_id, Some(enabled));
    }
    Ok(receipt)
}

#[tauri::command]
pub async fn ddl_execute(state: tauri::State<'_, ConnectionsState>, request: ExecuteBody) -> Result<ReceiptDto, String> {
    let loaded = request_from(request.database, request.schema, &request.engine, request.action)?;
    Ok(receipt_dto(execute_saved(&state.service, &request.connection_id, &loaded, &request.confirmed_sql).await?))
}
