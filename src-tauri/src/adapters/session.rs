//! 会话抽象（ADR-0）：各适配器的 [`Connector`] 打开连接产出 [`Session`] 与 [`Canceller`]，
//! 主代码只持有 `Arc<dyn Session>` / [`CancelTransport`]，不再对具体会话变体分支。
//!
//! [`CatalogSession`] 是目录、网格、结构等读写路径使用的句柄。A2–A5 迁移各自领域前它仍按变体承载
//! 驱动句柄；构造它只发生在适配器里（`Session::catalog` / `Session::query`）。

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::connections::model::SavedConnection;
use crate::connections::route::Route;
use crate::connections::SessionHealth;
use crate::connections::stages::{self, StageError};

/// tiberius 的连接句柄。futures-io 与 tokio 之间经 compat 桥接（tiberius 只发 futures 生态的流）。
pub type TdsClient = tiberius::Client<tokio_util::compat::Compat<tokio::net::TcpStream>>;

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
    /// MongoDB 客户端：库/集合目录与文档 CRUD 共用（驱动内部自带连接池）。
    Mongo(mongodb::Client),
    Scripted,
}

/// 一条已打开的会话。由适配器的 [`Connector`] 产出，连接服务按连接 id 持有 `Arc<dyn Session>`。
#[async_trait::async_trait]
pub trait Session: Send + Sync {
    /// 目录、结构、表浏览与补全使用的句柄。`foreign_keys` 是连接阶段读到的外键开关（文件型引擎用）。
    fn catalog(&self, foreign_keys: Option<bool>) -> CatalogSession;

    /// 查询批次使用的句柄。默认与目录句柄相同；需要独占执行连接的引擎另给。
    async fn query(&self, foreign_keys: Option<bool>) -> Result<CatalogSession, String> {
        Ok(self.catalog(foreign_keys))
    }

    /// 已认证的共享连接池：独立执行连接受限（max_user_connections、代理并发、跳板机通道）时回退复用。
    fn shared_pool(&self) -> Option<mysql_async::Pool> {
        None
    }

    /// 本地文件会话：没有“断线”可言，免探活。
    fn local_file(&self) -> bool {
        false
    }

    /// 在已打开的会话上发一条最小语句确认连接还活着。
    async fn ping(&self) -> Result<(), String>;

    /// Redis 专属操作入口。
    fn as_redis(&self) -> Option<&dyn RedisAccess> {
        None
    }

    /// MongoDB 专属操作入口。
    fn as_mongo(&self) -> Option<&dyn MongoAccess> {
        None
    }
}

/// Redis 会话的专属接口：键浏览、值读取与命令行共用的多路复用连接。
pub trait RedisAccess: Send + Sync {
    fn browser(&self) -> Arc<tokio::sync::Mutex<redis::aio::MultiplexedConnection>>;
}

/// MongoDB 会话的专属接口：库/集合浏览与文档 CRUD 共用的客户端。
pub trait MongoAccess: Send + Sync {
    fn client(&self) -> mongodb::Client;
}

/// 探针驱动的测试连接没有真实驱动句柄：目录读取按“没有可查询的数据库”处理，探活恒成功。
pub struct ScriptedSession;

#[async_trait::async_trait]
impl Session for ScriptedSession {
    fn catalog(&self, _foreign_keys: Option<bool>) -> CatalogSession {
        CatalogSession::Scripted
    }

    async fn ping(&self) -> Result<(), String> {
        Ok(())
    }
}

/// 取消进行中的语句。查询本身仍走已打开的会话，取消不从执行连接上发。
#[async_trait::async_trait]
pub trait Canceller: Send + Sync {
    /// 取消是否要经 SSH 另开通道。
    fn via_ssh(&self) -> bool {
        false
    }

    /// 本地执行的中断标志（执行循环轮询它）。
    fn interrupt_flag(&self) -> Option<Arc<AtomicBool>> {
        None
    }

