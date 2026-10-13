use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::connections::drivers::{self, TunnelFault};
use crate::connections::hostkey::{self, HostKeyDecision, PresentedKey};
use crate::connections::model::{
    password_account, ssh_password_account, ConnectionDraft, Engine, FieldError, SavedConnection, Secret, SecretInput, SshAuthKind,
    SshSettings,
};
use crate::connections::route::{self, Route};
use crate::connections::secrets::SecretStore;
use crate::connections::stages::{self, StageError, StageKind, StageRunner, TestReport, TestStatus};
use crate::connections::store::{self, Store};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionRecord {
    #[serde(flatten)]
    pub saved: SavedConnection,
    pub ssh_host_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveOutcome {
    pub connection: SavedConnection,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteOutcome {
    pub connection: SavedConnection,
    pub database_file_removed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenOutcome {
    pub report: TestReport,
    pub connected: bool,
    pub foreign_keys: Option<bool>,
}

#[derive(Debug)]
pub enum ServiceError {
    Fields(Vec<FieldError>),
    Message(String),
}

impl ServiceError {
    pub fn message(&self) -> String {
        match self {
            ServiceError::Fields(fields) => fields
                .iter()
                .map(|field| format!("{}：{}", field.field, field.message))
                .collect::<Vec<_>>()
                .join("；"),
            ServiceError::Message(message) => message.clone(),
        }
    }
}

#[async_trait::async_trait]
pub trait SshProbe: Send + Sync {
    async fn presented_key(&self, ssh: &SshSettings) -> Result<PresentedKey, StageError>;
    async fn authenticate(&self, ssh: &SshSettings, password: Option<&str>, passphrase: Option<&str>) -> Result<String, StageError>;
    async fn open_tunnel(&self, db_host: &str, db_port: u16, tls_server_name: &str, bastion_host: &str) -> Result<String, StageError>;
}

#[async_trait::async_trait]
pub trait DatabaseProbe: Send + Sync {
    async fn check_tls(&self, route: &Route) -> Result<String, StageError>;
    async fn authenticate(&self, config: &SavedConnection, route: &Route, password: &str) -> Result<String, StageError>;
    async fn server_version(&self, config: &SavedConnection, route: &Route) -> Result<String, StageError>;
}

#[async_trait::async_trait]
pub trait SqliteProbe: Send + Sync {
    async fn open_file(&self, path: &str) -> Result<String, StageError>;
    async fn read_schema(&self, path: &str) -> Result<String, StageError>;
    async fn foreign_keys(&self, path: &str) -> Result<(String, bool), StageError>;
}

#[derive(Clone, Default)]
pub struct Probes {
    pub ssh: Option<Arc<dyn SshProbe>>,
    pub database: Option<Arc<dyn DatabaseProbe>>,
    pub sqlite: Option<Arc<dyn SqliteProbe>>,
}

#[derive(Clone)]
struct PendingHostKey {
    host: String,
    port: u16,
    algorithm: String,
    fingerprint: String,
}

#[allow(dead_code)]
enum SessionKind {
    Sqlite(sqlx::SqlitePool),
    Mysql(mysql_async::Pool),
    Postgres(tokio_postgres::Client),
    Scripted,
}

struct Session {
    kind: SessionKind,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    #[allow(dead_code)]
    foreign_keys: Option<bool>,
}

pub struct Service {
    path: PathBuf,
    secrets: Arc<dyn SecretStore>,
    logs: Mutex<Vec<String>>,
    cell: tokio::sync::OnceCell<Store>,
    sessions: Mutex<HashMap<String, Session>>,
    pending: Mutex<HashMap<String, PendingHostKey>>,
    probes: Probes,
}

impl Service {
    pub fn new(path: PathBuf, secrets: Arc<dyn SecretStore>) -> Self {
        Self::with_probes(path, secrets, Probes::default())
    }

    pub fn with_probes(path: PathBuf, secrets: Arc<dyn SecretStore>, probes: Probes) -> Self {
        Self {
            path,
            secrets,
            logs: Mutex::new(Vec::new()),
            cell: tokio::sync::OnceCell::new(),
            sessions: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            probes,
        }
    }

    pub fn config_path(&self) -> &Path {
        &self.path
    }

    pub fn logs(&self) -> Vec<String> {
        self.logs.lock().expect("logs").clone()
    }

    pub fn is_connected(&self, id: &str) -> bool {
        self.sessions.lock().expect("sessions").contains_key(id)
    }

    pub async fn list(&self) -> Result<Vec<ConnectionRecord>, String> {
        let store = self.store().await?;
        let rows = store.list().await.map_err(db_err)?;
        let mut records = Vec::with_capacity(rows.len());
        for saved in rows {
            let ssh_host_key = match &saved.ssh {
                Some(ssh) => store.host_key(&ssh.host, ssh.port).await.map_err(db_err)?,
                None => None,
            };
            records.push(ConnectionRecord { saved, ssh_host_key });
        }
        Ok(records)
    }

    pub async fn save(&self, draft: ConnectionDraft, input: SecretInput) -> Result<SaveOutcome, ServiceError> {
        let id = draft
            .id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut saved = crate::connections::model::normalize(&draft, id).map_err(ServiceError::Fields)?;
        let store = self.store().await.map_err(ServiceError::Message)?;
        let existing = store.get(&saved.id).await.map_err(|err| ServiceError::Message(db_err(err)))?;
        let mut notes = Vec::new();
        if saved.engine == Engine::Sqlite {
            saved.password_saved = false;
            saved.ssh_password_saved = false;
        } else if draft.save_password {
            saved.password_saved = self.store_secret(
                &password_account(&saved.id),
                input.password.as_ref(),
                existing.as_ref().is_some_and(|item| item.password_saved),
                &mut notes,
            );
            if saved.ssh.as_ref().is_some_and(|ssh| ssh.auth == SshAuthKind::Password) {
                saved.ssh_password_saved = self.store_secret(
                    &ssh_password_account(&saved.id),
                    input.ssh_password.as_ref(),
                    existing.as_ref().is_some_and(|item| item.ssh_password_saved),
                    &mut notes,
                );
            }
        } else {
            saved.password_saved = self.clear_secret(
                &password_account(&saved.id),
                existing.as_ref().is_some_and(|item| item.password_saved),
                &mut notes,
            );
            saved.ssh_password_saved = self.clear_secret(
                &ssh_password_account(&saved.id),
                existing.as_ref().is_some_and(|item| item.ssh_password_saved),
                &mut notes,
            );
        }
        store.upsert(&saved).await.map_err(|err| ServiceError::Message(db_err(err)))?;
        self.log(format!("已保存连接 {}（{}）", saved.name, saved.engine.label_zh()));
        for note in &notes {
            self.log(note.clone());
        }
        Ok(SaveOutcome {
            connection: saved,
            note: if notes.is_empty() { None } else { Some(notes.join(" ")) },
        })
    }

    pub async fn delete(&self, id: &str) -> Result<DeleteOutcome, String> {
        self.disconnect(id);
        let store = self.store().await?;
        let existing = store.get(id).await.map_err(db_err)?.ok_or_else(|| "找不到这条连接配置".to_owned())?;
        let _ = self.secrets.delete(&password_account(id));
        let _ = self.secrets.delete(&ssh_password_account(id));
        store.delete(id).await.map_err(db_err)?;
        if let Some(ssh) = &existing.ssh {
            let still = store.ssh_host_still_used(&ssh.host, ssh.port, id).await.map_err(db_err)?;
            if !still {
                store.delete_host_key(&ssh.host, ssh.port).await.map_err(db_err)?;
            }
        }
        self.pending.lock().expect("pending").remove(id);
        self.log(format!("已删除连接配置 {}。数据库文件未删除。", existing.name));
        Ok(DeleteOutcome {
            connection: existing,
            database_file_removed: false,
        })
    }

    pub async fn test(&self, id: &str, input: SecretInput) -> Result<TestReport, String> {
        Ok(self.exercise(id, input, false).await?.report)
    }

    pub async fn open(&self, id: &str, input: SecretInput) -> Result<OpenOutcome, String> {
        self.exercise(id, input, true).await
    }

    pub fn disconnect(&self, id: &str) {
        let session = self.sessions.lock().expect("sessions").remove(id);
        if let Some(session) = session {
            if let Some(shutdown) = session.shutdown {
                let _ = shutdown.send(());
            }
            drop(session.kind);
            self.log(format!("已断开会话 {id}，隧道和通道已关闭。数据库文件未删除。"));
        }
    }

    pub async fn confirm_host_key(&self, id: &str, fingerprint: &str) -> Result<(), String> {
        let pending = self
            .pending
            .lock()
            .expect("pending")
            .get(id)
            .cloned()
            .ok_or_else(|| "没有等待确认的主机密钥".to_owned())?;
        if pending.fingerprint != fingerprint {
            return Err("指纹与等待确认的主机密钥不一致，没有保存。".to_owned());
        }
        let store = self.store().await?;
        store
            .save_host_key(&pending.host, pending.port, &pending.algorithm, &pending.fingerprint)
            .await
            .map_err(db_err)?;
        self.pending.lock().expect("pending").remove(id);
        self.log(format!("已保存 SSH 主机密钥 {}:{}", pending.host, pending.port));
        Ok(())
    }

    pub async fn forget_host_key(&self, id: &str) -> Result<(), String> {
        let store = self.store().await?;
        let config = store.get(id).await.map_err(db_err)?.ok_or_else(|| "找不到这条连接配置".to_owned())?;
        let ssh = config.ssh.ok_or_else(|| "这条连接没有 SSH 跳板".to_owned())?;
        store.delete_host_key(&ssh.host, ssh.port).await.map_err(db_err)?;
        self.log(format!("已忘记 {} 的主机密钥", ssh.host));
        Ok(())
    }

    fn store_secret(&self, account: &str, secret: Option<&Secret>, previous: bool, notes: &mut Vec<String>) -> bool {
        let Some(secret) = secret.filter(|item| !item.is_empty()) else {
            return previous;
        };
        match self.secrets.put(account, secret.expose()) {
            Ok(()) => true,
            Err(_) => {
                push_note(notes, "系统凭据库不可用：密码只用于本次连接，没有写入配置。");
                false
            }
        }
    }

    fn clear_secret(&self, account: &str, previous: bool, notes: &mut Vec<String>) -> bool {
        if !previous {
            let _ = self.secrets.delete(account);
            return false;
        }
        match self.secrets.delete(account) {
            Ok(()) => false,
            Err(_) => {
                push_note(notes, "凭据库不可用，已保存的密码可能仍在系统凭据库中。配置文件里没有密码。");
                true
            }
        }
    }

    fn resolve_secret(&self, account: &str, typed: Option<&Secret>, saved: bool) -> Option<Secret> {
        if let Some(secret) = typed.filter(|item| !item.is_empty()) {
            return Some(secret.clone());
        }
        if !saved {
            return None;
        }
        match self.secrets.get(account) {
            Ok(value) => value.map(Secret::new),
            Err(_) => None,
        }
    }

    async fn exercise(&self, id: &str, input: SecretInput, open: bool) -> Result<OpenOutcome, String> {
        let store = self.store().await?;
        let config = store.get(id).await.map_err(db_err)?.ok_or_else(|| "找不到这条连接配置".to_owned())?;
        let saved_fingerprint = match &config.ssh {
            Some(ssh) => store.host_key(&ssh.host, ssh.port).await.map_err(db_err)?,
            None => None,
        };
        let password = self.resolve_secret(&password_account(&config.id), input.password.as_ref(), config.password_saved);
        let ssh_password = self.resolve_secret(
            &ssh_password_account(&config.id),
            input.ssh_password.as_ref(),
            config.ssh_password_saved,
        );
        let passphrase = input.ssh_passphrase.filter(|item| !item.is_empty());
        let stages = stages::planned_stages(&config);
        let mut attempt = Attempt {
            config: config.clone(),
            probes: self.probes.clone(),
            password,
            ssh_password,
            passphrase,
            saved_fingerprint,
            ssh_fault: None,
            ssh_started: false,
            ssh_authed: false,
            forwarder_port: None,
            shutdown: None,
            remote: Remote::Fresh,
            sqlite_pool: None,
            mysql_pool: None,
            postgres: None,
            schema_note: None,
            foreign_keys: None,
        };
        let report = stages::execute_stages(&stages, &mut attempt).await;
        if report.status == TestStatus::Wait {
            if let (Some(ssh), Some(fingerprint), Some(algorithm)) = (&config.ssh, &report.fingerprint, &report.algorithm) {
                self.pending.lock().expect("pending").insert(
                    id.to_owned(),
                    PendingHostKey {
                        host: ssh.host.clone(),
                        port: ssh.port,
                        algorithm: algorithm.clone(),
                        fingerprint: fingerprint.clone(),
                    },
                );
            }
        }
        self.log(format!("测试 {} 结束", config.name));
        let connected = open && report.status == TestStatus::Ok;
        let foreign_keys = attempt.foreign_keys;
        if connected {
            let kind = if let Some(pool) = attempt.sqlite_pool.take() {
                SessionKind::Sqlite(pool)
            } else if let Some(pool) = attempt.mysql_pool.take() {
                SessionKind::Mysql(pool)
            } else if let Some(client) = attempt.postgres.take() {
                SessionKind::Postgres(client)
            } else {
                SessionKind::Scripted
            };
            self.sessions.lock().expect("sessions").insert(
                id.to_owned(),
                Session {
                    kind,
                    shutdown: attempt.shutdown.take(),
                    foreign_keys,
                },
            );
        }
        Ok(OpenOutcome {
            report,
            connected,
            foreign_keys,
        })
    }

    async fn store(&self) -> Result<Store, String> {
        let path = self.path.clone();
        self.cell
            .get_or_try_init(|| store::Store::open(path))
            .await
            .cloned()
            .map_err(|err| format!("打不开连接配置：{err}"))
    }

    fn log(&self, message: String) {
        self.logs.lock().expect("logs").push(message);
    }
}

pub fn default_config_path() -> PathBuf {
    directories::ProjectDirs::from("app", "RillSQL", "RillSQL")
        .map(|dirs| dirs.data_dir().join("connections.sqlite"))
        .unwrap_or_else(|| PathBuf::from("connections.sqlite"))
}

fn push_note(notes: &mut Vec<String>, text: &str) {
    if !notes.iter().any(|item| item == text) {
        notes.push(text.to_owned());
    }
}

fn db_err(err: sqlx::Error) -> String {
    format!("连接配置读写失败：{err}")
}

fn decision_error(decision: HostKeyDecision) -> StageError {
    match decision {
        HostKeyDecision::Unconfirmed { algorithm, fingerprint } => StageError::WaitHostKey { algorithm, fingerprint },
        HostKeyDecision::Changed { presented, saved } => stages::fail(
            format!("服务器出示的密钥 {presented} 与已保存的 {saved} 不同"),
            "已阻止连接，没有发送任何凭据。如果确认服务器更换了密钥，请先通过可信渠道核实新指纹，然后忘记已保存的密钥再重新测试。",
        ),
        HostKeyDecision::Trusted => stages::fail("SSH 主机密钥状态不一致", "没有继续认证。"),
    }
}

enum Remote {
    Fresh,
    Probed,
    Done { version: String },
    Failed { stage: StageKind, error: StageError },
}

struct Attempt {
    config: SavedConnection,
    probes: Probes,
    password: Option<Secret>,
    ssh_password: Option<Secret>,
    passphrase: Option<Secret>,
    saved_fingerprint: Option<String>,
    ssh_fault: Option<TunnelFault>,
    ssh_started: bool,
    ssh_authed: bool,
    forwarder_port: Option<u16>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    remote: Remote,
    sqlite_pool: Option<sqlx::SqlitePool>,
    mysql_pool: Option<mysql_async::Pool>,
    postgres: Option<tokio_postgres::Client>,
    schema_note: Option<String>,
    foreign_keys: Option<bool>,
}

impl Attempt {
    fn route(&self) -> Result<Route, StageError> {
        let forwarder = if self.config.ssh.is_some() { Some(self.forwarder_port.unwrap_or(0)) } else { None };
        route::resolve_route(&self.config, forwarder).map_err(|err| stages::fail(err, "没有改用转发器地址做证书校验。"))
    }

    async fn begin_ssh(&mut self) -> Result<(), StageError> {
        let ssh = self.config.ssh.clone().ok_or_else(|| stages::fail("没有 SSH 配置", "未执行"))?;
        if let Some(probe) = self.probes.ssh.clone() {
            let presented = probe.presented_key(&ssh).await?;
            let decision = hostkey::decide_host_key(self.saved_fingerprint.as_deref(), &presented.algorithm, &presented.fingerprint);
            if !hostkey::credentials_allowed(&decision) {
                self.ssh_fault = Some(TunnelFault::Identity(decision_error(decision)));
            }
            return Ok(());
        }
        let tls_name = self.config.tls.as_ref().map(|tls| tls.server_name.clone()).unwrap_or_default();
        let host = self.config.host.clone().unwrap_or_default();
        let port = self.config.port.unwrap_or(0);
        match drivers::start_tunnel(
            &ssh,
            self.saved_fingerprint.clone(),
            self.ssh_password.as_ref().map(Secret::expose),
            self.passphrase.as_ref().map(Secret::expose),
            &host,
            port,
            &tls_name,
        )
        .await
        {
            Ok(started) => {
                self.forwarder_port = Some(started.local_port);
                self.shutdown = Some(started.shutdown);
                self.ssh_authed = true;
            }
            Err(fault) => self.ssh_fault = Some(fault),
        }
        Ok(())
    }

    async fn ssh_identity(&mut self) -> Result<String, StageError> {
        if !self.ssh_started {
            self.begin_ssh().await?;
            self.ssh_started = true;
        }
        if let Some(TunnelFault::Identity(error)) = &self.ssh_fault {
            return Err(error.clone());
        }
        Ok(if self.saved_fingerprint.is_some() {
            "与已保存的指纹一致".to_owned()
        } else {
            "主机身份已确认".to_owned()
        })
    }

    async fn ssh_auth(&mut self) -> Result<String, StageError> {
        if let Some(TunnelFault::Authentication(error) | TunnelFault::Identity(error)) = &self.ssh_fault {
            return Err(error.clone());
        }
        if let Some(probe) = self.probes.ssh.clone() {
            if !self.ssh_authed {
                let ssh = self.config.ssh.clone().ok_or_else(|| stages::fail("没有 SSH 配置", "未执行"))?;
                let message = probe
                    .authenticate(&ssh, self.ssh_password.as_ref().map(Secret::expose), self.passphrase.as_ref().map(Secret::expose))
                    .await?;
                self.ssh_authed = true;
                return Ok(message);
            }
        }
        Ok(match self.config.ssh.as_ref().map(|ssh| ssh.auth) {
            Some(SshAuthKind::Key) => "私钥认证成功".to_owned(),
            _ => "密码认证成功".to_owned(),
        })
    }

    async fn ssh_tunnel(&mut self) -> Result<String, StageError> {
        if let Some(TunnelFault::Tunnel(error)) = &self.ssh_fault {
            return Err(error.clone());
        }
        let host = self.config.host.clone().unwrap_or_default();
        let port = self.config.port.unwrap_or(0);
        if let Some(probe) = self.probes.ssh.clone() {
            let ssh = self.config.ssh.clone().ok_or_else(|| stages::fail("没有 SSH 配置", "未执行"))?;
            let tls_name = self.config.tls.as_ref().map(|tls| tls.server_name.clone()).unwrap_or_default();
            let message = probe.open_tunnel(&host, port, &tls_name, &ssh.host).await?;
            self.forwarder_port = Some(1);
            return Ok(message);
        }
        let local = self.forwarder_port.ok_or_else(|| stages::fail("隧道没有建立", "尚未连接数据库。"))?;
        Ok(format!("127.0.0.1:{local} → {host}:{port}（每条本地连接单独一条 direct-tcpip）"))
    }

    async fn ensure_remote(&mut self) {
        if !matches!(self.remote, Remote::Fresh) {
            return;
        }
        if self.probes.database.is_some() {
            self.remote = Remote::Probed;
            return;
        }
        let Some(password) = self.password.as_ref() else {
            self.remote = Remote::Failed {
                stage: StageKind::DatabaseAuthentication,
                error: stages::fail("请填写数据库密码", "凭据库没有可用密码，本次也没有输入。没有把空密码写入配置。"),
            };
            return;
        };
        let route = match self.route() {
            Ok(route) => route,
            Err(error) => {
                self.remote = Remote::Failed { stage: StageKind::Network, error };
                return;
            }
        };
        if self.config.ssh.is_some() && self.forwarder_port.is_none() {
            self.remote = Remote::Failed {
                stage: StageKind::Network,
                error: stages::fail("隧道没有就绪", "尚未连接数据库。"),
            };
            return;
        }
        let password = password.expose().to_owned();
        let opened = match self.config.engine {
            Engine::Mysql | Engine::Mariadb => drivers::open_mysql(&self.config, &route, &password).await.map(|opened| {
                self.mysql_pool = Some(opened.pool);
                opened.version
            }),
            Engine::Postgres => drivers::open_postgres(&self.config, &route, &password).await.map(|opened| {
                self.postgres = Some(opened.client);
                opened.version
            }),
            Engine::Sqlite => Err(stages::fail("SQLite 不走远程连接", "未执行")),
        };
        match opened {
            Ok(version) => self.remote = Remote::Done { version },
            Err(error) => {
                let stage = classify_db(&error);
                self.remote = Remote::Failed { stage, error };
            }
        }
    }

    fn pass_until(&self, stage: StageKind, ok: impl FnOnce() -> String) -> Result<String, StageError> {
        match &self.remote {
            Remote::Done { version } => Ok(if stage == StageKind::ServerVersion { version.clone() } else { ok() }),
            Remote::Failed { stage: failed, error } if *failed == stage => Err(error.clone()),
            Remote::Failed { stage: failed, .. } if rank(*failed) > rank(stage) => Ok(ok()),
            Remote::Failed { error, .. } => Err(error.clone()),
            Remote::Fresh | Remote::Probed => Err(stages::fail("连接没有开始", "未执行")),
        }
    }

    async fn network(&mut self) -> Result<String, StageError> {
        if self.probes.database.is_some() {
            let host = self.config.host.clone().unwrap_or_default();
            let port = self.config.port.unwrap_or(0);
            return Ok(format!("{host}:{port}"));
        }
        self.ensure_remote().await;
        let host = self.config.host.clone().unwrap_or_default();
        let port = self.config.port.unwrap_or(0);
        self.pass_until(StageKind::Network, || format!("{host}:{port}"))
    }

    async fn tls(&mut self) -> Result<String, StageError> {
        if let Some(probe) = self.probes.database.clone() {
            let route = self.route()?;
            return probe.check_tls(&route).await;
        }
        self.ensure_remote().await;
        let name = self.config.host.clone().unwrap_or_default();
        self.pass_until(StageKind::Tls, || format!("证书主机名 {name} 已验证"))
    }

    async fn db_auth(&mut self) -> Result<String, StageError> {
        if let Some(probe) = self.probes.database.clone() {
            let password = self.password.as_ref().ok_or_else(|| {
                stages::fail("请填写数据库密码", "凭据库没有可用密码，本次也没有输入。没有把空密码写入配置。")
            })?;
            let route = self.route()?;
            return probe.authenticate(&self.config, &route, password.expose()).await;
        }
        self.ensure_remote().await;
        let user = self.config.username.clone().unwrap_or_default();
        self.pass_until(StageKind::DatabaseAuthentication, || format!("用户 {user}"))
    }

    async fn server_version(&mut self) -> Result<String, StageError> {
        if let Some(probe) = self.probes.database.clone() {
            let route = self.route()?;
            return probe.server_version(&self.config, &route).await;
        }
        self.ensure_remote().await;
        self.pass_until(StageKind::ServerVersion, || "未知版本".to_owned())
    }

    async fn open_file(&mut self) -> Result<String, StageError> {
        let path = self.config.file_path.clone().unwrap_or_default();
        if let Some(probe) = self.probes.sqlite.clone() {
            return probe.open_file(&path).await;
        }
        let opened = drivers::open_sqlite(Path::new(&path)).await?;
        self.schema_note = Some(format!("SQLite {} · 读取到 {} 个表或视图", opened.version, opened.schema_objects));
        self.foreign_keys = Some(opened.foreign_keys);
        self.sqlite_pool = Some(opened.pool);
        Ok(path)
    }

    async fn read_schema(&mut self) -> Result<String, StageError> {
        let path = self.config.file_path.clone().unwrap_or_default();
        if let Some(probe) = self.probes.sqlite.clone() {
            return probe.read_schema(&path).await;
        }
        Ok(self.schema_note.clone().unwrap_or_else(|| "读取到 0 个表或视图".to_owned()))
    }

    async fn foreign_keys_stage(&mut self) -> Result<String, StageError> {
        let path = self.config.file_path.clone().unwrap_or_default();
        if let Some(probe) = self.probes.sqlite.clone() {
            let (message, enabled) = probe.foreign_keys(&path).await?;
            self.foreign_keys = Some(enabled);
            return Ok(message);
        }
        let enabled = self.foreign_keys.unwrap_or(false);
        Ok(format!("foreign_keys {}", if enabled { "ON" } else { "OFF" }))
    }
}

#[async_trait::async_trait]
impl StageRunner for Attempt {
    async fn run_stage(&mut self, stage: StageKind) -> Result<String, StageError> {
        match stage {
            StageKind::SshHostIdentity => self.ssh_identity().await,
            StageKind::SshAuthentication => self.ssh_auth().await,
            StageKind::Tunnel => self.ssh_tunnel().await,
            StageKind::Network => self.network().await,
            StageKind::Tls => self.tls().await,
            StageKind::DatabaseAuthentication => self.db_auth().await,
            StageKind::ServerVersion => self.server_version().await,
            StageKind::OpenFile => self.open_file().await,
            StageKind::ReadSchema => self.read_schema().await,
            StageKind::ForeignKeys => self.foreign_keys_stage().await,
        }
    }
}

fn rank(stage: StageKind) -> u8 {
    match stage {
        StageKind::Network => 0,
        StageKind::Tls => 1,
        StageKind::DatabaseAuthentication => 2,
        StageKind::ServerVersion => 3,
        _ => 9,
    }
}

fn classify_db(error: &StageError) -> StageKind {
    let StageError::Fail { message, hint } = error else {
        return StageKind::Network;
    };
    if hint.contains("网络连接失败") || hint.contains("隧道没有") {
        return StageKind::Network;
    }
    if hint.contains("用户名和密码") || hint.contains("数据库认证") {
        return StageKind::DatabaseAuthentication;
    }
    let lower = format!("{message} {hint}").to_ascii_lowercase();
    if lower.contains("access denied") || lower.contains("password authentication") {
        return StageKind::DatabaseAuthentication;
    }
    if hint.contains("没有读到版本") {
        return StageKind::ServerVersion;
    }
    if lower.contains("certificate")
        || lower.contains("tls")
        || lower.contains("ssl")
        || message.contains("证书")
        || hint.contains("证书")
        || hint.contains("localhost")
        || hint.contains("不验证")
        || hint.contains("明文")
    {
        return StageKind::Tls;
    }
    StageKind::Network
}
