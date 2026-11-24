use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use futures::{stream, StreamExt};

use crate::adapters::adapter;
use crate::adapters::session::{Opened, ScriptedSession, Session as LiveSession};
use crate::connections::drivers::{self, TunnelFault};
use crate::connections::groups::{validate_group_name, ConnectionGroup};
use crate::connections::hostkey::{self, HostKeyDecision, PresentedKey};
use crate::connections::model::{
    password_account, ssh_password_account, validate_tag_label, ConnectionDraft, EnvTag, FieldError, SavedConnection, Secret, SecretInput,
    SshAuthKind, SshSettings, TlsMode, ENV_NONE,
};
use crate::connections::route::{self, Route};
use crate::connections::secrets::{MigrationReport, SecretStore, VaultNotice};
use crate::connections::stages::{self, StageError, StageKind, StageRunner, StageStatus, TestReport, TestStatus};
use crate::connections::store::{self, Store};
use crate::connections::transaction::{ExecutionLease, TransactionCoordinator};
use crate::transaction::{TransactionDialect, TransactionSnapshot, UnknownReason};

pub use crate::adapters::session::{CancelTransport, CatalogSession};

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
    /// 探活探针。None 时用会话自己的探活（各适配器的 `Session::ping`；本地文件免）。
    pub health: Option<Arc<dyn HealthProbe>>,
}

/// 会话探活：在已打开的会话上发一条最小语句确认连接还活着。
#[async_trait::async_trait]
pub trait HealthProbe: Send + Sync {
    async fn check(&self, session: &CatalogSession) -> Result<(), String>;
}

#[derive(Clone)]
struct PendingHostKey {
    host: String,
    port: u16,
    algorithm: String,
    fingerprint: String,
}

/// 会话表里的一条已打开会话。
struct LiveEntry {
    session: Arc<dyn LiveSession>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    foreign_keys: Option<bool>,
    cancel: CancelTransport,
    health: Arc<SessionHealth>,
    /// 连接阶段读到的服务器版本说明；没有读到时为 None。
    server_version: Option<String>,
    /// 连接级事务协调器：共享快照、业务执行租约与重连后的旧事务未知信息。
    transaction: Arc<TransactionCoordinator>,
}

/// 业务查询句柄与它的串行执行租约。租约持有到丢弃为止。
pub struct QuerySession {
    pub session: CatalogSession,
    /// 关系型连接才有；Redis/MongoDB 保持原有的并发语义（R12）。
    pub lease: Option<ExecutionLease>,
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

/// 一次对比用的临时读取会话（会话绑定单库的引擎）。丢弃时关闭它自己的通道，不动用户正在使用的会话。
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

pub struct Service {
    path: PathBuf,
    secrets: Arc<dyn SecretStore>,
    #[cfg(test)]
    logs: Mutex<Vec<String>>,
    cell: tokio::sync::OnceCell<Store>,
    sessions: Mutex<HashMap<String, LiveEntry>>,
    operation_sequence: AtomicU64,
    operation_epochs: Mutex<HashMap<String, u64>>,
    pending: Mutex<HashMap<String, PendingHostKey>>,
    probes: Probes,
    /// lost 会话的自动重连计划：有限次退避，确定性错误直接放弃（R4）。
    reconnects: Mutex<HashMap<String, ReconnectPlan>>,
    /// 本次运行里手动输入、没有存进凭据库的机密（凭据账户名 → 机密），只在内存里。
    /// 选择不保存密码或系统钥匙串不可用时，同一次运行里切换数据库、手动重连不必再问一遍；
    /// 自动重连仍只用凭据库里的密码（没保存就等手动入口）。删除或重新保存这条连接时作废，进程退出即消失。
    typed: Mutex<HashMap<String, Secret>>,
    /// 已断开连接保留的事务协调器：连接不在了，旧事务的未知结果仍要能读到（R10）。
    /// 重新打开同一连接时取回并按新会话处理；删除连接时清掉。
    retired: Mutex<HashMap<String, Arc<TransactionCoordinator>>>,
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
            operation_sequence: AtomicU64::new(1),
            operation_epochs: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            probes,
            reconnects: Mutex::new(HashMap::new()),
            typed: Mutex::new(HashMap::new()),
            retired: Mutex::new(HashMap::new()),
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

    /// 按连接保存的界面偏好（JSON 文本），没有时为 None。
    pub async fn connection_prefs(&self, id: &str, ns: &str) -> Result<Option<String>, String> {
        self.store().await?.prefs(id, ns).await.map_err(db_err)
    }

