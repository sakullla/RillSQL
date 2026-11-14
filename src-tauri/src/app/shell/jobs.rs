//! 连接、历史和状态写入都离开 GPUI 主线程。结果带任务代次，过期的不写回。

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use tokio::sync::oneshot;

use crate::app::views::connections::{self, Secrets};
use crate::app::views::history::{self, HistoryPage};
use crate::application::tasks::TaskToken;
use crate::application::{AppServices, TaskOutcome};
use crate::connections::groups::ConnectionGroup;
use crate::connections::model::ConnectionDraft;
use crate::query::{HistoryEntry, RenameRequest, SaveRequest, SavedQuery};
use crate::state::AppState;

use super::model::{GroupOp, PendingJob, WorkbenchModel};

#[derive(Clone, Debug)]
pub struct JobRequest {
    pub job: PendingJob,
    pub token: TaskToken,
    /// 发起这次连接测试的代号：过期结果不能收回更新一次的"正在测试"。
    pub generation: u64,
    pub draft: Option<ConnectionDraft>,
    pub secrets: Secrets,
    pub selected: Option<String>,
    pub export_path: Option<String>,
    pub import_path: Option<String>,
    pub bundle_passphrase: Option<String>,
    pub save: Option<SaveRequest>,
    pub rename: Option<RenameRequest>,
    pub run_id: Option<String>,
    pub saved_id: Option<String>,
    /// 这一次要落库的分组命令；其它任务为空。
    pub group: Option<GroupOp>,
}

pub enum JobDone {
    Rows(TaskToken, Result<connections::ConnectionSnapshot, String>),
    /// 保存成功：新列表和刚保存的连接 id，界面据此选中它。
    Saved(TaskToken, String, Result<connections::ConnectionSnapshot, String>),
    /// 测试结果带着发起时的代号与草稿指纹：界面据此丢弃对不上当前编辑的迟到结果。
    Test(TaskToken, u64, u64, Result<crate::connections::stages::TestReport, String>),
    Open(TaskToken, Result<crate::connections::OpenOutcome, String>),
    Disconnect(TaskToken, String),
    Notice(TaskToken, Result<String, String>),
    Imported(TaskToken, Result<(connections::ConnectionSnapshot, usize, Vec<String>), String>),
    History(TaskToken, Result<(Vec<HistoryEntry>, Vec<SavedQuery>), String>),
    /// 分组与成员关系的快照：(分组, 连接 id → 分组 id)。
    Groups(TaskToken, Result<(Vec<ConnectionGroup>, std::collections::BTreeMap<String, String>), String>),
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
    let rename = match job {
        PendingJob::RenameSaved => model.history.rename_request(),
        _ => None,
    };
    // 打开连接用的是“这次输入/刚存下的机密”，不是被清空后的表单机密。
    let secrets = match &job {
        PendingJob::OpenConnection => model
            .pending_open_secrets
            .as_ref()
            .filter(|(id, _)| model.connections.selected.as_deref() == Some(id.as_str()))
            .map(|(_, secrets)| secrets.clone())
            .unwrap_or_else(|| model.connections.secrets.clone()),
        _ => model.connections.secrets.clone(),
    };
    JobRequest {
        job,
        token,
        generation: model.connections.test_generation,
        draft: model.connections.editor.clone(),
        secrets,
        selected: model.connections.selected.clone(),
        export_path: model.export_path.clone(),
        import_path: model.import_path.clone(),
        bundle_passphrase: {
            let trimmed = model.bundle_passphrase.trim();
            if trimmed.is_empty() { None } else { Some(trimmed.to_owned()) }
        },
        save,
        rename,
        run_id: model.history.selected_run.clone(),
        saved_id: model.history.selected_saved.clone(),
        group: model.pending_group_op.clone(),
    }
}

