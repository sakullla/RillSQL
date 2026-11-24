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
//! 旧版逐条钥匙串条目（`conn:{id}:password` 等）按需迁移：vault 里没有某个账户且元数据没记过它时，
//! 读一次旧条目，迁入并删除旧条目；失败的逐个记进元数据并报告，不会每次启动都重复迁移。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};

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

/// 需要在界面上汇总提示的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultNotice {
    /// 保险库无法解密，原文件已改名保留。
    Corrupt { kept: PathBuf },
    /// 某个旧钥匙串条目没能迁入。
    MigrationFailed { account: String, reason: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub migrated: Vec<String>,
    /// 旧钥匙串里没有这个条目，无需迁移。
    pub absent: Vec<String>,
    pub failed: Vec<(String, String)>,
}

pub struct VaultStore {
    path: PathBuf,
    /// 只保存主密钥的后端：正式运行是系统钥匙串，测试用内存实现。
    master: Arc<dyn SecretStore>,
    /// 旧版逐条条目所在的后端；没有则不迁移。
    legacy: Option<Arc<dyn SecretStore>>,
    state: Mutex<Option<Loaded>>,
    notices: Mutex<Vec<VaultNotice>>,
}

impl VaultStore {
    pub fn new(path: PathBuf, master: Arc<dyn SecretStore>, legacy: Option<Arc<dyn SecretStore>>) -> Self {
        Self {
            path,
            master,
            legacy,
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

    /// 取走待提示的事件（损坏、迁移失败）。
    #[allow(dead_code)] // 界面汇总与“重试”入口；正式运行目前靠 get 按需迁移。
    pub fn take_notices(&self) -> Vec<VaultNotice> {
        std::mem::take(&mut *self.notices.lock().expect("vault notices"))
    }

    /// 元数据里记录的迁移失败（跨重启保留），供界面汇总。
    #[allow(dead_code)] // 界面汇总与“重试”入口；正式运行目前靠 get 按需迁移。
    pub fn migration_failures(&self) -> Result<Vec<(String, String)>, SecretError> {
        let mut guard = self.state.lock().expect("vault state");
        let loaded = self.ensure_loaded(&mut guard)?;
        Ok(loaded.contents.legacy_failed.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }

    /// 清掉失败记录，下次按需迁移时重新尝试这些账户（界面上的“重试”）。
    #[allow(dead_code)] // 界面汇总与“重试”入口；正式运行目前靠 get 按需迁移。
    pub fn retry_failed_migration(&self) -> Result<(), SecretError> {
        let mut guard = self.state.lock().expect("vault state");
        let loaded = self.ensure_loaded(&mut guard)?;
        if loaded.contents.legacy_failed.is_empty() {
            return Ok(());
        }
        let mut next = loaded.contents.clone();
        next.legacy_failed.clear();
        self.commit(loaded, next)
    }

    /// 批量迁移旧钥匙串条目：每个账户最多读一次旧条目。
    /// vault 写入成功后才删除已迁入的旧条目；失败的逐个报告并记进元数据。
    #[allow(dead_code)] // 界面汇总与“重试”入口；正式运行目前靠 get 按需迁移。
    pub fn migrate(&self, accounts: &[String]) -> Result<MigrationReport, SecretError> {
        let mut guard = self.state.lock().expect("vault state");
        let loaded = self.ensure_loaded(&mut guard)?;
        self.migrate_locked(loaded, accounts)
    }

    fn migrate_locked(&self, loaded: &mut Loaded, accounts: &[String]) -> Result<MigrationReport, SecretError> {
        let mut report = MigrationReport::default();
        let Some(legacy) = self.legacy.as_ref() else {
            return Ok(report);
        };
        let mut next = loaded.contents.clone();
        for account in accounts {
            if next.secrets.contains_key(account) || next.legacy_done.contains(account) || next.legacy_failed.contains_key(account) {
                continue;
            }
            match legacy.get(account) {
                Ok(Some(secret)) => {
                    next.secrets.insert(account.clone(), secret);
                    next.legacy_done.insert(account.clone());
                    report.migrated.push(account.clone());
                }
                Ok(None) => {
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
        self.commit(loaded, next)?;
        for account in &report.migrated {
            // 已经安全写进 vault；旧条目删不掉只是残留，不影响读取。
            let _ = legacy.delete(account);
        }
        if !report.failed.is_empty() {
            let mut notices = self.notices.lock().expect("vault notices");
            for (account, reason) in &report.failed {
                notices.push(VaultNotice::MigrationFailed { account: account.clone(), reason: reason.clone() });
            }
        }
        Ok(report)
    }

    /// 先写文件再更新内存：写失败时内存仍是磁盘上的旧状态。
    fn commit(&self, loaded: &mut Loaded, next: Contents) -> Result<(), SecretError> {
        write_vault(&self.path, &loaded.key, &next)?;
        loaded.contents = next;
        Ok(())
    }

    /// 新值或删除取代了旧条目：顺手清掉旧钥匙串条目，只做一次。
    fn retire_legacy(&self, next: &mut Contents, account: &str) {
        if next.legacy_done.contains(account) {
            return;
        }
        if let Some(legacy) = self.legacy.as_ref() {
            let _ = legacy.delete(account);
        }
        next.legacy_failed.remove(account);
        next.legacy_done.insert(account.to_owned());
    }

    fn ensure_loaded<'a>(&self, guard: &'a mut Option<Loaded>) -> Result<&'a mut Loaded, SecretError> {
        if guard.is_none() {
            let (loaded, outcome) = self.load()?;
            *guard = Some(loaded);
            outcome?;
        }
        Ok(guard.as_mut().expect("vault loaded"))
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
                    Ok((Loaded { key, contents: Contents::default() }, Err(self.corrupt_error(kept))))
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
                    Some(_) => Err(self.corrupt_error(self.keep_corrupt()?)),
                    None => Ok(()),
                };
                Ok((Loaded { key, contents: Contents::default() }, outcome))
            }
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

    fn corrupt_error(&self, kept: PathBuf) -> SecretError {
        SecretError::Corrupt(format!("保险库无法解密，原文件已保留为 {}。请重新输入密码。", kept.display()))
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
            return Err(SecretError::Unavailable(format!("旧凭据没能迁入保险库：{reason}")));
        }
        let report = self.migrate_locked(loaded, std::slice::from_ref(&account.to_owned()))?;
        if let Some((_, reason)) = report.failed.first() {
            return Err(SecretError::Unavailable(format!("旧凭据没能迁入保险库：{reason}")));
        }
        Ok(loaded.contents.secrets.get(account).cloned())
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        let mut guard = self.state.lock().expect("vault state");
        let loaded = self.ensure_loaded(&mut guard)?;
        let mut next = loaded.contents.clone();
        let removed = next.secrets.remove(account).is_some();
        let pending_legacy = !next.legacy_done.contains(account);
        if !removed && !pending_legacy {
            return Ok(());
        }
        self.retire_legacy(&mut next, account);
        self.commit(loaded, next)
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
