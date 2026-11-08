//! 连接、历史和状态写入都离开 GPUI 主线程。结果带任务代次，过期的不写回。

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use tokio::sync::oneshot;

use crate::app::views::connections::{self, Secrets};
use crate::app::views::history::{self, HistoryPage};
use crate::application::tasks::TaskToken;
use crate::application::{AppServices, TaskOutcome};
use crate::connections::model::ConnectionDraft;
use crate::query::{HistoryEntry, SaveRequest, SavedQuery};
use crate::state::AppState;

use super::model::{PendingJob, WorkbenchModel};

#[derive(Clone, Debug)]
pub struct JobRequest {
    pub job: PendingJob,
    pub token: TaskToken,
    pub draft: Option<ConnectionDraft>,
    pub secrets: Secrets,
    pub selected: Option<String>,
    pub export_path: Option<String>,
    pub import_path: Option<String>,
    pub bundle_passphrase: Option<String>,
    pub save: Option<SaveRequest>,
    pub run_id: Option<String>,
    pub saved_id: Option<String>,
}

pub enum JobDone {
    Rows(TaskToken, Result<Vec<crate::connections::ConnectionRecord>, String>),
    /// 保存成功：新列表和刚保存的连接 id，界面据此选中它。
    Saved(TaskToken, String, Result<Vec<crate::connections::ConnectionRecord>, String>),
    Test(TaskToken, Result<crate::connections::stages::TestReport, String>),
    Open(TaskToken, Result<crate::connections::OpenOutcome, String>),
    Disconnect(TaskToken, String),
    Notice(TaskToken, Result<String, String>),
    Imported(TaskToken, Result<(Vec<crate::connections::ConnectionRecord>, usize, Vec<String>), String>),
    History(TaskToken, Result<(Vec<HistoryEntry>, Vec<SavedQuery>), String>),
}

pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio"))
}

pub fn job_request(model: &WorkbenchModel, job: PendingJob, token: TaskToken) -> JobRequest {
    let save = match job {
        PendingJob::SaveNamed => save_request(model),
        _ => None,
    };
    JobRequest {
        job,
        token,
        draft: model.connections.editor.clone(),
        secrets: model.connections.secrets.clone(),
        selected: model.connections.selected.clone(),
        export_path: model.export_path.clone(),
        import_path: model.import_path.clone(),
        bundle_passphrase: {
            let trimmed = model.bundle_passphrase.trim();
            if trimmed.is_empty() { None } else { Some(trimmed.to_owned()) }
        },
        save,
        run_id: model.history.selected_run.clone(),
        saved_id: model.history.selected_saved.clone(),
    }
}

fn save_request(model: &WorkbenchModel) -> Option<SaveRequest> {
    let tab = model.active_tab()?;
    let name = model.history.save_name.trim();
    if name.is_empty() || tab.draft.trim().is_empty() {
        return None;
    }
    Some(SaveRequest {
        name: name.to_owned(),
        sql: tab.draft.clone(),
        connection_id: tab.connection_id.clone().unwrap_or_default(),
        connection_name: tab.connection_name.clone().unwrap_or_default(),
        database_name: tab.database.clone().unwrap_or_default(),
        engine: tab.engine.clone().unwrap_or_default(),
    })
}

pub fn spawn_job(services: Arc<AppServices>, request: JobRequest) -> oneshot::Receiver<JobDone> {
    let (tx, rx) = oneshot::channel();
    runtime().spawn(async move {
        let done = execute_job(&services, request).await;
        let _ = tx.send(done);
    });
    rx
}

