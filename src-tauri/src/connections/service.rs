use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::connections::drivers::{self, TunnelFault, TdsClient};
use crate::connections::hostkey::{self, HostKeyDecision, PresentedKey};
use crate::connections::model::{
    password_account, ssh_password_account, validate_tag_label, ConnectionDraft, Engine, EnvTag, FieldError, SavedConnection, Secret, SecretInput,
    SshAuthKind, SshSettings, TlsMode, ENV_NONE,
};
use crate::connections::route::{self, Route};
use crate::connections::secrets::SecretStore;
use crate::connections::stages::{self, StageError, StageKind, StageRunner, StageStatus, TestReport, TestStatus};
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
    /// 探活探针。None 时用真实探针（PG SELECT 1 / MySQL ping；SQLite 免）。
    pub health: Option<Arc<dyn HealthProbe>>,
}

/// 会话探活：在已打开的会话上发一条最小语句确认连接还活着。
#[async_trait::async_trait]
pub trait HealthProbe: Send + Sync {
    async fn check(&self, session: &CatalogSession) -> Result<(), String>;
}

struct SelectOneProbe;

#[async_trait::async_trait]
impl HealthProbe for SelectOneProbe {
    async fn check(&self, session: &CatalogSession) -> Result<(), String> {
        use mysql_async::prelude::Queryable;
        match session {
            // SQLite 是本地文件，没有“断线”可言，免探活（这里到不了，防御返回成功）。
            CatalogSession::Sqlite { .. } | CatalogSession::Scripted => Ok(()),
            CatalogSession::Mysql(pool) => {
                let mut conn = pool.get_conn().await.map_err(|err| err.to_string())?;
                conn.ping().await.map_err(|err| err.to_string())
            }
            CatalogSession::Postgres(client) => client
                .simple_query("SELECT 1")
                .await
                .map(|_| ())
                .map_err(|err| err.to_string()),
            CatalogSession::Sqlserver(client) => {
                let mut client = client.lock().await;
                use futures::TryStreamExt;
                let mut stream = client.simple_query("SELECT 1").await.map_err(|err| err.to_string())?;
                while let Some(item) = stream.try_next().await.map_err(|err| err.to_string())? {
                    if item.into_row().is_some() {
                        return Ok(());
                    }
                }
                Ok(())
            }
            CatalogSession::Redis(conn) => {
                let mut conn = conn.lock().await;
                let pong: String = redis::cmd("PING").query_async(&mut *conn).await.map_err(|err| err.to_string())?;
                if pong.eq_ignore_ascii_case("pong") {
                    Ok(())
                } else {
                    Err(format!("PING 返回了 {pong}"))
                }
            }
        }
    }
}

#[derive(Clone)]
struct PendingHostKey {
    host: String,
    port: u16,
    algorithm: String,
    fingerprint: String,
}

enum SessionKind {
    Sqlite(sqlx::SqlitePool),
    Mysql(mysql_async::Pool),
    Postgres(Arc<tokio_postgres::Client>),
    Sqlserver(Arc<tokio::sync::Mutex<TdsClient>>),
    Redis(Arc<tokio::sync::Mutex<redis::aio::MultiplexedConnection>>),
    Scripted,
}

struct Session {
    kind: SessionKind,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    foreign_keys: Option<bool>,
    cancel: CancelTransport,
    health: Arc<SessionHealth>,
}

/// 会话健康状态（R4）。执行路径通过 `CancelTransport` 报告断线类错误（suspect），
/// 轮询探活消费这个标记：探活失败把会话置为 lost，有限次退避重连成功后由新会话替代。
pub struct SessionHealth {
    suspect: AtomicBool,
    lost: AtomicBool,
    reason: Mutex<Option<String>>,
    busy: AtomicUsize,
    probing: AtomicBool,
    last_ok: Mutex<Instant>,
}

impl Default for SessionHealth {
    fn default() -> Self {
        Self {
            suspect: AtomicBool::new(false),
            lost: AtomicBool::new(false),
            reason: Mutex::new(None),
            busy: AtomicUsize::new(0),
            probing: AtomicBool::new(false),
            last_ok: Mutex::new(Instant::now()),
        }
    }
}

impl SessionHealth {
    pub fn note_activity(&self) {
        *self.last_ok.lock().expect("last_ok") = Instant::now();
    }

    /// 执行路径看到断线类错误：不直接判死，交给下一次轮询探活确认。
    pub fn note_disconnect(&self) {
        self.suspect.store(true, Ordering::SeqCst);
    }

    pub fn clear_suspect(&self) {
        self.suspect.store(false, Ordering::SeqCst);
    }

    pub fn is_suspect(&self) -> bool {
        self.suspect.load(Ordering::SeqCst)
    }

    pub fn mark_lost(&self, reason: String) {
        self.suspect.store(false, Ordering::SeqCst);
        self.lost.store(true, Ordering::SeqCst);
        *self.reason.lock().expect("reason") = Some(reason);
    }

    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    pub fn lost_reason(&self) -> Option<String> {
        self.reason.lock().expect("reason").clone()
    }

    /// 重连放弃等原因更新（保持 lost，不改变其余状态）。
    pub fn set_lost_reason(&self, reason: Option<String>) {
        *self.reason.lock().expect("reason") = reason;
    }

    pub fn note_busy(&self) {
        self.busy.fetch_add(1, Ordering::SeqCst);
    }

    pub fn note_idle(&self) {
        self.busy.fetch_sub(1, Ordering::SeqCst);
    }

    pub fn busy(&self) -> usize {
        self.busy.load(Ordering::SeqCst)
    }

    /// 空闲多久了。最近一次成功活动或打开时间起算。
    pub fn idle_for(&self) -> Duration {
        self.last_ok.lock().expect("last_ok").elapsed()
    }

    /// 探活是否需要触发：上次操作报告过断线，或空闲超过阈值。SQLite 免探活。
    pub fn needs_probe(&self, sqlite: bool) -> bool {
        if sqlite {
            return false;
        }
        self.is_suspect() || self.idle_for() >= IDLE_PROBE_AFTER
    }

    fn begin_probe(&self) -> bool {
        !self.probing.swap(true, Ordering::SeqCst)
    }

