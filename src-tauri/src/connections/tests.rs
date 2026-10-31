use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::forward::{Forwarder, RecordingOpener};
use super::hostkey::{self, Authenticator, HostKeyDecision};
use super::model::{self, leaked_secret, CaMode, ConnectionDraft, Engine, Secret, SecretInput, SshAuthKind, TlsMode};
use super::route::{self, Retry};
use super::secrets::{MemoryStore, UnavailableStore};
use super::service::{DatabaseProbe, Probes, Service, SshProbe};
use super::CatalogSession;
use super::stages::{self, StageError, StageKind, StageRunner, StageStatus, TestStatus};
use super::store::persisted_bytes;

fn temp_workspace() -> PathBuf {
    let path = std::env::temp_dir().join(format!("rillsql-conn-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn remote_draft(engine: Engine) -> ConnectionDraft {
    let mut draft = ConnectionDraft::blank(engine);
    draft.name = format!("{} 仓库", engine.label_zh());
    draft.host = "db.example.test".to_owned();
    draft.database = "app".to_owned();
    draft.username = "app_user".to_owned();
    draft.tls_mode = TlsMode::Verify;
    draft.save_password = true;
    draft
}

fn secrets(password: &str, ssh_password: &str, passphrase: &str) -> SecretInput {
    SecretInput {
        password: Some(Secret::new(password)),
        ssh_password: Some(Secret::new(ssh_password)),
        ssh_passphrase: Some(Secret::new(passphrase)),
        save_password: true,
    }
}

fn assert_secret_absent(blob: &str, input: &SecretInput) {
    assert!(leaked_secret(blob, input).is_none(), "{blob}");
}

struct CountingAuth {
    calls: usize,
}

impl Authenticator for CountingAuth {
    fn authenticate(&mut self) -> Result<(), String> {
        self.calls += 1;
        Ok(())
    }
}

struct ScriptSsh {
    fingerprint: Mutex<String>,
    auth_calls: AtomicUsize,
    tunnel_host: Mutex<String>,
    tunnel_port: Mutex<u16>,
}

impl ScriptSsh {
    fn new(fingerprint: &str) -> Self {
        Self {
            fingerprint: Mutex::new(fingerprint.to_owned()),
            auth_calls: AtomicUsize::new(0),
            tunnel_host: Mutex::new(String::new()),
            tunnel_port: Mutex::new(0),
        }
    }
}

#[async_trait::async_trait]
impl SshProbe for ScriptSsh {
    async fn presented_key(&self, _ssh: &model::SshSettings) -> Result<hostkey::PresentedKey, StageError> {
        Ok(hostkey::PresentedKey {
            algorithm: "ssh-ed25519".to_owned(),
            fingerprint: self.fingerprint.lock().expect("fp").clone(),
        })
    }

    async fn authenticate(&self, _ssh: &model::SshSettings, _password: Option<&str>, _passphrase: Option<&str>) -> Result<String, StageError> {
        self.auth_calls.fetch_add(1, Ordering::SeqCst);
        Ok("密码认证成功".to_owned())
    }

    async fn open_tunnel(&self, db_host: &str, db_port: u16, tls_server_name: &str, bastion_host: &str) -> Result<String, StageError> {
        assert_ne!(db_host, "127.0.0.1");
        assert_ne!(db_host, bastion_host);
        assert_ne!(tls_server_name, "127.0.0.1");
        assert_ne!(tls_server_name, bastion_host);
        *self.tunnel_host.lock().expect("host") = db_host.to_owned();
        *self.tunnel_port.lock().expect("port") = db_port;
        Ok(format!("127.0.0.1:1 → {db_host}:{db_port}（每条本地连接单独一条 direct-tcpip）"))
    }
}

struct ScriptDb {
    fail_tls: bool,
    tls_calls: AtomicUsize,
    auth_calls: AtomicUsize,
    version_calls: AtomicUsize,
    seen_password: Mutex<Option<String>>,
    seen_transport: Mutex<Option<String>>,
    seen_tls_name: Mutex<Option<String>>,
    seen_channel: Mutex<Option<String>>,
}

impl ScriptDb {
    fn new(fail_tls: bool) -> Self {
        Self {
            fail_tls,
            tls_calls: AtomicUsize::new(0),
            auth_calls: AtomicUsize::new(0),
            version_calls: AtomicUsize::new(0),
            seen_password: Mutex::new(None),
            seen_transport: Mutex::new(None),
            seen_tls_name: Mutex::new(None),
            seen_channel: Mutex::new(None),
        }
    }
}

#[async_trait::async_trait]
impl DatabaseProbe for ScriptDb {
    async fn check_tls(&self, route: &route::Route) -> Result<String, StageError> {
        self.tls_calls.fetch_add(1, Ordering::SeqCst);
        *self.seen_transport.lock().expect("transport") = Some(route.transport_host.clone());
        *self.seen_tls_name.lock().expect("name") = route.tls_server_name.clone();
        *self.seen_channel.lock().expect("channel") = route.channel_target_host.clone();
        if self.fail_tls {
            return Err(stages::fail(
                "证书签发给 localhost, 127.0.0.1，期望 db.example.test",
                "连接已终止。没有改用 localhost 身份校验，也没有降级为不验证证书或不加密后重试。",
            ));
        }
        let _ = route::on_tls_failure(route.tls_mode);
        Ok(format!("证书主机名 {} 已验证", route.tls_server_name.clone().unwrap_or_default()))
    }

    async fn authenticate(&self, _config: &model::SavedConnection, _route: &route::Route, password: &str) -> Result<String, StageError> {
        self.auth_calls.fetch_add(1, Ordering::SeqCst);
        *self.seen_password.lock().expect("password") = Some(password.to_owned());
        Ok("用户 app_user".to_owned())
    }

    async fn server_version(&self, config: &model::SavedConnection, _route: &route::Route) -> Result<String, StageError> {
        self.version_calls.fetch_add(1, Ordering::SeqCst);
        Ok(format!("{} 8.4.3", config.engine.label_zh()))
    }
}

#[test]
fn host_key_blocks_credentials_until_the_fingerprint_is_trusted() {
    let mut auth = CountingAuth { calls: 0 };
    let unconfirmed = hostkey::decide_host_key(None, "ssh-ed25519", "SHA256:new");
    assert!(matches!(unconfirmed, HostKeyDecision::Unconfirmed { .. }));
    assert!(hostkey::authenticate_after(&unconfirmed, &mut auth).is_err());
    let changed = hostkey::decide_host_key(Some("SHA256:saved"), "ssh-ed25519", "SHA256:other");
    let error = hostkey::authenticate_after(&changed, &mut auth).unwrap_err();
    assert!(error.contains("没有发送任何凭据"));
    assert_eq!(auth.calls, 0);
    let trusted = hostkey::decide_host_key(Some("SHA256:saved"), "ssh-ed25519", "SHA256:saved");
    assert!(hostkey::credentials_allowed(&trusted));
    hostkey::authenticate_after(&trusted, &mut auth).unwrap();
    assert_eq!(auth.calls, 1);
}

#[test]
fn tls_plans_keep_the_database_name_when_tcp_uses_the_forwarder() {
    let mut draft = remote_draft(Engine::Postgres);
    draft.ssh_on = true;
    draft.ssh_host = "bastion.example.com".to_owned();
    draft.ssh_user = "jump".to_owned();
    let saved = model::normalize(&draft, "pg".to_owned()).unwrap();
    let route = route::resolve_route(&saved, Some(52814)).unwrap();
    assert_eq!(route.transport_host, "127.0.0.1");
    assert_eq!(route.transport_port, 52814);
    assert_eq!(route.tls_server_name.as_deref(), Some("db.example.test"));
    assert_eq!(route::channel_target(&route), Some(("db.example.test".to_owned(), 5432)));
    assert_eq!(route.bastion_host.as_deref(), Some("bastion.example.com"));
    let mysql = route::mysql_tls_plan(&route);
    assert_eq!(mysql.tcp_host, "127.0.0.1");
    assert_eq!(mysql.hostname_override.as_deref(), Some("db.example.test"));
    assert!(!mysql.skip_domain_validation);
    assert!(!mysql.accept_invalid_certs);
    let postgres = route::postgres_tls_plan(&route).unwrap();
    assert_eq!(postgres.host, "db.example.test");
    assert_eq!(postgres.hostaddr, Some(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    assert!(postgres.verify_hostname);
    assert!(matches!(route::on_tls_failure(TlsMode::Verify), Retry::Stop { .. }));
    let again = route::mysql_tls_plan(&route);
    assert_eq!(again, mysql);

    let direct = route::resolve_route(&model::normalize(&remote_draft(Engine::Mysql), "my".to_owned()).unwrap(), None).unwrap();
    let direct_mysql = route::mysql_tls_plan(&direct);
    assert!(direct_mysql.hostname_override.is_none());
    assert_eq!(direct_mysql.tcp_host, "db.example.test");
    let direct_pg = route::postgres_tls_plan(&direct).unwrap();
    assert!(direct_pg.hostaddr.is_none());
    assert_eq!(direct_pg.host, "db.example.test");
}

#[test]
fn sqlite_has_no_tls_and_mysql_stays_distinct_from_mariadb() {
    let mut sqlite = ConnectionDraft::blank(Engine::Sqlite);
    sqlite.file_path = "C:/data/notes.sqlite".to_owned();
    sqlite.tls_mode = TlsMode::Verify;
    sqlite.ssh_on = true;
    let saved = model::normalize(&sqlite, "file".to_owned()).unwrap();
    assert!(saved.tls.is_none());
    assert!(saved.ssh.is_none());
    assert_eq!(saved.env, "local");
    assert!(route::resolve_route(&saved, None).unwrap().tls_server_name.is_none());
    let mysql = model::normalize(&remote_draft(Engine::Mysql), "my".to_owned()).unwrap();
    let maria = model::normalize(&remote_draft(Engine::Mariadb), "ma".to_owned()).unwrap();
    assert_ne!(mysql.engine, maria.engine);
    assert_eq!(mysql.engine, Engine::Mysql);
    assert_eq!(maria.engine, Engine::Mariadb);
    assert!(super::drivers::version_note(Engine::Mysql, "10.11.6-MariaDB").contains("仍保持为 MySQL"));
    assert!(super::drivers::version_note(Engine::Mariadb, "8.4.3").contains("仍保持为 MariaDB"));
}

#[tokio::test]
async fn stages_stop_before_later_work() {
    struct Stop {
        calls: Vec<StageKind>,
    }
    #[async_trait::async_trait]
    impl StageRunner for Stop {
        async fn run_stage(&mut self, stage: StageKind) -> Result<String, StageError> {
            self.calls.push(stage);
            if stage == StageKind::Tls {
                return Err(stages::fail("证书主机名不匹配", "不会关闭验证后重试"));
            }
            Ok("ok".to_owned())
        }
    }
    let mut draft = remote_draft(Engine::Postgres);
    draft.ssh_on = true;
    draft.ssh_host = "bastion.example.com".to_owned();
    draft.ssh_user = "jump".to_owned();
    let saved = model::normalize(&draft, "pg".to_owned()).unwrap();
    let planned = stages::planned_stages(&saved);
    let mut runner = Stop { calls: Vec::new() };
    let report = stages::execute_stages(&planned, &mut runner).await;
    assert_eq!(report.status, TestStatus::Err);
    assert_eq!(runner.calls, vec![StageKind::SshHostIdentity, StageKind::SshAuthentication, StageKind::Tunnel, StageKind::Tls]);
    assert!(report.stages.iter().any(|stage| stage.kind == StageKind::DatabaseAuthentication && stage.status == StageStatus::Skipped));
    assert!(report.stages.iter().any(|stage| stage.kind == StageKind::ServerVersion && stage.message == "未执行"));
}

#[tokio::test]
async fn each_local_accept_opens_one_channel_to_the_database() {
    let forwarder = Forwarder::bind().await.unwrap();
    let port = forwarder.local_port().unwrap();
    let opener = Arc::new(RecordingOpener::new());
    let task_opener = opener.clone();
    let serve = tokio::spawn(async move {
        forwarder
            .serve_n(task_opener, "10.1.2.3", 5432, "db.example.test", "bastion.example.com", 2)
            .await
            .unwrap();
    });
    let first = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let second = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    serve.await.unwrap();
    drop((first, second));
    let opens = opener.opens.lock().expect("opens").clone();
    assert_eq!(opens.len(), 2);
    assert_ne!(opens[0].id, opens[1].id);
    assert!(opens.iter().all(|item| item.host == "10.1.2.3" && item.port == 5432));
}

#[tokio::test]
async fn saved_config_survives_restart_and_delete_keeps_the_sqlite_file() {
    let dir = temp_workspace();
    let config_path = dir.join("connections.sqlite");
    let database = dir.join("notes.sqlite");
    std::fs::write(&database, b"sqlite-bytes").unwrap();
    let secret_store = Arc::new(MemoryStore::default());
    let input = secrets("db-secret-alpha", "ssh-secret-beta", "key-pass-gamma");
    let service = Service::new(config_path.clone(), secret_store.clone());
    let mut draft = remote_draft(Engine::Postgres);
    draft.ssh_on = true;
    draft.ssh_host = "bastion.example.com".to_owned();
    draft.ssh_port = "22".to_owned();
    draft.ssh_user = "jump".to_owned();
    draft.ssh_auth = SshAuthKind::Password;
    let saved = service.save(draft, input.clone()).await.unwrap();
    assert!(saved.connection.password_saved);
    assert!(saved.connection.ssh_password_saved);
    let mut file = ConnectionDraft::blank(Engine::Sqlite);
    file.file_path = database.display().to_string();
    file.name = "笔记".to_owned();
    let file_saved = service.save(file, SecretInput::default()).await.unwrap();
    drop(service);

    let reopened = Service::new(config_path.clone(), secret_store.clone());
    let listed = reopened.list().await.unwrap();
    assert_eq!(listed.len(), 2);
    let blob = String::from_utf8_lossy(&persisted_bytes(&config_path)).into_owned();
    assert_secret_absent(&blob, &input);
    assert_secret_absent(&reopened.logs().join("\n"), &input);
    let dumped = secret_store.dump();
    assert!(dumped.iter().any(|(account, value)| account.ends_with(":password") && value == "db-secret-alpha"));
    assert!(dumped.iter().any(|(account, value)| account.ends_with(":ssh-password") && value == "ssh-secret-beta"));
    assert!(dumped.iter().all(|(_, value)| value != "key-pass-gamma"));

    let deleted = reopened.delete(&file_saved.connection.id).await.unwrap();
    assert!(!deleted.database_file_removed);
    assert!(database.is_file());
    assert!(std::fs::read(&database).unwrap() == b"sqlite-bytes");
    assert!(reopened.list().await.unwrap().iter().all(|item| item.saved.engine != Engine::Sqlite));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn unavailable_keyring_keeps_the_typed_password_out_of_config() {
    let dir = temp_workspace();
    let config_path = dir.join("connections.sqlite");
    let service = Service::new(config_path.clone(), Arc::new(UnavailableStore));
    let input = secrets("typed-db-secret", "typed-ssh-secret", "typed-passphrase");
    let saved = service.save(remote_draft(Engine::Mysql), input.clone()).await.unwrap();
    assert!(!saved.connection.password_saved);
    assert!(saved.note.as_deref().unwrap_or("").contains("没有写入配置"));
    let json = serde_json::to_string(&saved.connection).unwrap();
    assert_secret_absent(&json, &input);
    assert_secret_absent(&String::from_utf8_lossy(&persisted_bytes(&config_path)), &input);
    assert_secret_absent(&service.logs().join("\n"), &input);

    let probe = Arc::new(ScriptDb::new(false));
    let ssh = Arc::new(ScriptSsh::new("SHA256:unused"));
    let probed = Service::with_probes(
        config_path,
        Arc::new(UnavailableStore),
        Probes {
            ssh: Some(ssh),
            database: Some(probe.clone()),
            sqlite: None,
            health: None,
        },
    );
    let mut draft = remote_draft(Engine::Mysql);
    draft.id = Some(saved.connection.id.clone());
    probed.save(draft, SecretInput { save_password: false, ..input.clone() }).await.unwrap();
    let report = probed.test(&saved.connection.id, input.clone()).await.unwrap();
    assert_eq!(report.status, TestStatus::Ok);
    assert_eq!(probe.seen_password.lock().expect("pw").as_deref(), Some("typed-db-secret"));
    assert_secret_absent(&probed.logs().join("\n"), &input);
    assert_eq!(probe.seen_transport.lock().expect("t").as_deref(), Some("db.example.test"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn tls_failure_skips_authentication_and_uses_the_database_name_over_ssh() {
    let dir = temp_workspace();
    let config_path = dir.join("connections.sqlite");
    let store = Arc::new(MemoryStore::default());
    let probe = Arc::new(ScriptDb::new(true));
    let ssh = Arc::new(ScriptSsh::new("SHA256:trusted"));
    let service = Service::with_probes(
        config_path,
        store.clone(),
        Probes {
            ssh: Some(ssh.clone()),
            database: Some(probe.clone()),
            sqlite: None,
            health: None,
        },
    );
    let mut draft = remote_draft(Engine::Postgres);
    draft.ssh_on = true;
    draft.ssh_host = "bastion.example.com".to_owned();
    draft.ssh_user = "jump".to_owned();
    draft.ssh_auth = SshAuthKind::Password;
    let input = secrets("db-secret-alpha", "ssh-secret-beta", "key-pass-gamma");
    let saved = service.save(draft, input).await.unwrap();
    service.confirm_host_key(&saved.connection.id, "SHA256:missing").await.unwrap_err();
    let waiting = service.test(&saved.connection.id, SecretInput::default()).await.unwrap();
    assert_eq!(waiting.status, TestStatus::Wait);
    assert_eq!(ssh.auth_calls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.auth_calls.load(Ordering::SeqCst), 0);
    service.confirm_host_key(&saved.connection.id, &waiting.fingerprint.unwrap()).await.unwrap();
    let failed = service.test(&saved.connection.id, SecretInput::default()).await.unwrap();
    assert_eq!(failed.status, TestStatus::Err);
    assert_eq!(probe.tls_calls.load(Ordering::SeqCst), 1);
    assert_eq!(probe.auth_calls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.version_calls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.seen_transport.lock().expect("t").as_deref(), Some("127.0.0.1"));
    assert_eq!(probe.seen_tls_name.lock().expect("n").as_deref(), Some("db.example.test"));
    assert_eq!(probe.seen_channel.lock().expect("c").as_deref(), Some("db.example.test"));
    assert_eq!(ssh.tunnel_host.lock().expect("h").as_str(), "db.example.test");
    assert_eq!(*ssh.tunnel_port.lock().expect("p"), 5432);
    assert!(failed.stages.iter().any(|stage| stage.kind == StageKind::DatabaseAuthentication && stage.status == StageStatus::Skipped));
    *ssh.fingerprint.lock().expect("fp") = "SHA256:changed".to_owned();
    let calls = ssh.auth_calls.load(Ordering::SeqCst);
    let changed = service.test(&saved.connection.id, SecretInput::default()).await.unwrap();
    assert_eq!(changed.status, TestStatus::Err);
    assert!(changed.hint.unwrap_or_default().contains("没有发送任何凭据"));
    assert_eq!(ssh.auth_calls.load(Ordering::SeqCst), calls);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn deleting_one_connection_keeps_a_shared_host_key_and_does_not_create_a_missing_sqlite_file() {
    let dir = temp_workspace();
    let config_path = dir.join("connections.sqlite");
    let service = Service::new(config_path, Arc::new(MemoryStore::default()));
    let mut first = remote_draft(Engine::Mysql);
    first.ssh_on = true;
    first.ssh_host = "bastion.example.com".to_owned();
    first.ssh_user = "jump".to_owned();
    first.save_password = false;
    let mut second = first.clone();
    second.name = "第二台".to_owned();
    second.host = "other.example.test".to_owned();
    let left = service.save(first, SecretInput::default()).await.unwrap();
    let right = service.save(second, SecretInput::default()).await.unwrap();
    service
        .confirm_host_key(&left.connection.id, "nope")
        .await
        .unwrap_err();
    let ssh = Arc::new(ScriptSsh::new("SHA256:shared"));
    let probed = Service::with_probes(service.config_path().to_path_buf(), Arc::new(MemoryStore::default()), Probes {
        ssh: Some(ssh),
        database: None,
        sqlite: None,
        health: None,
    });
    // 主机密钥存在另一份配置库里，这里直接写入当前库。
    let store = super::store::Store::open(service.config_path()).await.unwrap();
    store.save_host_key("bastion.example.com", 22, "ssh-ed25519", "SHA256:shared").await.unwrap();
    drop(store);
    probed.delete(&left.connection.id).await.unwrap();
    let store = super::store::Store::open(service.config_path()).await.unwrap();
    assert_eq!(store.host_key("bastion.example.com", 22).await.unwrap().as_deref(), Some("SHA256:shared"));
    probed.delete(&right.connection.id).await.unwrap();
    assert!(store.host_key("bastion.example.com", 22).await.unwrap().is_none());

    let missing = dir.join("missing.sqlite");
    let mut file = ConnectionDraft::blank(Engine::Sqlite);
    file.file_path = missing.display().to_string();
    let saved = probed.save(file, SecretInput::default()).await.unwrap();
    let report = probed.test(&saved.connection.id, SecretInput::default()).await.unwrap();
    assert_eq!(report.status, TestStatus::Err);
    assert_eq!(report.fail_stage.as_deref(), Some(StageKind::OpenFile.label_zh()));
    assert!(report.stages.iter().any(|stage| stage.kind == StageKind::ReadSchema && stage.status == StageStatus::Skipped));
    assert!(!Path::new(&missing).exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn focusing_another_postgres_database_does_not_rewrite_the_saved_config() {
    let dir = temp_workspace();
    let config_path = dir.join("connections.sqlite");
    let probe = Arc::new(RememberDatabase::default());
    let service = Service::with_probes(
        config_path,
        Arc::new(MemoryStore::default()),
        Probes {
            ssh: None,
            database: Some(probe.clone()),
            sqlite: None,
            health: None,
        },
    );
    let saved = service.save(remote_draft(Engine::Postgres), secrets("db-secret-alpha", "", "")).await.unwrap();
    let id = saved.connection.id.clone();
    assert!(service.open(&id, SecretInput::default()).await.unwrap().connected);

    let missing = service.focus_database(&id, "missing").await.unwrap_err();
    assert!(missing.contains("数据库不存在"), "{missing}");
    assert!(missing.contains("原来的会话还在"), "{missing}");
    assert!(service.is_connected(&id));

    service.focus_database(&id, "rillsql").await.unwrap();
    assert!(service.is_connected(&id));
    let listed = service.list().await.unwrap();
    assert_eq!(listed[0].saved.database.as_deref(), Some("app"));
    assert_eq!(probe.seen.lock().expect("seen").as_slice(), ["app", "missing", "rillsql"]);
    service.disconnect(&id);
    let closed = service.focus_database(&id, "rillsql").await.unwrap_err();
    assert!(closed.contains("还没有打开"), "{closed}");

    let mysql = service.save(remote_draft(Engine::Mysql), secrets("db-secret-alpha", "", "")).await.unwrap();
    let rejected = service.focus_database(&mysql.connection.id, "other").await.unwrap_err();
    assert!(rejected.contains("只有 PostgreSQL"), "{rejected}");
    assert_eq!(service.focus_database(&id, "  ").await.unwrap_err(), "需要数据库名。");
    let idle = service.focus_database("missing-id", "rillsql").await.unwrap_err();
    assert!(idle.contains("找不到这条连接配置") || idle.contains("还没有打开"), "{idle}");
    let _ = std::fs::remove_dir_all(dir);
}

#[derive(Default)]
struct RememberDatabase {
    seen: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl DatabaseProbe for RememberDatabase {
    async fn check_tls(&self, route: &route::Route) -> Result<String, StageError> {
        Ok(format!("证书主机名 {} 已验证", route.tls_server_name.clone().unwrap_or_default()))
    }

    async fn authenticate(&self, config: &model::SavedConnection, _route: &route::Route, _password: &str) -> Result<String, StageError> {
        let database = config.database.clone().unwrap_or_default();
        self.seen.lock().expect("seen").push(database.clone());
        if database == "missing" {
            return Err(stages::fail("数据库不存在", "没有改连。"));
        }
        Ok("用户 app_user".to_owned())
    }

    async fn server_version(&self, config: &model::SavedConnection, _route: &route::Route) -> Result<String, StageError> {
        Ok(format!("{} 16", config.engine.label_zh()))
    }
}

#[tokio::test]
async fn stored_password_is_reused_without_being_copied_into_the_config() {
    let dir = temp_workspace();
    let config_path = dir.join("connections.sqlite");
    let secret_store = Arc::new(MemoryStore::default());
    let probe = Arc::new(ScriptDb::new(false));
    let service = Service::with_probes(
        config_path.clone(),
        secret_store.clone(),
        Probes {
            ssh: None,
            database: Some(probe.clone()),
            sqlite: None,
            health: None,
        },
    );
    let input = secrets("db-secret-alpha", "", "");
    let saved = service.save(remote_draft(Engine::Mariadb), input.clone()).await.unwrap();
    assert_eq!(saved.connection.engine, Engine::Mariadb);
    assert_eq!(saved.connection.env, "dev");
    let report = service.test(&saved.connection.id, SecretInput::default()).await.unwrap();
    assert_eq!(report.status, TestStatus::Ok);
    assert_eq!(report.summary.as_deref(), Some("MariaDB 8.4.3"));
    assert_eq!(probe.seen_password.lock().expect("pw").as_deref(), Some("db-secret-alpha"));
    assert_secret_absent(&String::from_utf8_lossy(&persisted_bytes(&config_path)), &input);
    let opened = service.open(&saved.connection.id, SecretInput::default()).await.unwrap();
    assert!(opened.connected);
    assert!(service.is_connected(&saved.connection.id));
    service.disconnect(&saved.connection.id);
    assert!(!service.is_connected(&saved.connection.id));
    let _ = std::fs::remove_dir_all(dir);
}

fn assert_not_verified(message: &str) {
    assert!(!message.contains("已核对"), "{message}");
    assert!(message.contains("没有核对"), "{message}");
    assert!(message.contains("已加密"), "{message}");
}

#[tokio::test]
async fn only_verified_tls_success_says_the_certificate_was_checked() {
    let mut require = remote_draft(Engine::Mysql);
    require.tls_mode = TlsMode::Require;
    let require = model::normalize(&require, "require".to_owned()).unwrap();
    let require_ok = super::service::report_canned_remote(require.clone(), super::service::CannedRemote::Connected).await;
    assert_eq!(require_ok.status, TestStatus::Ok);
    let require_tls_ok = require_ok.stages.iter().find(|stage| stage.kind == StageKind::Tls).unwrap();
    assert_eq!(require_tls_ok.label, "加密连接");
    assert!(!require_tls_ok.label.contains("核对证书"));
    assert_eq!(require_tls_ok.message, "已加密，但没有核对证书和主机名");
    assert_not_verified(&require_tls_ok.message);

    let require_auth = super::service::report_canned_remote(require, super::service::CannedRemote::AuthenticationFailed).await;
    assert_eq!(require_auth.status, TestStatus::Err);
    assert_eq!(require_auth.fail_stage.as_deref(), Some(StageKind::DatabaseAuthentication.label_zh()));
    let require_tls = require_auth.stages.iter().find(|stage| stage.kind == StageKind::Tls).unwrap();
    assert_eq!(require_tls.status, StageStatus::Ok);
    assert_eq!(require_tls.label, "加密连接");
    assert!(!require_tls.label.contains("核对证书"));
    assert_not_verified(&require_tls.message);

    let verify = model::normalize(&remote_draft(Engine::Postgres), "verify".to_owned()).unwrap();
    let verify_ok = super::service::report_canned_remote(verify.clone(), super::service::CannedRemote::Connected).await;
    let verify_tls_ok = verify_ok.stages.iter().find(|stage| stage.kind == StageKind::Tls).unwrap();
    assert_eq!(verify_tls_ok.label, "加密连接并核对证书");
    assert_eq!(verify_tls_ok.message, "已核对证书，主机名 db.example.test 与证书一致");
    let verify_auth = super::service::report_canned_remote(verify, super::service::CannedRemote::AuthenticationFailed).await;
    let verify_tls = verify_auth.stages.iter().find(|stage| stage.kind == StageKind::Tls).unwrap();
    assert_eq!(verify_tls.status, StageStatus::Ok);
    assert_eq!(verify_tls.label, "加密连接并核对证书");
    assert_eq!(verify_tls.message, "已核对证书，主机名 db.example.test 与证书一致");
    assert_eq!(
        verify_auth
            .stages
            .iter()
            .find(|stage| stage.kind == StageKind::DatabaseAuthentication)
            .unwrap()
            .status,
        StageStatus::Err
    );
}

#[tokio::test]
async fn missing_password_does_not_mark_network_or_tls_successful() {
    let dir = temp_workspace();
    let service = Service::new(dir.join("connections.sqlite"), Arc::new(MemoryStore::default()));
    for (engine, mode) in [(Engine::Postgres, TlsMode::Verify), (Engine::Mysql, TlsMode::Require)] {
        let mut draft = remote_draft(engine);
        draft.tls_mode = mode;
        draft.save_password = false;
        let saved = service.save(draft, SecretInput::default()).await.unwrap();
        let report = service.test(&saved.connection.id, SecretInput::default()).await.unwrap();
        assert_eq!(report.status, TestStatus::Err, "{mode:?}");
        assert_eq!(report.fail_stage.as_deref(), Some(StageKind::DatabaseAuthentication.label_zh()));
        for kind in [StageKind::Network, StageKind::Tls] {
            let stage = report.stages.iter().find(|stage| stage.kind == kind).unwrap();
            assert_ne!(stage.status, StageStatus::Ok, "{mode:?} {kind:?} {}", stage.message);
            assert!(!stage.message.contains("已加密"), "{mode:?} {}", stage.message);
            assert!(!stage.message.contains("已核对"), "{mode:?} {}", stage.message);
        }
        let tls = report.stages.iter().find(|stage| stage.kind == StageKind::Tls).unwrap();
        if mode == TlsMode::Require {
            assert!(!tls.label.contains("核对证书"), "{}", tls.label);
        }
        assert_eq!(
            report
                .stages
                .iter()
                .find(|stage| stage.kind == StageKind::DatabaseAuthentication)
                .unwrap()
                .status,
            StageStatus::Err
        );
        assert_eq!(
            report.stages.iter().find(|stage| stage.kind == StageKind::ServerVersion).unwrap().status,
            StageStatus::Skipped
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn users_keep_their_own_environment_tags() {
    let dir = temp_workspace();
    let path = dir.join("connections.sqlite");
    let service = Service::new(path.clone(), Arc::new(MemoryStore::default()));
    let tags = service.list_tags().await.unwrap();
    assert_eq!(
        tags.iter().map(|tag| (tag.id.as_str(), tag.label.as_str(), tag.tone.as_str())).collect::<Vec<_>>(),
        vec![("dev", "开发", "dev"), ("test", "测试", "test"), ("prod", "生产", "prod"), ("local", "本地", "local")]
    );

    let renamed = service.save_tag(Some("dev".into()), " 开发环境 ".into(), "prod".into()).await.unwrap();
    assert_eq!(renamed.id, "dev");
    assert_eq!(renamed.label, "开发环境");
    assert_eq!(renamed.tone, "prod");
    assert!(service.save_tag(None, "开发环境".into(), "dev".into()).await.unwrap_err().message().contains("同名"));
    assert!(service.save_tag(None, "  ".into(), "dev".into()).await.unwrap_err().message().contains("请填写"));
    assert!(service.save_tag(None, "预发".into(), "purple".into()).await.unwrap_err().message().contains("颜色"));
    assert!(service.save_tag(None, "无标签".into(), "dev".into()).await.unwrap_err().message().contains("无标签"));
    assert!(service
        .save_tag(None, "标".repeat(25), "dev".into())
        .await
        .unwrap_err()
        .message()
        .contains("24"));

    let created = service.save_tag(None, " 预发 ".into(), "test".into()).await.unwrap();
    assert_eq!(created.label, "预发");
    assert_eq!(created.tone, "test");
    assert_ne!(created.id, "none");

    let mut draft = remote_draft(Engine::Postgres);
    draft.save_password = false;
    draft.env = created.id.clone();
    let saved = service.save(draft, SecretInput::default()).await.unwrap();
    assert_eq!(saved.connection.env, created.id);
    let mut missing = remote_draft(Engine::Mysql);
    missing.save_password = false;
    missing.env = "missing-tag".into();
    let coerced = service.save(missing, SecretInput::default()).await.unwrap();
    assert_eq!(coerced.connection.env, "none");

    let catalogs = crate::catalog::commands::load_catalog(&service, 0, &[], None, false).await.unwrap();
    let preview = catalogs.iter().find(|item| item.id == saved.connection.id).unwrap();
    assert_eq!(preview.env_label, "预发");
    assert_eq!(preview.env_tone, "test");
    let cleared = catalogs.iter().find(|item| item.id == coerced.connection.id).unwrap();
    assert_eq!(cleared.env_label, "无标签");
    assert_eq!(cleared.env_tone, "none");

    service.delete_tag(&created.id).await.unwrap();
    let listed = service.list().await.unwrap();
    assert_eq!(listed.iter().find(|item| item.saved.id == saved.connection.id).unwrap().saved.env, "none");
    let after = crate::catalog::commands::load_catalog(&service, 0, &[], None, false).await.unwrap();
    let detached = after.iter().find(|item| item.id == saved.connection.id).unwrap();
    assert_eq!(detached.env, "none");
    assert_eq!(detached.env_label, "无标签");
    assert_eq!(detached.env_tone, "none");

    drop(service);
    {
        let store = super::store::Store::open(&path).await.unwrap();
        store.testing_set_env(&saved.connection.id, "ghost").await.unwrap();
    }
    let service = Service::new(path.clone(), Arc::new(MemoryStore::default()));
    let ghost = service.list().await.unwrap();
    assert_eq!(ghost.iter().find(|item| item.saved.id == saved.connection.id).unwrap().saved.env, "ghost");
    let shown = crate::catalog::commands::load_catalog(&service, 0, &[], None, false).await.unwrap();
    let ghost_row = shown.iter().find(|item| item.id == saved.connection.id).unwrap();
    assert_eq!(ghost_row.env_label, "无标签");
    assert_eq!(ghost_row.env_tone, "none");

    let renamed_local = service.save_tag(Some("local".into()), "本机".into(), "local".into()).await.unwrap();
    assert_eq!(renamed_local.id, "local");
    for id in ["dev", "test", "prod", "local"] {
        service.delete_tag(id).await.unwrap();
    }
    assert!(service.list_tags().await.unwrap().is_empty());
    drop(service);
    let reopened = Service::new(path, Arc::new(MemoryStore::default()));
    let kept = reopened.list_tags().await.unwrap();
    assert!(kept.is_empty(), "删光的标签不应在下次打开时被种回去");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn mysql_system_roots_come_from_the_os_store_and_ca_files_drop_them() {
    let native = rustls_native_certs::load_native_certs();
    assert!(!native.certs.is_empty(), "{:?}", native.errors);
    let expected: Vec<Vec<u8>> = native.certs.iter().map(|cert| cert.as_ref().to_vec()).collect();
    assert!(!mysql_async::SslOpts::default().disable_built_in_roots());

    for engine in [Engine::Mysql, Engine::Mariadb] {
        for mode in [TlsMode::Verify, TlsMode::Require] {
            let mut draft = remote_draft(engine);
            draft.tls_mode = mode;
            draft.ca_mode = CaMode::System;
            let saved = model::normalize(&draft, "system".to_owned()).unwrap();
            let route = route::resolve_route(&saved, None).unwrap();
            let ssl = super::drivers::mysql_ssl(&route::mysql_tls_plan(&route), saved.tls.as_ref()).unwrap();
            assert!(ssl.disable_built_in_roots(), "{engine:?} {mode:?} 仍会装入 webpki Mozilla 根");
            let mut actual = Vec::new();
            for cert in ssl.root_certs() {
                actual.push(cert.read().await.unwrap().into_owned());
            }
            assert_eq!(actual, expected, "{engine:?} {mode:?}");
        }
    }

    let ca = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/connections/fixtures/ca.pem");
    let mut file_draft = remote_draft(Engine::Mysql);
    file_draft.ca_mode = CaMode::File;
    file_draft.ca_file = ca.display().to_string();
    let saved = model::normalize(&file_draft, "file".to_owned()).unwrap();
    let route = route::resolve_route(&saved, None).unwrap();
    let ssl = super::drivers::mysql_ssl(&route::mysql_tls_plan(&route), saved.tls.as_ref()).unwrap();
    assert!(ssl.disable_built_in_roots());
    let roots = ssl.root_certs();
    assert_eq!(roots.len(), 1);
    let bytes = roots[0].read().await.unwrap().into_owned();
    assert_eq!(bytes, std::fs::read(&ca).unwrap());
    assert_ne!(bytes, expected.concat());
}

/// 与前端 draftFromSaved 对齐的测试助手：把导出文件里的连接变回保存草稿（不带任何机密）。
fn redraft(saved: &model::SavedConnection) -> ConnectionDraft {
    let mut draft = ConnectionDraft::blank(saved.engine);
    draft.id = Some(saved.id.clone());
    draft.name = saved.name.clone();
    draft.env = saved.env.clone();
    draft.save_password = false;
    if saved.engine == Engine::Sqlite {
        draft.file_path = saved.file_path.clone().unwrap_or_default();
        return draft;
    }
    draft.host = saved.host.clone().unwrap_or_default();
    draft.port = saved.port.map(|port| port.to_string()).unwrap_or_default();
    draft.database = saved.database.clone().unwrap_or_default();
    draft.username = saved.username.clone().unwrap_or_default();
    if let Some(tls) = &saved.tls {
        draft.tls_mode = tls.mode;
        draft.ca_mode = tls.ca_mode;
        draft.ca_file = tls.ca_file.clone().unwrap_or_default();
        draft.client_cert = tls.client_cert.clone().unwrap_or_default();
        draft.client_key = tls.client_key.clone().unwrap_or_default();
    }
    if let Some(ssh) = &saved.ssh {
        draft.ssh_on = true;
        draft.ssh_host = ssh.host.clone();
        draft.ssh_port = ssh.port.to_string();
        draft.ssh_user = ssh.username.clone();
        draft.ssh_auth = ssh.auth;
        draft.ssh_key = ssh.key_path.clone().unwrap_or_default();
    }
    draft
}

#[tokio::test]
async fn export_delete_import_roundtrip_restores_config_with_empty_password() {
    let dir = temp_workspace();
    let service = Service::new(dir.join("connections.sqlite"), Arc::new(MemoryStore::default()));

    let mut draft = remote_draft(Engine::Postgres);
    draft.env = "prod".to_owned();
    draft.tls_mode = TlsMode::Verify;
    draft.ca_mode = CaMode::File;
    draft.ca_file = "C:/certs/ca.pem".to_owned();
    draft.ssh_on = true;
    draft.ssh_host = "bastion.example.test".to_owned();
    draft.ssh_user = "jump".to_owned();
    draft.ssh_key = "C:/ssh/id_ed25519".to_owned();
    let saved = service.save(draft, secrets("db-secret-value", "ssh-secret-value", "key-secret-value")).await.unwrap();
    assert!(saved.connection.password_saved);
    let mut lite = ConnectionDraft::blank(Engine::Sqlite);
    lite.id = Some("lite-1".to_owned());
    lite.file_path = dir.join("notes.sqlite").display().to_string();
    service.save(lite, SecretInput::default()).await.unwrap();

    let export_path = dir.join("rillsql-connections.rillsqlcx");
    let count = service
        .export_connections(None, Some("导出口令"), &export_path)
        .await
        .unwrap();
    assert_eq!(count, 2);
    let file = std::fs::read(&export_path).unwrap();
    assert!(file.starts_with(super::export::MAGIC));
    assert!(!String::from_utf8_lossy(&file).contains("db-secret-value"));

    for id in [saved.connection.id.as_str(), "lite-1"] {
        let _ = service.delete(id).await;
    }
    assert!(service.list().await.unwrap().is_empty());

    // 错误口令整体拒绝：一条也不导入。
    let wrong = service.import_file(&export_path, Some("错误口令")).await.unwrap_err();
    assert!(wrong.contains("口令不正确"), "{wrong}");

    let parsed = service.import_file(&export_path, Some("导出口令")).await.unwrap();
    assert!(parsed.skipped.is_empty());
    assert_eq!(parsed.connections.len(), 2);
    for row in &parsed.connections {
        let outcome = service.save(redraft(row), SecretInput::default()).await.unwrap();
        assert!(!outcome.connection.password_saved);
        assert!(!outcome.connection.ssh_password_saved);
    }

    let restored = service.list().await.unwrap();
    let pg = restored.iter().find(|item| item.saved.name.contains("PostgreSQL")).unwrap();
    assert_eq!(pg.saved.env, "prod");
    let tls = pg.saved.tls.as_ref().unwrap();
    assert_eq!(tls.mode, TlsMode::Verify);
    assert_eq!(tls.ca_mode, CaMode::File);
    assert_eq!(tls.ca_file.as_deref(), Some("C:/certs/ca.pem"));
    let ssh = pg.saved.ssh.as_ref().unwrap();
    assert_eq!(ssh.host, "bastion.example.test");
    assert_eq!(ssh.auth, model::SshAuthKind::Key);
    assert_eq!(ssh.key_path.as_deref(), Some("C:/ssh/id_ed25519"));
    assert!(!pg.saved.password_saved);
    assert!(!pg.saved.ssh_password_saved);
    let _ = std::fs::remove_dir_all(dir);
}

struct FlakyHealth {
    fail: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl super::service::HealthProbe for FlakyHealth {
    async fn check(&self, _session: &CatalogSession) -> Result<(), String> {
        if self.fail.load(Ordering::SeqCst) {
            Err("connection reset by peer".to_owned())
        } else {
            Ok(())
        }
    }
}

/// 可切换失败的探针：认证或版本读取失败用来驱动确定性/瞬态重连分支。
struct SwitchingDb {
    fail_auth: std::sync::atomic::AtomicBool,
    fail_version: std::sync::atomic::AtomicBool,
    auth_calls: AtomicUsize,
    version_calls: AtomicUsize,
}

impl SwitchingDb {
    fn new() -> Self {
        Self {
            fail_auth: std::sync::atomic::AtomicBool::new(false),
            fail_version: std::sync::atomic::AtomicBool::new(false),
            auth_calls: AtomicUsize::new(0),
            version_calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl DatabaseProbe for SwitchingDb {
    async fn check_tls(&self, _route: &route::Route) -> Result<String, StageError> {
        Ok("证书主机名已验证".to_owned())
    }

    async fn authenticate(&self, _config: &model::SavedConnection, _route: &route::Route, _password: &str) -> Result<String, StageError> {
        self.auth_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_auth.load(Ordering::SeqCst) {
            return Err(stages::fail("Access denied for user 'app_user'", "请检查数据库用户名和密码。"));
        }
        Ok("用户 app_user".to_owned())
    }

    async fn server_version(&self, _config: &model::SavedConnection, _route: &route::Route) -> Result<String, StageError> {
        self.version_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_version.load(Ordering::SeqCst) {
            return Err(stages::fail("server closed the connection unexpectedly", "没有读到版本。"));
        }
        Ok("PostgreSQL 16.2".to_owned())
    }
}

fn health_probes(database: Arc<dyn DatabaseProbe>, health: Arc<FlakyHealth>) -> Probes {
    Probes {
        database: Some(database),
        health: Some(health),
        ..Probes::default()
    }
}

#[tokio::test]
async fn idle_session_probes_marks_lost_then_reconnects_with_saved_password() {
    let dir = temp_workspace();
    let db = Arc::new(SwitchingDb::new());
    let health = Arc::new(FlakyHealth { fail: std::sync::atomic::AtomicBool::new(true) });
    let service = Arc::new(Service::with_probes(
        dir.join("conn.sqlite"),
        Arc::new(MemoryStore::default()),
        health_probes(db.clone(), health.clone()),
    ));
    let mut draft = remote_draft(Engine::Postgres);
    draft.id = Some("pg".to_owned());
    service.save(draft, secrets("pw", "", "")).await.unwrap();
    assert!(service.open("pg", SecretInput::default()).await.unwrap().connected);
    assert_eq!(db.auth_calls.load(Ordering::SeqCst), 1);

    // 空闲超过阈值 → 探活失败标记 lost；同一次轮询里按保存凭据重连成功，恢复 connected
    service.session_health("pg").unwrap().rewind_idle(std::time::Duration::from_secs(60));
    let statuses = Arc::clone(&service).session_statuses().await;
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].status, "connected", "{:?}", statuses[0]);
    // 打开一次 + 重连一次
    assert_eq!(db.auth_calls.load(Ordering::SeqCst), 2);
    assert!(service.catalog_session("pg").is_some());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn lost_session_without_saved_password_stays_lost_without_dialing() {
    let dir = temp_workspace();
    let db = Arc::new(SwitchingDb::new());
    let health = Arc::new(FlakyHealth { fail: std::sync::atomic::AtomicBool::new(true) });
    let service = Arc::new(Service::with_probes(
        dir.join("conn.sqlite"),
        Arc::new(MemoryStore::default()),
        health_probes(db.clone(), health.clone()),
    ));
    let mut draft = remote_draft(Engine::Postgres);
    draft.id = Some("pg".to_owned());
    draft.save_password = false;
    service.save(draft, SecretInput::default()).await.unwrap();
    // 打开时手输密码但不保存：会话可用，凭据库里没有可供自动重连使用的密码
    let typed = SecretInput {
        password: Some(Secret::new("typed-once")),
        ssh_password: None,
        ssh_passphrase: None,
        save_password: false,
    };
    assert!(service.open("pg", typed).await.unwrap().connected);
    service.session_health("pg").unwrap().rewind_idle(std::time::Duration::from_secs(60));

    let statuses = Arc::clone(&service).session_statuses().await;
    assert_eq!(statuses[0].status, "lost");
    assert!(statuses[0].reason.as_deref().unwrap_or_default().contains("手动重连"), "{:?}", statuses[0]);
    // 没有保存密码：一次重连拨号都没有，等手动入口
    assert_eq!(db.auth_calls.load(Ordering::SeqCst), 1);
    // lost 会话不再提供执行句柄：重连期间不重发写语句
    assert!(service.catalog_session("pg").is_none());
    assert_eq!(service.session_condition("pg"), Some(super::service::SessionCondition::Lost { reason: statuses[0].reason.clone() }));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn deterministic_auth_failure_is_not_retried() {
    let dir = temp_workspace();
    let db = Arc::new(SwitchingDb::new());
    let health = Arc::new(FlakyHealth { fail: std::sync::atomic::AtomicBool::new(true) });
    let service = Arc::new(Service::with_probes(
        dir.join("conn.sqlite"),
        Arc::new(MemoryStore::default()),
        health_probes(db.clone(), health.clone()),
    ));
    let mut draft = remote_draft(Engine::Postgres);
    draft.id = Some("pg".to_owned());
    service.save(draft, secrets("pw", "", "")).await.unwrap();
    assert!(service.open("pg", SecretInput::default()).await.unwrap().connected);

    db.fail_auth.store(true, Ordering::SeqCst);
    service.session_health("pg").unwrap().rewind_idle(std::time::Duration::from_secs(60));
    let statuses = Arc::clone(&service).session_statuses().await;
    assert_eq!(statuses[0].status, "lost");
    let reason = statuses[0].reason.clone().unwrap_or_default();
    assert!(reason.contains("Access denied"), "{reason}");
    assert!(reason.contains("不会再自动重试"), "{reason}");
    let dialed = db.auth_calls.load(Ordering::SeqCst);

    // 再轮询也不再拨号
    let again = Arc::clone(&service).session_statuses().await;
    assert_eq!(again[0].status, "lost");
    assert_eq!(db.auth_calls.load(Ordering::SeqCst), dialed);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn transient_failures_back_off_then_give_up_after_limited_attempts() {
    let dir = temp_workspace();
    let db = Arc::new(SwitchingDb::new());
    let health = Arc::new(FlakyHealth { fail: std::sync::atomic::AtomicBool::new(true) });
    let service = Arc::new(Service::with_probes(
        dir.join("conn.sqlite"),
        Arc::new(MemoryStore::default()),
        health_probes(db.clone(), health.clone()),
    ));
    let mut draft = remote_draft(Engine::Postgres);
    draft.id = Some("pg".to_owned());
    service.save(draft, secrets("pw", "", "")).await.unwrap();
    assert!(service.open("pg", SecretInput::default()).await.unwrap().connected);
    let initial_version_calls = db.version_calls.load(Ordering::SeqCst);

    // 读版本失败是瞬态：第一次失败后处于退避等待，不是放弃
    db.fail_version.store(true, Ordering::SeqCst);
    service.session_health("pg").unwrap().rewind_idle(std::time::Duration::from_secs(60));
    let statuses = Arc::clone(&service).session_statuses().await;
    assert_eq!(statuses[0].status, "reconnecting", "{:?}", statuses[0]);
    assert_eq!(db.version_calls.load(Ordering::SeqCst), initial_version_calls + 1);

    // 退避到期后的两次重试都失败：达到上限，放弃并带出原因
    service.attempt_reconnect("pg").await;
    service.attempt_reconnect("pg").await;
    let settled = Arc::clone(&service).session_statuses().await;
    assert_eq!(settled[0].status, "lost");
    let reason = settled[0].reason.clone().unwrap_or_default();
    assert!(reason.contains("server closed"), "{reason}");
    assert!(reason.contains("停止自动重连"), "{reason}");
    assert_eq!(db.version_calls.load(Ordering::SeqCst), initial_version_calls + 3);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn sqlserver_version_banner_parsing_maps_years_to_engine_majors() {
    use super::drivers::{sqlserver_product_major, SQLSERVER_MIN_MAJOR};
    assert_eq!(sqlserver_product_major("Microsoft SQL Server 2022 (RTM) - 16.0.1000.6 (X64)"), Some(16));
    assert_eq!(sqlserver_product_major("Microsoft SQL Server 2019 (RTM) - 15.0.2000.5"), Some(15));
    assert_eq!(sqlserver_product_major("Microsoft SQL Server 2012 (SP4) - 11.0.7001.08"), Some(11));
    assert_eq!(sqlserver_product_major("Microsoft SQL Server 2008 (SP3) - 10.0.5512.0"), Some(10));
    assert_eq!(sqlserver_product_major("Microsoft SQL Server 2005 - 9.00.5000.00"), Some(0));
    assert_eq!(sqlserver_product_major(""), None);
    // 方言下限：2012 是 OFFSET/FETCH 与新版目录视图的边界。
    assert_eq!(SQLSERVER_MIN_MAJOR, 11);
    assert!(sqlserver_product_major("Microsoft SQL Server 2022 (RTM) - 16.0.1000.6").unwrap() >= SQLSERVER_MIN_MAJOR);
    assert!(sqlserver_product_major("Microsoft SQL Server 2008 (SP3) - 10.0.5512.0").unwrap() < SQLSERVER_MIN_MAJOR);
}

#[test]
fn mssql_fixture_seed_batches_and_sqlcmd_flags_stay_wired() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let compose = std::fs::read_to_string(root.join("../docker/compose.yaml")).unwrap();
    let seed = std::fs::read_to_string(root.join("../docker/init/mssql-seed.sql")).unwrap();

    // CREATE VIEW / CREATE PROCEDURE 必须是批次首语句（T-SQL 规则），seed 用 GO 分批。
    // 按行首定位真实语句（首行注释里也提到这两个关键字）。
    let view_at = seed.find("
CREATE VIEW").map(|at| at + 1).expect("seed 有视图");
    let proc_at = seed.find("
CREATE PROCEDURE").map(|at| at + 1).expect("seed 有存储过程");
    let before_view = seed[..view_at].trim_end();
    let before_proc = seed[..proc_at].trim_end();
    assert!(before_view.ends_with("GO"), "CREATE VIEW 前要有 GO 分批：{before_view}");
    assert!(before_proc.ends_with("GO"), "CREATE PROCEDURE 前要有 GO 分批：{before_proc}");
    assert!(seed[proc_at..].trim_end().ends_with(";"), "过程体结尾完整");

    // sqlcmd：-b 让失败退出码可见；-f 65001 按 UTF-8 读 seed（中文数据不乱码）。
    let init_command = compose.split("mssql-init:").nth(1).expect("compose 有 mssql-init");
    assert!(init_command.contains("-b"), "sqlcmd 需要 -b：{init_command}");
    assert!(init_command.contains("-f 65001"), "读 seed 需要 -f 65001：{init_command}");
    assert!(init_command.contains("-i /seed/mssql-seed.sql"), "seed 挂载执行：{init_command}");
}

#[test]
fn redis_engine_variant_is_canonical_and_relational_gated() {
    use super::model::{normalize, ConnectionDraft, Engine, SecretInput};
    // serde 正典值是 "redis"（与前端 ENGINES 的 id 一致），能力位为 kv。
    assert_eq!(serde_json::to_string(&Engine::Redis).unwrap(), "\"redis\"");
    assert_eq!(Engine::parse("redis").unwrap(), Engine::Redis);
    assert!(Engine::parse("valkey").is_err(), "只接受 redis 一种键值引擎");
    assert!(!Engine::Redis.is_relational(), "Redis 没有 SQL 面/结构编辑/对比/导出");
    assert!(Engine::Postgres.is_relational());

    // Redis 表单：用户名可留空（密码认证），database 字段是库索引。
    let mut draft = ConnectionDraft::blank(Engine::Redis);
    draft.host = "127.0.0.1".to_owned();
    draft.port = "6379".to_owned();
    draft.database = "2".to_owned();
    draft.username = String::new();
    let saved = normalize(&draft, "redis-1".to_owned()).unwrap();
    assert_eq!(saved.database.as_deref(), Some("2"));
    assert_eq!(saved.username, None, "空 ACL 用户名按匿名密码认证保存");
    // 库索引必须是数字。
    draft.database = "main".to_owned();
    let errors = normalize(&draft, "redis-2".to_owned()).unwrap_err();
    assert!(errors.iter().any(|item| item.field == "database" && item.message.contains("库索引")));

    // 无密码 Redis 直连是合法场景：保存不要求密码（凭据缺失错误只在关系引擎出现）。
    let _ = SecretInput::default();
}

#[test]
fn redis_fixture_seed_covers_all_value_types_and_tls_free_healthcheck() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let compose = std::fs::read_to_string(root.join("../docker/compose.yaml")).unwrap();
    let seed = std::fs::read_to_string(root.join("../docker/init/redis-seed.sh")).unwrap();
    let redis = compose.split("  redis:").nth(1).unwrap_or("").split("  redis-init:").next().unwrap_or("");
    // 密码夹具 + 健康检查带认证。
    assert!(redis.contains("--requirepass"), "redis 夹具要求密码：{redis}");
    assert!(redis.contains("redis-cli"), "healthcheck 用 redis-cli 探活：{redis}");
    // seed：string/hash/list/set/zset/stream 各至少一键，含中文值，TTL 有差异。
    for needle in ["rillsql:string", "rillsql:hash", "rillsql:list", "rillsql:set", "rillsql:zset", "rillsql:stream"] {
        assert!(seed.contains(needle), "seed 缺少 {needle}");
    }
    assert!(seed.contains("林夏"), "seed 含中文值");
    assert!(seed.contains("EXPIRE"), "seed 设置不同 TTL");
    assert!(!seed.contains("\nKEYS ") && !seed.starts_with("KEYS "), "seed 不使用 KEYS");
}
