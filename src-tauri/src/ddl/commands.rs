use serde::{Deserialize, Serialize};

use crate::connections::commands::ConnectionsState;

use super::exec::{self, Receipt};
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
    materialized: bool,
    definition_error: Option<String>,
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
    /// 这次执行对共享事务的效果：活动事务里是 `still_active`，不是最终提交（R4）。
    effect: crate::transaction::TransactionEffect,
    /// 执行结束后的同连接共享快照；非关系型连接没有事务面时为 `None`。
    transaction: Option<crate::transaction::TransactionSnapshot>,
    /// 这次执行命中 MySQL/MariaDB 的隐式提交（R9）。
    implicit_commit: bool,
    /// 步骤只应用到当前事务、尚未提交。
    applied_not_committed: bool,
    /// 步骤已经是数据库的最终结果（没有活动事务）。
    committed: bool,
    /// 独立的事务说明（已应用未提交 / 隐式提交）；与 rule 分开以保持整句可翻译。
    transaction_note: Option<String>,
}

fn request_from(database: String, schema: String, engine: &str, action: Action) -> Result<Request, String> {
    Ok(Request { database, schema, engine: Engine::parse(engine)?, action })
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
        materialized: snapshot.materialized,
        definition_error: snapshot.definition_error.clone(),
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

fn receipt_dto(executed: crate::application::ddl::Executed) -> ReceiptDto {
    ReceiptDto {
        blocked: executed.receipt.blocked,
        rule: executed.receipt.rule,
        target: executed.receipt.target,
        rebuild: executed.receipt.rebuild,
        effect: executed.effect,
        transaction: executed.transaction,
        implicit_commit: executed.implicit_commit,
        applied_not_committed: executed.applied_not_committed,
        committed: executed.committed,
        transaction_note: executed.transaction_note,
        steps: executed
            .receipt
            .steps
            .into_iter()
            .map(|step| ReceiptStepDto { sql: step.sql, label: step.label, status: step.status, message: step.message })
            .collect(),
    }
}

pub async fn ddl_describe(state: &ConnectionsState, request: DescribeBody) -> Result<DescribeDto, String> {
    let snapshot = crate::application::ddl::describe_saved(&state.service, &request.connection_id, request.database, request.schema, &request.engine, &request.table).await?;
    Ok(describe_dto(&snapshot))
}

pub async fn ddl_preview(state: &ConnectionsState, request: PreviewBody) -> Result<PlanDto, String> {
    let loaded = request_from(request.database, request.schema, &request.engine, request.action)?;
    Ok(plan_dto(crate::application::ddl::preview_request(&state.service, &request.connection_id, &loaded).await?))
}

/// 在调用方已经取得的结构业务会话上执行，并按新开关更新连接记下的 foreign_keys。
pub(crate) async fn execute_on(
    service: &crate::connections::Service,
    connection_id: &str,
    session: &crate::connections::CatalogSession,
    request: &Request,
    confirmed_sql: &[String],
    state: crate::transaction::TransactionState,
) -> Result<Receipt, String> {
    let receipt = exec::execute_in(session, request, confirmed_sql, state).await?;
    if let Some(enabled) = exec::committed_foreign_keys(request.engine, &request.action, &receipt) {
        service.set_catalog_foreign_keys(connection_id, Some(enabled));
    }
    Ok(receipt)
}

pub async fn ddl_execute(state: &ConnectionsState, request: ExecuteBody) -> Result<ReceiptDto, String> {
    let loaded = request_from(request.database, request.schema, &request.engine, request.action)?;
    let executed = crate::application::ddl::execute_request_tracked(&state.service, &request.connection_id, &loaded, &request.confirmed_sql, true).await?;
    Ok(receipt_dto(executed))
}