    fn end_probe(&self) {
        self.probing.store(false, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub fn rewind_idle(&self, elapsed: Duration) {
        let mut last_ok = self.last_ok.lock().expect("last_ok");
        *last_ok = last_ok.checked_sub(elapsed).unwrap_or(*last_ok);
    }
}

/// 取消正在执行的语句。查询本身仍走已打开的会话，不从这里再拨一条执行连接。
#[derive(Clone)]
pub enum CancelTransport {
    Sqlite(Arc<AtomicBool>, Arc<SessionHealth>),
    Mysql {
        connection_id: Arc<AtomicU64>,
        opts: mysql_async::Opts,
        via_ssh: bool,
        health: Arc<SessionHealth>,
    },
    Postgres {
        token: tokio_postgres::CancelToken,
        tls: Option<tokio_postgres_rustls::MakeRustlsConnect>,
        via_ssh: bool,
        health: Arc<SessionHealth>,
    },
    Sqlserver {
        spid: Arc<AtomicU32>,
        config: tiberius::Config,
        via_ssh: bool,
        health: Arc<SessionHealth>,
    },
    /// Redis 命令没有服务端取消通道；这里只承载共享健康句柄（活动/断线报告）。
    Redis {
        health: Arc<SessionHealth>,
    },
    None,
}

impl CancelTransport {
    pub fn via_ssh(&self) -> bool {
        match self {
            Self::Mysql { via_ssh, .. } | Self::Postgres { via_ssh, .. } | Self::Sqlserver { via_ssh, .. } => *via_ssh,
            _ => false,
        }
    }

    pub fn sqlite_flag(&self) -> Option<Arc<AtomicBool>> {
        match self {
            Self::Sqlite(flag, _) => Some(Arc::clone(flag)),
            _ => None,
        }
    }

    pub fn set_mysql_connection_id(&self, id: u64) {
        if let Self::Mysql { connection_id, .. } = self {
            connection_id.store(id, Ordering::SeqCst);
        }
    }

    pub fn health(&self) -> Option<&Arc<SessionHealth>> {
        match self {
            Self::Sqlite(_, health) | Self::Mysql { health, .. } | Self::Postgres { health, .. } | Self::Sqlserver { health, .. } | Self::Redis { health } => {
                Some(health)
            }
            Self::None => None,
        }
    }

    /// 会话入表时把执行共享的健康句柄接到取消通道上，执行路径据此报告断线。
    pub fn attach_health(&mut self, health: Arc<SessionHealth>) {
        match self {
            Self::Sqlite(_, slot) => *slot = health,
            Self::Mysql { health: slot, .. } => *slot = health,
            Self::Postgres { health: slot, .. } => *slot = health,
            Self::Sqlserver { health: slot, .. } => *slot = health,
            Self::Redis { health: slot } => *slot = health,
            Self::None => {}
        }
    }

    pub fn note_activity(&self) {
        if let Some(health) = self.health() {
            health.note_activity();
        }
    }

    pub fn note_disconnect(&self) {
        if let Some(health) = self.health() {
            health.note_disconnect();
        }
    }
}

/// 一次对比用的临时 PostgreSQL 会话。丢弃时关闭它自己的通道，不动用户正在使用的会话。
pub struct EphemeralRead {
    catalog: CatalogSession,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl EphemeralRead {
    pub fn catalog(&self) -> CatalogSession {
        self.catalog.clone()
    }
}

impl Drop for EphemeralRead {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

/// 已打开会话的查询句柄。调用方只能拿去查元数据，不能据此再拨一条数据库连接。
#[derive(Clone)]
pub enum CatalogSession {
    Sqlite {
        pool: sqlx::SqlitePool,
        foreign_keys: Option<bool>,
    },
    Mysql(mysql_async::Pool),
    Postgres(Arc<tokio_postgres::Client>),
    /// tiberius 句柄不可共享借用，目录/查询/网格/对比都经这一把互斥锁串行使用同一条连接。
    Sqlserver(Arc<tokio::sync::Mutex<TdsClient>>),
    /// Redis 单条多路复用连接：键浏览、值读取与命令行共用，经互斥锁串行。
    Redis(Arc<tokio::sync::Mutex<redis::aio::MultiplexedConnection>>),
    Scripted,
}

pub struct Service {
    path: PathBuf,
    secrets: Arc<dyn SecretStore>,
    #[cfg(test)]
    logs: Mutex<Vec<String>>,
    cell: tokio::sync::OnceCell<Store>,
    sessions: Mutex<HashMap<String, Session>>,
    pending: Mutex<HashMap<String, PendingHostKey>>,
    probes: Probes,
    /// lost 会话的自动重连计划：有限次退避，确定性错误直接放弃（R4）。
    reconnects: Mutex<HashMap<String, ReconnectPlan>>,
}

impl Service {
    pub fn new(path: PathBuf, secrets: Arc<dyn SecretStore>) -> Self {
        Self::with_probes(path, secrets, Probes::default())
    }

    pub fn with_probes(path: PathBuf, secrets: Arc<dyn SecretStore>, probes: Probes) -> Self {
        Self {
            path,
            secrets,
            #[cfg(test)]
            logs: Mutex::new(Vec::new()),
            cell: tokio::sync::OnceCell::new(),
            sessions: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            probes,
            reconnects: Mutex::new(HashMap::new()),
        }
    }

    pub fn config_path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub fn logs(&self) -> Vec<String> {
        self.logs.lock().expect("logs").clone()
    }

    #[cfg(test)]
    pub fn is_connected(&self, id: &str) -> bool {
        self.sessions.lock().expect("sessions").contains_key(id)
    }

    /// 测试访问器：直接拿到会话健康句柄，模拟空闲超时或执行路径的断线报告。
    #[cfg(test)]
    pub fn session_health(&self, id: &str) -> Option<std::sync::Arc<SessionHealth>> {
        self.sessions.lock().expect("sessions").get(id).map(|session| std::sync::Arc::clone(&session.health))
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

    pub async fn list_tags(&self) -> Result<Vec<EnvTag>, String> {
        self.store().await?.list_tags().await.map_err(db_err)
    }

    pub async fn save_tag(&self, id: Option<String>, label: String, tone: String) -> Result<EnvTag, ServiceError> {
        let label = validate_tag_label(&label).map_err(ServiceError::Message)?;
        let tone = crate::connections::model::parse_tone(&tone)
            .ok_or_else(|| ServiceError::Message("请选择标签颜色".to_owned()))?
            .to_owned();
        let store = self.store().await.map_err(ServiceError::Message)?;
        let requested = id.unwrap_or_default();
        let requested = requested.trim();
        if requested == ENV_NONE {
            return Err(ServiceError::Message("不能把无标签保存成环境标签".to_owned()));
        }
        let (id, position) = if requested.is_empty() {
            let position = store.next_tag_position().await.map_err(|err| ServiceError::Message(db_err(err)))?;
            (uuid::Uuid::new_v4().to_string(), position)
        } else {
            let existing = store
                .tag(requested)
                .await
                .map_err(|err| ServiceError::Message(db_err(err)))?
                .ok_or_else(|| ServiceError::Message("找不到这个环境标签".to_owned()))?;
            (existing.id, existing.position)
        };
        if store
            .label_taken(&label, &id)
            .await
            .map_err(|err| ServiceError::Message(db_err(err)))?
        {
            return Err(ServiceError::Message("已经有同名环境标签".to_owned()));
        }
        let tag = EnvTag { id, label, tone, position };
        store.upsert_tag(&tag).await.map_err(|err| ServiceError::Message(db_err(err)))?;
        Ok(tag)
    }

    pub async fn delete_tag(&self, id: &str) -> Result<(), String> {
        let id = id.trim();
        if id.is_empty() || id == ENV_NONE {
            return Err("无标签不能删除".to_owned());
        }
        let store = self.store().await?;
        if store.tag(id).await.map_err(db_err)?.is_none() {
            return Err("找不到这个环境标签".to_owned());
        }
        store.delete_tag(id).await.map_err(db_err)?;
        Ok(())
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
        if saved.env != ENV_NONE && store.tag(&saved.env).await.map_err(|err| ServiceError::Message(db_err(err)))?.is_none() {
            saved.env = ENV_NONE.to_owned();
        }
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

    /// 导出全部（ids 为 None）或选中的连接到用户经保存对话框选定的路径。
    /// 机密永不进入导出内容；写入失败时返回明确原因。
    pub async fn export_connections(
        &self,
        ids: Option<Vec<String>>,
        passphrase: Option<&str>,
        path: &Path,
    ) -> Result<usize, String> {
        let store = self.store().await?;
        let rows = store.list().await.map_err(db_err)?;
        let selected = match ids {
            None => rows,
            Some(wanted) => {
                let mut picked = Vec::with_capacity(wanted.len());
                let mut absent = Vec::new();
                for id in &wanted {
                    match rows.iter().find(|row| &row.id == id) {
                        Some(row) => picked.push(row.clone()),
                        None => absent.push(id.clone()),
                    }
                }
                if !absent.is_empty() {
                    return Err(format!("找不到要导出的连接：{}", absent.join("、")));
                }
                picked
            }
        };
        let bytes = super::export::encode_export(&selected, passphrase)?;
        tokio::fs::write(path, &bytes)
            .await
            .map_err(|err| format!("写入导出文件失败：{err}"))?;
        self.log(format!("已导出 {} 条连接配置到 {}", selected.len(), path.display()));
        Ok(selected.len())
    }

    /// 读取 RillSQL 导出文件并解密。格式不符或口令错误时整体拒绝（Err），
    /// 解密成功后逐条解析：引擎不受支持的条目进 skipped 报告，不阻塞其余条目。
    pub async fn import_file(&self, path: &Path, passphrase: Option<&str>) -> Result<super::export::ParsedImport, String> {
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|err| format!("读不到这份连接文件：{err}"))?;
        super::export::parse_import(&bytes, passphrase)
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

    /// 把已经打开的 PostgreSQL 会话改连到同一个服务器上的另一个库。
    /// 不改已保存的配置。连不上时保留原来的会话。
    pub async fn focus_database(&self, id: &str, database: &str) -> Result<(), String> {
        let database = database.trim();
        if database.is_empty() {
            return Err("需要数据库名。".to_owned());
        }
        let store = self.store().await?;
        let config = store.get(id).await.map_err(db_err)?.ok_or_else(|| "找不到这条连接配置".to_owned())?;
        if config.engine != Engine::Postgres {
            return Err("只有 PostgreSQL 需要改连到另一个库。".to_owned());
        }
        if !self.sessions.lock().expect("sessions").contains_key(id) {
            return Err("这条连接还没有打开。".to_owned());
        }
        let outcome = self
            .exercise_overriding(id, SecretInput::default(), true, Some(database.to_owned()))
            .await?;
        if outcome.connected {
            return Ok(());
        }
        Err(focus_failure(&outcome.report))
    }

    /// 为这一次读取另开 PostgreSQL 会话。不替换当前会话，不调用改连，也不写已保存的配置。
    pub async fn open_ephemeral_postgres(&self, id: &str, database: &str) -> Result<EphemeralRead, String> {
        let database = database.trim();
        if database.is_empty() {
            return Err("需要数据库名。".to_owned());
        }
        let store = self.store().await?;
        let saved = store.get(id).await.map_err(db_err)?.ok_or_else(|| "找不到这条连接配置".to_owned())?;
        if saved.engine != Engine::Postgres {
            return Err("只有 PostgreSQL 需要临时读取会话。".to_owned());
        }
        if !self.sessions.lock().expect("sessions").contains_key(id) {
            return Err("这条连接还没有打开。".to_owned());
        }
        let saved_database = saved.database.clone();
        let mut config = saved;
        config.database = Some(database.to_owned());
        let saved_fingerprint = match &config.ssh {
            Some(ssh) => store.host_key(&ssh.host, ssh.port).await.map_err(db_err)?,
            None => None,
        };
        let password = self.resolve_secret(&password_account(&config.id), None, config.password_saved);
        let ssh_password = self.resolve_secret(&ssh_password_account(&config.id), None, config.ssh_password_saved);
        let stages = stages::planned_stages(&config);
        let mut attempt = Attempt {
            config: config.clone(),
            probes: self.probes.clone(),
            password,
            ssh_password,
            passphrase: None,
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
            sqlserver: None,
            redis: None,
            schema_note: None,
            foreign_keys: None,
            cancel: CancelTransport::None,
        };
        let report = stages::execute_stages(&stages, &mut attempt).await;
        let persisted = store.get(id).await.map_err(db_err)?.ok_or_else(|| "找不到这条连接配置".to_owned())?;
        if persisted.database != saved_database {
            return Err("临时读取会话改动了已保存的库名，已停止。".to_owned());
        }
        if !self.sessions.lock().expect("sessions").contains_key(id) {
            return Err("临时读取会话替换了当前会话，已停止。".to_owned());
        }
        if report.status != TestStatus::Ok {
            return Err(ephemeral_failure(&report));
        }
        let Some(client) = attempt.postgres.take() else {
            return Err("没有打开临时读取会话。".to_owned());
        };
        Ok(EphemeralRead { catalog: CatalogSession::Postgres(Arc::new(client)), shutdown: attempt.shutdown.take() })
    }

    pub fn disconnect(&self, id: &str) {
        let session = self.sessions.lock().expect("sessions").remove(id);
        self.reconnects.lock().expect("reconnects").remove(id);
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
        self.exercise_overriding(id, input, open, None).await
    }

    async fn exercise_overriding(
        &self,
        id: &str,
        input: SecretInput,
        open: bool,
        database: Option<String>,
    ) -> Result<OpenOutcome, String> {
        let store = self.store().await?;
        let mut config = store.get(id).await.map_err(db_err)?.ok_or_else(|| "找不到这条连接配置".to_owned())?;
        if let Some(database) = database {
            config.database = Some(database);
        }
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
            sqlserver: None,
            redis: None,
            schema_note: None,
            foreign_keys: None,
            cancel: CancelTransport::None,
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
                SessionKind::Postgres(Arc::new(client))
            } else if let Some(client) = attempt.sqlserver.take() {
                SessionKind::Sqlserver(Arc::new(tokio::sync::Mutex::new(client)))
            } else if let Some(conn) = attempt.redis.take() {
                SessionKind::Redis(Arc::new(tokio::sync::Mutex::new(conn)))
            } else {
                SessionKind::Scripted
            };
            let health = Arc::new(SessionHealth::default());
            let mut cancel = attempt.cancel.clone();
            cancel.attach_health(Arc::clone(&health));
            self.sessions.lock().expect("sessions").insert(
                id.to_owned(),
                Session {
                    kind,
                    shutdown: attempt.shutdown.take(),
                    foreign_keys,
                    cancel,
                    health,
                },
            );
            // 新会话（手动打开或自动重连成功）都视为健康，旧的自动重连计划作废。
            self.reconnects.lock().expect("reconnects").remove(id);
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
        #[cfg(test)]
        self.logs.lock().expect("logs").push(message);
        #[cfg(not(test))]
        let _ = message;
    }

    /// 会话当前状态：Some(Connected) / Some(Lost) / None（未打开）。lost 会话仍留在表里，
    /// 树上显示断开前读到的内容，但不再接受新的执行（R4：不静默排队重放）。
    pub fn session_condition(&self, id: &str) -> Option<SessionCondition> {
        let sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get(id)?;
        Some(if session.health.is_lost() {
            SessionCondition::Lost {
                reason: session.health.lost_reason(),
            }
        } else {
            SessionCondition::Connected
        })
    }

    pub fn cancel_transport(&self, id: &str) -> Option<CancelTransport> {
        self.sessions.lock().expect("sessions").get(id).map(|session| session.cancel.clone())
    }

    pub fn catalog_session(&self, id: &str) -> Option<CatalogSession> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get_mut(id)?;
        if session.health.is_lost() {
            return None;
        }
        // 读路径也算活动：目录/网格读取会刷新空闲计时，避免长任务期间误探活。
        session.health.note_activity();
        Some(match &session.kind {
            SessionKind::Sqlite(pool) => CatalogSession::Sqlite {
                pool: pool.clone(),
                foreign_keys: session.foreign_keys,
            },
            SessionKind::Mysql(pool) => CatalogSession::Mysql(pool.clone()),
            SessionKind::Postgres(client) => CatalogSession::Postgres(Arc::clone(client)),
            SessionKind::Sqlserver(client) => CatalogSession::Sqlserver(Arc::clone(client)),
            SessionKind::Redis(conn) => CatalogSession::Redis(Arc::clone(conn)),
            SessionKind::Scripted => CatalogSession::Scripted,
        })
    }

    /// Redis 会话句柄：键浏览、值读取与命令行共用同一条多路复用连接（互斥锁串行）。
    /// 返回共享的健康句柄供执行路径报告断线（R4 探活消费）。
    pub fn redis_session(
        &self,
        id: &str,
    ) -> Option<(std::sync::Arc<tokio::sync::Mutex<redis::aio::MultiplexedConnection>>, std::sync::Arc<SessionHealth>)> {
        let sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get(id)?;
        if session.health.is_lost() {
            return None;
        }
        session.health.note_activity();
        match &session.kind {
            SessionKind::Redis(conn) => Some((Arc::clone(conn), Arc::clone(&session.health))),
            _ => None,
        }
    }

    /// 前端 1s 轮询的落点：返回全部会话状态，并按需探活、驱动有限次退避重连（R4）。
    pub async fn session_statuses(self: &Arc<Self>) -> Vec<SessionStatus> {
        let targets: Vec<SessionHealthProbe> = {
            let sessions = self.sessions.lock().expect("sessions");
            sessions
                .iter()
                .map(|(id, session)| SessionHealthProbe {
                    id: id.clone(),
                    sqlite: matches!(session.kind, SessionKind::Sqlite(_)),
                    health: Arc::clone(&session.health),
                })
                .collect()
        };
        let mut out = Vec::with_capacity(targets.len());
        for target in targets {
            if !target.health.is_lost() && target.health.needs_probe(target.sqlite) {
                self.probe_session(&target).await;
            }
            let Some(condition) = self.session_condition(&target.id) else {
                // 快照与状态之间会话被显式断开：不再报告。
                continue;
            };
            match condition {
                SessionCondition::Connected => out.push(SessionStatus {
                    id: target.id,
                    status: "connected",
                    reason: None,
                }),
                SessionCondition::Lost { .. } => {
                    self.run_reconnect_if_due(&target.id).await;
                    // 重连可能就发生在这次调用里：成功后按 connected 报告
                    match self.session_condition(&target.id) {
                        Some(SessionCondition::Connected) => out.push(SessionStatus {
                            id: target.id,
                            status: "connected",
                            reason: None,
                        }),
                        _ => {
                            if self.reconnect_pending(&target.id) {
                                out.push(SessionStatus {
                                    id: target.id,
                                    status: "reconnecting",
                                    reason: None,
                                });
                            } else {
                                let reason = self.session_condition(&target.id).and_then(|item| match item {
                                    SessionCondition::Lost { reason } => reason,
                                    _ => None,
                                });
                                out.push(SessionStatus {
                                    id: target.id,
                                    status: "lost",
                                    reason,
                                });
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// 探活一个会话：失败标记 lost 并安排重连；超时无法判定，不动状态下一轮再试。
    async fn probe_session(&self, target: &SessionHealthProbe) {
        if target.health.busy() > 0 || !target.health.begin_probe() {
            return;
        }
        let session = {
            let sessions = self.sessions.lock().expect("sessions");
            match sessions.get(&target.id) {
                Some(session) if !session.health.is_lost() => Some(clone_catalog_handle(&session.kind)),
                _ => None,
            }
        };
        let Some(session) = session else {
            target.health.end_probe();
            return;
        };
        let probe = self.probes.health.as_deref().unwrap_or(&SelectOneProbe);
        let outcome = tokio::time::timeout(PROBE_TIMEOUT, probe.check(&session)).await;
        target.health.end_probe();
        match outcome {
            Ok(Ok(())) => {
                target.health.clear_suspect();
                target.health.note_activity();
            }
            Ok(Err(reason)) => {
                let text = format!("探活失败：{reason}");
                target.health.mark_lost(text);
                self.reconnects
                    .lock()
                    .expect("reconnects")
                    .entry(target.id.clone())
                    .or_insert_with(ReconnectPlan::fresh);
            }
            Err(_elapsed) => {
                // 探活超时说明无法判定（可能只是这条连接正忙）：不改状态。
            }
        }
    }

    /// lost 会话到期就重连一次（内联执行，in_flight 防止轮询并发重复拨号）。
    async fn run_reconnect_if_due(&self, id: &str) {
        {
            let mut plans = self.reconnects.lock().expect("reconnects");
            let Some(plan) = plans.get_mut(id) else { return };
            if plan.in_flight || plan.gave_up {
                return;
            }
            match plan.next_due {
                Some(due) if due <= Instant::now() => {}
                _ => return,
            }
            plan.in_flight = true;
        }
        self.attempt_reconnect(id).await;
    }

    /// 拨一次重连并按结果推进计划：确定性错误立即放弃，网络类按退避重试到上限。
    pub(crate) async fn attempt_reconnect(&self, id: &str) {
        let config = match self.store().await {
            Ok(store) => store.get(id).await.ok().flatten(),
            Err(_) => None,
        };
        let Some(config) = config else {
            self.finish_reconnect(
                id,
                DecisionWithMessage {
                    decision: RetryDecision::Stop {
                        reason: "连接配置已删除或读不出来，不再自动重连。".to_owned(),
                    },
                    message: String::new(),
                },
            );
            return;
        };
        // Redis 可以无密码直连：没有保存密码不阻止自动重连；其余远程引擎仍要求已保存密码。
        let passwordless_ok = matches!(config.engine, Engine::Sqlite | Engine::Redis);
        if !passwordless_ok && !config.password_saved {
            self.finish_reconnect(
                id,
                DecisionWithMessage {
                    decision: RetryDecision::Stop {
                        reason: "凭据库里没有保存的密码，已停在断开状态，请手动重连。".to_owned(),
                    },
                    message: String::new(),
                },
            );
            return;
        }
        let outcome = match self.exercise_overriding(id, SecretInput::default(), true, None).await {
            Ok(outcome) => outcome,
            Err(reason) => {
                self.finish_reconnect(
                    id,
                    DecisionWithMessage {
                        decision: RetryDecision::Stop { reason },
                        message: String::new(),
                    },
                );
                return;
            }
        };
        if outcome.connected {
            // exercise 已用新会话替换旧条目（全新健康状态），计划也已在插入时清掉。
            return;
        }
        let message = report_failure_text(&outcome.report);
        let decision = classify_reconnect_failure(&outcome.report);
        self.finish_reconnect(id, DecisionWithMessage { decision, message });
    }

    fn finish_reconnect(&self, id: &str, decision: DecisionWithMessage) {
        let DecisionWithMessage { decision, message } = decision;
        let mut plans = self.reconnects.lock().expect("reconnects");
        let plan = plans.entry(id.to_owned()).or_insert_with(ReconnectPlan::fresh);
        plan.in_flight = false;
        match decision {
            RetryDecision::Stop { reason } => {
                plan.gave_up = true;
                plan.next_due = None;
                plan.reason = Some(reason);
            }
            RetryDecision::Retry => {
                plan.attempts += 1;
                if plan.attempts >= MAX_RECONNECT_ATTEMPTS {
                    plan.gave_up = true;
                    plan.next_due = None;
                    plan.reason = Some(format!("{message}。已重试 {} 次仍失败，停止自动重连。", plan.attempts));
                } else {
                    plan.next_due = Some(Instant::now() + reconnect_backoff(plan.attempts));
                }
            }
        }
        // 退避等待期间保留探活失败的原因；只有最终结论（放弃）才更新提示。
        if let Some(reason) = plan.reason.clone() {
            if let Some(session) = self.sessions.lock().expect("sessions").get_mut(id) {
                if session.health.is_lost() {
                    session.health.set_lost_reason(Some(reason));
                }
            }
        }
    }

    fn reconnect_pending(&self, id: &str) -> bool {
        let plans = self.reconnects.lock().expect("reconnects");
        plans.get(id).is_some_and(|plan| !plan.gave_up)
    }

    pub fn set_catalog_foreign_keys(&self, id: &str, value: Option<bool>) -> bool {
        let mut sessions = self.sessions.lock().expect("sessions");
        let Some(session) = sessions.get_mut(id) else {
            return false;
        };
        session.foreign_keys = value;
        true
    }

    #[cfg(test)]
    pub fn new_for_test(path: PathBuf) -> Self {
        Self::new(path, Arc::new(super::secrets::MemoryStore::default()))
    }

    #[cfg(test)]
    pub async fn open_sqlite_for_test(&self, id: &str, name: &str, path: &Path) -> Result<(), String> {
        let mut draft = ConnectionDraft::blank(Engine::Sqlite);
        draft.id = Some(id.to_owned());
        draft.name = name.to_owned();
        draft.file_path = path.display().to_string();
        self.save(draft, SecretInput::default()).await.map_err(|err| err.message())?;
        let outcome = self.open(id, SecretInput::default()).await?;
        if !outcome.connected {
            let detail = outcome
                .report
                .stages
                .iter()
                .map(|stage| format!("{}:{:?}:{}", stage.label, stage.status, stage.message))
                .collect::<Vec<_>>()
                .join(" | ");
            return Err(format!(
                "测试连接没有打开：{} {}",
                outcome.report.summary.unwrap_or_default(),
                detail
            ));
        }
        Ok(())
    }
}

/// 空闲多久后开始探活（R4：长时间空闲或断线类错误后触发，避免每秒全量 ping）。
pub const IDLE_PROBE_AFTER: Duration = Duration::from_secs(30);
/// 单次探活的判定时限。超时视为无法判定，不改状态。
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// 自动重连的最大尝试次数（有限退避，之后停在 lost 等手动重连）。
pub const MAX_RECONNECT_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatus {
    pub id: String,
    /// connected | lost | reconnecting
    pub status: &'static str,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCondition {
    Connected,
    Lost { reason: Option<String> },
}

struct SessionHealthProbe {
    id: String,
    sqlite: bool,
    health: Arc<SessionHealth>,
}

fn clone_catalog_handle(kind: &SessionKind) -> CatalogSession {
    match kind {
        SessionKind::Sqlite(pool) => CatalogSession::Sqlite {
            pool: pool.clone(),
            foreign_keys: None,
        },
        SessionKind::Mysql(pool) => CatalogSession::Mysql(pool.clone()),
        SessionKind::Postgres(client) => CatalogSession::Postgres(Arc::clone(client)),
        SessionKind::Sqlserver(client) => CatalogSession::Sqlserver(Arc::clone(client)),
        SessionKind::Redis(conn) => CatalogSession::Redis(Arc::clone(conn)),
        SessionKind::Scripted => CatalogSession::Scripted,
    }
}

#[derive(Default)]
struct ReconnectPlan {
    attempts: u32,
    next_due: Option<Instant>,
    gave_up: bool,
    reason: Option<String>,
    in_flight: bool,
}

impl ReconnectPlan {
    fn fresh() -> Self {
        Self {
            attempts: 0,
            next_due: Some(Instant::now()),
            gave_up: false,
            reason: None,
            in_flight: false,
        }
    }
}

struct DecisionWithMessage {
    decision: RetryDecision,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryDecision {
    /// 认证 / TLS / 主机密钥 / 打不开文件等确定性错误：不重试，直接 lost + 原因（R4）。
    Stop { reason: String },
    /// 网络类失败：按退避再试。
    Retry,
}

/// 有限退避：第 n 次失败后等多久再试。
pub fn reconnect_backoff(attempt: u32) -> Duration {
    match attempt {
        1 => Duration::from_secs(1),
        2 => Duration::from_secs(5),
        _ => Duration::from_secs(15),
    }
}

/// 从分阶段报告判断重连是否值得再试：只有网络/隧道/版本类失败是瞬态的。
pub fn classify_reconnect_failure(report: &TestReport) -> RetryDecision {
    if report.status == TestStatus::Wait {
        return RetryDecision::Stop {
            reason: "服务器出示了新的 SSH 主机密钥，需要先在连接页确认后才能重连。".to_owned(),
        };
    }
    let Some(stage) = report.stages.iter().find(|stage| stage.status == StageStatus::Err) else {
        return RetryDecision::Stop {
            reason: report.summary.clone().unwrap_or_else(|| "重连没有成功".to_owned()),
        };
    };
    let message = report_failure_text(report);
    match stage.kind {
        StageKind::SshHostIdentity | StageKind::SshAuthentication | StageKind::Tls | StageKind::DatabaseAuthentication | StageKind::OpenFile => {
            RetryDecision::Stop {
                reason: format!("{message}。这是确定性错误，不会再自动重试。"),
            }
        }
        _ => RetryDecision::Retry,
    }
}

/// 分阶段报告里最可读的失败原因：先取失败阶段消息，其次提示，再次阶段名。
pub fn report_failure_text(report: &TestReport) -> String {
    if let Some(stage) = report.stages.iter().find(|stage| stage.status == StageStatus::Err) {
        if !stage.message.is_empty() {
            return stage.message.clone();
        }
    }
    if let Some(hint) = report.hint.as_deref().filter(|hint| !hint.is_empty()) {
        return hint.to_owned();
    }
    report.fail_stage.clone().unwrap_or_else(|| "重连没有成功".to_owned())
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
    Failed {
        stage: StageKind,
        error: StageError,
        network_done: bool,
        tls_done: bool,
    },
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
    sqlserver: Option<TdsClient>,
    redis: Option<redis::aio::MultiplexedConnection>,
    schema_note: Option<String>,
    foreign_keys: Option<bool>,
    cancel: CancelTransport,
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
            Some(SshAuthKind::Key) => "使用私钥登录成功".to_owned(),
            _ => "使用密码登录成功".to_owned(),
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
        Ok(format!("本机 127.0.0.1:{local} 经 SSH 跳板机打通到数据库 {host}:{port} 的通道"))
    }

    async fn ensure_remote(&mut self) {
        if !matches!(self.remote, Remote::Fresh) {
            return;
        }
        if self.probes.database.is_some() {
            self.remote = Remote::Probed;
            return;
        }
        let tls_mode = self.tls_mode();
        // Redis 允许无密码直连（本地开发常见）：空密码不是“凭据缺失”，照常建连。
        let empty_password;
        let password = match self.password.as_ref() {
            Some(secret) => secret,
            None if self.config.engine == Engine::Redis => {
                empty_password = Secret::new("");
                &empty_password
            }
            None => {
                // 密码在 TCP/TLS 之前就缺了，不能按失败阶段的先后把前面标成已完成。
                self.remote = Remote::Failed {
                    stage: StageKind::DatabaseAuthentication,
                    error: stages::fail("请填写数据库密码", "凭据库没有可用密码，本次也没有输入。没有把空密码写入配置。"),
                    network_done: false,
                    tls_done: false,
                };
                return;
            }
        };
        let route = match self.route() {
            Ok(route) => route,
            Err(error) => {
                self.remote = remote_failed(StageKind::Network, error, tls_mode);
                return;
            }
        };
        if self.config.ssh.is_some() && self.forwarder_port.is_none() {
            self.remote = remote_failed(
                StageKind::Network,
                stages::fail("隧道没有就绪", "尚未连接数据库。"),
                tls_mode,
            );
            return;
        }
        let password = password.expose().to_owned();
        let opened = match self.config.engine {
            Engine::Mysql | Engine::Mariadb => drivers::open_mysql(&self.config, &route, &password).await.map(|opened| {
                let via_ssh = self.config.ssh.is_some();
                self.cancel = CancelTransport::Mysql {
                    connection_id: Arc::new(AtomicU64::new(opened.connection_id)),
                    opts: opened.kill_opts,
                    via_ssh,
                    health: Arc::new(SessionHealth::default()),
                };
                self.mysql_pool = Some(opened.pool);
                opened.version
            }),
            Engine::Postgres => drivers::open_postgres(&self.config, &route, &password).await.map(|opened| {
                let via_ssh = self.config.ssh.is_some();
                self.cancel = CancelTransport::Postgres {
                    token: opened.cancel,
                    tls: opened.tls,
                    via_ssh,
                    health: Arc::new(SessionHealth::default()),
                };
                self.postgres = Some(opened.client);
                opened.version
            }),
            Engine::Sqlserver => drivers::open_sqlserver(&self.config, &route, &password).await.map(|opened| {
                let via_ssh = self.config.ssh.is_some();
                self.cancel = CancelTransport::Sqlserver {
                    spid: Arc::new(AtomicU32::new(opened.spid)),
                    config: opened.kill_config,
                    via_ssh,
                    health: Arc::new(SessionHealth::default()),
                };
                self.sqlserver = Some(opened.client);
                opened.version
            }),
            Engine::Redis => drivers::open_redis(&self.config, &route, &password).await.map(|opened| {
                self.cancel = CancelTransport::Redis {
                    health: Arc::new(SessionHealth::default()),
                };
                self.redis = Some(opened.conn);
                opened.version
            }),
            Engine::Sqlite => Err(stages::fail("SQLite 不走远程连接", "未执行")),
        };
        match opened {
            Ok(version) => self.remote = Remote::Done { version },
            Err(error) => {
                let stage = classify_db(&error);
                self.remote = remote_failed(stage, error, tls_mode);
            }
        }
    }

    fn tls_mode(&self) -> TlsMode {
        self.config.tls.as_ref().map(|tls| tls.mode).unwrap_or(TlsMode::Off)
    }

    fn tls_server_name(&self) -> String {
        self.config
            .tls
            .as_ref()
            .map(|tls| tls.server_name.clone())
            .filter(|name| !name.is_empty())
            .or_else(|| self.config.host.clone())
            .unwrap_or_default()
    }

    fn finish_stage(&self, stage: StageKind, ok: impl FnOnce() -> String) -> Result<String, StageError> {
        match &self.remote {
            Remote::Done { version } => Ok(if stage == StageKind::ServerVersion { version.clone() } else { ok() }),
            Remote::Failed {
                stage: failed,
                error,
                network_done,
                tls_done,
            } => {
                if *failed == stage {
                    return Err(error.clone());
                }
                let completed = match stage {
                    StageKind::Network => *network_done,
                    StageKind::Tls => *tls_done,
                    StageKind::DatabaseAuthentication => rank(*failed) > rank(StageKind::DatabaseAuthentication),
                    _ => false,
                };
                if completed {
                    return Ok(ok());
                }
                if rank(*failed) > rank(stage) {
                    return Err(StageError::NotAttempted);
                }
                Err(error.clone())
            }
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
        self.finish_stage(StageKind::Network, || format!("{host}:{port}"))
    }

    async fn tls(&mut self) -> Result<String, StageError> {
        if let Some(probe) = self.probes.database.clone() {
            let route = self.route()?;
            return probe.check_tls(&route).await;
        }
        self.ensure_remote().await;
        let server_name = self.tls_server_name();
        let mode = self.tls_mode();
        self.finish_stage(StageKind::Tls, || tls_stage_message(mode, &server_name))
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
        if self.config.engine == Engine::Redis {
            let note = if user.is_empty() {
                "密码认证".to_owned()
            } else {
                format!("ACL 用户 {user}")
            };
            return self.finish_stage(StageKind::DatabaseAuthentication, || note);
        }
        self.finish_stage(StageKind::DatabaseAuthentication, || format!("用户 {user}"))
    }

    async fn server_version(&mut self) -> Result<String, StageError> {
        if let Some(probe) = self.probes.database.clone() {
            let route = self.route()?;
            return probe.server_version(&self.config, &route).await;
        }
        self.ensure_remote().await;
        self.finish_stage(StageKind::ServerVersion, || "未知版本".to_owned())
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
        self.cancel = CancelTransport::Sqlite(Arc::new(AtomicBool::new(false)), Arc::new(SessionHealth::default()));
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
    fn stage_label(&self, stage: StageKind) -> String {
        if stage == StageKind::Tls {
            return StageKind::tls_label(self.tls_mode()).to_owned();
        }
        stage.label_zh().to_owned()
    }

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

pub(crate) fn tls_stage_message(mode: TlsMode, server_name: &str) -> String {
    if mode == TlsMode::Verify {
        format!("已核对证书，主机名 {server_name} 与证书一致")
    } else {
        "已加密，但没有核对证书和主机名".to_owned()
    }
}

fn remote_failed(stage: StageKind, error: StageError, tls_mode: TlsMode) -> Remote {
    Remote::Failed {
        stage,
        error,
        network_done: rank(stage) > rank(StageKind::Network),
        tls_done: tls_mode != TlsMode::Off && rank(stage) > rank(StageKind::Tls),
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

fn ephemeral_failure(report: &TestReport) -> String {
    let message = report
        .stages
        .iter()
        .find(|stage| stage.status == StageStatus::Err)
        .map(|stage| stage.message.as_str())
        .filter(|message| !message.is_empty())
        .unwrap_or("没有连上这个库");
    format!("{message}。当前会话没有改连。")
}

fn focus_failure(report: &TestReport) -> String {
    let message = report
        .stages
        .iter()
        .find(|stage| stage.status == StageStatus::Err)
        .map(|stage| stage.message.as_str())
        .filter(|message| !message.is_empty())
        .unwrap_or("没有连上这个库");
    format!("{message}。原来的会话还在。")
}

fn classify_db(error: &StageError) -> StageKind {
    let StageError::Fail { message, hint } = error else {
        return StageKind::Network;
    };
    if hint.contains("网络连接失败") || hint.contains("隧道没有") || hint.contains("通道没有打通") {
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

#[cfg(test)]
pub(crate) enum CannedRemote {
    Connected,
    AuthenticationFailed,
}

#[cfg(test)]
pub(crate) async fn report_canned_remote(config: SavedConnection, canned: CannedRemote) -> TestReport {
    let tls_mode = config.tls.as_ref().map(|tls| tls.mode).unwrap_or(TlsMode::Off);
    let remote = match canned {
        CannedRemote::Connected => Remote::Done { version: "8.4.3".to_owned() },
        // 驱动只有在握手完成后才会报认证失败，所以网络和 TLS 都已完成。
        CannedRemote::AuthenticationFailed => remote_failed(
            StageKind::DatabaseAuthentication,
            stages::fail("Access denied for user", "请检查数据库用户名和密码。"),
            tls_mode,
        ),
    };
    let mut attempt = Attempt {
        config: config.clone(),
        probes: Probes::default(),
        password: None,
        ssh_password: None,
        passphrase: None,
        saved_fingerprint: None,
        ssh_fault: None,
        ssh_started: false,
        ssh_authed: false,
        forwarder_port: None,
        shutdown: None,
        remote,
        sqlite_pool: None,
        mysql_pool: None,
        postgres: None,
        sqlserver: None,
        redis: None,
        schema_note: None,
        foreign_keys: None,
        cancel: CancelTransport::None,
    };
    stages::execute_stages(&stages::planned_stages(&config), &mut attempt).await
}

#[cfg(test)]
mod health_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::{
        classify_reconnect_failure, reconnect_backoff, report_failure_text, CancelTransport, RetryDecision, SessionHealth,
        IDLE_PROBE_AFTER, MAX_RECONNECT_ATTEMPTS,
    };
    use crate::connections::stages::{StageKind, StageStatus, StageView, TestReport, TestStatus};

    fn report(failed: Option<StageKind>, wait: bool) -> TestReport {
        let mut stages = Vec::new();
        for kind in [StageKind::SshHostIdentity, StageKind::Network, StageKind::Tls, StageKind::DatabaseAuthentication] {
            let status = if failed == Some(kind) {
                if wait { StageStatus::Wait } else { StageStatus::Err }
            } else if failed.is_some() {
                StageStatus::Skipped
            } else {
                StageStatus::Ok
            };
            stages.push(StageView {
                kind,
                label: kind.label_zh().to_owned(),
                status,
                message: if status == StageStatus::Err { "Access denied for user".to_owned() } else { String::new() },
            });
        }
        TestReport {
            status: if wait { TestStatus::Wait } else if failed.is_some() { TestStatus::Err } else { TestStatus::Ok },
            fail_stage: failed.map(|kind| kind.label_zh().to_owned()),
            hint: None,
            summary: None,
            fingerprint: None,
            algorithm: None,
            stages,
        }
    }

    #[test]
    fn probe_triggers_on_suspect_or_idle_but_never_for_sqlite() {
        let health = SessionHealth::default();
        assert!(!health.needs_probe(false));
        health.note_disconnect();
        assert!(health.needs_probe(false));
        assert!(!health.needs_probe(true), "SQLite 免探活");
        health.clear_suspect();
        assert!(!health.needs_probe(false));
        health.rewind_idle(Duration::from_secs(31));
        assert!(health.idle_for() >= IDLE_PROBE_AFTER);
        assert!(health.needs_probe(false));
    }

    #[test]
    fn busy_sessions_are_not_probed() {
        let health = SessionHealth::default();
        health.note_disconnect();
        health.note_busy();
        // busy 只是轮询侧的跳过条件，这里确认计数语义
        assert_eq!(health.busy(), 1);
        health.note_idle();
        assert_eq!(health.busy(), 0);
    }

    #[test]
    fn transport_notes_disconnect_on_the_shared_health_handle() {
        let shared = Arc::new(SessionHealth::default());
        let placeholder = Arc::new(SessionHealth::default());
        let mut transport = CancelTransport::Sqlite(Arc::new(std::sync::atomic::AtomicBool::new(false)), Arc::clone(&placeholder));
        transport.note_disconnect();
        assert!(placeholder.is_suspect(), "断线报告落在当前挂着的句柄上");
        transport.attach_health(Arc::clone(&shared));
        assert!(!shared.is_suspect(), "替换句柄时旧状态不带入");
        transport.note_disconnect();
        assert!(shared.is_suspect());
        transport.note_activity();
        shared.clear_suspect();
        assert!(!shared.is_suspect());
        // lost 之后执行路径的断线报告不会再把状态改回去
        shared.mark_lost("探活失败：connection reset".to_owned());
        assert!(shared.is_lost());
        assert_eq!(shared.lost_reason().as_deref(), Some("探活失败：connection reset"));
        transport.note_disconnect();
        assert!(shared.is_lost());
    }

    #[test]
    fn deterministic_failures_stop_reconnect_and_network_failures_retry() {
        let auth = classify_reconnect_failure(&report(Some(StageKind::DatabaseAuthentication), false));
        let RetryDecision::Stop { reason } = &auth else {
            panic!("认证失败必须停止重试");
        };
        assert!(reason.contains("Access denied"));
        assert!(reason.contains("不会再自动重试"));
        assert!(matches!(classify_reconnect_failure(&report(Some(StageKind::Tls), false)), RetryDecision::Stop { .. }));
        assert!(matches!(
            classify_reconnect_failure(&report(Some(StageKind::SshHostIdentity), false)),
            RetryDecision::Stop { .. }
        ));
        // 等待确认主机密钥也直接停：需要人工介入
        assert!(matches!(classify_reconnect_failure(&report(Some(StageKind::SshHostIdentity), true)), RetryDecision::Stop { .. }));
        assert!(matches!(classify_reconnect_failure(&report(Some(StageKind::Network), false)), RetryDecision::Retry));
        // 完全成功的报告没有失败原因可读
        assert_eq!(report_failure_text(&report(None, false)), "重连没有成功");
    }

    #[test]
    fn backoff_is_bounded_and_limited() {
        assert_eq!(reconnect_backoff(1), Duration::from_secs(1));
        assert_eq!(reconnect_backoff(2), Duration::from_secs(5));
        assert_eq!(reconnect_backoff(3), Duration::from_secs(15));
        assert_eq!(reconnect_backoff(9), Duration::from_secs(15));
        assert_eq!(MAX_RECONNECT_ATTEMPTS, 3);
    }
}
