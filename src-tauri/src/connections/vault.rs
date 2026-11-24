//! 本地加密保险库（R7）：连接密码集中存进 `secrets.vault`，系统钥匙串只保存一把主密钥。
//!
//! 文件格式：`RILLSQLV` + 版本字节 + 24 字节 nonce + XChaCha20-Poly1305 密文，AAD 为魔数加版本。
//! 主密钥是 32 字节随机数，存在钥匙串 `app.rillsql.desktop / vault-master-key`；
//! 首次用到时读一次，之后主密钥和解密后的映射都缓存在进程内，get 不再访问钥匙串。
//!
//! 失败边界：
//! - 钥匙串拒绝授权或不可用：返回错误，不生成新密钥，也不改写已有的 vault；下次调用会重新尝试。
//! - vault 无法解密或格式损坏：原文件改名为 `secrets.vault.corrupt-时间戳` 保留，本次返回可恢复的错误，
//!   之后从空保险库继续，用户重新输入密码即可。
//! - 版本比当前程序新：不改名也不改写，只报错。
//!
//! 旧版逐条钥匙串条目（`conn:{id}:password` 等）的迁移：服务层打开连接库时按 password_saved /
//! ssh_password_saved 登记可能有旧条目的账户，保险库首次加载时一次性迁移整份名单；vault 写入成功后
//! 才删除已迁入的旧条目。失败的逐个记进元数据并报告（`SecretError::MigrationFailed`），不会每次启动
//! 都重复读取，用户可通过 `retry_failed_migration` 重试。名单之外的账户（新建连接等）不访问旧钥匙串。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};

pub use crate::connections::secrets::{MigrationReport, VaultNotice};
use crate::connections::secrets::{KeyringStore, SecretError, SecretStore};

pub const VAULT_FILE: &str = "secrets.vault";
/// 钥匙串里唯一的一条条目（service 仍是 `app.rillsql.desktop`）。
pub const MASTER_KEY_ACCOUNT: &str = "vault-master-key";

const MAGIC: &[u8; 8] = b"RILLSQLV";
const VERSION: u8 = 1;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = MAGIC.len() + 1;

/// 解密后的保险库内容。元数据字段带默认值，旧文件缺字段也能读。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Contents {
    #[serde(default)]
    secrets: BTreeMap<String, String>,
    /// 已处理过旧钥匙串条目的账户（迁入成功、旧条目不存在或已被新值取代）。
    #[serde(default)]
    legacy_done: BTreeSet<String>,
    /// 迁移失败的账户和原因；不自动重试，避免每次启动都弹授权。
    #[serde(default)]
    legacy_failed: BTreeMap<String, String>,
}

struct Loaded {
    key: [u8; KEY_LEN],
    contents: Contents,
}

/// 可能还有旧钥匙串条目的账户名单（由服务层按 password_saved / ssh_password_saved 登记）。
struct LegacyScope {
    accounts: BTreeSet<String>,
    /// 本进程里已经对名单跑过一次迁移。
    migrated: bool,
}

pub struct VaultStore {
    path: PathBuf,
    /// 只保存主密钥的后端：正式运行是系统钥匙串，测试用内存实现。
    master: Arc<dyn SecretStore>,
    /// 旧版逐条条目所在的后端；没有则不迁移。
    legacy: Option<Arc<dyn SecretStore>>,
    /// None：没人登记过名单，按需迁移任意缺失账户（兼容独立使用）。
    /// Some：只有名单里的账户可能有旧条目，其余账户不访问旧钥匙串。
    scope: Mutex<Option<LegacyScope>>,
    state: Mutex<Option<Loaded>>,
    notices: Mutex<Vec<VaultNotice>>,
}

impl VaultStore {
    pub fn new(path: PathBuf, master: Arc<dyn SecretStore>, legacy: Option<Arc<dyn SecretStore>>) -> Self {
        Self {
            path,
            master,
            legacy,
            scope: Mutex::new(None),
            state: Mutex::new(None),
            notices: Mutex::new(Vec::new()),
        }
    }

