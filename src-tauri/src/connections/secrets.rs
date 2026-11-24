#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("系统凭据库不可用：{0}")]
    Unavailable(String),
    /// 本地保险库无法解密，原文件已改名保留；重新输入密码即可恢复。
    #[error("{0}")]
    Corrupt(String),
    /// 旧版逐条钥匙串条目没能迁入保险库（原因来自钥匙串）；旧条目原样保留，可重试。
    #[error("旧凭据没能迁入保险库：{0}")]
    MigrationFailed(String),
}

impl SecretError {
    /// 给用户看的完整说明：原因加恢复指引。连接打开失败、保存密码失败时都用它。
    pub fn guidance(&self) -> String {
        match self {
            SecretError::Unavailable(_) => format!(
                "保存的密码暂时取不到（{self}）。请在系统钥匙串的授权提示中允许 RillSQL 访问后重试，或这次手动输入密码。"
            ),
            SecretError::Corrupt(_) => format!("{self} 这次请手动输入密码并勾选“保存密码”，会写进新的保险库。"),
            SecretError::MigrationFailed(_) => format!(
                "{self}。旧条目仍在系统钥匙串里：允许钥匙串访问后可重试迁移，或这次手动输入密码并保存。"
            ),
        }
    }
}

/// 旧钥匙串条目迁移的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub migrated: Vec<String>,
    /// 旧钥匙串里没有这个条目，无需迁移。
    pub absent: Vec<String>,
    pub failed: Vec<(String, String)>,
}

/// 需要在界面上汇总提示的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultNotice {
    /// 保险库无法解密，原文件已改名保留。
    Corrupt { kept: std::path::PathBuf },
    /// 某个旧钥匙串条目没能迁入。
    MigrationFailed { account: String, reason: String },
    /// 钥匙串暂时没给出主密钥（拒绝授权、被锁定）；保险库原样保留，下次访问会重试。
    KeyUnavailable { reason: String },
    /// 重试迁移后不再失败的账户：界面据此把它们从失败计数里去掉。
    MigrationRecovered { account: String },
}

pub trait SecretStore: Send + Sync {
    fn put(&self, account: &str, secret: &str) -> Result<(), SecretError>;
    fn get(&self, account: &str) -> Result<Option<String>, SecretError>;
    fn delete(&self, account: &str) -> Result<(), SecretError>;

    /// 登记可能还有旧版逐条钥匙串条目的账户（按 password_saved / ssh_password_saved 枚举）。
    /// 保险库首次加载时一次性迁移这些账户；不在名单里的账户不再访问旧钥匙串。
    fn register_legacy(&self, _accounts: Vec<String>) {}

    /// 重新尝试迁移之前失败的旧条目。
    fn retry_failed_migration(&self) -> Result<MigrationReport, SecretError> {
        Ok(MigrationReport::default())
    }

    /// 仍未迁入的旧条目（账户、原因），跨重启保留。
    fn migration_failures(&self) -> Result<Vec<(String, String)>, SecretError> {
        Ok(Vec::new())
    }

    /// 取走待提示的事件（保险库损坏、迁移失败）。
    fn take_notices(&self) -> Vec<VaultNotice> {
        Vec::new()
    }

    /// 本地保险库文件位置；不是保险库实现时为 None。
    fn vault_path(&self) -> Option<&std::path::Path> {
        None
    }
}

pub const KEYRING_SERVICE: &str = "app.rillsql.desktop";

pub struct KeyringStore {
    service: &'static str,
}

#[allow(dead_code)]
impl KeyringStore {
    pub fn new() -> Self {
        Self { service: KEYRING_SERVICE }
    }

    pub fn service_name(&self) -> &'static str {
        self.service
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeyringStore {
    fn put(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(self.service, account).map_err(|err| SecretError::Unavailable(err.to_string()))?;
        entry
            .set_password(secret)
            .map_err(|err| SecretError::Unavailable(err.to_string()))
    }

    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        let entry = keyring::Entry::new(self.service, account).map_err(|err| SecretError::Unavailable(err.to_string()))?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(SecretError::Unavailable(err.to_string())),
        }
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(self.service, account).map_err(|err| SecretError::Unavailable(err.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(SecretError::Unavailable(err.to_string())),
        }
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemoryStore {
    values: Mutex<HashMap<String, String>>,
}

#[cfg(test)]
impl MemoryStore {
    pub fn dump(&self) -> Vec<(String, String)> {
        self.values.lock().expect("secret map").iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}

#[cfg(test)]
impl SecretStore for MemoryStore {
    fn put(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        self.values
            .lock()
            .expect("secret map")
            .insert(account.to_owned(), secret.to_owned());
        Ok(())
    }

    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        Ok(self.values.lock().expect("secret map").get(account).cloned())
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.values.lock().expect("secret map").remove(account);
        Ok(())
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct UnavailableStore;

#[cfg(test)]
impl SecretStore for UnavailableStore {
    fn put(&self, _account: &str, _secret: &str) -> Result<(), SecretError> {
        Err(SecretError::Unavailable("Secret Service 未运行".to_owned()))
    }

    fn get(&self, _account: &str) -> Result<Option<String>, SecretError> {
        Err(SecretError::Unavailable("Secret Service 未运行".to_owned()))
    }

    fn delete(&self, _account: &str) -> Result<(), SecretError> {
        Ok(())
    }
}
