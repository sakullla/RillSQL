use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::connections::model::{password_account, ssh_password_account};
use crate::connections::secrets::{MemoryStore, UnavailableStore, VaultProblem};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rillsql-vault-{name}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// 计数的后端：记录 get/put/delete 次数，可指定某些账户读取失败。
#[derive(Default)]
struct Counting {
    inner: MemoryStore,
    gets: AtomicUsize,
    puts: AtomicUsize,
    deletes: AtomicUsize,
    failing: Mutex<HashSet<String>>,
}

impl Counting {
    fn with(entries: &[(&str, &str)]) -> Arc<Self> {
        let store = Arc::new(Self::default());
        for (account, secret) in entries {
            store.inner.put(account, secret).unwrap();
        }
        store
    }

    fn fail(&self, account: &str) {
        self.failing.lock().unwrap().insert(account.to_owned());
    }

    fn values(&self) -> HashMap<String, String> {
        self.inner.dump().into_iter().collect()
    }
}

impl SecretStore for Counting {
    fn put(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        self.puts.fetch_add(1, Ordering::SeqCst);
        self.inner.put(account, secret)
    }

    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        if self.failing.lock().unwrap().contains(account) {
            return Err(SecretError::Unavailable("用户拒绝了钥匙串授权".to_owned()));
        }
        self.inner.get(account)
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        self.inner.delete(account)
    }
}

fn vault(dir: &std::path::Path, master: Arc<dyn SecretStore>, legacy: Option<Arc<dyn SecretStore>>) -> VaultStore {
    VaultStore::new(dir.join(VAULT_FILE), master, legacy)
}

#[test]
fn put_get_delete_roundtrip_survives_reopen_and_keeps_secrets_out_of_the_file() {
    let dir = temp_dir("roundtrip");
    let master = Counting::with(&[]);
    let store = vault(&dir, master.clone(), None);
    store.put("conn:a:password", "db-secret-1").unwrap();
    store.put("conn:a:ssh-password", "ssh-secret-1").unwrap();
    assert_eq!(store.get("conn:a:password").unwrap().as_deref(), Some("db-secret-1"));
    assert_eq!(store.get("conn:missing:password").unwrap(), None);

    let raw = fs::read(dir.join(VAULT_FILE)).unwrap();
    assert!(raw.starts_with(b"RILLSQLV"));
    let text = String::from_utf8_lossy(&raw);
    assert!(!text.contains("db-secret-1"));
    assert!(!text.contains("conn:a:password"));
    // 钥匙串里只有主密钥一条。
    assert_eq!(master.values().keys().cloned().collect::<Vec<_>>(), vec![MASTER_KEY_ACCOUNT.to_owned()]);

    let reopened = vault(&dir, master.clone(), None);
    assert_eq!(reopened.get("conn:a:ssh-password").unwrap().as_deref(), Some("ssh-secret-1"));
    reopened.delete("conn:a:password").unwrap();
    assert_eq!(reopened.get("conn:a:password").unwrap(), None);

    let again = vault(&dir, master, None);
    assert_eq!(again.get("conn:a:password").unwrap(), None);
    assert_eq!(again.get("conn:a:ssh-password").unwrap().as_deref(), Some("ssh-secret-1"));
    // 没留下临时文件。
    let names: Vec<String> = fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, vec![VAULT_FILE.to_owned()]);
}

#[test]
fn repeated_gets_read_the_master_key_backend_once() {
    let dir = temp_dir("cache");
    let master = Counting::with(&[]);
    vault(&dir, master.clone(), None).put("conn:a:password", "s").unwrap();

    let master_reads_before = master.gets.load(Ordering::SeqCst);
    let store = vault(&dir, master.clone(), None);
    assert_eq!(master.gets.load(Ordering::SeqCst), master_reads_before, "构造时不读钥匙串");
    for _ in 0..10 {
        assert_eq!(store.get("conn:a:password").unwrap().as_deref(), Some("s"));
    }
    store.put("conn:b:password", "t").unwrap();
    store.delete("conn:b:password").unwrap();
    assert_eq!(master.gets.load(Ordering::SeqCst), master_reads_before + 1);
    assert_eq!(master.puts.load(Ordering::SeqCst), 1, "只在首次运行写一次主密钥");
}

