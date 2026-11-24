//! 版本化应用状态，以及旧 SQLite / keyring / localStorage 的兼容读取。

mod atomic;
mod column_layout;
mod compat;
mod document;
mod legacy;
mod leveldb;
mod ui_prefs;

use atomic::write_atomic_off_ui;
#[allow(unused_imports)]
pub use compat::{
    keyring_service, password_account_for, read_connection_export, read_connections, read_env_tags, read_history, read_secret, ssh_password_account_for,
};
pub use column_layout::{ColumnLayout, ColumnLayoutKey, ColumnLayoutStore};
pub use document::{parse_stored, to_bytes, AppState, SessionState, SessionTab, ShellSettings, StateError, APP_STATE_FILE};
use legacy::import_legacy;
pub use legacy::{ImportReport, Platform};
#[allow(unused_imports)]
pub use ui_prefs::{UiPrefs, UiPrefsStore, UI_PREFS_FILE};

use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct StateStore {
    path: PathBuf,
    /// 表格列布局（R3）：克隆的 StateStore 共用同一份。
    column_layouts: Arc<ColumnLayoutStore>,
    /// 界面小偏好（R11 计划视图等）：克隆的 StateStore 共用同一份。
    ui_prefs: Arc<UiPrefsStore>,
}

impl StateStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        let dir = data_dir.as_ref();
        Self { path: dir.join(APP_STATE_FILE), column_layouts: Arc::new(ColumnLayoutStore::new(dir)), ui_prefs: Arc::new(UiPrefsStore::new(dir)) }
    }

    pub fn column_layouts(&self) -> &Arc<ColumnLayoutStore> {
        &self.column_layouts
    }

    pub fn ui_prefs(&self) -> &Arc<UiPrefsStore> {
        &self.ui_prefs
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn load(&self) -> Result<AppState, StateError> {
        load_on_pool(self.path.clone()).await.map(|(state, _)| state)
    }

    pub async fn save(&self, state: &AppState) -> Result<(), String> {
        let bytes = to_bytes(state).map_err(|err| err.to_string())?;
        write_atomic_off_ui(self.path.clone(), bytes).await
    }

    pub async fn import_legacy(&self, legacy_root: &Path, platform: Platform) -> ImportReport {
        import_legacy(&self.path, legacy_root, platform).await
    }
}

pub(crate) async fn load_on_pool(path: PathBuf) -> Result<(AppState, std::thread::ThreadId), StateError> {
    tokio::task::spawn_blocking(move || {
        let reader = std::thread::current().id();
        let bytes = std::fs::read(&path).map_err(|_| StateError::Malformed)?;
        parse_stored(&bytes).map(|state| (state, reader))
    })
    .await
    .map_err(|_| StateError::Malformed)?
}

#[cfg(test)]
mod tests;
