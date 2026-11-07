//! 版本化应用状态，以及旧 SQLite / keyring / localStorage 的兼容读取。

mod atomic;
mod compat;
mod document;
mod legacy;

use atomic::write_atomic_off_ui;
pub use compat::{
    keyring_service, password_account_for, read_connection_export, read_connections, read_env_tags, read_history, read_secret, ssh_password_account_for,
};
pub use document::{parse_stored, to_bytes, AppState, SessionState, ShellSettings, StateError, APP_STATE_FILE};
use legacy::import_legacy;
pub use legacy::{ImportReport, Platform};

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self { path: data_dir.as_ref().join(APP_STATE_FILE) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<AppState, StateError> {
        let bytes = std::fs::read(&self.path).map_err(|_| StateError::Malformed)?;
        parse_stored(&bytes)
    }

    pub async fn save(&self, state: &AppState) -> Result<(), String> {
        let bytes = to_bytes(state).map_err(|err| err.to_string())?;
        write_atomic_off_ui(self.path.clone(), bytes).await
    }

    pub async fn import_legacy(&self, legacy_root: &Path, platform: Platform) -> ImportReport {
        import_legacy(&self.path, legacy_root, platform).await
    }
}

#[cfg(test)]
mod tests;