pub async fn execute_job(services: &AppServices, request: JobRequest) -> JobDone {
    let token = request.token;
    match request.job {
        PendingJob::RefreshConnections => JobDone::Rows(token, connections::list_rows(services).await),
        PendingJob::SaveConnection => {
            let Some(draft) = request.draft else {
                return JobDone::Notice(token, Err("connections.editor.newTitle".into()));
            };
            match connections::save_draft(services, draft, &request.secrets).await {
                Ok(outcome) => JobDone::Saved(token, outcome.connection.id, connections::list_rows(services).await),
                Err(err) => JobDone::Notice(token, Err(err)),
            }
        }
        PendingJob::DeleteConnection => {
            let Some(id) = request.selected else {
                return JobDone::Notice(token, Err("connections.page.menuDelete".into()));
            };
            match connections::delete_row(services, &id).await {
                Ok(()) => JobDone::Rows(token, connections::list_rows(services).await),
                Err(err) => JobDone::Notice(token, Err(err)),
            }
        }
        PendingJob::TestConnection => {
            let Some(id) = request.selected else {
                return JobDone::Notice(token, Err("connections.editor.test.button".into()));
            };
            JobDone::Test(token, connections::test_row(services, &id, &request.secrets).await)
        }
        PendingJob::OpenConnection => {
            let Some(id) = request.selected else {
                return JobDone::Notice(token, Err("connections.page.connect".into()));
            };
            JobDone::Open(token, connections::open_row(services, &id, &request.secrets).await)
        }
        PendingJob::Disconnect => {
            let Some(id) = request.selected else {
                return JobDone::Notice(token, Err("connections.page.menuDisconnect".into()));
            };
            connections::disconnect_row(services, &id);
            JobDone::Disconnect(token, id)
        }
        PendingJob::ExportConnections => {
            let Some(path) = request.export_path else {
                return JobDone::Notice(token, Err("connections.page.exportButton".into()));
            };
            match connections::export_rows(services, None, request.bundle_passphrase.as_deref(), PathBuf::from(path).as_path()).await {
                Ok(count) => JobDone::Notice(token, Ok(count.to_string())),
                Err(err) => JobDone::Notice(token, Err(err)),
            }
        }
        PendingJob::ImportConnections => {
            let Some(path) = request.import_path else {
                return JobDone::Notice(token, Err("connections.page.importRillSql".into()));
            };
            match connections::import_rows(services, PathBuf::from(path).as_path(), request.bundle_passphrase.as_deref()).await {
                Ok((count, skipped)) => match connections::list_rows(services).await {
                    Ok(rows) => JobDone::Imported(token, Ok((rows, count, skipped))),
                    Err(err) => JobDone::Notice(token, Err(err)),
                },
                Err(err) => JobDone::Notice(token, Err(err)),
            }
        }
        PendingJob::RefreshHistory => JobDone::History(token, load_history(services).await),
        PendingJob::SaveNamed => {
            let Some(save) = request.save else {
                return JobDone::Notice(token, Err("history.savedPanel.needTab".into()));
            };
            match history::save_named(services, &save).await {
                Ok(_) => JobDone::History(token, load_history(services).await),
                Err(err) => JobDone::Notice(token, Err(err)),
            }
        }
        PendingJob::ClearHistory => match history::clear_runs(services).await {
            Ok(()) => JobDone::History(token, load_history(services).await),
            Err(err) => JobDone::Notice(token, Err(err)),
        },
        PendingJob::DeleteRun => {
            let Some(id) = request.run_id else {
                return JobDone::Notice(token, Err("history.page.titleRuns".into()));
            };
            match history::delete_run(services, &id).await {
                Ok(()) => JobDone::History(token, load_history(services).await),
                Err(err) => JobDone::Notice(token, Err(err)),
            }
        }
        PendingJob::DeleteSaved => {
            let Some(id) = request.saved_id else {
                return JobDone::Notice(token, Err("history.page.saved".into()));
            };
            match history::delete_saved(services, &id).await {
                Ok(()) => JobDone::History(token, load_history(services).await),
                Err(err) => JobDone::Notice(token, Err(err)),
            }
        }
    }
}

async fn load_history(services: &AppServices) -> Result<(Vec<HistoryEntry>, Vec<SavedQuery>), String> {
    let runs = history::load_runs(services).await?;
    let saved = history::load_saved(services).await?;
    Ok((runs, saved))
}