#[test]
fn corrupt_vault_is_renamed_kept_and_reported_as_recoverable() {
    let dir = temp_dir("corrupt");
    let master = Counting::with(&[]);
    vault(&dir, master.clone(), None).put("conn:a:password", "s").unwrap();
    let path = dir.join(VAULT_FILE);
    let mut bytes = fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&path, &bytes).unwrap();

    let store = vault(&dir, master.clone(), None);
    let err = store.get("conn:a:password").unwrap_err();
    assert!(matches!(err, SecretError::Corrupt(_)), "{err:?}");
    let notices = store.take_notices();
    let kept = match notices.as_slice() {
        [VaultNotice::Corrupt { kept }] => kept.clone(),
        other => panic!("unexpected notices {other:?}"),
    };
    assert!(kept.file_name().unwrap().to_string_lossy().starts_with("secrets.vault.corrupt-"));
    assert_eq!(fs::read(&kept).unwrap(), bytes, "原文件原样保留");
    assert!(!path.exists());

    // 之后可以继续用：重新输入的密码写进新的保险库，主密钥不变。
    assert_eq!(store.get("conn:a:password").unwrap(), None);
    store.put("conn:a:password", "new").unwrap();
    assert_eq!(vault(&dir, master.clone(), None).get("conn:a:password").unwrap().as_deref(), Some("new"));
    assert_eq!(master.puts.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read(&kept).unwrap(), bytes);
}

#[test]
fn garbage_file_counts_as_corrupt_too() {
    let dir = temp_dir("garbage");
    let master = Counting::with(&[]);
    vault(&dir, master.clone(), None).put("x", "y").unwrap();
    fs::write(dir.join(VAULT_FILE), b"not a vault").unwrap();
    let store = vault(&dir, master, None);
    assert!(matches!(store.put("x", "z"), Err(SecretError::Corrupt(_))));
    store.put("x", "z").unwrap();
    assert_eq!(store.get("x").unwrap().as_deref(), Some("z"));
}

#[test]
fn unavailable_master_key_backend_never_rewrites_the_vault() {
    let dir = temp_dir("unavailable");
    let master = Counting::with(&[]);
    vault(&dir, master, None).put("conn:a:password", "s").unwrap();
    let path = dir.join(VAULT_FILE);
    let before = fs::read(&path).unwrap();

    let denied = vault(&dir, Arc::new(UnavailableStore), None);
    assert!(matches!(denied.get("conn:a:password"), Err(SecretError::Unavailable(_))));
    assert!(matches!(denied.put("conn:b:password", "t"), Err(SecretError::Unavailable(_))));
    assert!(matches!(denied.delete("conn:a:password"), Err(SecretError::Unavailable(_))));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);

    // 首次运行（没有 vault）钥匙串也不可用：不建文件。
    let fresh = temp_dir("unavailable-fresh");
    let store = vault(&fresh, Arc::new(UnavailableStore), None);
    assert!(store.put("conn:a:password", "s").is_err());
    assert_eq!(fs::read_dir(&fresh).unwrap().count(), 0);
}

#[test]
fn migration_moves_two_of_three_legacy_entries_and_reports_the_failure() {
    let dir = temp_dir("migrate");
    let master = Counting::with(&[]);
    let a = password_account("a");
    let b = ssh_password_account("a");
    let c = password_account("c");
    let legacy = Counting::with(&[(&a, "pa"), (&b, "sa"), (&c, "pc")]);
    legacy.fail(&c);

    let store = vault(&dir, master.clone(), Some(legacy.clone()));
    let report = store.migrate(&[a.clone(), b.clone(), c.clone()]).unwrap();
    assert_eq!(report.migrated, vec![a.clone(), b.clone()]);
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].0, c);
    assert!(report.failed[0].1.contains("拒绝"));
    assert_eq!(store.take_notices(), vec![VaultNotice::MigrationFailed { account: c.clone(), reason: report.failed[0].1.clone() }]);

    assert_eq!(store.get(&a).unwrap().as_deref(), Some("pa"));
    assert_eq!(store.get(&b).unwrap().as_deref(), Some("sa"));
    assert!(store.get(&c).is_err(), "失败的账户如实报错，由调用方改为手动输入");
    let left = legacy.values();
    assert!(!left.contains_key(&a) && !left.contains_key(&b), "已迁入的旧条目被删除");
    assert_eq!(left.get(&c).map(String::as_str), Some("pc"), "失败的旧条目原样保留");
    assert_eq!(store.migration_failures().unwrap().len(), 1);

    // 重新启动：不重复读旧条目。
    let gets_before = legacy.gets.load(Ordering::SeqCst);
    let restarted = vault(&dir, master.clone(), Some(legacy.clone()));
    let report = restarted.migrate(&[a.clone(), b.clone(), c.clone()]).unwrap();
    assert_eq!(report, MigrationReport::default());
    assert_eq!(restarted.get(&a).unwrap().as_deref(), Some("pa"));
    assert!(restarted.get(&c).is_err());
    assert_eq!(legacy.gets.load(Ordering::SeqCst), gets_before);
    assert_eq!(restarted.migration_failures().unwrap(), vec![(c.clone(), "系统凭据库不可用：用户拒绝了钥匙串授权".to_owned())]);

    // 用户点“重试”且授权恢复后可以迁入。
    legacy.failing.lock().unwrap().clear();
    restarted.retry_failed_migration().unwrap();
    assert_eq!(restarted.get(&c).unwrap().as_deref(), Some("pc"));
    assert!(legacy.values().is_empty());
    assert!(restarted.migration_failures().unwrap().is_empty());
}

