use std::sync::Arc;

use serde::Serialize;

use crate::connections::model::{ConnectionDraft, FieldError, Secret, SecretInput};
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
        Self {
            service: Arc::new(Service::new(service::default_config_path(), Arc::new(KeyringStore::new()))),
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

#[tauri::command]
pub async fn connections_list(state: tauri::State<'_, ConnectionsState>) -> Result<Vec<ConnectionRecord>, CommandError> {
    state.service.list().await.map_err(CommandError::from)
}

#[tauri::command]
pub async fn connections_save(
    state: tauri::State<'_, ConnectionsState>,
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

#[tauri::command]
pub async fn connections_delete(state: tauri::State<'_, ConnectionsState>, id: String) -> Result<DeleteOutcome, CommandError> {
    state.service.delete(&id).await.map_err(CommandError::from)
}

#[tauri::command]
pub async fn connections_test(
    state: tauri::State<'_, ConnectionsState>,
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

#[tauri::command]
pub async fn connections_open(
    state: tauri::State<'_, ConnectionsState>,
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

#[tauri::command]
pub async fn connections_disconnect(state: tauri::State<'_, ConnectionsState>, id: String) -> Result<(), CommandError> {
    state.service.disconnect(&id);
    Ok(())
}

#[tauri::command]
pub async fn connections_confirm_host_key(
    state: tauri::State<'_, ConnectionsState>,
    id: String,
    fingerprint: String,
) -> Result<(), CommandError> {
    state.service.confirm_host_key(&id, &fingerprint).await.map_err(CommandError::from)
}

#[tauri::command]
pub async fn connections_forget_host_key(state: tauri::State<'_, ConnectionsState>, id: String) -> Result<(), CommandError> {
    state.service.forget_host_key(&id).await.map_err(CommandError::from)
}
