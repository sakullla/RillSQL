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
    /// 本地保险库本身用不了（文件读写失败、版本过新等），原因只带底层信息。
    #[error("{}", problem.describe(reason))]
    Vault { problem: VaultProblem, reason: String },
}

/// 保险库加载或写盘失败的类别：界面按类别给出不同的提示和恢复指引。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultProblem {
    /// 钥匙串没给出主密钥（拒绝授权、被锁定、服务未运行）。
    KeyUnavailable,
    /// 保险库文件读不出或写不进（权限、磁盘）。
    FileAccess,
    /// 文件由更新版本的 RillSQL 写入。
    NewerVersion,
    /// 文件无法解密，而且没能改名保留。
    CorruptNotKept,
    /// 随机数、序列化、加密等内部步骤失败。
    Internal,
}

impl VaultProblem {
    /// 日志与连接错误里用的中文说明（界面提示条另走 i18n）。
    fn describe(self, reason: &str) -> String {
        match self {
            VaultProblem::KeyUnavailable => format!("保险库密钥暂不可用：{reason}"),
            VaultProblem::FileAccess => format!("读写不了本地保险库文件：{reason}"),
            VaultProblem::NewerVersion => "本地保险库由更新版本的 RillSQL 写入，当前版本无法读取".to_owned(),
            VaultProblem::CorruptNotKept => format!("本地保险库无法解密，也没能改名保留：{reason}"),
            VaultProblem::Internal => format!("本地保险库暂时无法准备：{reason}"),
        }
    }

    /// 提示条用的 i18n 模板键（模板里可用 `{reason}`）。
    pub fn message_key(self) -> &'static str {
        match self {
            VaultProblem::KeyUnavailable => "ui.vault.keyUnavailable",
            VaultProblem::FileAccess => "ui.vault.fileAccess",
            VaultProblem::NewerVersion => "ui.vault.newerVersion",
            VaultProblem::CorruptNotKept => "ui.vault.corruptNotKept",
            VaultProblem::Internal => "ui.vault.internal",
        }
    }
}

/// 分好类的保险库失败：类别加底层原因（不含任何说明性前缀）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultFailure {
    pub problem: VaultProblem,
    pub reason: String,
}

impl SecretError {
    /// 底层原因本身，不带变体的说明前缀。
    pub fn reason(&self) -> &str {
        match self {
            SecretError::Unavailable(reason) | SecretError::Corrupt(reason) | SecretError::MigrationFailed(reason) => reason,
            SecretError::Vault { reason, .. } => reason,
        }
    }

    /// 归入提示条的保险库失败类别；损坏（已由事件报告）与单条迁移失败不在此列。
    pub fn vault_failure(&self) -> Option<VaultFailure> {
        match self {
            SecretError::Unavailable(reason) => Some(VaultFailure { problem: VaultProblem::KeyUnavailable, reason: reason.clone() }),
            SecretError::Vault { problem, reason } => Some(VaultFailure { problem: *problem, reason: reason.clone() }),
            SecretError::Corrupt(_) | SecretError::MigrationFailed(_) => None,
        }
    }

    /// 给用户看的完整说明：原因加恢复指引。连接打开失败、保存密码失败时都用它。
    pub fn guidance(&self) -> String {
        match self {
            SecretError::Unavailable(reason) => format!(
                "保存的密码暂时取不到，保险库密钥暂不可用（{reason}）。请在系统钥匙串的授权提示中选择“始终允许”后重试，或这次手动输入密码。"
            ),
            SecretError::Corrupt(_) => format!("{self} 这次请手动输入密码并勾选“保存密码”，会写进新的保险库。"),
            SecretError::MigrationFailed(_) => format!(
                "{self}。旧条目仍在系统钥匙串里：允许钥匙串访问后可重试迁移，或这次手动输入密码并保存。"
            ),
            SecretError::Vault { problem, .. } => match problem {
                VaultProblem::KeyUnavailable => format!("{self}。请在钥匙串授权提示中选择“始终允许”后重试，或这次手动输入密码。"),
                VaultProblem::FileAccess => format!("{self}。请检查文件权限和磁盘空间后重试，或这次手动输入密码。"),
                VaultProblem::NewerVersion => format!("{self}，文件原样保留。请升级 RillSQL，或这次手动输入密码。"),
                VaultProblem::CorruptNotKept => format!("{self}。请退出 RillSQL，把 secrets.vault 移到别处后重试，或这次手动输入密码。"),
                VaultProblem::Internal => format!("{self}。请重试；一直失败时重启 RillSQL，或这次手动输入密码。"),
            },
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
    /// 保险库这次没能加载（钥匙串没给出主密钥、文件读不出、版本过新等）；文件原样保留，下次访问会重试。
    /// `reason` 只是底层原因，提示文字按 `problem` 走 i18n。
    LoadFailed { problem: VaultProblem, reason: String },
    /// 之前加载失败的保险库这次加载成功：界面据此收起加载失败的提示。
    LoadRecovered,
    /// 钥匙串里的主密钥格式或长度无效：已换成新主密钥，原保险库改名保留在 `kept`。
    /// 没有原文件时不会排出这条事件（没有已保存的密码会丢）。
    KeyInvalid { kept: std::path::PathBuf },
    /// 重试迁移后不再失败的账户：界面据此把它们从失败计数里去掉。
    MigrationRecovered { account: String },
}

pub trait SecretStore: Send + Sync {
    fn put(&self, account: &str, secret: &str) -> Result<(), SecretError>;
    fn get(&self, account: &str) -> Result<Option<String>, SecretError>;
    fn delete(&self, account: &str) -> Result<(), SecretError>;

    /// 登记可能还有旧版逐条钥匙串条目的账户（按 password_saved / ssh_password_saved 枚举）。
    /// 名单里的账户第一次取密码时才按需迁移；不在名单里的账户不再访问旧钥匙串。
    fn register_legacy(&self, _accounts: Vec<String>) {}

    /// 名单里还没迁入的旧条目数：只看已加载的元数据，不访问钥匙串；保险库还没加载时为 None。
    fn pending_legacy(&self) -> Option<usize> {
        None
    }

    /// 现在全部迁入：每个待迁移（含之前失败）的账户各读一次旧条目。
    fn migrate_all_legacy(&self) -> Result<MigrationReport, SecretError> {
        Ok(MigrationReport::default())
    }

    /// 不再迁移：清掉待迁移标记，返回清掉的个数。
    fn dismiss_legacy(&self) -> Result<usize, SecretError> {
        Ok(0)
    }

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