#[test]
fn get_migrates_a_missing_account_lazily_once() {
    let dir = temp_dir("lazy");
    let master = Counting::with(&[]);
    let a = password_account("a");
    let gone = password_account("gone");
    let legacy = Counting::with(&[(&a, "pa")]);
    let store = vault(&dir, master.clone(), Some(legacy.clone()));
    assert_eq!(store.get(&a).unwrap().as_deref(), Some("pa"));
    assert_eq!(store.get(&gone).unwrap(), None);
    assert_eq!(legacy.gets.load(Ordering::SeqCst), 2);
    for _ in 0..3 {
        store.get(&a).unwrap();
        store.get(&gone).unwrap();
    }
    let restarted = vault(&dir, master, Some(legacy.clone()));
    assert_eq!(restarted.get(&a).unwrap().as_deref(), Some("pa"));
    assert_eq!(restarted.get(&gone).unwrap(), None);
    assert_eq!(legacy.gets.load(Ordering::SeqCst), 2, "每个账户最多读一次旧条目");
    assert!(legacy.values().is_empty());
}

#[test]
fn put_and_delete_retire_the_legacy_entry() {
    let dir = temp_dir("retire");
    let a = password_account("a");
    let b = password_account("b");
    let legacy = Counting::with(&[(&a, "old-a"), (&b, "old-b")]);
    let store = vault(&dir, Counting::with(&[]), Some(legacy.clone()));
    store.put(&a, "new-a").unwrap();
    store.delete(&b).unwrap();
    assert!(legacy.values().is_empty());
    assert_eq!(store.get(&a).unwrap().as_deref(), Some("new-a"));
    assert_eq!(store.get(&b).unwrap(), None);
    assert_eq!(legacy.gets.load(Ordering::SeqCst), 0);
}

#[test]
fn missing_master_key_with_an_existing_vault_keeps_the_old_file() {
    let dir = temp_dir("lost-key");
    let master = Counting::with(&[]);
    vault(&dir, master.clone(), None).put("x", "y").unwrap();
    let before = fs::read(dir.join(VAULT_FILE)).unwrap();
    master.delete(MASTER_KEY_ACCOUNT).unwrap();

    let store = vault(&dir, master.clone(), None);
    assert!(matches!(store.get("x"), Err(SecretError::Corrupt(_))));
    let kept = match store.take_notices().as_slice() {
        [VaultNotice::Corrupt { kept }] => kept.clone(),
        other => panic!("unexpected notices {other:?}"),
    };
    assert_eq!(fs::read(kept).unwrap(), before);
    store.put("x", "z").unwrap();
    assert_eq!(vault(&dir, master, None).get("x").unwrap().as_deref(), Some("z"));
}

#[test]
fn vault_sits_next_to_the_connection_store() {
    let config = PathBuf::from("/data/RillSQL/connections.sqlite");
    assert_eq!(vault_path_for(&config), PathBuf::from("/data/RillSQL").join(VAULT_FILE));
    assert_eq!(vault_path_for(std::path::Path::new("connections.sqlite")), PathBuf::from(VAULT_FILE));
}

