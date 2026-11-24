//! 旧 `connections.sqlite`、`query-history.sqlite`、凭据存储与连接导出的只读兼容入口。
//! 不改变连接 id、钥匙串 service、账号规则、导出格式或外部数据库文件引用。
//! 连接密码存在 `connections.sqlite` 旁的本地保险库（`secrets.vault`）里，系统钥匙串只保存一把主密钥；
//! 旧版逐条钥匙串条目由保险库首次加载时迁移。

use std::path::Path;
use std::sync::Arc;

use crate::connections::export::{self, ParsedImport};
use crate::connections::model::{password_account, ssh_password_account, SavedConnection};
use crate::connections::secrets::{SecretStore, KEYRING_SERVICE};
use crate::connections::store::Store;
use crate::query::{HistoryEntry, HistoryStore};

#[allow(dead_code)]
pub fn keyring_service() -> &'static str {
    KEYRING_SERVICE
}

#[allow(dead_code)]
pub fn password_account_for(id: &str) -> String {
    password_account(id)
}

#[allow(dead_code)]
pub fn ssh_password_account_for(id: &str) -> String {
    ssh_password_account(id)
}

#[allow(dead_code)]
pub fn read_secret(store: &dyn SecretStore, account: &str) -> Result<Option<String>, String> {
    store.get(account).map_err(|err| err.to_string())
}

#[allow(dead_code)]
pub async fn read_connections(path: &Path) -> Result<Vec<SavedConnection>, String> {
    let store = Store::open(path).await.map_err(|err| format!("打不开连接库：{err}"))?;
    store.list().await.map_err(|err| format!("读不了连接库：{err}"))
}

#[allow(dead_code)]
pub async fn read_env_tags(path: &Path) -> Result<Vec<crate::connections::model::EnvTag>, String> {
    let store = Store::open(path).await.map_err(|err| format!("打不开连接库：{err}"))?;
    store.list_tags().await.map_err(|err| format!("读不了环境标签：{err}"))
}

#[allow(dead_code)]
pub async fn read_history(path: &Path) -> Result<Vec<HistoryEntry>, String> {
    let store = HistoryStore::open(path).await?;
    store.list().await
}

#[allow(dead_code)]
pub fn read_connection_export(bytes: &[u8], passphrase: Option<&str>) -> Result<ParsedImport, String> {
    export::parse_import(bytes, passphrase)
}

/// 正式运行使用的凭据存储：默认配置路径旁的本地保险库（不是逐条钥匙串条目）。
/// 构造时不访问钥匙串也不读文件，首次 get/put/delete 才加载。
#[allow(dead_code)]
pub fn vault_store() -> Arc<dyn SecretStore> {
    Arc::new(crate::connections::vault::VaultStore::for_config(&crate::connections::default_config_path()))
}
