use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::forward::{Forwarder, RecordingOpener};
use super::hostkey::{self, Authenticator, HostKeyDecision};
use super::model::{self, leaked_secret, CaMode, ConnectionDraft, Engine, EnvLabel, Secret, SecretInput, SshAuthKind, TlsMode};
use super::route::{self, Retry};
use super::secrets::{MemoryStore, UnavailableStore};
use super::service::{DatabaseProbe, Probes, Service, SshProbe};
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
    assert_eq!(saved.env.label_zh(), "本地");
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
        },
    );
    let input = secrets("db-secret-alpha", "", "");
    let saved = service.save(remote_draft(Engine::Mariadb), input.clone()).await.unwrap();
    assert_eq!(saved.connection.engine, Engine::Mariadb);
    assert_eq!(saved.connection.env, EnvLabel::Dev);
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
    assert!(!message.contains("已验证"), "{message}");
    assert!(message.contains("未验证"), "{message}");
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
    assert_eq!(require_tls_ok.label, "TLS 握手");
    assert!(!require_tls_ok.label.contains("证书验证"));
    assert_eq!(require_tls_ok.message, "已加密，但未验证证书和主机名");
    assert_not_verified(&require_tls_ok.message);

    let require_auth = super::service::report_canned_remote(require, super::service::CannedRemote::AuthenticationFailed).await;
    assert_eq!(require_auth.status, TestStatus::Err);
    assert_eq!(require_auth.fail_stage.as_deref(), Some(StageKind::DatabaseAuthentication.label_zh()));
    let require_tls = require_auth.stages.iter().find(|stage| stage.kind == StageKind::Tls).unwrap();
    assert_eq!(require_tls.status, StageStatus::Ok);
    assert_eq!(require_tls.label, "TLS 握手");
    assert!(!require_tls.label.contains("证书验证"));
    assert_not_verified(&require_tls.message);

    let verify = model::normalize(&remote_draft(Engine::Postgres), "verify".to_owned()).unwrap();
    let verify_ok = super::service::report_canned_remote(verify.clone(), super::service::CannedRemote::Connected).await;
    let verify_tls_ok = verify_ok.stages.iter().find(|stage| stage.kind == StageKind::Tls).unwrap();
    assert_eq!(verify_tls_ok.label, "TLS 握手与证书验证");
    assert_eq!(verify_tls_ok.message, "证书主机名 db.example.test 已验证");
    let verify_auth = super::service::report_canned_remote(verify, super::service::CannedRemote::AuthenticationFailed).await;
    let verify_tls = verify_auth.stages.iter().find(|stage| stage.kind == StageKind::Tls).unwrap();
    assert_eq!(verify_tls.status, StageStatus::Ok);
    assert_eq!(verify_tls.label, "TLS 握手与证书验证");
    assert_eq!(verify_tls.message, "证书主机名 db.example.test 已验证");
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
            assert!(!stage.message.contains("已验证"), "{mode:?} {}", stage.message);
        }
        let tls = report.stages.iter().find(|stage| stage.kind == StageKind::Tls).unwrap();
        if mode == TlsMode::Require {
            assert!(!tls.label.contains("证书验证"), "{}", tls.label);
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
