//! 网格提交。未确认或未知变更不会打开会话，也不会写库。

use crate::connections::Service;
use crate::grid::commands::{ChangeArg, CommitDto, CommitRequest, FailedDto, StepDto};
use crate::grid::{commit_mutations, Cell, Mutation, MutationKind};

pub fn require_commit(confirmed: bool) -> Result<(), String> {
    if !confirmed {
        return Err("提交网格变更需要先确认预览，未确认不会写入数据库。".to_owned());
    }
    Ok(())
}

pub async fn commit(service: &Service, request: &CommitRequest, confirmed: bool) -> Result<CommitDto, String> {
    require_commit(confirmed)?;
    let changes = map_changes(&request.changes)?;
    let session = service.catalog_session(&request.table.connection_id).ok_or_else(|| "这条连接还没有打开，没有另开新的连接。".to_owned())?;
    let report = commit_mutations(&session, &request.table.database, &request.table.schema, &request.table.table, &request.table.engine, &changes).await?;
    Ok(CommitDto {
        applied: report.applied.into_iter().map(|step| StepDto { label: step_label(&step.id), id: step.id, sql: step.sql }).collect(),
        failed: report.failed.map(|step| FailedDto { label: step_label(&step.id), id: step.id, sql: step.sql, message: step.message }),
        pending_ids: report.pending_ids,
        rolled_back: report.rolled_back,
        rule: "按顺序逐条提交，遇到错误即停止；已成功的语句不会自动撤销。".to_owned(),
    })
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
    if id.starts_with("u:") {
        format!("修改 {}", &id[2..])
    } else if id.starts_with("d:") {
        format!("删除 {}", &id[2..])
    } else {
        "新增 1 行".to_owned()
    }
}
