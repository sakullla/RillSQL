//! 网格读取、导出和提交。筛选、导出范围和确认在打开会话之前完成。
//!
//! 网格是连接级用户事务的消费者（ADR-1、ADR-4）：describe/page/count/export/commit 都从
//! `Service::query_session` 取业务会话并持有串行执行租约，所以活动事务里既能读到同连接其他标签
//! 的未提交修改，也不会让元数据连接替业务操作执行。提交回执区分“已应用到当前事务”与
//! “已经是最终结果”，并附共享事务快照。

use std::sync::Arc;

use crate::connections::transaction::{ExecutionLease, TransactionCoordinator};
use crate::connections::{CatalogSession, Service};
use crate::grid::commands::{
    ChangeArg, ColumnDto, CommitDto, CommitRequest, CountDto, CountRequest, ExportDto, ExportRequest, FailedDto, FilterArg, ForeignKeyDto, PageDto, PageRequest, RowDto, SchemaDto, SortArg, StepDto,
    TableRef,
};
use crate::grid::{
    commit_mutations_with_preview, count_rows_search, describe as describe_table, foreign_keys, query_page_search_preview, run_export, Cell, ExportMode, ExportSpec, Filter, FilterOp, ForeignKey,
    Mutation, MutationKind, Sort, TableSchema,
};
use crate::transaction::{StatementOutcome, TransactionEffect, TransactionSnapshot, TransactionState, UnknownReason};

const NOT_OPEN: &str = "这条连接还没有打开，没有另开新的连接。";

pub fn require_commit(confirmed: bool) -> Result<(), String> {
    if !confirmed {
        return Err("提交网格变更需要先确认预览，未确认不会写入数据库。".to_owned());
    }
    Ok(())
}

/// 一次网格业务操作的业务会话：共享用户事务句柄、串行执行租约与协调器。
struct Business {
    session: CatalogSession,
    /// 租约持有到这个值被丢弃为止：同连接的业务执行因此不会并发驱动同一条事务连接。
    _lease: Option<ExecutionLease>,
    coordinator: Option<Arc<TransactionCoordinator>>,
}

impl Business {
    /// 当前事务状态；非关系型连接没有事务面，按没有活动事务处理。
    fn state(&self) -> TransactionState {
        self.snapshot().map(|snapshot| snapshot.state).unwrap_or(TransactionState::Idle)
    }

    fn snapshot(&self) -> Option<TransactionSnapshot> {
        self.coordinator.as_ref().and_then(|coordinator| coordinator.snapshot())
    }

    /// 数据库拒绝的读取也要按真实事务结果登记：PostgreSQL 的事务内错误会把事务置为中止（R8）。
    /// 写入前的本地拒绝不经过这里。
    fn note_read_failure(&self, message: &str) {
        let Some(coordinator) = &self.coordinator else { return };
        if coordinator.snapshot().is_none() {
            return;
        }
        let outcome = if coordinator.error_may_have_ended_transaction(None, message) {
            // 数据库可能自行回滚了整个事务（死锁、磁盘错误）：不继续宣称事务活动（R10）。
            StatementOutcome::Unconfirmed(UnknownReason::UnrecognizedEffect)
        } else {
            StatementOutcome::Failed(coordinator.statement_failure(None, message))
        };
        coordinator.observe_statement(outcome);
    }
}

/// 打开网格业务会话并占住串行执行租约。失败时不另开连接、也不回退到元数据会话。
async fn business(service: &Service, id: &str) -> Result<Business, String> {
    let coordinator = service.transaction_coordinator(id);
    let query = service.query_session(id).await?.ok_or_else(|| NOT_OPEN.to_owned())?;
    Ok(Business { session: query.session, _lease: query.lease, coordinator })
}

pub async fn describe(service: &Service, request: &TableRef) -> Result<SchemaDto, String> {
    let _busy = service.busy_scope(&request.connection_id);
    if request.table.is_empty() {
        return Err("没有表名".to_owned());
    }
    let business = business(service, &request.connection_id).await?;
    let schema = match describe_table(&business.session, &request.database, &request.schema, &request.table, &request.engine).await {
        Ok(schema) => schema,
        Err(message) => {
            business.note_read_failure(&message);
            return Err(message);
        }
    };
    let keys = if request.include_foreign_keys {
        // 外键失败只让这一项为空，不影响表结构本身；失败的语句仍要登记事务结果。
        match foreign_keys(&business.session, &request.database, &request.schema, &request.table, &request.engine).await {
            Ok(keys) => Some(keys),
            Err(message) => {
                business.note_read_failure(&message);
                Some(Vec::new())
            }
        }
    } else {
        None
    };
    Ok(schema_dto(&schema, &request.database, keys))
}