fn save_request(model: &WorkbenchModel) -> Option<SaveRequest> {
    if let Some(request) = model.preview_save.clone() {
        return Some(request);
    }
    let name = model.history.save_name.trim();
    if name.is_empty() {
        return None;
    }
    if let Some(tab) = model.active_tab().filter(|tab| tab.kind.is_query() && !tab.draft.trim().is_empty()) {
        return Some(SaveRequest {
            name: name.to_owned(),
            sql: tab.draft.clone(),
            connection_id: tab.connection_id.clone().unwrap_or_default(),
            connection_name: tab.connection_name.clone().unwrap_or_default(),
            database_name: tab.database.clone().unwrap_or_default(),
            engine: tab.engine.clone().unwrap_or_default(),
        });
    }
    let plan = model.history.preview_plan(&model.known_ids())?;
    if plan.sql.trim().is_empty() {
        return None;
    }
    Some(SaveRequest {
        name: name.to_owned(),
        sql: plan.sql,
        connection_id: plan.connection_id.unwrap_or_default(),
        connection_name: plan.connection_name,
        database_name: plan.database.unwrap_or_default(),
        engine: plan.engine,
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
            // 测试的是这份请求里的草稿快照，不是 selected id 在配置库里的旧值。
            let Some(draft) = request.draft.clone() else {
                return JobDone::Notice(token, Err("connections.editor.test.button".into()));
            };
            let fingerprint = connections::draft_fingerprint(&request.draft, &request.secrets);
            JobDone::Test(
                token,
                request.generation,
                fingerprint,
                connections::test_row(services, draft, &request.secrets).await,
            )
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
        PendingJob::RefreshGroups => JobDone::Groups(token, services.connections.groups().await),
        PendingJob::GroupOp => {
            let Some(op) = request.group else {
                return JobDone::Notice(token, Err("ui.group.new".into()));
            };
            let result: Result<(), String> = match op {
                GroupOp::Save { id, name } => {
                    services.connections.save_group(id, name).await.map(|_| ()).map_err(|err| err.message())
                }
                GroupOp::Delete { id } => services.connections.delete_group(&id).await,
                GroupOp::Assign { connection, group } => {
                    services.connections.assign_group(&connection, group.as_deref()).await
                }
                GroupOp::Collapsed { id, collapsed } => services.connections.set_group_collapsed(&id, collapsed).await,
            };
            match result {
                Ok(()) => JobDone::Groups(token, services.connections.groups().await),
                // 失败只报告原因：本地分组与连接列表保持原样，绝不丢连接。
                Err(detail) => JobDone::Groups(token, Err(detail)),
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
        PendingJob::RenameSaved => {
            let Some(rename) = request.rename else {
                return JobDone::Notice(token, Err("history.page.needName".into()));
            };
            match history::rename_saved(services, &rename).await {
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
        JobDone::Rows(token, Ok(snapshot)) => {
            if owner == "connections" && model.connections.accept_list(&services.tasks, token, snapshot.rows) {
                model.connections.recent_ids = snapshot.recent;
                model.welcome.has_connections = !model.connections.rows.is_empty();
            }
        }
        JobDone::Saved(token, id, Ok(snapshot)) => {
            if owner == "connections" && model.connections.accept_list(&services.tasks, token, snapshot.rows) {
                model.connections.recent_ids = snapshot.recent;
                model.welcome.has_connections = !model.connections.rows.is_empty();
                model.connections.selected = Some(id.clone());
                if let Some(editor) = model.connections.editor.as_mut() {
                    editor.id = Some(id.clone());
                }
                model.connections.message_key = None;
                model.notice_detail.clear();
                let connect_after = model.conn_modal.save_and_connect;
                // 关闭模态会清掉本次机密；“保存并连接”要带着刚输入的密码去连接，
                // 否则没保存密码的配置（复制出来的、编辑过的）会在 OpenConnection 时拿空密码去认证。
                let typed_secrets = model.connections.secrets.clone();
                // 保存成功就关掉模态：普通保存到此为止，绝不因为保存过一次就去连接。
                let _ = model.close_connection_modal();
                model.connections.selected = Some(id.clone());
                if connect_after {
                    // 用刚才输入/存下的机密去连接，重试也带着它；配置已经落盘，失败不再存一次。
                    model.pending_open_secrets = Some((id.clone(), typed_secrets));
                    model.queue_connect(super::conn_modal::ConnectIntent::new(
                        id,
                        super::conn_modal::ConnectAction::Expand,
                        None,
                    ));
                } else {
                    model.notice_key = Some("common.save");
                }
            }
        }
        JobDone::Rows(token, Err(detail)) | JobDone::Saved(token, _, Err(detail)) | JobDone::Notice(token, Err(detail)) => {
            if owner == "connections" && model.connections.accept_notice(&services.tasks, token, detail.clone()) {
                model.notice_key = Some(static_notice(&detail));
                model.notice_detail = model.connections.message_detail.clone();
            } else if accepted(services, owner, token) {
                if owner == "history" {
                    // 失败只记在页面上，原列表保持不变，不静默移除条目。
                    model.history.mark_failure(detail.clone());
                    model.notice_key = Some("history.page.failed");
                } else {
                    model.notice_key = Some(static_notice(&detail));
                }
                model.notice_detail = detail;
            }
        }
        JobDone::Notice(token, Ok(detail)) => {
            if accepted(services, owner, token) {
                model.notice_detail = detail;
                model.notice_key = Some("common.save");
            }
        }
        JobDone::Imported(token, Ok((snapshot, count, skipped))) => {
            if owner == "connections" && model.connections.accept_list(&services.tasks, token, snapshot.rows) {
                model.connections.recent_ids = snapshot.recent;
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
        JobDone::Test(token, generation, fingerprint, report) => {
            let current = model.conn_modal.is_current_test(generation);
            let accepted = model.connections.note_test(&services.tasks, token, generation, fingerprint, report);
            if current {
                // 只有自己那一代能收回"正在测试"；更新的一次仍在跑时指示保持原样。
                model.conn_modal.end_test(generation);
            }
            if accepted {
                // 测试失败保留草稿与机密：只补一条字段错误与说明，模态不关。
                model.notice_key = model.connections.message_key;
                model.notice_detail = model.connections.message_detail.clone();
            }
        }
        JobDone::Open(token, outcome) => {
            let _ = model.connections.note_open(&services.tasks, token, outcome);
            model.notice_key = model.connections.message_key;
            model.notice_detail = model.connections.message_detail.clone();
            // 连上之后那份输入机密就用完了；失败时留着，重试还要用同一份去认证。
            let used = model
                .pending_open_secrets
                .as_ref()
                .is_some_and(|(id, _)| model.connections.row_status(id) == crate::app::views::connections::LinkStatus::Connected);
            if used {
                model.pending_open_secrets = None;
            }
            model.fail_pending_intents();
        }
        JobDone::Disconnect(token, id) => {
            if accepted(services, owner, token) {
                model.connections.mark_disconnected(&id);
            }
        }
        JobDone::History(token, Ok((runs, saved))) => {
            if let TaskOutcome::Current { value, .. } = services.tasks.accept(owner, token, (runs, saved)) {
                model.history.apply_list(value.0, value.1);
                model.history.rename_open = false;
                model.history.rename_text.clear();
                model.history.message_key = Some("history.page.loadOnlySql");
            }
        }
        JobDone::Groups(token, Ok((groups, members))) => {
            if accepted(services, owner, token) {
                // 落库成功后唯一一次改写内存分组：失败时这里根本不会走到，界面保持原样。
                model.apply_groups(groups, members);
                model.notice_key = None;
            }
        }
        JobDone::Groups(token, Err(detail)) => {
            if accepted(services, owner, token) {
                model.notice_key = Some(static_notice(&detail));
                model.notice_detail = detail;
            }
        }
        JobDone::History(token, Err(detail)) => {
            if accepted(services, owner, token) {
                model.history.mark_failure(detail.clone());
                model.notice_detail = detail;
                model.notice_key = Some("history.page.failed");
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