#[test]
fn registered_accounts_migrate_eagerly_on_first_load_and_only_once() {
    let dir = temp_dir("eager");
    let master = Counting::with(&[]);
    let a = password_account("a");
    let b = ssh_password_account("a");
    let c = password_account("c");
    let legacy = Counting::with(&[(&a, "pa"), (&b, "sa"), (&c, "pc")]);
    legacy.fail(&c);

    let store = vault(&dir, master.clone(), Some(legacy.clone()));
    store.register_legacy(vec![a.clone(), b.clone(), c.clone()]);
    assert_eq!(legacy.gets.load(Ordering::SeqCst), 0, "登记不访问钥匙串");
    // 第一次用到保险库（读一个不相干的账户）就把整份名单迁完。
    assert_eq!(store.get("conn:other:password").unwrap(), None);
    assert_eq!(legacy.gets.load(Ordering::SeqCst), 3);
    let left = legacy.values();
    assert!(!left.contains_key(&a) && !left.contains_key(&b), "写入 vault 后才删旧条目");
    assert_eq!(left.get(&c).map(String::as_str), Some("pc"), "失败的旧条目保留");
    assert_eq!(store.get(&a).unwrap().as_deref(), Some("pa"));
    assert!(matches!(store.get(&c), Err(SecretError::MigrationFailed(_))));
    assert!(matches!(store.take_notices().as_slice(), [VaultNotice::MigrationFailed { account, .. }] if account == &c));
    assert_eq!(legacy.gets.load(Ordering::SeqCst), 3, "失败的账户不自动反复重试");

    // 重启后再次登记：不再读旧条目。
    let restarted = vault(&dir, master.clone(), Some(legacy.clone()));
    restarted.register_legacy(vec![a.clone(), b.clone(), c.clone()]);
    assert_eq!(restarted.get(&b).unwrap().as_deref(), Some("sa"));
    assert_eq!(legacy.gets.load(Ordering::SeqCst), 3);
    assert_eq!(restarted.migration_failures().unwrap().len(), 1);

    // 授权恢复后重试：迁入并删除旧条目。
    legacy.failing.lock().unwrap().clear();
    let report = restarted.retry_failed_migration().unwrap();
    assert_eq!(report.migrated, vec![c.clone()]);
    assert_eq!(restarted.get(&c).unwrap().as_deref(), Some("pc"));
    assert!(legacy.values().is_empty());
    assert!(restarted.migration_failures().unwrap().is_empty());
}

#[test]
fn accounts_outside_the_registered_list_never_touch_the_legacy_keychain() {
    let dir = temp_dir("scope");
    let legacy = Counting::with(&[]);
    let store = vault(&dir, Counting::with(&[]), Some(legacy.clone()));
    store.register_legacy(Vec::new());
    let fresh = password_account("fresh");
    store.put(&fresh, "s").unwrap();
    assert_eq!(store.get(&fresh).unwrap().as_deref(), Some("s"));
    store.delete(&fresh).unwrap();
    assert_eq!(store.get(&fresh).unwrap(), None);
    store.delete(&password_account("never-saved")).unwrap();
    assert_eq!(legacy.gets.load(Ordering::SeqCst), 0);
    assert_eq!(legacy.puts.load(Ordering::SeqCst), 0);
    assert_eq!(legacy.deletes.load(Ordering::SeqCst), 0);
}

#[cfg(unix)]
#[test]
fn legacy_entries_stay_when_the_vault_cannot_be_written() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("readonly");
    let a = password_account("a");
    let legacy = Counting::with(&[(&a, "pa")]);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let store = vault(&dir, Counting::with(&[]), Some(legacy.clone()));
    store.register_legacy(vec![a.clone()]);
    let result = store.get(&a);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(result, Err(SecretError::Vault { problem: VaultProblem::FileAccess, .. })), "{result:?}");
    assert_eq!(legacy.values().get(&a).map(String::as_str), Some("pa"), "vault 没写成就不删旧条目");
    assert_eq!(legacy.deletes.load(Ordering::SeqCst), 0);
}

/// 旧钥匙串读完就把目录改成只读：模拟“损坏文件已改名保留，随后迁移写盘失败”。
#[cfg(unix)]
struct FreezeOnRead {
    dir: PathBuf,
    inner: Arc<Counting>,
    frozen: std::sync::atomic::AtomicBool,
}

#[cfg(unix)]
impl SecretStore for FreezeOnRead {
    fn put(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        self.inner.put(account, secret)
    }

    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        use std::os::unix::fs::PermissionsExt;
        if !self.frozen.swap(true, Ordering::SeqCst) {
            fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o500)).unwrap();
        }
        self.inner.get(account)
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.inner.delete(account)
    }
}

