//! 旧 `connections.sqlite`、`query-history.sqlite`、keyring 与连接导出的只读兼容入口。
//! 不改变连接 id、keyring service、账号规则、导出格式或外部数据库文件引用。

use std::path::Path;
use std::sync::Arc;

use crate::connections::export::{self, ParsedImport};
use crate::connections::model::{password_account, ssh_password_account, SavedConnection};
use crate::connections::secrets::{SecretStore, KEYRING_SERVICE};
use crate::connections::store::Store;
use crate::query::{HistoryEntry, HistoryStore};

pub fn keyring_service() -> &'static str {
    KEYRING_SERVICE
}

pub fn password_account_for(id: &str) -> String {
    password_account(id)
}

pub fn ssh_password_account_for(id: &str) -> String {
    ssh_password_account(id)
}

pub fn read_secret(store: &dyn SecretStore, account: &str) -> Result<Option<String>, String> {
    store.get(account).map_err(|err| err.to_string())
}

pub async fn read_connections(path: &Path) -> Result<Vec<SavedConnection>, String> {
    let store = Store::open(path).await.map_err(|err| format!("打不开连接库：{err}"))?;
    store.list().await.map_err(|err| format!("读不了连接库：{err}"))
}

pub async fn read_env_tags(path: &Path) -> Result<Vec<crate::connections::model::EnvTag>, String> {
    let store = Store::open(path).await.map_err(|err| format!("打不开连接库：{err}"))?;
    store.list_tags().await.map_err(|err| format!("读不了环境标签：{err}"))
}

pub async fn read_history(path: &Path) -> Result<Vec<HistoryEntry>, String> {
    let store = HistoryStore::open(path).await?;
    store.list().await
}

pub fn read_connection_export(bytes: &[u8], passphrase: Option<&str>) -> Result<ParsedImport, String> {
    export::parse_import(bytes, passphrase)
}

pub fn keyring_store() -> Arc<dyn SecretStore> {
    Arc::new(crate::connections::secrets::KeyringStore::new())
}