pub fn apply_done(model: &mut WorkbenchModel, services: &AppServices, owner: &str, done: JobDone) {
    match done {
        JobDone::Rows(token, Ok(rows)) => {
            if owner == "connections" {
                let _ = model.connections.accept_list(&services.tasks, token, rows);
                model.welcome.has_connections = !model.connections.rows.is_empty();
            }
        }
        JobDone::Saved(token, id, Ok(rows)) => {
            if owner == "connections" && model.connections.accept_list(&services.tasks, token, rows) {
                model.welcome.has_connections = !model.connections.rows.is_empty();
                model.connections.selected = Some(id.clone());
                if let Some(editor) = model.connections.editor.as_mut() {
                    editor.id = Some(id);
                }
                model.connections.message_key = None;
                model.notice_key = Some("common.save");
                model.notice_detail.clear();
            }
        }
        JobDone::Rows(token, Err(detail)) | JobDone::Saved(token, _, Err(detail)) | JobDone::Notice(token, Err(detail)) => {
            if owner == "connections" && model.connections.accept_notice(&services.tasks, token, detail.clone()) {
                model.notice_key = Some(static_notice(&detail));
                model.notice_detail = model.connections.message_detail.clone();
            } else if accepted(services, owner, token) {
                model.notice_key = Some(static_notice(&detail));
                model.notice_detail = detail;
            }
        }
        JobDone::Notice(token, Ok(detail)) => {
            if accepted(services, owner, token) {
                model.notice_detail = detail;
                model.notice_key = Some("common.save");
            }
        }
        JobDone::Imported(token, Ok((rows, count, skipped))) => {
            if owner == "connections" && model.connections.accept_list(&services.tasks, token, rows) {
                model.welcome.has_connections = !model.connections.rows.is_empty();
                model.import_notes = skipped.clone();
                if skipped.is_empty() {
                    model.notice_key = Some("connections.page.importRillSql");
                    model.notice_detail = count.to_string();
                } else {
                    model.notice_key = Some("connections.importReport.skippedTitle");
                    model.notice_detail = skipped.join("\n");
                }
            }
        }
        JobDone::Imported(token, Err(detail)) => {
            if owner == "connections" && model.connections.accept_notice(&services.tasks, token, detail.clone()) {
                model.notice_key = Some(static_notice(&detail));
                model.notice_detail = model.connections.message_detail.clone();
            }
        }
        JobDone::Test(token, report) => {
            let _ = model.connections.note_test(&services.tasks, token, report);
            model.notice_key = model.connections.message_key;
            model.notice_detail = model.connections.message_detail.clone();
        }
        JobDone::Open(token, outcome) => {
            let _ = model.connections.note_open(&services.tasks, token, outcome);
            model.notice_key = model.connections.message_key;
            model.notice_detail = model.connections.message_detail.clone();
        }
        JobDone::Disconnect(token, id) => {
            if accepted(services, owner, token) {
                model.connections.mark_disconnected(&id);
            }
        }
        JobDone::History(token, Ok((runs, saved))) => {
            if let TaskOutcome::Current { value, .. } = services.tasks.accept(owner, token, (runs, saved)) {
                model.history.runs = value.0;
                model.history.saved = value.1;
                model.history.message_key = Some("history.page.loadOnlySql");
            }
        }
        JobDone::History(token, Err(detail)) => {
            if accepted(services, owner, token) {
                model.notice_detail = detail;
                model.notice_key = Some("history.page.titleRuns");
            }
        }
    }
    let _ = HistoryPage::default();
}

fn accepted(services: &AppServices, owner: &str, token: TaskToken) -> bool {
    matches!(services.tasks.accept(owner, token, ()), TaskOutcome::Current { .. })
}

fn static_notice(detail: &str) -> &'static str {
    match detail {
        "history.savedPanel.needTab" => "history.savedPanel.needTab",
        "connections.page.exportButton" => "connections.page.exportButton",
        "connections.page.importRillSql" => "connections.page.importRillSql",
        "connections.editor.newTitle" => "connections.editor.newTitle",
        _ => "connections.status.error",
    }
}

pub async fn persist_revision(services: &AppServices, state: &AppState, revision: &std::sync::atomic::AtomicU64, generation: u64, gate: &tokio::sync::Mutex<()>) -> Result<(), String> {
    let _guard = gate.lock().await;
    if revision.load(std::sync::atomic::Ordering::SeqCst) != generation {
        return Ok(());
    }
    super::model::persist_state(services, state).await
}
