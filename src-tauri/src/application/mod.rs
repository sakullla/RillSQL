//! 面向 GPUI 的应用服务。核心模块不依赖 GPUI 类型。
//! Tauri command 只做参数转发；确认、JSON 校验、会话和健康标记从这里进入。

pub(crate) mod ddl;
pub(crate) mod files;
pub(crate) mod grid;
pub(crate) mod mongo;
pub(crate) mod redis;
pub(crate) mod tasks;
pub(crate) mod work;

use std::path::Path;
use std::sync::Arc;

use crate::connections::commands::ConnectionsState;
use crate::connections::model::{ConnectionDraft, EnvTag, Secret, SecretInput};
use crate::connections::{ConnectionRecord, DeleteOutcome, OpenOutcome, SaveOutcome, Service, ServiceError};
use crate::connections::stages::TestReport;
use crate::connections::export::ParsedImport;
use crate::grid::commands::{CommitDto, CommitRequest};
use crate::grid::request_grid_export_cancel;
use crate::state::{ImportReport, Platform, StateStore};

pub(crate) use tasks::{run_blocking, TaskBoard, TaskOutcome};
pub(crate) use work::{cancel_compare, cancel_dump, cancel_migrate, compare_cancelled};

pub struct AppServices {
    pub connections: Arc<Service>,
    pub query: crate::query::QueryState,
    pub tasks: TaskBoard,
    pub state: StateStore,
}

impl AppServices {
    pub fn new(connections: Arc<Service>) -> Self {
        let query = crate::query::QueryState::from_config_path(connections.config_path());
        let directory = connections.config_path().parent().map(|path| path.to_path_buf()).unwrap_or_else(|| Path::new(".").to_path_buf());
        Self {
            state: StateStore::new(directory),
            connections,
            query,
            tasks: TaskBoard::new(),
        }
    }

    pub fn from_state(state: &ConnectionsState) -> Self {
        Self::new(Arc::clone(&state.service))
    }

    pub async fn list_connections(&self) -> Result<Vec<ConnectionRecord>, String> {
        self.connections.list().await
    }

    pub async fn save_connection(&self, draft: ConnectionDraft, password: Option<String>, ssh_password: Option<String>, ssh_passphrase: Option<String>) -> Result<SaveOutcome, ServiceError> {
        let save_password = draft.save_password;
        self.connections
            .save(
                draft,
                SecretInput {
                    password: password.map(Secret::new),
                    ssh_password: ssh_password.map(Secret::new),
                    ssh_passphrase: ssh_passphrase.map(Secret::new),
                    save_password,
                },
            )
            .await
    }

    pub async fn delete_connection(&self, id: &str) -> Result<DeleteOutcome, String> {
        self.connections.delete(id).await
    }

    pub async fn test_connection(&self, id: &str, password: Option<String>, ssh_password: Option<String>, ssh_passphrase: Option<String>) -> Result<TestReport, String> {
        self.connections
            .test(
                id,
                SecretInput {
                    password: password.map(Secret::new),
                    ssh_password: ssh_password.map(Secret::new),
                    ssh_passphrase: ssh_passphrase.map(Secret::new),
                    save_password: false,
                },
            )
            .await
    }

    pub async fn open_connection(&self, id: &str, password: Option<String>, ssh_password: Option<String>, ssh_passphrase: Option<String>) -> Result<OpenOutcome, String> {
        self.connections
            .open(
                id,
                SecretInput {
                    password: password.map(Secret::new),
                    ssh_password: ssh_password.map(Secret::new),
                    ssh_passphrase: ssh_passphrase.map(Secret::new),
                    save_password: false,
                },
            )
            .await
    }

    pub fn disconnect(&self, id: &str) {
        self.connections.disconnect(id);
    }

    pub async fn focus_database(&self, id: &str, database: &str) -> Result<(), String> {
        self.connections.focus_database(id, database).await
    }

    pub async fn confirm_host_key(&self, id: &str, fingerprint: &str) -> Result<(), String> {
        self.connections.confirm_host_key(id, fingerprint).await
    }

    pub async fn forget_host_key(&self, id: &str) -> Result<(), String> {
        self.connections.forget_host_key(id).await
    }

    pub async fn export_connections(&self, ids: Option<Vec<String>>, passphrase: Option<&str>, path: &Path) -> Result<usize, String> {
        self.connections.export_connections(ids, passphrase, path).await
    }

    pub async fn import_connections(&self, path: &Path, passphrase: Option<&str>) -> Result<ParsedImport, String> {
        self.connections.import_file(path, passphrase).await
    }

    pub async fn list_tags(&self) -> Result<Vec<EnvTag>, String> {
        self.connections.list_tags().await
    }

    pub async fn save_tag(&self, id: Option<String>, label: String, tone: String) -> Result<EnvTag, ServiceError> {
        self.connections.save_tag(id, label, tone).await
    }

    pub async fn delete_tag(&self, id: &str) -> Result<(), String> {
        self.connections.delete_tag(id).await
    }

    pub async fn commit_grid(&self, request: &CommitRequest, confirmed: bool) -> Result<CommitDto, String> {
        grid::commit(&self.connections, request, confirmed).await
    }

    pub fn cancel_grid_export(&self, owner: &str, id: &str) {
        self.tasks.cancel_owner_tasks(owner);
        request_grid_export_cancel(id);
    }

    pub async fn import_legacy(&self, legacy_root: &Path, platform: Platform) -> ImportReport {
        self.state.import_legacy(legacy_root, platform).await
    }

    pub async fn save_state(&self, state: &crate::state::AppState) -> Result<(), String> {
        self.state.save(state).await
    }

    pub fn load_state(&self) -> Result<crate::state::AppState, crate::state::StateError> {
        self.state.load()
    }
}

#[cfg(test)]
mod tests;