#[cfg(unix)]
#[test]
fn corrupt_report_is_not_masked_by_a_failed_migration_write() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("corrupt-masked");
    let master = Counting::with(&[]);
    vault(&dir, master.clone(), None).put("x", "y").unwrap();
    fs::write(dir.join(VAULT_FILE), b"not a vault").unwrap();
    let a = password_account("a");
    let legacy = Arc::new(FreezeOnRead { dir: dir.clone(), inner: Counting::with(&[(&a, "pa")]), frozen: Default::default() });
    let store = vault(&dir, master, Some(legacy.clone()));
    store.register_legacy(vec![a.clone()]);
    let first = store.get("x");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(first, Err(SecretError::Corrupt(_))), "损坏说明优先：{first:?}");
    assert!(matches!(store.take_notices().as_slice(), [VaultNotice::Corrupt { .. }]));
    // 迁移没写成：名单仍待迁移，下次访问时再跑并迁入。
    assert_eq!(store.get(&a).unwrap().as_deref(), Some("pa"));
    assert!(legacy.inner.values().is_empty());
}

#[test]
fn denied_master_key_queues_a_single_key_unavailable_notice() {
    let dir = temp_dir("denied-notice");
    let store = vault(&dir, Arc::new(UnavailableStore), None);
    assert!(store.get("conn:a:password").is_err());
    assert!(store.get("conn:a:password").is_err());
    let notices = store.take_notices();
    // 原因只是钥匙串给出的底层信息，不带“密钥暂不可用”之类的前缀（前缀由界面模板按语言补）。
    assert_eq!(
        notices,
        vec![VaultNotice::LoadFailed { problem: VaultProblem::KeyUnavailable, reason: "Secret Service 未运行".to_owned() }]
    );
    assert!(store.take_notices().is_empty());
}

/// 三类加载失败各归各的类别：钥匙串不可用、文件读不出、更新版本写入；文件都原样保留。
#[test]
fn load_failures_are_classified_and_leave_the_file_alone() {
    // 文件读不出：vault 路径是个目录。
    let dir = temp_dir("unreadable");
    fs::create_dir_all(dir.join(VAULT_FILE)).unwrap();
    let master = Counting::with(&[]);
    let store = vault(&dir, master.clone(), None);
    let err = store.get("x").unwrap_err();
    assert!(matches!(err, SecretError::Vault { problem: VaultProblem::FileAccess, .. }), "{err:?}");
    assert!(matches!(store.take_notices().as_slice(), [VaultNotice::LoadFailed { problem: VaultProblem::FileAccess, .. }]));
    assert_eq!(master.puts.load(Ordering::SeqCst), 0, "读不出文件时不生成新主密钥");
    assert!(dir.join(VAULT_FILE).is_dir());

    // 更新版本写入：魔数相同、版本更高。
    let dir = temp_dir("newer");
    let mut newer = b"RILLSQLV".to_vec();
    newer.push(VERSION + 1);
    newer.extend_from_slice(&[0u8; 64]);
    fs::write(dir.join(VAULT_FILE), &newer).unwrap();
    let master = Counting::with(&[]);
    let store = vault(&dir, master.clone(), None);
    let err = store.put("x", "y").unwrap_err();
    assert!(matches!(err, SecretError::Vault { problem: VaultProblem::NewerVersion, .. }), "{err:?}");
    assert!(matches!(store.take_notices().as_slice(), [VaultNotice::LoadFailed { problem: VaultProblem::NewerVersion, .. }]));
    assert_eq!(fs::read(dir.join(VAULT_FILE)).unwrap(), newer, "不改名也不改写");
    assert_eq!(master.gets.load(Ordering::SeqCst) + master.puts.load(Ordering::SeqCst), 0, "不访问钥匙串");

    // 钥匙串不可用：类别是 KeyUnavailable，原因不带前缀。
    let dir = temp_dir("key-unavailable");
    let store = vault(&dir, Arc::new(UnavailableStore), None);
    let err = store.get("x").unwrap_err();
    assert_eq!(err.reason(), "Secret Service 未运行");
    assert_eq!(err.vault_failure().map(|failure| failure.problem), Some(VaultProblem::KeyUnavailable));
}