    pub async fn save_connection_prefs(&self, id: &str, ns: &str, value: &str) -> Result<(), String> {
        self.store().await?.save_prefs(id, ns, value).await.map_err(db_err)
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

    /// 标签面板里“移出此标签”：连接变为无标签，其余配置不变。
    pub async fn untag_connection(&self, id: &str) -> Result<(), String> {
        let store = self.store().await?;
        if !store.clear_env(id).await.map_err(db_err)? {
            return Err("找不到这个连接".to_owned());
        }
        Ok(())
    }

    /// 分组与成员关系一次读出；资源管理器、启动台与分组命令都只认这一份快照。
    pub async fn groups(&self) -> Result<(Vec<ConnectionGroup>, std::collections::BTreeMap<String, String>), String> {
        let store = self.store().await?;
        let groups = store.list_groups().await.map_err(db_err)?;
        let members = store.memberships().await.map_err(db_err)?;
        Ok((groups, members.into_iter().collect()))
    }

    /// 新建或重命名分组。重名允许（靠稳定 id 区分），空名与超长名在这里拒绝。
    pub async fn save_group(&self, id: Option<String>, name: String) -> Result<ConnectionGroup, ServiceError> {
        let name = validate_group_name(&name).map_err(ServiceError::Message)?;
        let store = self.store().await.map_err(ServiceError::Message)?;
        let requested = id.unwrap_or_default();
        let requested = requested.trim();
        let group = if requested.is_empty() {
            let position = store.next_group_position().await.map_err(|err| ServiceError::Message(db_err(err)))?;
            ConnectionGroup::new(uuid::Uuid::new_v4().to_string(), name, position)
        } else {
            let existing = store
                .group(requested)
                .await
                .map_err(|err| ServiceError::Message(db_err(err)))?
                .ok_or_else(|| ServiceError::Message("找不到这个分组".to_owned()))?;
            ConnectionGroup { name, ..existing }
        };
        store.upsert_group(&group).await.map_err(|err| ServiceError::Message(db_err(err)))?;
        Ok(group)
    }

    /// 删分组：只删分组记录与成员关系，连接回到顶层；连接配置一行都不动。
    pub async fn delete_group(&self, id: &str) -> Result<(), String> {
        let id = id.trim();
        if id.is_empty() {
            return Err("找不到这个分组".to_owned());
        }
        let store = self.store().await?;
        if store.group(id).await.map_err(db_err)?.is_none() {
            return Err("找不到这个分组".to_owned());
        }
        store.delete_group(id).await.map_err(db_err)?;
        Ok(())
    }

    pub async fn set_group_collapsed(&self, id: &str, collapsed: bool) -> Result<(), String> {
        let store = self.store().await?;
        if store.group(id).await.map_err(db_err)?.is_none() {
            return Err("找不到这个分组".to_owned());
        }
        store.set_group_collapsed(id, collapsed).await.map_err(db_err)
    }

    /// 拖拽与右键“移动到分组”走同一条命令；None 表示移回顶层。
    pub async fn assign_group(&self, connection_id: &str, group: Option<&str>) -> Result<(), String> {
        let store = self.store().await?;
        if store.get(connection_id).await.map_err(db_err)?.is_none() {
            return Err("找不到这个连接".to_owned());
        }
        if let Some(group) = group {
            if store.group(group).await.map_err(db_err)?.is_none() {
                return Err("找不到这个分组".to_owned());
            }
        }
        store.assign_group(connection_id, group).await.map_err(db_err)
    }

    /// R6：重排一个容器（分组；None 为未分组）里的连接。列表里的连接一并归入这个容器，
    /// 所以“拖到另一个分组的某个位置”也是这一条命令。未知的连接或分组整体拒绝。
    pub async fn reorder_connections(&self, group: Option<&str>, ordered_ids: &[String]) -> Result<(), String> {
        let store = self.store().await?;
        if let Some(group) = group {
            if store.group(group).await.map_err(db_err)?.is_none() {
                return Err("找不到这个分组".to_owned());
            }
        }
        let known: std::collections::BTreeSet<String> = store.list().await.map_err(db_err)?.into_iter().map(|row| row.id).collect();
        if ordered_ids.iter().any(|id| !known.contains(id)) {
            return Err("找不到这个连接".to_owned());
        }
        store.reorder_connections(group, ordered_ids).await.map_err(db_err)
    }

    /// R6：按给定顺序重排分组；未知分组整体拒绝。
    pub async fn reorder_groups(&self, ordered_ids: &[String]) -> Result<(), String> {
        let store = self.store().await?;
        let known: std::collections::BTreeSet<String> = store.list_groups().await.map_err(db_err)?.into_iter().map(|group| group.id).collect();
        if ordered_ids.iter().any(|id| !known.contains(id)) {
            return Err("找不到这个分组".to_owned());
        }
        store.reorder_groups(ordered_ids).await.map_err(db_err)
    }

    /// R6：导入保存完连接后调用一次，恢复文件里的分组与顺序。
    /// `groups` 为 `ParsedImport::groups`，`imported_ids` 为实际导入成功的连接 id（按文件顺序）。
    /// 同 id 或同名的分组复用，没有的新建并排到最后；导入的连接依次接到各自容器的末尾。
    /// 旧格式文件（没有分组信息）只保持文件顺序，不改动已有的成员关系。
    pub async fn restore_import_layout(&self, groups: &[super::export::ExportGroup], imported_ids: &[String]) -> Result<(), String> {
        let store = self.store().await?;
        let imported: std::collections::BTreeSet<&str> = imported_ids.iter().map(String::as_str).collect();
        let mut existing = store.list_groups().await.map_err(db_err)?;
        let mut ordered: Vec<&super::export::ExportGroup> = groups.iter().collect();
        ordered.sort_by_key(|group| group.position);
        let mut placed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for incoming in ordered {
            let members: Vec<String> = incoming
                .members
                .iter()
                .filter(|id| imported.contains(id.as_str()) && !placed.contains(*id))
                .cloned()
                .collect();
            let reuse = existing
                .iter()
                .find(|group| group.id == incoming.id)
                .or_else(|| existing.iter().find(|group| group.name == incoming.name))
                .map(|group| group.id.clone());
            let group_id = match reuse {
                Some(id) => id,
                None => {
                    let Ok(name) = validate_group_name(&incoming.name) else { continue };
                    let position = store.next_group_position().await.map_err(db_err)?;
                    let id = if incoming.id.trim().is_empty() { uuid::Uuid::new_v4().to_string() } else { incoming.id.clone() };
                    let group = ConnectionGroup { id, name, position, collapsed: incoming.collapsed };
                    store.upsert_group(&group).await.map_err(db_err)?;
                    existing.push(group.clone());
                    group.id
                }
            };
            store.append_in_order(Some(&group_id), &members).await.map_err(db_err)?;
            placed.extend(members);
        }
        let rest: Vec<String> = imported_ids.iter().filter(|id| !placed.contains(*id)).cloned().collect();
        if !groups.is_empty() {
            // 文件带分组信息：没在任何分组里的连接就是未分组。
            for id in &rest {
                store.assign_group(id, None).await.map_err(db_err)?;
            }
        }
        store.append_in_order(None, &rest).await.map_err(db_err)
    }

    /// 迁入新连接只处理本地配置，不访问驱动或钥匙串。
    pub(crate) async fn insert_imported_connection(&self, draft: &ConnectionDraft) -> Result<store::ImportInsert, ServiceError> {
        for (field, host) in [("host", draft.host.as_str()), ("ssh_host", draft.ssh_host.as_str())] {
            if host.contains("://") || host.contains(['@', '?', ';', '#', '=']) {
                return Err(ServiceError::Fields(vec![FieldError {
                    field: field.to_owned(),
                    message: "请填写单独的主机地址".to_owned(),
                }]));
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let candidate = crate::connections::model::normalize(draft, id).map_err(ServiceError::Fields)?;
        let store = self.store().await.map_err(ServiceError::Message)?;
        store.insert_imported(&candidate).await.map_err(|_| ServiceError::Message("写入本地连接配置失败".to_owned()))
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
        // 配置要改了：这次运行里手输的机密可能已对不上新配置，作废，下次连接按新配置再要。
        self.forget_typed(&saved.id);
        if saved.env != ENV_NONE && store.tag(&saved.env).await.map_err(|err| ServiceError::Message(db_err(err)))?.is_none() {
            saved.env = ENV_NONE.to_owned();
        }
        let mut notes = Vec::new();
        if adapter(saved.engine).meta().file_based() {
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
        let ids_was_none = ids.is_none();
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
        // R6：分组与组内顺序一起导出（只含被导出的连接；导出全部时空分组也带上）。
        let all = ids_was_none;
        let mut groups = Vec::new();
        let members: std::collections::BTreeMap<String, String> = store.memberships().await.map_err(db_err)?.into_iter().collect();
        for group in store.list_groups().await.map_err(db_err)? {
            let inside: Vec<String> = selected
                .iter()
                .filter(|row| members.get(&row.id) == Some(&group.id))
                .map(|row| row.id.clone())
                .collect();
            if all || !inside.is_empty() {
                groups.push(super::export::ExportGroup {
                    id: group.id,
                    name: group.name,
                    position: group.position,
                    collapsed: group.collapsed,
                    members: inside,
                });
            }
        }
        let bytes = super::export::encode_export_with_groups(&selected, &groups, passphrase)?;
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
        // 连接配置删除后不再保留它的事务记录。
        self.retired.lock().expect("retired transactions").remove(id);
        let store = self.store().await?;
        let existing = store.get(id).await.map_err(db_err)?.ok_or_else(|| "找不到这条连接配置".to_owned())?;
        // 只清理确实保存过的机密；没有保存过的连接不访问凭据库。删不掉只留下加密残留，不阻止删除配置。
        for (account, saved) in [
            (password_account(id), existing.password_saved),
            (ssh_password_account(id), existing.ssh_password_saved),
        ] {
            if saved {
                if let Err(err) = self.secrets.delete(&account) {
                    self.log(format!("删除已保存的密码失败：{}", err.guidance()));
                }
            }
        }
        self.forget_typed(id);
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

    /// 把已经打开的会话改连到同一个服务器上的另一个库（会话绑定单库的引擎，如 PostgreSQL）。
    /// 不改已保存的配置。连不上时保留原来的会话。
    pub async fn focus_database(&self, id: &str, database: &str) -> Result<(), String> {
        let database = database.trim();
        if database.is_empty() {
            return Err("需要数据库名。".to_owned());
        }
        let store = self.store().await?;
        let config = store.get(id).await.map_err(db_err)?.ok_or_else(|| "找不到这条连接配置".to_owned())?;
        if !adapter(config.engine).capabilities().database_scoped_session {
            return Err(format!("只有 {} 需要改连到另一个库。", crate::adapters::labels_with(|caps| caps.database_scoped_session, " 和 ")));
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

    /// 为这一次读取另开会话（会话绑定单库的引擎，如 PostgreSQL）。不替换当前会话，不调用改连，也不写已保存的配置。
    pub async fn open_ephemeral_postgres(&self, id: &str, database: &str) -> Result<EphemeralRead, String> {
        let database = database.trim();
        if database.is_empty() {
            return Err("需要数据库名。".to_owned());
        }
        let store = self.store().await?;
        let saved = store.get(id).await.map_err(db_err)?.ok_or_else(|| "找不到这条连接配置".to_owned())?;
        if !adapter(saved.engine).capabilities().database_scoped_session {
            return Err(format!("只有 {} 需要临时读取会话。", crate::adapters::labels_with(|caps| caps.database_scoped_session, " 和 ")));
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
        let password = self.resolve_secret(&password_account(&config.id), None, config.password_saved)?;
        let ssh_password = self.resolve_secret(&ssh_password_account(&config.id), None, config.ssh_password_saved)?;
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
            opened: None,
            schema_note: None,
            foreign_keys: None,
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
        let Some(opened) = attempt.opened.take() else {
            return Err("没有打开临时读取会话。".to_owned());
        };
        Ok(EphemeralRead { catalog: opened.session.catalog(None), shutdown: attempt.shutdown.take() })
    }

    pub fn disconnect(&self, id: &str) {
        let session = {
            let mut epochs = self.operation_epochs.lock().expect("connection epochs");
            epochs.insert(id.to_owned(), self.operation_sequence.fetch_add(1, Ordering::SeqCst));
            let session = self.sessions.lock().expect("sessions").remove(id);
            self.reconnects.lock().expect("reconnects").remove(id);
            session
        };
        if let Some(session) = session {
            // 断开后旧事务的最终结果无法从数据库确认：保留为未知，重连/重开后仍能看到（R10）。
            session.transaction.reconnect(UnknownReason::ConnectionLost);
            self.retired
                .lock()
                .expect("retired transactions")
                .insert(id.to_owned(), Arc::clone(&session.transaction));
            if let Some(shutdown) = session.shutdown {
                let _ = shutdown.send(());
            }
            drop(session.session);
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
            Err(err) => {
                push_note(notes, &format!("密码没有保存，只用于本次连接，没有写入配置。{}", err.guidance()));
                false
            }
        }
    }

    fn clear_secret(&self, account: &str, previous: bool, notes: &mut Vec<String>) -> bool {
        if !previous {
            // 从没保存过：不访问凭据库（新建连接不触发钥匙串授权）。
            return false;
        }
        match self.secrets.delete(account) {
            Ok(()) => false,
            Err(err) => {
                push_note(notes, &format!("已保存的密码没能清除，可能仍在凭据库中。配置文件里没有密码。{}", err.guidance()));
                true
            }
        }
    }

    /// 本次输入 > 凭据库里保存的 > 本次运行手输过的。凭据库读取失败且没有手输机密可用时，
    /// 返回带恢复指引的错误文字（钥匙串被拒、保险库损坏、旧条目迁移失败），而不是当成“没有密码”。
    fn resolve_secret(&self, account: &str, typed: Option<&Secret>, saved: bool) -> Result<Option<Secret>, String> {
        if let Some(secret) = typed.filter(|item| !item.is_empty()) {
            return Ok(Some(secret.clone()));
        }
        let remembered = self.typed.lock().expect("typed secrets").get(account).cloned();
        if !saved {
            return Ok(remembered);
        }
        match self.secrets.get(account) {
            Ok(Some(value)) => Ok(Some(Secret::new(value))),
            Ok(None) => Ok(remembered),
            Err(err) => {
                self.log(format!("读取已保存的密码失败：{err}"));
                match remembered {
                    Some(secret) => Ok(Some(secret)),
                    None => Err(err.guidance()),
                }
            }
        }
    }

    /// 重新迁移之前没能迁入保险库的旧钥匙串条目（界面上的“重试”）。
    #[allow(dead_code)] // 界面入口在 T9（设置页/提示条）接入。
    pub async fn retry_failed_migration(&self) -> Result<MigrationReport, String> {
        self.store().await?;
        let report = self.secrets.retry_failed_migration().map_err(|err| err.guidance())?;
        if !report.migrated.is_empty() {
            self.log(format!("已把 {} 条旧凭据迁入本地保险库", report.migrated.len()));
        }
        Ok(report)
    }

    /// 仍未迁入保险库的旧条目（凭据账户、原因）。
    #[allow(dead_code)] // 界面入口在 T9（设置页/提示条）接入。
    pub async fn secret_migration_failures(&self) -> Result<Vec<(String, String)>, String> {
        self.store().await?;
        self.secrets.migration_failures().map_err(|err| err.guidance())
    }

    /// 取走待提示的凭据事件（保险库损坏、迁移失败）。
    #[allow(dead_code)] // 界面入口在 T9（设置页/提示条）接入。
    pub fn take_secret_notices(&self) -> Vec<VaultNotice> {
        self.secrets.take_notices()
    }

    /// 本地保险库文件位置；凭据后端不是保险库时为 None。
    #[allow(dead_code)]
    pub fn secret_vault_path(&self) -> Option<PathBuf> {
        self.secrets.vault_path().map(Path::to_path_buf)
    }

    /// 这次运行里是否记着这条连接手动输入过的数据库密码。
    pub fn remembers_password(&self, id: &str) -> bool {
        self.typed.lock().expect("typed secrets").contains_key(&password_account(id))
    }

    fn forget_typed(&self, id: &str) {
        let mut typed = self.typed.lock().expect("typed secrets");
        typed.remove(&password_account(id));
        typed.remove(&ssh_password_account(id));
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
        let epoch = if open {
            let mut epochs = self.operation_epochs.lock().expect("connection epochs");
            let epoch = self.operation_sequence.fetch_add(1, Ordering::SeqCst);
            epochs.insert(id.to_owned(), epoch);
            self.reconnects.lock().expect("reconnects").remove(id);
            Some(epoch)
        } else { None };
        self.exercise_versioned(id, input, open, database, epoch).await
    }

    async fn exercise_versioned(&self, id: &str, input: SecretInput, open: bool, database: Option<String>, epoch: Option<u64>) -> Result<OpenOutcome, String> {
        let store = self.store().await?;
        let mut config = store.get(id).await.map_err(db_err)?.ok_or_else(|| "找不到这条连接配置".to_owned())?;
        if let Some(database) = database {
            config.database = Some(database);
        }
        let saved_fingerprint = match &config.ssh {
            Some(ssh) => store.host_key(&ssh.host, ssh.port).await.map_err(db_err)?,
            None => None,
        };
        self.run_attempt(ProbeRequest {
            session_id: id.to_owned(),
            pending_key: Some(id.to_owned()),
            config,
            saved_fingerprint,
            input,
            open,
            epoch,
        })
        .await
    }

    /// 对候选草稿执行测试：不要求先保存，不写连接配置，也不更新最近使用时间。
    /// 编辑已有连接时测的是屏幕上的字段，而不是这个 id 在配置库里的旧值（R6/ADR-3）。
    pub async fn test_candidate(&self, draft: ConnectionDraft, input: SecretInput) -> Result<TestReport, String> {
        let store = self.store().await?;
        let saved_id = draft.id.clone().filter(|id| !id.trim().is_empty());
        // 未保存的草稿也需要一个探测用的会话键，但它不会进配置库，也不会进会话表。
        let scan_id = saved_id.clone().unwrap_or_else(|| format!("draft-{}", uuid::Uuid::new_v4()));
        let mut config = crate::connections::model::normalize(&draft, scan_id.clone())
            .map_err(|errors| ServiceError::Fields(errors).message())?;
        // 复用已保存机密的条件与保存路径一致：只有这个草稿仍要保存密码时才沿用旧机密。
        if let Some(saved_id) = &saved_id {
            if let Some(stored) = store.get(saved_id).await.map_err(db_err)? {
                let keep = draft.save_password;
                config.password_saved = keep && stored.password_saved;
                config.ssh_password_saved = keep
                    && stored.ssh_password_saved
                    && config.ssh.as_ref().is_some_and(|ssh| ssh.auth == SshAuthKind::Password);
            }
        }
        let saved_fingerprint = match &config.ssh {
            Some(ssh) => store.host_key(&ssh.host, ssh.port).await.map_err(db_err)?,
            None => None,
        };
        // open=false：不建会话、不改会话表，所以不会写最近使用时间。
        let outcome = self
            .run_attempt(ProbeRequest {
                session_id: scan_id,
                pending_key: saved_id,
                config,
                saved_fingerprint,
                input,
                open: false,
                epoch: None,
            })
            .await?;
        Ok(outcome.report)
    }

    /// 最近成功打开的连接 id，最新在前。从未打开过的连接不出现，已删除连接的用量按当前列表过滤掉。
    pub async fn recent_ids(&self, limit: usize) -> Result<Vec<String>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let store = self.store().await?;
        let existing = store.list().await.map_err(db_err)?;
        let mut ids = Vec::new();
        for (id, _) in store.last_used().await.map_err(db_err)? {
            if existing.iter().any(|row| row.id == id) {
                ids.push(id);
            }
            if ids.len() >= limit {
                break;
            }
        }
        Ok(ids)
    }

    /// 走一遍分阶段探测。会话键、待确认主机密钥的归属、候选配置与本次机密都在请求里；
    /// `open=false`（测试草稿）从不改会话表，也不写最近使用时间。
    async fn run_attempt(&self, request: ProbeRequest) -> Result<OpenOutcome, String> {
        let ProbeRequest {
            session_id,
            pending_key,
            config,
            saved_fingerprint,
            input,
            open,
            epoch,
        } = request;
        let session_id = session_id.as_str();
        let password = self.resolve_secret(&password_account(&config.id), input.password.as_ref(), config.password_saved)?;
        let ssh_password = self.resolve_secret(
            &ssh_password_account(&config.id),
            input.ssh_password.as_ref(),
            config.ssh_password_saved,
        )?;
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
            opened: None,
            schema_note: None,
            foreign_keys: None,
        };
        let report = stages::execute_stages(&stages, &mut attempt).await;
        if report.status == TestStatus::Wait {
            if let (Some(key), Some(ssh), Some(fingerprint), Some(algorithm)) =
                (pending_key, &config.ssh, &report.fingerprint, &report.algorithm)
            {
                self.pending.lock().expect("pending").insert(
                    key,
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
        let mut opened = false;
        if connected {
            // 提交会话与断开操作使用同一把短锁；迟到握手不能恢复已断开/被更新的会话。
            {
                let epochs = self.operation_epochs.lock().expect("connection epochs");
                if epochs.get(session_id).copied() != epoch { return Err("连接请求已过期，没有替换当前会话。".to_owned()); }
            }
            // 旧协调器上可能还有正在执行的业务批次：等它释放租约后再替换会话，
            // 避免把执行中的查询与它的事务留在被替换的会话上（ADR-1）。等待期间旧条目仍在表里，取消通道可用。
            let previous_lease = self.previous_lease(session_id).await;
            let epochs = self.operation_epochs.lock().expect("connection epochs");
            if epochs.get(session_id).copied() != epoch { return Err("连接请求已过期，没有替换当前会话。".to_owned()); }
            let health = Arc::new(SessionHealth::default());
            // 探针驱动的测试连接没有真实驱动句柄，也没有取消通道。
            let server_version = match &attempt.remote {
                Remote::Done { version } => Some(version.clone()),
                _ => attempt.opened.as_ref().map(|opened| opened.version.clone()),
            }
            .filter(|version| !version.trim().is_empty());
            let (session, cancel): (Arc<dyn LiveSession>, CancelTransport) = match attempt.opened.take() {
                Some(opened) => (opened.session, CancelTransport::new(opened.canceller, Arc::clone(&health))),
                None => (Arc::new(ScriptedSession), CancelTransport::default()),
            };
            // 重连或手动重开沿用同一个协调器：新会话从无活动事务开始，旧事务的未知记录保留（R10）。
            let transaction = self.take_transaction(session_id, adapter(config.engine).transaction());
            self.sessions.lock().expect("sessions").insert(
                session_id.to_owned(),
                LiveEntry {
                    session,
                    shutdown: attempt.shutdown.take(),
                    foreign_keys,
                    cancel,
                    health,
                    server_version,
                    transaction,
                },
            );
            // 新旧两个协调器是同一个对象时，这里释放的只是旧批次的租约本身；新会话随后才能开始执行。
            drop(previous_lease);
            // 新会话（手动打开或自动重连成功）都视为健康，旧的自动重连计划作废。
            self.reconnects.lock().expect("reconnects").remove(session_id);
            // 认证通过的手输机密记在内存里，供切换数据库和自动重连复用。
            let mut typed = self.typed.lock().expect("typed secrets");
            if let Some(secret) = input.password.as_ref().filter(|item| !item.is_empty()) {
                typed.insert(password_account(&config.id), secret.clone());
            }
            if let Some(secret) = input.ssh_password.as_ref().filter(|item| !item.is_empty()) {
                typed.insert(ssh_password_account(&config.id), secret.clone());
            }
            drop(typed);
            opened = true;
        }
        if opened {
            let saved_in_keychain = input.save_password
                && input
                    .password
                    .as_ref()
                    .filter(|item| !item.is_empty())
                    .is_some_and(|secret| match self.secrets.put(&password_account(&config.id), secret.expose()) {
                        Ok(()) => true,
                        Err(err) => {
                            self.log(format!("密码没有保存：{}", err.guidance()));
                            false
                        }
                    });
            // 只有真的打开了会话才记最近使用时间：保存连接、测试草稿都不写这一行。
            match self.store().await {
                Ok(store) => {
                    if saved_in_keychain {
                        if let Ok(Some(mut persisted)) = store.get(session_id).await {
                            persisted.password_saved = true;
                            if let Err(err) = store.upsert(&persisted).await {
                                self.log(format!("密码保存标记没有写入：{err}"));
                            }
                        }
                    }
                    if let Err(err) = store.touch_last_used(session_id, now_millis()).await {
                        self.log(format!("最近使用时间没有写入：{err}"));
                    }
                }
                Err(err) => self.log(err),
            }
        }
        Ok(OpenOutcome {
            report,
            connected,
            foreign_keys,
        })
    }

    async fn store(&self) -> Result<Store, String> {
        let path = self.path.clone();
        let secrets = Arc::clone(&self.secrets);
        self.cell
            .get_or_try_init(|| async move {
                let store = store::Store::open(path).await?;
                // 保险库首次加载时要迁移的旧钥匙串条目：只有标记过已保存密码的账户才可能有。
                // 读不出列表时不登记，保险库退回按需迁移。
                if let Ok(rows) = store.list().await {
                    secrets.register_legacy(legacy_secret_accounts(&rows));
                }
                Ok::<_, sqlx::Error>(store)
            })
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
    pub(crate) fn busy_scope(&self, id: &str) -> Option<SessionActivity> {
        let health = self.sessions.lock().expect("sessions").get(id).map(|session| Arc::clone(&session.health))?;
        health.note_busy();
        Some(SessionActivity(health))
    }

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

    /// 探活确认断线：旧事务的最终结果无法再从数据库确认，登记为未知（R10）。
    fn note_transaction_lost(&self, id: &str) {
        if let Some(entry) = self.sessions.lock().expect("sessions").get(id) {
            entry.transaction.reconnect(UnknownReason::ConnectionLost);
        }
    }

    /// 打开会话时读到的服务器版本说明（如 `8.0.36`）；会话未打开或没有读到时为 None。
    pub fn server_version(&self, id: &str) -> Option<String> {
        self.sessions.lock().expect("sessions").get(id).and_then(|session| session.server_version.clone())
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
        Some(clone_catalog_handle(session))
    }

    /// 查询批次使用的句柄：需要独占执行连接的引擎（MySQL 执行池、Redis 独占命令连接）由适配器另给。
    /// 关系型连接同时返回串行执行租约：同一连接的业务执行不会并发驱动同一条事务连接。
    pub async fn query_session(&self, id: &str) -> Result<Option<QuerySession>, String> {
        let (session, foreign_keys, transaction) = {
            let sessions = self.sessions.lock().expect("sessions");
            let Some(entry) = sessions.get(id).filter(|entry| !entry.health.is_lost()) else {
                return Ok(None);
            };
            entry.health.note_activity();
            (Arc::clone(&entry.session), entry.foreign_keys, Arc::clone(&entry.transaction))
        };
        let lease = if transaction.is_transactional() {
            Some(transaction.lease().await)
        } else {
            // Redis/MongoDB 不在用户事务范围内（R12）：保持原有的并发语义。
            None
        };
        let handle = session.query(foreign_keys).await?;
        Ok(Some(QuerySession { session: handle, lease }))
    }

    /// 同连接共享的事务快照；连接已断开但上一事务结果未知时仍返回旧记录（R10）。
    pub fn transaction_snapshot(&self, id: &str) -> Option<TransactionSnapshot> {
        if let Some(entry) = self.sessions.lock().expect("sessions").get(id) {
            return entry.transaction.snapshot();
        }
        self.retired
            .lock()
            .expect("retired transactions")
            .get(id)
            .and_then(|coordinator| coordinator.snapshot())
    }

    /// 连接级事务协调器：控制命令、探针登记与快照都从这里进入。
    pub fn transaction_coordinator(&self, id: &str) -> Option<Arc<TransactionCoordinator>> {
        if let Some(entry) = self.sessions.lock().expect("sessions").get(id) {
            return Some(Arc::clone(&entry.transaction));
        }
        self.retired.lock().expect("retired transactions").get(id).cloned()
    }

    /// 取回这条连接已有的事务协调器；没有时新建。新会话一律从无活动事务开始。
    fn take_transaction(&self, id: &str, dialect: Option<&'static dyn TransactionDialect>) -> Arc<TransactionCoordinator> {
        let previous = self.sessions.lock().expect("sessions").remove(id).map(|entry| entry.transaction);
        let retired = self.retired.lock().expect("retired transactions").remove(id);
        let coordinator = match previous.or(retired) {
            Some(coordinator) if same_dialect(coordinator.dialect(), dialect) => coordinator,
            // 引擎变了：旧方言不能再驱动新会话，但旧事务的未知记录必须保留（R10）；
            // 它作为只读历史随新协调器展示，不参与新会话的代次与控制能力。
            Some(previous) => {
                let coordinator = Arc::new(TransactionCoordinator::new(id.to_owned(), dialect));
                if let Some(record) = previous.snapshot().and_then(|snapshot| snapshot.unresolved) {
                    coordinator.inherit_unknown(record);
                }
                coordinator
            }
            None => Arc::new(TransactionCoordinator::new(id.to_owned(), dialect)),
        };
        coordinator.reconnect(UnknownReason::ConnectionLost);
        coordinator
    }

    /// 替换会话前先等旧协调器上的业务执行结束：重开不能把正在执行的查询和它的事务租约
    /// 留在被替换的会话上（ADR-1）。旧会话条目此时仍然在表里，取消通道可用。
    async fn previous_lease(&self, id: &str) -> Option<ExecutionLease> {
        let coordinator = self.sessions.lock().expect("sessions").get(id).map(|entry| Arc::clone(&entry.transaction))?;
        if coordinator.is_transactional() {
            Some(coordinator.lease().await)
        } else {
            None
        }
    }

    /// Redis 会话句柄：键浏览、值读取与命令行共用的连接（多路复用，调用方克隆后并发使用）。
    /// 返回共享的健康句柄供执行路径报告断线（R4 探活消费）。
    pub fn redis_session(
        &self,
        id: &str,
    ) -> Option<(std::sync::Arc<tokio::sync::Mutex<crate::adapters::redis::conn::RedisConn>>, std::sync::Arc<SessionHealth>)> {
        let sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get(id)?;
        if session.health.is_lost() {
            return None;
        }
        session.health.note_activity();
        let browser = session.session.as_redis()?.browser();
        Some((browser, Arc::clone(&session.health)))
    }

    /// Redis 会话的完整句柄：主连接、拓扑（单机/集群节点表）与按节点建连、Pub/Sub 专用连接的入口。
    pub fn redis_handle(&self, id: &str) -> Option<(crate::adapters::redis::conn::RedisHandle, std::sync::Arc<SessionHealth>)> {
        let sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get(id)?;
        if session.health.is_lost() {
            return None;
        }
        session.health.note_activity();
        let handle = session.session.as_redis()?.handle();
        Some((handle, Arc::clone(&session.health)))
    }

    /// MongoDB 会话句柄：库/集合浏览与文档 CRUD 共用这一个客户端（驱动内部连接池）。
    /// 返回共享的健康句柄供执行路径报告断线（R4 探活消费）。
    pub fn mongo_session(&self, id: &str) -> Option<(mongodb::Client, std::sync::Arc<SessionHealth>)> {
        let sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get(id)?;
        if session.health.is_lost() {
            return None;
        }
        session.health.note_activity();
        let client = session.session.as_mongo()?.client();
        Some((client, Arc::clone(&session.health)))
    }

    /// 前端 1s 轮询的落点：返回全部会话状态，并按需探活、驱动有限次退避重连（R4）。
    pub async fn session_statuses(self: &Arc<Self>) -> Vec<SessionStatus> {
        let targets: Vec<SessionHealthProbe> = {
            let epochs = self.operation_epochs.lock().expect("connection epochs");
            let sessions = self.sessions.lock().expect("sessions");
            sessions.iter().map(|(id, session)| SessionHealthProbe {
                id: id.clone(), local_file: session.session.local_file(),
                health: Arc::clone(&session.health), epoch: epochs.get(id).copied(),
            }).collect()
        };
        stream::iter(targets).map(|target| async move {
            if !target.health.is_lost() && target.health.needs_probe(target.local_file) { self.probe_session(&target).await; }
            let condition = self.session_condition(&target.id)?;
            if matches!(condition, SessionCondition::Lost { .. }) { self.run_reconnect_if_due(&target.id).await; }
            match self.session_condition(&target.id)? {
                SessionCondition::Connected => Some(SessionStatus { id: target.id, status: "connected", reason: None }),
                SessionCondition::Lost { reason } => Some(SessionStatus {
                    status: if self.reconnect_pending(&target.id) { "reconnecting" } else { "lost" }, id: target.id, reason,
                }),
            }
        }).buffer_unordered(4).filter_map(|status| async { status }).collect().await
    }

    /// 探活一个会话：失败标记 lost 并安排重连；超时无法判定，不动状态下一轮再试。
    async fn probe_session(&self, target: &SessionHealthProbe) {
        if target.health.busy() > 0 || !target.health.begin_probe() {
            return;
        }
        let _probe = ProbeGuard(Arc::clone(&target.health));
        let session = {
            let epochs = self.operation_epochs.lock().expect("connection epochs");
            if epochs.get(&target.id).copied() != target.epoch { return; }
            let sessions = self.sessions.lock().expect("sessions");
            match sessions.get(&target.id) {
                Some(entry) if !entry.health.is_lost() => Some((Arc::clone(&entry.session), entry.foreign_keys)),
                _ => None,
            }
        };
        let Some((session, foreign_keys)) = session else { return };
        let outcome = match self.probes.health.as_deref() {
            Some(probe) => tokio::time::timeout(PROBE_TIMEOUT, probe.check(&session.catalog(foreign_keys))).await,
            None => tokio::time::timeout(PROBE_TIMEOUT, session.ping()).await,
        };
        let epochs = self.operation_epochs.lock().expect("connection epochs");
        if epochs.get(&target.id).copied() != target.epoch { return; }
        let current = self.sessions.lock().expect("sessions").get(&target.id).is_some_and(|session| Arc::ptr_eq(&session.health, &target.health));
        if !current || target.health.busy() > 0 { return; }
        match outcome {
            Ok(Ok(())) => {
                target.health.clear_suspect();
                target.health.note_activity();
            }
            Ok(Err(reason)) => {
                let text = format!("探活失败：{reason}");
                target.health.mark_lost(text);
                self.note_transaction_lost(&target.id);
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
        let Some(epoch) = self.operation_epochs.lock().expect("connection epochs").get(id).copied() else { return };
        let config = match self.store().await {
            Ok(store) => store.get(id).await.ok().flatten(),
            Err(_) => None,
        };
        let Some(config) = config else {
            self.finish_reconnect(
                id, epoch,
                DecisionWithMessage {
                    decision: RetryDecision::Stop {
                        reason: "连接配置已删除或读不出来，不再自动重连。".to_owned(),
                    },
                    message: String::new(),
                },
            );
            return;
        };
        // 文件型与可无认证直连的引擎（本地开发常见）：没有保存密码不阻止自动重连；其余远程引擎仍要求已保存密码。
        let passwordless_ok = !adapter(config.engine).meta().reconnect_needs_saved_password();
        if !passwordless_ok && !config.password_saved {
            self.finish_reconnect(
                id, epoch,
                DecisionWithMessage {
                    decision: RetryDecision::Stop {
                        reason: "凭据库里没有保存的密码，已停在断开状态，请手动重连。".to_owned(),
                    },
                    message: String::new(),
                },
            );
            return;
        }
        let outcome = match self.exercise_versioned(id, SecretInput::default(), true, None, Some(epoch)).await {
            Ok(outcome) => outcome,
            Err(reason) => {
                self.finish_reconnect(
                    id, epoch,
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
        self.finish_reconnect(id, epoch, DecisionWithMessage { decision, message });
    }

    fn finish_reconnect(&self, id: &str, epoch: u64, decision: DecisionWithMessage) {
        let epochs = self.operation_epochs.lock().expect("connection epochs");
        if epochs.get(id).copied() != Some(epoch) { return; }
        let DecisionWithMessage { decision, message } = decision;
        let mut plans = self.reconnects.lock().expect("reconnects");
        let Some(plan) = plans.get_mut(id) else { return };
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
        let mut draft = ConnectionDraft::blank(crate::connections::model::Engine::Sqlite);
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

pub(crate) struct SessionActivity(Arc<SessionHealth>);
impl Drop for SessionActivity {
    fn drop(&mut self) { self.0.note_idle(); }
}

struct ProbeGuard(Arc<SessionHealth>);
impl Drop for ProbeGuard {
    fn drop(&mut self) { self.0.end_probe(); }
}

struct SessionHealthProbe {
    id: String,
    epoch: Option<u64>,
    local_file: bool,
    health: Arc<SessionHealth>,
}

fn clone_catalog_handle(entry: &LiveEntry) -> CatalogSession {
    entry.session.catalog(entry.foreign_keys)
}

/// 两个方言是不是同一份静态实现（连接引擎没变）。
fn same_dialect(a: Option<&'static dyn TransactionDialect>, b: Option<&'static dyn TransactionDialect>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => std::ptr::eq(a, b),
        (None, None) => true,
        _ => false,
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
    // 原生启动验证使用独立资料目录；正式发布构建始终使用系统应用数据目录。
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("RILLSQL_DEBUG_DATA_DIR").map(PathBuf::from).filter(|path| path.is_absolute()) {
        return path.join("connections.sqlite");
    }
    directories::ProjectDirs::from("app", "RillSQL", "RillSQL")
        .map(|dirs| dirs.data_dir().join("connections.sqlite"))
        .unwrap_or_else(|| PathBuf::from("connections.sqlite"))
}

/// 按 password_saved / ssh_password_saved 枚举可能存有旧版钥匙串条目的凭据账户。
fn legacy_secret_accounts(rows: &[SavedConnection]) -> Vec<String> {
    let mut accounts = Vec::new();
    for row in rows {
        if row.password_saved {
            accounts.push(password_account(&row.id));
        }
        if row.ssh_password_saved {
            accounts.push(ssh_password_account(&row.id));
        }
    }
    accounts
}

fn push_note(notes: &mut Vec<String>, text: &str) {
    if !notes.iter().any(|item| item == text) {
        notes.push(text.to_owned());
    }
}

fn db_err(err: sqlx::Error) -> String {
    format!("连接配置读写失败：{err}")
}

/// 最近使用时间的唯一取值方式：UTC 毫秒。只用于排序，不呈现成本地时间字符串。
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn decision_error(decision: HostKeyDecision) -> StageError {
    match decision {
        HostKeyDecision::Unconfirmed { algorithm, fingerprint } => StageError::WaitHostKey { algorithm, fingerprint },
        HostKeyDecision::Changed { presented, saved } => stages::fail_code(
            crate::connections::error_codes::CONN_HOST_KEY_CHANGED,
            format!("服务器出示的密钥 {presented} 与已保存的 {saved} 不同"),
            "已阻止连接，没有发送任何凭据。如果确认服务器更换了密钥，请先通过可信渠道核实新指纹，然后忘记已保存的密钥再重新测试。",
        ),
        HostKeyDecision::Trusted => stages::fail_code(crate::connections::error_codes::CONN_HOST_KEY_INCONSISTENT, "SSH 主机密钥状态不一致", "没有继续认证。"),
    }
}

/// 一次探测请求。`session_id` 是会话表的键；`pending_key` 只在这份配置确实对应一条已保存
/// 连接时给出，用来接住“等确认 SSH 主机密钥”的结论。
struct ProbeRequest {
    session_id: String,
    pending_key: Option<String>,
    config: SavedConnection,
    saved_fingerprint: Option<String>,
    input: SecretInput,
    open: bool,
    epoch: Option<u64>,
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
    /// 适配器打开的会话与取消通道（探针驱动的测试连接没有）。
    opened: Option<Opened>,
    schema_note: Option<String>,
    foreign_keys: Option<bool>,
}

impl Attempt {
    fn route(&self) -> Result<Route, StageError> {
        let forwarder = if self.config.ssh.is_some() { Some(self.forwarder_port.unwrap_or(0)) } else { None };
        route::resolve_route(&self.config, forwarder).map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_ROUTE_RESOLVE_FAILED, err, "没有改用转发器地址做证书校验。"))
    }

    async fn begin_ssh(&mut self) -> Result<(), StageError> {
        let ssh = self.config.ssh.clone().ok_or_else(|| stages::fail_code(crate::connections::error_codes::CONN_SSH_NOT_CONFIGURED, "没有 SSH 配置", "未执行"))?;
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
                let ssh = self.config.ssh.clone().ok_or_else(|| stages::fail_code(crate::connections::error_codes::CONN_SSH_NOT_CONFIGURED, "没有 SSH 配置", "未执行"))?;
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
            let ssh = self.config.ssh.clone().ok_or_else(|| stages::fail_code(crate::connections::error_codes::CONN_SSH_NOT_CONFIGURED, "没有 SSH 配置", "未执行"))?;
            let tls_name = self.config.tls.as_ref().map(|tls| tls.server_name.clone()).unwrap_or_default();
            let message = probe.open_tunnel(&host, port, &tls_name, &ssh.host).await?;
            self.forwarder_port = Some(1);
            return Ok(message);
        }
        let local = self.forwarder_port.ok_or_else(|| stages::fail_code(crate::connections::error_codes::CONN_TUNNEL_NOT_READY, "隧道没有建立", "尚未连接数据库。"))?;
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
        let adapter = adapter(self.config.engine);
        // 可无密码直连的引擎（本地开发常见）：空密码不是“凭据缺失”，照常建连。
        // 是否允许由适配器按用户名判断（如无用户名即无认证直连）；不允许时仍按凭据缺失报错。
        let empty_password;
        let password = match self.password.as_ref() {
            Some(secret) => secret,
            None if adapter.meta().password_optional(self.config.username.as_deref().unwrap_or("")) => {
                empty_password = Secret::new("");
                &empty_password
            }
            None => {
                // 密码在 TCP/TLS 之前就缺了，不能按失败阶段的先后把前面标成已完成。
                self.remote = Remote::Failed {
                    stage: StageKind::DatabaseAuthentication,
                    error: stages::fail_code(crate::connections::error_codes::CONN_PASSWORD_REQUIRED, "请填写数据库密码", "凭据库没有可用密码，本次也没有输入。没有把空密码写入配置。"),
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
                stages::fail_code(crate::connections::error_codes::CONN_TUNNEL_NOT_READY, "隧道没有就绪", "尚未连接数据库。"),
                tls_mode,
            );
            return;
        }
        let password = password.expose().to_owned();
        let opened = match adapter.connector() {
            Some(connector) => connector.open_remote(&self.config, &route, &password).await.map(|opened| {
                let version = opened.version.clone();
                self.opened = Some(opened);
                version
            }),
            None => Err(stages::fail_code(crate::connections::error_codes::CONN_NOT_STARTED, "这个引擎没有连接器", "未执行")),
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
            Remote::Fresh | Remote::Probed => Err(stages::fail_code(crate::connections::error_codes::CONN_NOT_STARTED, "连接没有开始", "未执行")),
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
                stages::fail_code(crate::connections::error_codes::CONN_PASSWORD_REQUIRED, "请填写数据库密码", "凭据库没有可用密码，本次也没有输入。没有把空密码写入配置。")
            })?;
            let route = self.route()?;
            return probe.authenticate(&self.config, &route, password.expose()).await;
        }
        self.ensure_remote().await;
        let note = adapter(self.config.engine).meta().auth_stage_note(&self.config);
        self.finish_stage(StageKind::DatabaseAuthentication, || note)
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
        let connector = adapter(self.config.engine)
            .connector()
            .ok_or_else(|| stages::fail_code(crate::connections::error_codes::CONN_NOT_STARTED, "这个引擎没有连接器", "未执行"))?;
        let opened = connector.open_file(Path::new(&path)).await?;
        self.schema_note = opened.schema_note.clone();
        self.foreign_keys = opened.foreign_keys;
        self.opened = Some(opened);
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
    let StageError::Fail { message, hint, .. } = error else {
        return StageKind::Network;
    };
    if hint.contains("网络连接失败") || hint.contains("隧道没有") || hint.contains("通道没有打通") {
        return StageKind::Network;
    }
    if hint.contains("用户名和密码") || hint.contains("数据库认证") || hint.contains("数据库名") {
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
            stages::fail_code(crate::connections::error_codes::CONN_AUTH_FAILED, "Access denied for user", "请检查数据库用户名和密码。"),
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
        opened: None,
        schema_note: None,
        foreign_keys: None,
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
                code: None,
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
            code: None,
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
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut transport = CancelTransport::new(Arc::new(crate::adapters::sqlite::connect::InterruptFlag(flag)), Arc::clone(&placeholder));
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