pub async fn page(service: &Service, request: &PageRequest) -> Result<PageDto, String> {
    let _busy = service.busy_scope(&request.table.connection_id);
    let filters = filters_of(&request.filters)?;
    let expression = expression_of(&request.table.engine, request.filter_expression.as_deref())?;
    let sort = sort_of(request.sort.as_ref());
    let business = business(service, &request.table.connection_id).await?;
    let page = match query_page_search_preview(
        &business.session,
        &request.table.database,
        &request.table.schema,
        &request.table.table,
        &request.table.engine,
        &filters,
        sort.as_ref(),
        request.search.as_deref(),
        expression.as_deref(),
        request.offset,
        request.limit,
        usize::try_from(request.preview_bytes).unwrap_or(1024).max(1),
    )
    .await
    {
        Ok(page) => page,
        Err(message) => {
            business.note_read_failure(&message);
            return Err(message);
        }
    };
    Ok(page_dto(page))
}

pub async fn page_described(service: &Service, request: &PageRequest, schema: &crate::grid::TableSchema) -> Result<PageDto, String> {
    let _busy = service.busy_scope(&request.table.connection_id);
    let filters = filters_of(&request.filters)?;
    let expression = expression_of(&request.table.engine, request.filter_expression.as_deref())?;
    let sort = sort_of(request.sort.as_ref());
    let business = business(service, &request.table.connection_id).await?;
    let page = match crate::grid::query_page_with_schema(
        &business.session,
        schema,
        &filters,
        sort.as_ref(),
        request.search.as_deref(),
        expression.as_deref(),
        request.offset,
        request.limit,
        usize::try_from(request.preview_bytes).unwrap_or(1024).max(1),
    )
    .await
    {
        Ok(page) => page,
        Err(message) => {
            business.note_read_failure(&message);
            return Err(message);
        }
    };
    Ok(page_dto(page))
}

/// 直接 SQL 筛选表达式在命令边界上再走一次闸门：界面已经校验过，这里不信任调用方。
/// 空表达式就是没有条件；被拒绝时返回原因，调用方不能发出这条读取。
fn expression_of(engine: &str, expression: Option<&str>) -> Result<Option<String>, String> {
    match expression {
        None => Ok(None),
        Some(raw) => crate::grid::gate_filter_expression(engine, raw)
            .map_err(|refusal| refusal.message(crate::i18n::Locale::ZhCn)),
    }
}

fn page_dto(page: crate::grid::LivePage) -> PageDto {
    PageDto {
        truncated: page.truncated,
        sql: page.sql,
        rows: page
            .rows
            .into_iter()
            .map(|row| RowDto {
                id: row.id,
                cells: row.cells.into_iter().map(|(name, cell)| (name, match cell { Cell::Null => None, Cell::Text(text) => Some(text) })).collect(),
                located: row.located,
                locator_columns: row.locator.iter().map(|(name, _)| name.clone()).collect(),
                locator_values: row.locator.iter().map(|(_, cell)| cell_dto(cell)).collect(),
            })
            .collect(),
    }
}

pub async fn count(service: &Service, request: &CountRequest) -> Result<CountDto, String> {
    let _busy = service.busy_scope(&request.table.connection_id);
    let filters = filters_of(&request.filters)?;
    let expression = expression_of(&request.table.engine, request.filter_expression.as_deref())?;
    let business = business(service, &request.table.connection_id).await?;
    let matched = match count_rows_search(
        &business.session,
        &request.table.database,
        &request.table.schema,
        &request.table.table,
        &request.table.engine,
        &filters,
        request.search.as_deref(),
        expression.as_deref(),
    )
    .await
    {
        Ok(matched) => matched,
        Err(message) => {
            business.note_read_failure(&message);
            return Err(message);
        }
    };
    Ok(CountDto { matched })
}