/// 钥匙串里的主密钥格式或长度无效：换成新主密钥，原保险库改名保留，提示重新输入密码。
#[test]
fn invalid_master_key_is_replaced_and_the_old_vault_kept() {
    for bad in ["not base64 !!", "c2hvcnQ="] {
        let dir = temp_dir("invalid-key");
        let master = Counting::with(&[]);
        vault(&dir, master.clone(), None).put("conn:a:password", "s").unwrap();
        let before = fs::read(dir.join(VAULT_FILE)).unwrap();
        master.inner.put(MASTER_KEY_ACCOUNT, bad).unwrap();

        let store = vault(&dir, master.clone(), None);
        let err = store.get("conn:a:password").unwrap_err();
        assert!(matches!(err, SecretError::Corrupt(_)), "{err:?}");
        let kept = match store.take_notices().as_slice() {
            [VaultNotice::KeyInvalid { kept }] => kept.clone(),
            other => panic!("unexpected notices {other:?}"),
        };
        assert_eq!(fs::read(&kept).unwrap(), before, "原保险库原样保留");
        let replaced = master.values().get(MASTER_KEY_ACCOUNT).cloned().unwrap();
        assert_ne!(replaced, bad);
        assert!(decode_key(&replaced).is_some(), "换上的是有效主密钥");
        // 重新输入的密码能写进新保险库。
        store.put("conn:a:password", "new").unwrap();
        assert_eq!(vault(&dir, master, None).get("conn:a:password").unwrap().as_deref(), Some("new"));
    }
}

/// 加载失败后某次访问成功：排出 LoadRecovered，界面据此收起旧提示。
#[test]
fn later_successful_load_reports_recovery() {
    let dir = temp_dir("recovered");
    let master = Counting::with(&[]);
    master.fail(MASTER_KEY_ACCOUNT);
    let store = vault(&dir, master.clone(), None);
    assert!(store.get("x").is_err());
    assert!(matches!(store.take_notices().as_slice(), [VaultNotice::LoadFailed { problem: VaultProblem::KeyUnavailable, .. }]));
    master.failing.lock().unwrap().clear();
    store.put("x", "y").unwrap();
    assert_eq!(store.take_notices(), vec![VaultNotice::LoadRecovered]);
    // 一直成功时不再排。
    let fresh = vault(&dir, master, None);
    assert_eq!(fresh.get("x").unwrap().as_deref(), Some("y"));
    assert!(fresh.take_notices().is_empty());
}

/// 损坏的 vault 改名失败：仍排出加载失败事件（CorruptNotKept），不会悄无声息。
#[cfg(unix)]
#[test]
fn corrupt_vault_that_cannot_be_renamed_is_still_reported() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("corrupt-not-kept");
    let master = Counting::with(&[]);
    vault(&dir, master.clone(), None).put("x", "y").unwrap();
    fs::write(dir.join(VAULT_FILE), b"not a vault").unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let store = vault(&dir, master, None);
    let result = store.get("x");
    let notices = store.take_notices();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(result, Err(SecretError::Vault { problem: VaultProblem::CorruptNotKept, .. })), "{result:?}");
    assert!(matches!(notices.as_slice(), [VaultNotice::LoadFailed { problem: VaultProblem::CorruptNotKept, .. }]), "{notices:?}");
    assert_eq!(fs::read(dir.join(VAULT_FILE)).unwrap(), b"not a vault");
}

/// 迁移失败的账户被用户重新保存后不再算失败：排出 MigrationRecovered。
#[test]
fn saving_over_a_failed_migration_reports_it_recovered() {
    let dir = temp_dir("retire-recovered");
    let a = password_account("a");
    let legacy = Counting::with(&[(&a, "pa")]);
    legacy.fail(&a);
    let store = vault(&dir, Counting::with(&[]), Some(legacy.clone()));
    store.register_legacy(vec![a.clone()]);
    assert!(store.get(&a).is_err());
    assert!(matches!(store.take_notices().as_slice(), [VaultNotice::MigrationFailed { .. }]));
    store.put(&a, "typed").unwrap();
    assert_eq!(store.take_notices(), vec![VaultNotice::MigrationRecovered { account: a.clone() }]);
    assert!(store.migration_failures().unwrap().is_empty());
}

#[test]
fn retry_reports_recovered_accounts_for_the_ui() {
    let dir = temp_dir("retry-notice");
    let a = password_account("a");
    let legacy = Counting::with(&[(&a, "pa")]);
    legacy.fail(&a);
    let store = vault(&dir, Counting::with(&[]), Some(legacy.clone()));
    store.register_legacy(vec![a.clone()]);
    assert!(store.get(&a).is_err());
    assert!(matches!(store.take_notices().as_slice(), [VaultNotice::MigrationFailed { .. }]));
    legacy.failing.lock().unwrap().clear();
    store.retry_failed_migration().unwrap();
    assert_eq!(store.take_notices(), vec![VaultNotice::MigrationRecovered { account: a.clone() }]);
}