    /// 正式运行：vault 文件放在 `connections.sqlite` 旁边，主密钥与旧条目都在系统钥匙串。
    /// 构造时不访问钥匙串也不读文件，首次 get/put/delete 才加载。
    pub fn for_config(config_path: &Path) -> Self {
        let keyring: Arc<dyn SecretStore> = Arc::new(KeyringStore::new());
        Self::new(vault_path_for(config_path), keyring.clone(), Some(keyring))
    }

    /// 批量迁移旧钥匙串条目：每个账户最多读一次旧条目。
    /// vault 写入成功后才删除已迁入的旧条目；失败的逐个报告并记进元数据。
    #[cfg(test)]
    pub fn migrate(&self, accounts: &[String]) -> Result<MigrationReport, SecretError> {
        let mut guard = self.state.lock().expect("vault state");
        let loaded = self.ensure_loaded(&mut guard)?;
        self.migrate_locked(loaded, accounts, false)
    }

    /// `retry` 为 true 时连之前失败过的账户也重新读一次。
    fn migrate_locked(&self, loaded: &mut Loaded, accounts: &[String], retry: bool) -> Result<MigrationReport, SecretError> {
        let mut report = MigrationReport::default();
        let Some(legacy) = self.legacy.as_ref() else {
            return Ok(report);
        };
        let mut next = loaded.contents.clone();
        for account in accounts {
            let failed_before = next.legacy_failed.contains_key(account);
            if next.secrets.contains_key(account) || next.legacy_done.contains(account) || (failed_before && !retry) {
                continue;
            }
            match legacy.get(account) {
                Ok(Some(secret)) => {
                    next.secrets.insert(account.clone(), secret);
                    next.legacy_failed.remove(account);
                    next.legacy_done.insert(account.clone());
                    report.migrated.push(account.clone());
                }
                Ok(None) => {
                    next.legacy_failed.remove(account);
                    next.legacy_done.insert(account.clone());
                    report.absent.push(account.clone());
                }
                Err(err) => {
                    let reason = err.to_string();
                    next.legacy_failed.insert(account.clone(), reason.clone());
                    report.failed.push((account.clone(), reason));
                }
            }
        }
        if report.migrated.is_empty() && report.absent.is_empty() && report.failed.is_empty() {
            return Ok(report);
        }
        // 先落盘；写失败时旧条目一个都不删，内存也保持旧状态。
        self.commit(loaded, next)?;
        for account in &report.migrated {
            // 已经安全写进 vault；旧条目删不掉只是残留，不影响读取。
            let _ = legacy.delete(account);
        }
        if retry {
            let mut notices = self.notices.lock().expect("vault notices");
            for account in report.migrated.iter().chain(&report.absent) {
                notices.push(VaultNotice::MigrationRecovered { account: account.clone() });
            }
        }
        if !report.failed.is_empty() {
            let mut notices = self.notices.lock().expect("vault notices");
            for (account, reason) in &report.failed {
                notices.push(VaultNotice::MigrationFailed { account: account.clone(), reason: reason.clone() });
            }
        }
        Ok(report)
    }

    /// 名单登记后、保险库加载后：对名单跑一次迁移（本进程只跑一次，跨重启靠元数据去重）。
    fn migrate_scope(&self, loaded: &mut Loaded) -> Result<(), SecretError> {
        let accounts: Vec<String> = match self.scope.lock().expect("vault scope").as_ref() {
            Some(scope) if !scope.migrated => scope.accounts.iter().cloned().collect(),
            _ => return Ok(()),
        };
        self.migrate_locked(loaded, &accounts, false)?;
        if let Some(scope) = self.scope.lock().expect("vault scope").as_mut() {
            scope.migrated = true;
        }
        Ok(())
    }

    /// 这个账户在旧钥匙串里可能还有条目吗：处理过的没有；登记过名单时只认名单。
    fn may_have_legacy(&self, contents: &Contents, account: &str) -> bool {
        if self.legacy.is_none() || contents.legacy_done.contains(account) {
            return false;
        }
        if contents.legacy_failed.contains_key(account) {
            return true;
        }
        match self.scope.lock().expect("vault scope").as_ref() {
            Some(scope) => scope.accounts.contains(account),
            None => true,
        }
    }

    /// 先写文件再更新内存：写失败时内存仍是磁盘上的旧状态。
    fn commit(&self, loaded: &mut Loaded, next: Contents) -> Result<(), SecretError> {
        write_vault(&self.path, &loaded.key, &next)?;
        loaded.contents = next;
        Ok(())
    }