pub async fn export(service: &Service, request: &ExportRequest) -> Result<ExportDto, String> {
    let mode = export_mode(&request.mode)?;
    let filters = filters_of(&request.filters)?;
    let expression = expression_of(&request.table.engine, request.filter_expression.as_deref())?;
    let sort = sort_of(request.sort.as_ref());
    let business = business(service, &request.table.connection_id).await?;
    let spec = ExportSpec {
        id: request.id.clone(),
        mode,
        database: request.table.database.clone(),
        schema: request.table.schema.clone(),
        table: request.table.table.clone(),
        engine_hint: request.table.engine.clone(),
        filters,
        sort,
        search: request.search.clone(),
        filter_expression: expression,
        limit: request.limit,
        truncated: request.truncated,
        statement: request.statement.clone(),
        acknowledged: request.acknowledged,
        path: request.path.clone(),
        delimiter: request.delimiter.clone(),
        null_token: request.null_token.clone(),
        bom: request.bom,
        header: request.header,
        format: request.format.clone(),
        fail_after_rows: None,
        cancel_after_rows: None,
    };
    let outcome = run_export(&business.session, &spec).await;
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

pub async fn commit(service: &Service, request: &CommitRequest, confirmed: bool) -> Result<CommitDto, String> {
    require_commit(confirmed)?;
    let changes = map_changes(&request.changes)?;
    let business = business(service, &request.table.connection_id).await?;
    let state = business.state();
    if !state.allows_statements() {
        return Err(commit_blocked(state));
    }
    let report = match commit_mutations_with_preview(
        &business.session,
        &request.table.database,
        &request.table.schema,
        &request.table.table,
        &request.table.engine,
        &changes,
        usize::try_from(request.preview_bytes).unwrap_or(usize::MAX),
    )
    .await
    {
        Ok(report) => report,
        Err(message) => {
            business.note_read_failure(&message);
            return Err(message);
        }
    };
    // 成功的语句先登记，再登记数据库拒绝的那一条：某条失败不会让之前已应用的语句变成没执行（R8）。
    let mut effect = TransactionEffect::Unchanged;
    if let Some(coordinator) = &business.coordinator {
        if !report.applied.is_empty() {
            effect = coordinator.observe_statement(StatementOutcome::Succeeded);
        }
        if let Some(failed) = &report.failed {
            if failed.database_error {
                let outcome = if coordinator.error_may_have_ended_transaction(None, &failed.message) {
                    // 数据库可能自行回滚了整个事务（死锁、磁盘错误）：不继续宣称事务活动（R10）。
                    StatementOutcome::Unconfirmed(UnknownReason::UnrecognizedEffect)
                } else {
                    StatementOutcome::Failed(coordinator.statement_failure(None, &failed.message))
                };
                effect = coordinator.observe_statement(outcome);
            }
        }
    }
    let snapshot = business.snapshot();
    let applied = !report.applied.is_empty();
    let state = snapshot.as_ref().map(|snapshot| snapshot.state).unwrap_or(TransactionState::Idle);
    Ok(CommitDto {
        applied: report.applied.into_iter().map(|step| StepDto { label: step_label(&step.id), id: step.id, sql: step.sql }).collect(),
        failed: report.failed.map(|step| FailedDto { label: step_label(&step.id), id: step.id, sql: step.sql, message: step.message }),
        pending_ids: report.pending_ids,
        rolled_back: report.rolled_back,
        rule: commit_rule(state),
        effect,
        transaction: snapshot,
        applied_not_committed: applied && state == TransactionState::Active,
        committed: applied && state == TransactionState::Idle,
    })
}

/// 中止或未知事务里不再发送写入：先让使用者回滚或核对，不能把新写入塞进无法确认的事务（R8、R10）。
fn commit_blocked(state: TransactionState) -> String {
    match state {
        TransactionState::Aborted => "当前事务已中止：先回滚或回滚到保存点，再应用网格变更；没有自动回滚或重放。".to_owned(),
        TransactionState::Unknown => "事务结果未知：先核对或回滚事务再应用网格变更；不会把写入塞进无法确认的事务。".to_owned(),
        TransactionState::Idle | TransactionState::Active => String::new(),
    }
}

fn commit_rule(state: TransactionState) -> String {
    match state {
        TransactionState::Active => "按顺序把变更应用到当前事务，遇到错误即停止；这些变更尚未提交，提交事务后才会成为最终结果。".to_owned(),
        TransactionState::Aborted => "当前事务已中止：已应用的变更只有回滚事务或回滚到保存点后才会撤销。".to_owned(),
        TransactionState::Unknown => "事务结果未知：已应用的变更是否保留无法确认，需独立核对；不会自动重放。".to_owned(),
        TransactionState::Idle => "按顺序逐条提交，遇到错误即停止；已成功的语句不会自动撤销。".to_owned(),
    }
}

fn export_mode(mode: &str) -> Result<ExportMode, String> {
    Ok(match mode {
        "table-loaded" => ExportMode::TableLoaded,
        "table-full" => ExportMode::TableFull,
        "query" => ExportMode::Query,
        other => return Err(format!("未知导出范围 {other}")),
    })
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

fn schema_dto(schema: &TableSchema, database: &str, foreign_keys: Option<Vec<ForeignKey>>) -> SchemaDto {
    SchemaDto {
        engine: schema.engine.as_str().to_owned(),
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
                default: column.default.clone(),
                generated: column.generated,
                choices: column.choices.clone(),
            })
            .collect(),
    }
}

fn cell_dto(cell: &Cell) -> Option<String> {
    match cell {
        Cell::Null => None,
        Cell::Text(text) => Some(text.clone()),
    }
}

fn map_changes(changes: &[ChangeArg]) -> Result<Vec<Mutation>, String> {
    changes
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
                resolved: change.sets.iter().map(|item| item.resolved).collect(),
            })
        })
        .collect()
}

fn cell_of(value: Option<String>) -> Cell {
    match value {
        Some(text) => Cell::Text(text),
        None => Cell::Null,
    }
}

fn step_label(id: &str) -> String {
    if let Some(rest) = id.strip_prefix("u:") {
        format!("修改 {rest}")
    } else if let Some(rest) = id.strip_prefix("d:") {
        format!("删除 {rest}")
    } else {
        "新增 1 行".to_owned()
    }
}