    /// 执行器拿到专用连接后登记服务端会话编号，作为取消目标。
    fn note_server_session(&self, _id: u64) {}

    /// 发出取消。返回 true 表示这次取消使用了新的传输通道。
    async fn cancel(&self) -> Result<bool, String>;
}

/// 没有服务端取消通道的会话（Redis 命令、MongoDB 文档操作）。
pub struct NoCancel;

pub(crate) const NO_CANCEL_CHANNEL: &str = "这条连接没有可用来取消的数据库通道。查询仍在执行。";

#[async_trait::async_trait]
impl Canceller for NoCancel {
    async fn cancel(&self) -> Result<bool, String> {
        Err(NO_CANCEL_CHANNEL.to_owned())
    }
}

/// 一条会话的取消通道与共享健康句柄。执行路径通过它报告活动与断线（R4 探活消费）。
#[derive(Clone)]
pub enum CancelTransport {
    Session {
        canceller: Arc<dyn Canceller>,
        health: Arc<SessionHealth>,
    },
    None,
}

impl Default for CancelTransport {
    /// 没有取消通道，也没有健康句柄（探针驱动的测试连接）。
    fn default() -> Self {
        Self::None
    }
}

impl CancelTransport {
    pub fn new(canceller: Arc<dyn Canceller>, health: Arc<SessionHealth>) -> Self {
        Self::Session { canceller, health }
    }

    pub fn via_ssh(&self) -> bool {
        match self {
            Self::Session { canceller, .. } => canceller.via_ssh(),
            Self::None => false,
        }
    }

    pub fn sqlite_flag(&self) -> Option<Arc<AtomicBool>> {
        match self {
            Self::Session { canceller, .. } => canceller.interrupt_flag(),
            Self::None => None,
        }
    }

    pub fn set_mysql_connection_id(&self, id: u64) {
        if let Self::Session { canceller, .. } = self {
            canceller.note_server_session(id);
        }
    }

    pub fn health(&self) -> Option<&Arc<SessionHealth>> {
        match self {
            Self::Session { health, .. } => Some(health),
            Self::None => None,
        }
    }

    /// 会话入表时把执行共享的健康句柄接到取消通道上，执行路径据此报告断线。
    pub fn attach_health(&mut self, health: Arc<SessionHealth>) {
        if let Self::Session { health: slot, .. } = self {
            *slot = health;
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

    /// 向数据库发出取消。返回 true 表示这次取消使用了新的传输通道。
    pub async fn deliver(&self) -> Result<bool, String> {
        match self {
            Self::Session { canceller, .. } => canceller.cancel().await,
            Self::None => Err(NO_CANCEL_CHANNEL.to_owned()),
        }
    }
}

/// 打开连接的结果：会话、取消通道与连接阶段读到的说明。
pub struct Opened {
    /// 服务器版本说明（ServerVersion 阶段原文）。
    pub version: String,
    pub session: Arc<dyn Session>,
    pub canceller: Arc<dyn Canceller>,
    /// 文件型引擎：读取 schema 阶段的说明。
    pub schema_note: Option<String>,
    /// 文件型引擎：外键约束开关。
    pub foreign_keys: Option<bool>,
}

/// 打开连接并产出 [`Session`]。远程引擎实现 `open_remote`，文件型引擎实现 `open_file`。
#[async_trait::async_trait]
pub trait Connector: Sync {
    /// 经路由（直连或 SSH 转发器）打开远程会话。
    async fn open_remote(&self, config: &SavedConnection, route: &Route, password: &str) -> Result<Opened, StageError>;

    /// 打开本地数据库文件。
    async fn open_file(&self, _path: &Path) -> Result<Opened, StageError> {
        Err(stages::fail_code(crate::connections::error_codes::CONN_NOT_STARTED, "这个引擎不打开本地文件", "未执行"))
    }
}
