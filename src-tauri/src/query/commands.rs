//! 查询命令：在已打开的会话上执行、取消，并把已发起的语句写入本机历史。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::connections::commands::ConnectionsState;
use crate::connections::{CancelTransport, CatalogSession, Service};

use super::cancel::{self, CancelView};
use super::exec::{self, LiveDriver, StatementReport};
use super::history::{HistoryDraft, HistoryEntry, HistoryStore};
use super::split::split_sql;

#[derive(Clone)]
pub struct QueryState {
    runs: Arc<Mutex<HashMap<String, Arc<RunSlot>>>>,
    history_path: PathBuf,
    history: Arc<tokio::sync::Mutex<Option<HistoryStore>>>,
}

struct RunSlot {
    snapshot: Mutex<RunDto>,
    cancel_flag: Arc<AtomicBool>,
    started: Instant,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunDto {
    pub id: String,
    pub connection_id: String,
    pub connection_name: String,
    pub database: String,
    pub engine: String,
    pub status: String,
    pub cancel: String,
    pub cancel_message: String,
    pub via_ssh: bool,
    pub fresh_channel: bool,
    pub statements: Vec<StatementDto>,
    pub started_at: String,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementDto {
    pub sql: String,
    pub status: String,
    pub message: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub truncated: bool,
    pub affected: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRequest {
    pub connection_id: String,
    pub connection_name: String,
    pub database: String,
    pub engine: String,
    pub sql: String,
    pub limit: u64,
}

impl QueryState {
    pub fn new(history_path: PathBuf) -> Self {
        Self {
            runs: Arc::new(Mutex::new(HashMap::new())),
            history_path,
            history: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    pub fn from_config_path(config: &Path) -> Self {
        let history = config.parent().unwrap_or_else(|| Path::new(".")).join("query-history.sqlite");
        Self::new(history)
    }

    pub async fn history(&self) -> Result<HistoryStore, String> {
        let mut guard = self.history.lock().await;
        if guard.is_none() {
            *guard = Some(HistoryStore::open(&self.history_path).await?);
        }
        Ok(guard.as_ref().expect("历史刚刚打开").clone())
    }
}

pub async fn start_run(service: &Arc<Service>, state: &QueryState, request: StartRequest) -> Result<RunDto, String> {
    let parts = split_sql(&request.sql);
    if parts.is_empty() {
        return Err("没有可执行的 SQL".to_owned());
    }
    let session = service
        .catalog_session(&request.connection_id)
        .ok_or_else(|| "连接未打开，没有另开数据库连接。".to_owned())?;
    if matches!(session, CatalogSession::Scripted) {
        return Err("这条已打开的会话没有可查询的数据库通道".to_owned());
    }
    if connection_busy(state, &request.connection_id) {
        return Err("这个连接已有正在执行的查询。可以切换到其他标签继续编辑。".to_owned());
    }
    let transport = service.cancel_transport(&request.connection_id).unwrap_or(CancelTransport::None);
    exec::align_database(&session, &request.engine, &request.database).await?;
    let id = uuid::Uuid::new_v4().to_string();
    let started_at = chrono::Utc::now().to_rfc3339();
    let cancel_flag = transport.sqlite_flag().unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
    cancel_flag.store(false, Ordering::SeqCst);
    let statements = parts
        .iter()
        .map(|piece| StatementDto {
            sql: piece.text.clone(),
            status: "pending".to_owned(),
            message: String::new(),
            columns: Vec::new(),
            rows: Vec::new(),
            truncated: false,
            affected: None,
        })
        .collect::<Vec<_>>();
    let snapshot = RunDto {
        id: id.clone(),
        connection_id: request.connection_id.clone(),
        connection_name: request.connection_name.clone(),
        database: request.database.clone(),
        engine: request.engine.clone(),
        status: "running".to_owned(),
        cancel: "none".to_owned(),
        cancel_message: String::new(),
        via_ssh: transport.via_ssh(),
        fresh_channel: false,
        statements,
        started_at: started_at.clone(),
        elapsed_ms: 0,
    };
    let slot = Arc::new(RunSlot {
        snapshot: Mutex::new(snapshot.clone()),
        cancel_flag: Arc::clone(&cancel_flag),
        started: Instant::now(),
    });
    state.runs.lock().expect("runs").insert(id.clone(), Arc::clone(&slot));
    let history = state.history().await?;
    history
        .insert(&HistoryDraft {
            id: id.clone(),
            sql: request.sql.clone(),
            connection_id: request.connection_id.clone(),
            connection_name: request.connection_name.clone(),
            database_name: request.database.clone(),
            engine: request.engine.clone(),
            started_at: started_at.clone(),
            summary: "执行中".to_owned(),
        })
        .await?;
    let limit = exec::clamp_limit(request.limit);
    let via_ssh = transport.via_ssh();
    let state_runs = Arc::clone(&state.runs);
    let history_state = state.clone();
    tokio::spawn(async move {
        let driver = LiveDriver { session, transport };
        let batch = exec::run_batch(&parts, limit, &cancel_flag, via_ssh, &driver).await;
        finish_run(&slot, &batch);
        let summary = summary_of(&batch);
        let status = history_status(&batch.status);
        if let Ok(store) = history_state.history().await {
            let _ = store
                .finish(&id, &chrono::Utc::now().to_rfc3339(), status, slot.started.elapsed().as_millis() as i64, &summary)
                .await;
        }
        let _ = state_runs;
    });
    Ok(snapshot)
}

fn finish_run(slot: &RunSlot, batch: &exec::BatchReport) {
    let mut snap = slot.snapshot.lock().expect("snapshot");
    if batch.cancel.phase != "none" {
        snap.cancel = batch.cancel.phase.to_owned();
        snap.cancel_message = batch.cancel.message.clone();
        snap.fresh_channel = batch.cancel.fresh_channel;
    }
    snap.status = batch.status.to_owned();
    snap.elapsed_ms = u64::try_from(slot.started.elapsed().as_millis()).unwrap_or(u64::MAX);
    snap.statements = batch.statements.iter().map(statement_dto).collect();
}

fn statement_dto(statement: &StatementReport) -> StatementDto {
    StatementDto {
        sql: statement.sql.clone(),
        status: statement.status.to_owned(),
        message: statement.message.clone(),
        columns: statement.columns.clone(),
        rows: statement.rows.clone(),
        truncated: statement.truncated,
        affected: statement.affected,
    }
}

fn summary_of(batch: &exec::BatchReport) -> String {
    match batch.status {
        "done" => format!("{} 条语句完成", batch.statements.iter().filter(|item| item.status == "ok").count()),
        "error" => batch
            .statements
            .iter()
            .find(|item| item.status == "error")
            .map(|item| item.message.clone())
            .unwrap_or_else(|| "执行失败".to_owned()),
        "unknown" => "连接中断，结果未知".to_owned(),
        "cancelled" => "数据库已确认取消".to_owned(),
        _ => "执行结束".to_owned(),
    }
}

fn history_status(status: &str) -> &'static str {
    match status {
        "done" => "ok",
        "error" => "error",
        "unknown" => "unknown",
        "cancelled" => "cancelled",
        _ => "error",
    }
}

fn connection_busy(state: &QueryState, connection_id: &str) -> bool {
    state.runs.lock().expect("runs").values().any(|slot| {
        let snap = slot.snapshot.lock().expect("snapshot");
        snap.connection_id == connection_id && matches!(snap.status.as_str(), "running" | "cancel-requested")
    })
}

pub async fn request_cancel(service: &Arc<Service>, state: &QueryState, run_id: &str) -> Result<RunDto, String> {
    let slot = state
        .runs
        .lock()
        .expect("runs")
        .get(run_id)
        .cloned()
        .ok_or_else(|| "找不到这次执行".to_owned())?;
    let connection_id = slot.snapshot.lock().expect("snapshot").connection_id.clone();
    let status = slot.snapshot.lock().expect("snapshot").status.clone();
    if !matches!(status.as_str(), "running" | "cancel-requested") {
        return Ok(slot.snapshot.lock().expect("snapshot").clone());
    }
    let transport = service.cancel_transport(&connection_id).unwrap_or(CancelTransport::None);
    let via_ssh = transport.via_ssh();
    {
        let mut snap = slot.snapshot.lock().expect("snapshot");
        let requested = cancel::on_request(via_ssh);
        snap.status = "cancel-requested".to_owned();
        snap.cancel = requested.phase.to_owned();
        snap.cancel_message = requested.message;
        snap.via_ssh = via_ssh;
    }
    let delivery = cancel::deliver_cancel(&transport).await;
    let mut view: CancelView = cancel::on_request(via_ssh);
    cancel::on_delivery(&mut view, delivery, via_ssh);
    if view.phase == "requested" {
        slot.cancel_flag.store(true, Ordering::SeqCst);
    }
    let mut snap = slot.snapshot.lock().expect("snapshot");
    if matches!(snap.status.as_str(), "running" | "cancel-requested") {
        snap.cancel = view.phase.to_owned();
        snap.cancel_message = view.message;
        snap.fresh_channel = view.fresh_channel;
        snap.via_ssh = via_ssh;
        snap.status = if view.phase == "failed" { "running".to_owned() } else { "cancel-requested".to_owned() };
    }
    Ok(snap.clone())
}

pub fn poll_run(state: &QueryState, run_id: &str) -> Result<RunDto, String> {
    let slot = state
        .runs
        .lock()
        .expect("runs")
        .get(run_id)
        .cloned()
        .ok_or_else(|| "找不到这次执行".to_owned())?;
    let mut snap = slot.snapshot.lock().expect("snapshot").clone();
    if matches!(snap.status.as_str(), "running" | "cancel-requested") {
        snap.elapsed_ms = u64::try_from(slot.started.elapsed().as_millis()).unwrap_or(u64::MAX);
    }
    Ok(snap)
}

#[tauri::command]
pub async fn query_start(connections: tauri::State<'_, ConnectionsState>, query: tauri::State<'_, QueryState>, request: StartRequest) -> Result<RunDto, String> {
    start_run(&connections.service, &query, request).await
}

#[tauri::command]
pub async fn query_poll(query: tauri::State<'_, QueryState>, run_id: String) -> Result<RunDto, String> {
    poll_run(&query, &run_id)
}

#[tauri::command]
pub async fn query_cancel(connections: tauri::State<'_, ConnectionsState>, query: tauri::State<'_, QueryState>, run_id: String) -> Result<RunDto, String> {
    request_cancel(&connections.service, &query, &run_id).await
}

#[tauri::command]
pub async fn query_history(query: tauri::State<'_, QueryState>) -> Result<Vec<HistoryEntry>, String> {
    query.history().await?.list().await
}

#[tauri::command]
pub async fn query_history_clear(query: tauri::State<'_, QueryState>) -> Result<(), String> {
    query.history().await?.clear().await
}
