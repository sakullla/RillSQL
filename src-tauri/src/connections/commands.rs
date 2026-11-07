use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use crate::connections::export::ParsedImport;
use crate::connections::model::{ConnectionDraft, EnvTag, FieldError, Secret, SecretInput};
use crate::connections::secrets::KeyringStore;
use crate::connections::service::{self, ConnectionRecord, DeleteOutcome, OpenOutcome, SaveOutcome, Service, ServiceError};
use crate::connections::stages::TestReport;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub message: String,
    pub fields: Vec<FieldError>,
}

impl From<ServiceError> for CommandError {
    fn from(error: ServiceError) -> Self {
        let message = error.message();
        let fields = match error {
            ServiceError::Fields(fields) => fields,
            ServiceError::Message(_) => Vec::new(),
        };
        CommandError { message, fields }
    }
}

impl From<String> for CommandError {
    fn from(message: String) -> Self {
        CommandError { message, fields: Vec::new() }
    }
}

pub struct ConnectionsState {
    pub service: Arc<Service>,
}

impl ConnectionsState {
    pub fn new() -> Self {
        Self::with_path(service::default_config_path())
    }

    pub fn with_path(path: PathBuf) -> Self {
        Self {
            service: Arc::new(Service::new(path, Arc::new(KeyringStore::new()))),
        }
    }
}

impl Default for ConnectionsState {
    fn default() -> Self {
        Self::new()
    }
}

fn secrets_from(password: Option<String>, ssh_password: Option<String>, ssh_passphrase: Option<String>, save_password: bool) -> SecretInput {
    SecretInput {
        password: password.map(Secret::new),
        ssh_password: ssh_password.map(Secret::new),
        ssh_passphrase: ssh_passphrase.map(Secret::new),
        save_password,
    }
}

pub async fn env_tags_list(state: &ConnectionsState) -> Result<Vec<EnvTag>, CommandError> {
    state.service.list_tags().await.map_err(CommandError::from)
}

pub async fn env_tags_save(
    state: &ConnectionsState,
    id: Option<String>,
    label: String,
    tone: String,
) -> Result<EnvTag, CommandError> {
    state.service.save_tag(id, label, tone).await.map_err(CommandError::from)
}

pub async fn env_tags_delete(state: &ConnectionsState, id: String) -> Result<(), CommandError> {
    state.service.delete_tag(&id).await.map_err(CommandError::from)
}

pub async fn connections_list(state: &ConnectionsState) -> Result<Vec<ConnectionRecord>, CommandError> {
    state.service.list().await.map_err(CommandError::from)
}

pub async fn connections_save(
    state: &ConnectionsState,
    draft: ConnectionDraft,
    password: Option<String>,
    ssh_password: Option<String>,
    ssh_passphrase: Option<String>,
) -> Result<SaveOutcome, CommandError> {
    let save_password = draft.save_password;
    state
        .service
        .save(draft, secrets_from(password, ssh_password, ssh_passphrase, save_password))
        .await
        .map_err(CommandError::from)
}

pub async fn connections_delete(state: &ConnectionsState, id: String) -> Result<DeleteOutcome, CommandError> {
    state.service.delete(&id).await.map_err(CommandError::from)
}

pub async fn connections_test(
    state: &ConnectionsState,
    id: String,
    password: Option<String>,
    ssh_password: Option<String>,
    ssh_passphrase: Option<String>,
) -> Result<TestReport, CommandError> {
    state
        .service
        .test(&id, secrets_from(password, ssh_password, ssh_passphrase, false))
        .await
        .map_err(CommandError::from)
}

pub async fn connections_open(
    state: &ConnectionsState,
    id: String,
    password: Option<String>,
    ssh_password: Option<String>,
    ssh_passphrase: Option<String>,
) -> Result<OpenOutcome, CommandError> {
    state
        .service
        .open(&id, secrets_from(password, ssh_password, ssh_passphrase, false))
        .await
        .map_err(CommandError::from)
}

pub async fn connections_disconnect(state: &ConnectionsState, id: String) -> Result<(), CommandError> {
    state.service.disconnect(&id);
    Ok(())
}

pub async fn connections_focus_database(state: &ConnectionsState, id: String, database: String) -> Result<(), CommandError> {
    state.service.focus_database(&id, &database).await.map_err(CommandError::from)
}

pub async fn connections_confirm_host_key(
    state: &ConnectionsState,
    id: String,
    fingerprint: String,
) -> Result<(), CommandError> {
    state.service.confirm_host_key(&id, &fingerprint).await.map_err(CommandError::from)
}

pub async fn connections_forget_host_key(state: &ConnectionsState, id: String) -> Result<(), CommandError> {
    state.service.forget_host_key(&id).await.map_err(CommandError::from)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOutcome {
    pub count: usize,
}

/// 导出全部（ids 为 None）或选中连接。passphrase 留空导出明文 JSON；填写则整体加密。
/// 文件路径来自前端的原生保存对话框，后端只写这一个文件。
pub async fn connections_export(
    state: &ConnectionsState,
    ids: Option<Vec<String>>,
    passphrase: Option<String>,
    path: String,
) -> Result<ExportOutcome, CommandError> {
    let count = state
        .service
        .export_connections(ids, passphrase.as_deref(), Path::new(&path))
        .await
        .map_err(CommandError::from)?;
    Ok(ExportOutcome { count })
}

/// 导入 RillSQL 导出文件：识别 RILLSQLCX1 加密头则要求口令；格式不符或口令错误整体拒绝。
pub async fn connections_import_file(
    state: &ConnectionsState,
    path: String,
    passphrase: Option<String>,
) -> Result<ParsedImport, CommandError> {
    state
        .service
        .import_file(Path::new(&path), passphrase.as_deref())
        .await
        .map_err(CommandError::from)
}
