//! 预览、确认导入、已读取/全量导出，以及 dump / migrate 的任务结果。
//! 取消优先于迟到成功；未确认的导入不调用提交。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::application::tasks::{TaskBoard, TaskOutcome, TaskToken};
use crate::application::work::{dump_database, migrate_data};
use crate::application::{cancel_dump, cancel_migrate};
use crate::connections::Service;
use crate::dropped::PREVIEW_LIMIT_BYTES;
use crate::dump::dump::{DumpProgress, DumpRequest};
use crate::grid::commands::{ChangeArg, CommitRequest, ExportRequest, TableRef, ValueArg};
use crate::migrate::migrate::{MigrateProgress, MigrateRequest};

use super::parse::{parse_csv, parse_json, required_gaps, suggest_mapping, ParsedTable, TableField};
use super::tasks::{prefer_cancel, Phase, TaskView};
use super::write::LoadedExport;
use super::xlsx::read_xlsx;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportEntry {
    Drop,
    Dialog,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelPoint {
    No,
    /// 只取消任务代次，让已经跑完的结果不能显示为成功。
    BoardOnData,
    /// 取消任务并请求 dump 停写、删除未完成文件。
    DumpOnStructure,
    /// 迁移进度出现后取消任务。
    MigrateOnProgress,
}

#[derive(Clone, Debug)]
pub struct ImportPreview {
    pub path: String,
    pub format: String,
    pub table: ParsedTable,
    pub truncated: bool,
    pub file_size: u64,
    pub read_bytes: u64,
    pub notice: String,
}

#[derive(Clone, Debug)]
pub struct ImportReport {
    pub wrote: bool,
    pub success: bool,
    pub applied: usize,
    pub message: String,
    /// 取消优先于迟到成功。调用方不能把这种结果再写成全部成功或运行中。
    pub cancelled: bool,
}

pub async fn open_import(path: &str, entry: ImportEntry) -> Result<ImportPreview, String> {
    let extension = extension_of(path);
    match entry {
        ImportEntry::Drop => preview_text(path).await,
        ImportEntry::Dialog if extension == "xls" => Err(".xls 不能从拖放预览进入，导入对话框只读取 .xlsx。".to_owned()),
        ImportEntry::Dialog if extension == "xlsx" => preview_xlsx(path).await,
        ImportEntry::Dialog => preview_text(path).await,
    }
}

pub async fn commit_import(
    service: &Service,
    table: &TableRef,
    preview: &ParsedTable,
    mapping: &BTreeMap<String, String>,
    confirmed: bool,
) -> Result<ImportReport, String> {
    write_import(service, table, preview, mapping, confirmed, None).await
}

/// 每一行写入前都看任务代次。取消后不再写后面的行。
pub async fn commit_import_guarded(
    service: &Service,
    tasks: &TaskBoard,
    token: TaskToken,
    table: &TableRef,
    preview: &ParsedTable,
    mapping: &BTreeMap<String, String>,
    confirmed: bool,
) -> Result<ImportReport, String> {
    write_import(service, table, preview, mapping, confirmed, Some((tasks, token))).await
}

async fn write_import(
    service: &Service,
    table: &TableRef,
    preview: &ParsedTable,
    mapping: &BTreeMap<String, String>,
    confirmed: bool,
    stop: Option<(&TaskBoard, TaskToken)>,
) -> Result<ImportReport, String> {
    if !confirmed {
        return Ok(ImportReport { wrote: false, success: false, applied: 0, cancelled: false, message: "未确认，没有写入目标表。".to_owned() });
    }
    let schema = crate::application::grid::describe(service, table).await?;
    if schema.kind != "table" {
        return Ok(ImportReport { wrote: false, success: false, applied: 0, cancelled: false, message: "只有数据表可以导入，不能写入视图。".to_owned() });
    }
    let fields = schema
        .columns
        .iter()
        .map(|column| TableField { name: column.name.clone(), nullable: column.nullable, auto: column.auto })
        .collect::<Vec<_>>();
    let gaps = required_gaps(&fields, mapping);
    if !gaps.is_empty() {
        return Ok(ImportReport {
            wrote: false,
            success: false,
            applied: 0,
            cancelled: false,
            message: format!("必填字段 {} 没有对应列，不能开始写入。", gaps.join("、")),
        });
    }
    let changes = import_changes(preview, mapping, &fields);
    let mut applied = 0usize;
    for change in &changes {
        if let Some((tasks, token)) = stop {
            if matches!(tasks.accept("import", token, ()), TaskOutcome::Cancelled { .. }) {
                return Ok(ImportReport {
                    wrote: applied > 0,
                    success: false,
                    applied,
                    cancelled: true,
                    message: format!("已取消。实际完成范围 {applied} 行。不是全部成功，也不是仍在运行。"),
                });
            }
        }
        let request = CommitRequest {
            table: TableRef {
                connection_id: table.connection_id.clone(),
                database: table.database.clone(),
                schema: table.schema.clone(),
                table: table.table.clone(),
                engine: table.engine.clone(),
                include_foreign_keys: table.include_foreign_keys,
            },
            changes: vec![change.clone()],
        };
        let report = crate::application::grid::commit(service, &request, true).await?;
        applied += report.applied.len();
        if let Some(failed) = report.failed {
            let row = failed.id.trim_start_matches("import-");
            let waiting = changes.iter().filter(|item| item.id != failed.id).count().saturating_sub(applied);
            return Ok(ImportReport {
                wrote: applied > 0,
                success: false,
                applied,
                cancelled: false,
                message: format!("第 {row} 行失败：{}。已写入 {applied} 行，还有 {waiting} 行没有写入。导入不是全部成功。", failed.message),
            });
        }
    }
    let success = applied == preview.rows.len();
    let message = if success {
        format!("已写入 {applied} 行。")
    } else {
        format!("已写入 {applied} 行，但不是全部 {} 行。导入不是全部成功。", preview.rows.len())
    };
    Ok(ImportReport { wrote: applied > 0, success, applied, cancelled: false, message })
}

pub fn mapping_for(schema_fields: &[TableField], preview: &ParsedTable) -> BTreeMap<String, String> {
    suggest_mapping(schema_fields, &preview.columns)
}

pub fn export_loaded(spec: &LoadedExport) -> TaskView {
    spec.write()
}

pub async fn export_full(service: &Service, tasks: &TaskBoard, request: &ExportRequest, cancel_late: bool) -> Result<TaskView, String> {
    let token = tasks.begin("export");
    let dto = crate::application::grid::export(service, request).await?;
    let written = dto.written_rows;
    let status = dto.status.clone();
    let path = dto.path.clone();
    let partial = dto.partial;
    let message = dto.message.clone();
    let succeeded = dto.success && status == "done";
    if cancel_late {
        tasks.cancel("export", token.id);
    }
    let outcome = tasks.accept("export", token, dto);
    let task = match outcome {
        TaskOutcome::Cancelled { .. } => TaskView::finish(
            request.id.clone(),
            "export",
            Phase::Cancelled,
            written,
            format!("已取消。实际完成范围 {written} 行。原先状态 {status}：{message}"),
            None,
        ),
        TaskOutcome::Stale { .. } => TaskView::finish(request.id.clone(), "export", Phase::Failed, written, "这次结果已经过期，没有写回界面。", None),
        TaskOutcome::Current { value, .. } => {
            let phase = if value.status == "cancelled" {
                Phase::Cancelled
            } else if value.status == "blocked" {
                Phase::Blocked
            } else if !value.success || value.partial {
                Phase::Partial
            } else {
                Phase::Done
            };
            let _ = succeeded;
            TaskView::finish(request.id.clone(), "export", phase, value.written_rows, value.message, if phase == Phase::Done { value.path } else { path.filter(|_| phase == Phase::Partial || phase == Phase::Done) })
        }
    };
    let task = if partial && task.phase == Phase::Done {
        TaskView::finish(request.id.clone(), "export", Phase::Partial, task.done_rows, format!("{} 这不是完整结果。", task.detail), task.path)
    } else {
        task
    };
    Ok(if cancel_late { prefer_cancel(task, true) } else { task })
}

pub async fn watch_dump(service: &Service, tasks: &TaskBoard, request: &DumpRequest, stop: CancelPoint) -> Result<TaskView, String> {
    let token = tasks.begin("dump");
    let snap = Arc::new(Mutex::new(Snap::default()));
    let outcome = dump_database(service, tasks, "dump", token, request, &|event: DumpProgress| {
        let mut snap = snap.lock().expect("dump progress");
        if event.phase == "ddl" {
            snap.objects += 1;
        }
        if event.phase == "data" {
            snap.rows = event.rows;
        }
        snap.object = event.object.clone();
        let rows = snap.rows;
        let objects = snap.objects;
        drop(snap);
        match stop {
            CancelPoint::BoardOnData if event.phase == "data" && rows > 0 => {
                tasks.cancel("dump", token.id);
            }
            CancelPoint::DumpOnStructure if event.phase == "ddl" && objects > 0 => {
                cancel_dump(tasks, "dump", &request.id);
            }
            _ => {}
        }
        let _ = rows;
    })
    .await?;
    let snap = snap.lock().expect("dump snap").clone();
    Ok(finish_dump(request, token, outcome, &snap))
}

pub async fn watch_migrate(service: &Service, tasks: &TaskBoard, request: &MigrateRequest, stop: CancelPoint) -> Result<TaskView, String> {
    let token = tasks.begin("migrate");
    let snap = Arc::new(Mutex::new(Snap::default()));
    let outcome = migrate_data(service, tasks, "migrate", token, request, &|event: MigrateProgress| {
        let mut snap = snap.lock().expect("migrate progress");
        snap.rows = event.rows;
        snap.inserted = event.inserted;
        snap.skipped = event.skipped;
        snap.failed = event.failed;
        drop(snap);
        if stop == CancelPoint::MigrateOnProgress && event.rows > 0 {
            cancel_migrate(tasks, "migrate", &request.id);
        }
    })
    .await?;
    let snap = snap.lock().expect("migrate snap").clone();
    Ok(finish_migrate(request, outcome, &snap))
}

#[derive(Clone, Debug, Default)]
struct Snap {
    rows: u64,
    inserted: u64,
    skipped: u64,
    failed: u64,
    objects: u64,
    object: String,
}

fn finish_dump(request: &DumpRequest, token: TaskToken, outcome: TaskOutcome<crate::dump::dump::DumpDto>, snap: &Snap) -> TaskView {
    match outcome {
        TaskOutcome::Cancelled { .. } => TaskView::finish(
            request.id.clone(),
            "dump",
            Phase::Cancelled,
            snap.rows,
            format!("已取消。实际完成范围 {} 个对象、{} 行（{}）。不是全部成功，也不是仍在运行。", snap.objects, snap.rows, snap.object),
            None,
        ),
        TaskOutcome::Stale { .. } => TaskView::finish(request.id.clone(), "dump", Phase::Failed, snap.rows, "这次结果已经过期，没有写回界面。", None),
        TaskOutcome::Current { value, .. } => {
            let _ = token;
            let phase = if value.status == "cancelled" {
                Phase::Cancelled
            } else if !value.success {
                Phase::Failed
            } else {
                Phase::Done
            };
            TaskView::finish(
                request.id.clone(),
                "dump",
                phase,
                value.rows,
                value.message,
                if phase == Phase::Done { value.path } else { value.residual_path },
            )
        }
    }
}

fn finish_migrate(request: &MigrateRequest, outcome: TaskOutcome<crate::migrate::migrate::MigrateDto>, snap: &Snap) -> TaskView {
    match outcome {
        TaskOutcome::Cancelled { .. } => {
            let rows = snap.rows.max(snap.inserted + snap.skipped + snap.failed);
            TaskView::finish(
                request.id.clone(),
                "migrate",
                Phase::Cancelled,
                rows,
                format!("已取消。实际完成范围 {rows} 行（插入 {}、跳过 {}、失败 {}）。不是全部成功，也不是仍在运行。", snap.inserted, snap.skipped, snap.failed),
                None,
            )
        }
        TaskOutcome::Stale { .. } => TaskView::finish(request.id.clone(), "migrate", Phase::Failed, snap.rows, "这次结果已经过期，没有写回界面。", None),
        TaskOutcome::Current { value, .. } => {
            let rows = value.inserted + value.skipped + value.failed;
            let phase = if value.status == "cancelled" {
                Phase::Cancelled
            } else if value.status == "blocked" {
                Phase::Blocked
            } else if value.failed > 0 || value.skipped > 0 || !value.success {
                Phase::Partial
            } else {
                Phase::Done
            };
            let detail = if phase == Phase::Partial {
                format!("{} 不是全部成功。", value.message)
            } else {
                value.message
            };
            TaskView::finish(request.id.clone(), "migrate", phase, rows, detail, None)
        }
    }
}

async fn preview_text(path: &str) -> Result<ImportPreview, String> {
    let preview = crate::application::files::read_dropped_preview(path.to_owned(), None).await?;
    let table = match preview.format.as_str() {
        "json" => parse_json(&preview.text)?,
        _ => parse_csv(&preview.text, ',')?,
    };
    let notice = if preview.truncated {
        "预览已按 2 MB 上限截断，确认后也只写入已读取的行。".to_owned()
    } else {
        "预览阶段不写入任何数据".to_owned()
    };
    Ok(ImportPreview {
        path: preview.path,
        format: preview.format,
        table,
        truncated: preview.truncated,
        file_size: preview.file_size,
        read_bytes: preview.read_bytes,
        notice,
    })
}

async fn preview_xlsx(path: &str) -> Result<ImportPreview, String> {
    let target = Path::new(path);
    if !target.is_absolute() {
        return Err("拖放的路径不完整，无法读取。".to_owned());
    }
    let file = tokio::fs::File::open(target).await.map_err(|error| format!("无法读取拖放的文件：{error}。"))?;
    let file_size = file.metadata().await.map_err(|error| format!("无法读取拖放的文件信息：{error}。"))?.len();
    if file_size > PREVIEW_LIMIT_BYTES {
        return Err("xlsx 预览超过 2 MB 上限，没有打开工作簿，也没有写入。".to_owned());
    }
    let bytes = tokio::fs::read(target).await.map_err(|error| format!("无法读取拖放的文件：{error}。"))?;
    let table = read_xlsx(&bytes)?;
    Ok(ImportPreview {
        path: path.to_owned(),
        format: "xlsx".to_owned(),
        table,
        truncated: false,
        file_size,
        read_bytes: file_size,
        notice: "预览阶段不写入任何数据".to_owned(),
    })
}

fn import_changes(preview: &ParsedTable, mapping: &BTreeMap<String, String>, fields: &[TableField]) -> Vec<ChangeArg> {
    preview
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| ChangeArg {
            id: format!("import-{}", index + 1),
            kind: "insert".to_owned(),
            sets: Vec::new(),
            locator: Vec::new(),
            values: fields
                .iter()
                .filter_map(|field| {
                    let source = mapping.get(&field.name).filter(|name| !name.is_empty())?;
                    let column_index = preview.columns.iter().position(|name| name == source)?;
                    Some(ValueArg { column: field.name.clone(), value: row.get(column_index).cloned().flatten(), resolved: false })
                })
                .collect(),
        })
        .collect()
}

fn extension_of(path: &str) -> String {
    Path::new(path).extension().and_then(|raw| raw.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn stage_loaded(id: &str, format: &str, path: &Path, columns: Vec<String>, rows: Vec<Vec<Option<String>>>, truncated: bool, table: &str) -> LoadedExport {
    LoadedExport {
        id: id.to_owned(),
        format: format.to_owned(),
        path: path.to_path_buf(),
        columns,
        rows,
        truncated,
        delimiter: ',',
        null_token: "NULL".to_owned(),
        bom: format != "json",
        table: table.to_owned(),
        schema: String::new(),
        cancel_after_rows: None,
        fail_after_rows: None,
    }
}