    /// 新值或删除取代了旧条目：清掉旧钥匙串条目，只做一次。
    /// 从来没有旧条目的账户（新建连接等）不访问旧钥匙串，返回 false。
    fn retire_legacy(&self, next: &mut Contents, account: &str) -> bool {
        if !self.may_have_legacy(next, account) {
            return false;
        }
        if let Some(legacy) = self.legacy.as_ref() {
            let _ = legacy.delete(account);
        }
        next.legacy_failed.remove(account);
        next.legacy_done.insert(account.to_owned());
        true
    }

    fn ensure_loaded<'a>(&self, guard: &'a mut Option<Loaded>) -> Result<&'a mut Loaded, SecretError> {
        let mut outcome = Ok(());
        if guard.is_none() {
            let (loaded, load_outcome) = match self.load() {
                Ok(pair) => pair,
                Err(err) => {
                    if let SecretError::Unavailable(reason) = &err {
                        self.note_key_unavailable(reason);
                    }
                    return Err(err);
                }
            };
            // 之前的运行里没迁入的旧条目：重启后也要让界面知道，并提供重试。
            if !loaded.contents.legacy_failed.is_empty() {
                let mut notices = self.notices.lock().expect("vault notices");
                for (account, reason) in &loaded.contents.legacy_failed {
                    notices.push(VaultNotice::MigrationFailed { account: account.clone(), reason: reason.clone() });
                }
            }
            *guard = Some(loaded);
            outcome = load_outcome;
        }
        let loaded = guard.as_mut().expect("vault loaded");
        // 损坏报告优先：迁移写盘失败不能把“原文件已改名保留”的说明盖掉。
        // 迁移失败时名单保持未迁移，下次访问再跑。
        let migrated = self.migrate_scope(loaded);
        outcome?;
        migrated?;
        Ok(loaded)
    }

    /// 读主密钥和 vault 文件。外层 Err 表示没有可用状态（不缓存，下次重试）；
    /// 内层 Err 表示已经得到可用的空保险库，但本次调用仍要把损坏报告给调用方。
    fn load(&self) -> Result<(Loaded, Result<(), SecretError>), SecretError> {
        let file = match fs::read(&self.path) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => return Err(SecretError::Unavailable(format!("读不了保险库文件：{err}"))),
        };
        if let Some(bytes) = file.as_deref() {
            // 更新版本写的文件：既不能解也不能覆盖。
            if bytes.len() >= HEADER_LEN && bytes.starts_with(MAGIC) && bytes[MAGIC.len()] > VERSION {
                return Err(SecretError::Unavailable("保险库由更新版本的 RillSQL 写入，请升级后再试。".to_owned()));
            }
        }
        let stored = self
            .master
            .get(MASTER_KEY_ACCOUNT)
            .map_err(|err| SecretError::Unavailable(format!("密钥暂不可用：{err}")))?;
        let key = match stored {
            Some(text) => Some(decode_key(&text)?),
            None => None,
        };
        match (key, file) {
            (Some(key), Some(bytes)) => match open_vault(&bytes, &key) {
                Some(contents) => Ok((Loaded { key, contents }, Ok(()))),
                None => {
                    let kept = self.keep_corrupt()?;
                    Ok((Loaded { key, contents: Contents::default() }, Err(self.corrupt_error(kept, false))))
                }
            },
            (Some(key), None) => Ok((Loaded { key, contents: Contents::default() }, Ok(()))),
            (None, file) => {
                // 主密钥不存在：首次运行，或钥匙串条目被删掉（此时旧 vault 已无法解密）。
                // 先把新密钥写进钥匙串，成功后才动旧文件。
                let key = random_key()?;
                self.master
                    .put(MASTER_KEY_ACCOUNT, &base64::engine::general_purpose::STANDARD.encode(key))
                    .map_err(|err| SecretError::Unavailable(format!("密钥暂不可用：{err}")))?;
                let outcome = match file {
                    Some(_) => Err(self.corrupt_error(self.keep_corrupt()?, true)),
                    None => Ok(()),
                };
                Ok((Loaded { key, contents: Contents::default() }, outcome))
            }
        }
    }

    /// 同一原因的“密钥暂不可用”只排一条待提示，界面不会被重复的授权失败刷屏。
    fn note_key_unavailable(&self, reason: &str) {
        let mut notices = self.notices.lock().expect("vault notices");
        let duplicate = notices.iter().any(|notice| matches!(notice, VaultNotice::KeyUnavailable { reason: seen } if seen == reason));
        if !duplicate {
            notices.push(VaultNotice::KeyUnavailable { reason: reason.to_owned() });
        }
    }

    fn keep_corrupt(&self) -> Result<PathBuf, SecretError> {
        let stamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
        let name = self.path.file_name().and_then(|name| name.to_str()).unwrap_or(VAULT_FILE).to_owned();
        let mut kept = self.path.with_file_name(format!("{name}.corrupt-{stamp}"));
        let mut suffix = 1;
        while kept.exists() {
            kept = self.path.with_file_name(format!("{name}.corrupt-{stamp}-{suffix}"));
            suffix += 1;
        }
        fs::rename(&self.path, &kept).map_err(|err| SecretError::Corrupt(format!("保险库无法解密，且没能改名保留：{err}")))?;
        self.notices.lock().expect("vault notices").push(VaultNotice::Corrupt { kept: kept.clone() });
        Ok(kept)
    }

    fn corrupt_error(&self, kept: PathBuf, key_missing: bool) -> SecretError {
        if key_missing {
            // 主密钥条目找不到：文件本身可能完好，只是钥匙串暂时没给出密钥。
            return SecretError::Corrupt(format!(
                "钥匙串里找不到保险库主密钥，原保险库已保留为 {}。如果主密钥能从钥匙串备份中恢复，退出 RillSQL 后把它改回 {} 即可找回已保存的密码。",
                kept.display(),
                VAULT_FILE
            ));
        }
        SecretError::Corrupt(format!("保险库无法解密，原文件已保留为 {}。", kept.display()))
    }
}

