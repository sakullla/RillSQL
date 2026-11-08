//! 网格读取、导出和提交。筛选、导出范围和确认在打开会话之前完成。

use crate::connections::Service;
use crate::grid::commands::{
    ChangeArg, ColumnDto, CommitDto, CommitRequest, CountDto, CountRequest, ExportDto, ExportRequest, FailedDto, FilterArg, ForeignKeyDto, PageDto, PageRequest, RowDto, SchemaDto, SortArg, StepDto,
    TableRef,
};
use crate::grid::{
    commit_mutations, count_rows_search, describe as describe_table, foreign_keys, query_page_search_preview, run_export, Cell, Engine, ExportMode, ExportSpec, Filter, FilterOp, ForeignKey,
    Mutation, MutationKind, Sort, TableSchema,
};

pub fn require_commit(confirmed: bool) -> Result<(), String> {
    if !confirmed {
        return Err("提交网格变更需要先确认预览，未确认不会写入数据库。".to_owned());
    }
    Ok(())
}

pub async fn describe(service: &Service, request: &TableRef) -> Result<SchemaDto, String> {
    if request.table.is_empty() {
        return Err("没有表名".to_owned());
    }
    let session = session_of(service, &request.connection_id)?;
    let schema = describe_table(&session, &request.database, &request.schema, &request.table, &request.engine).await?;
    let keys = if request.include_foreign_keys {
        // 外键失败只让这一项为空，不影响表结构本身。
        Some(foreign_keys(&session, &request.database, &request.schema, &request.table, &request.engine).await.unwrap_or_default())
    } else {
        None
    };
    Ok(schema_dto(&schema, &request.database, keys))
}

pub async fn page(service: &Service, request: &PageRequest) -> Result<PageDto, String> {
    let filters = filters_of(&request.filters)?;
    let sort = sort_of(request.sort.as_ref());
    let session = session_of(service, &request.table.connection_id)?;
    let page = query_page_search_preview(
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
        usize::try_from(request.preview_bytes).unwrap_or(1024).max(1),
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

pub async fn count(service: &Service, request: &CountRequest) -> Result<CountDto, String> {
    let filters = filters_of(&request.filters)?;
    let session = session_of(service, &request.table.connection_id)?;
    let matched = count_rows_search(
        &session,
        &request.table.database,
        &request.table.schema,
        &request.table.table,
        &request.table.engine,
        &filters,
        request.search.as_deref(),
    )
    .await?;
    Ok(CountDto { matched })
}

pub async fn export(service: &Service, request: &ExportRequest) -> Result<ExportDto, String> {
    let mode = export_mode(&request.mode)?;
    let filters = filters_of(&request.filters)?;
    let sort = sort_of(request.sort.as_ref());
    let session = session_of(service, &request.table.connection_id)?;
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
    let outcome = run_export(&session, &spec).await;
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
    let session = session_of(service, &request.table.connection_id)?;
    let report = commit_mutations(&session, &request.table.database, &request.table.schema, &request.table.table, &request.table.engine, &changes).await?;
    Ok(CommitDto {
        applied: report.applied.into_iter().map(|step| StepDto { label: step_label(&step.id), id: step.id, sql: step.sql }).collect(),
        failed: report.failed.map(|step| FailedDto { label: step_label(&step.id), id: step.id, sql: step.sql, message: step.message }),
        pending_ids: report.pending_ids,
        rolled_back: report.rolled_back,
        rule: "按顺序逐条提交，遇到错误即停止；已成功的语句不会自动撤销。".to_owned(),
    })
}

fn session_of(service: &Service, id: &str) -> Result<crate::connections::CatalogSession, String> {
    service.catalog_session(id).ok_or_else(|| "这条连接还没有打开，没有另开新的连接。".to_owned())
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

fn engine_name(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "postgres",
        Engine::Mysql => "mysql",
        Engine::Mariadb => "mariadb",
        Engine::Sqlite => "sqlite",
        Engine::Sqlserver => "sqlserver",
        Engine::Redis => "redis",
        Engine::Mongodb => "mongodb",
    }
}

fn schema_dto(schema: &TableSchema, database: &str, foreign_keys: Option<Vec<ForeignKey>>) -> SchemaDto {
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