impl SecretStore for VaultStore {
    fn put(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        let mut guard = self.state.lock().expect("vault state");
        let loaded = self.ensure_loaded(&mut guard)?;
        let mut next = loaded.contents.clone();
        next.secrets.insert(account.to_owned(), secret.to_owned());
        self.retire_legacy(&mut next, account);
        self.commit(loaded, next)
    }

    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        let mut guard = self.state.lock().expect("vault state");
        let loaded = self.ensure_loaded(&mut guard)?;
        if let Some(secret) = loaded.contents.secrets.get(account) {
            return Ok(Some(secret.clone()));
        }
        if let Some(reason) = loaded.contents.legacy_failed.get(account) {
            return Err(SecretError::MigrationFailed(reason.clone()));
        }
        if !self.may_have_legacy(&loaded.contents, account) {
            return Ok(None);
        }
        let report = self.migrate_locked(loaded, std::slice::from_ref(&account.to_owned()), false)?;
        if let Some((_, reason)) = report.failed.first() {
            return Err(SecretError::MigrationFailed(reason.clone()));
        }
        Ok(loaded.contents.secrets.get(account).cloned())
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        let mut guard = self.state.lock().expect("vault state");
        let loaded = self.ensure_loaded(&mut guard)?;
        let mut next = loaded.contents.clone();
        let removed = next.secrets.remove(account).is_some();
        let retired = self.retire_legacy(&mut next, account);
        if !removed && !retired {
            return Ok(());
        }
        self.commit(loaded, next)
    }

    fn register_legacy(&self, accounts: Vec<String>) {
        *self.scope.lock().expect("vault scope") = Some(LegacyScope { accounts: accounts.into_iter().collect(), migrated: false });
    }

    /// 重新读取之前失败的旧条目，迁入成功的写进 vault 并删除旧条目。
    fn retry_failed_migration(&self) -> Result<MigrationReport, SecretError> {
        let mut guard = self.state.lock().expect("vault state");
        let loaded = self.ensure_loaded(&mut guard)?;
        let accounts: Vec<String> = loaded.contents.legacy_failed.keys().cloned().collect();
        if accounts.is_empty() {
            return Ok(MigrationReport::default());
        }
        self.migrate_locked(loaded, &accounts, true)
    }

    fn migration_failures(&self) -> Result<Vec<(String, String)>, SecretError> {
        let mut guard = self.state.lock().expect("vault state");
        let loaded = self.ensure_loaded(&mut guard)?;
        Ok(loaded.contents.legacy_failed.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }

    fn take_notices(&self) -> Vec<VaultNotice> {
        std::mem::take(&mut *self.notices.lock().expect("vault notices"))
    }

    fn vault_path(&self) -> Option<&Path> {
        Some(&self.path)
    }
}

pub fn vault_path_for(config_path: &Path) -> PathBuf {
    match config_path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(VAULT_FILE),
        _ => PathBuf::from(VAULT_FILE),
    }
}

fn decode_key(text: &str) -> Result<[u8; KEY_LEN], SecretError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|_| SecretError::Unavailable("钥匙串里的保险库主密钥格式无效。".to_owned()))?;
    bytes
        .try_into()
        .map_err(|_| SecretError::Unavailable("钥匙串里的保险库主密钥长度无效。".to_owned()))
}

fn random_key() -> Result<[u8; KEY_LEN], SecretError> {
    let mut key = [0u8; KEY_LEN];
    getrandom::fill(&mut key).map_err(|err| SecretError::Unavailable(format!("系统随机数不可用：{err}")))?;
    Ok(key)
}

fn aad() -> [u8; HEADER_LEN] {
    let mut aad = [0u8; HEADER_LEN];
    aad[..MAGIC.len()].copy_from_slice(MAGIC);
    aad[MAGIC.len()] = VERSION;
    aad
}

#[allow(deprecated)] // XNonce 仍来自 generic-array 0.14，与 export.rs 相同。
fn seal(key: &[u8; KEY_LEN], contents: &Contents) -> Result<Vec<u8>, SecretError> {
    let plain = serde_json::to_vec(contents).map_err(|err| SecretError::Unavailable(format!("序列化保险库失败：{err}")))?;
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|err| SecretError::Unavailable(format!("系统随机数不可用：{err}")))?;
    let header = aad();
    let cipher = XChaCha20Poly1305::new(key.into());
    let sealed = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: &plain, aad: &header })
        .map_err(|_| SecretError::Unavailable("加密保险库失败。".to_owned()))?;
    let mut file = Vec::with_capacity(HEADER_LEN + NONCE_LEN + sealed.len());
    file.extend_from_slice(&header);
    file.extend_from_slice(&nonce);
    file.extend_from_slice(&sealed);
    Ok(file)
}

/// 魔数、版本、长度、认证或 JSON 任一不对都视为损坏。
#[allow(deprecated)]
fn open_vault(file: &[u8], key: &[u8; KEY_LEN]) -> Option<Contents> {
    if file.len() < HEADER_LEN + NONCE_LEN + TAG_LEN || !file.starts_with(MAGIC) || file[MAGIC.len()] != VERSION {
        return None;
    }
    let (header, rest) = file.split_at(HEADER_LEN);
    let (nonce, sealed) = rest.split_at(NONCE_LEN);
    let cipher = XChaCha20Poly1305::new(key.into());
    let plain = cipher.decrypt(XNonce::from_slice(nonce), Payload { msg: sealed, aad: header }).ok()?;
    serde_json::from_slice(&plain).ok()
}

/// 先写同目录临时文件并落盘，再原子改名覆盖；中途失败时旧文件保持原样。
fn write_vault(path: &Path, key: &[u8; KEY_LEN], contents: &Contents) -> Result<(), SecretError> {
    let bytes = seal(key, contents)?;
    let dir = path.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(|err| SecretError::Unavailable(format!("建不了保险库目录：{err}")))?;
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or(VAULT_FILE);
    let tmp = dir.join(format!(".{name}.tmp-{}", uuid::Uuid::new_v4().simple()));
    let result = (|| -> std::io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut handle = options.open(&tmp)?;
        handle.write_all(&bytes)?;
        handle.sync_all()?;
        drop(handle);
        fs::rename(&tmp, path)
    })();
    if let Err(err) = result {
        let _ = fs::remove_file(&tmp);
        return Err(SecretError::Unavailable(format!("写不了保险库文件：{err}")));
    }
    Ok(())
}

#[cfg(test)]
#[path = "vault_tests.rs"]
mod tests;
