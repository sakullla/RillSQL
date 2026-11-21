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
use crate::ddl::model::{Step, StepReceipt};

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
    /// Redis 连接（单机多路复用或集群自动路由）。查询批次另开独占连接。
    Redis(Arc<tokio::sync::Mutex<crate::adapters::redis::conn::RedisConn>>),
    /// MongoDB 客户端：库/集合目录与文档 CRUD 共用（驱动内部自带连接池）。
    Mongo(mongodb::Client),
    Scripted,
}

impl CatalogSession {
    /// 探针驱动的测试连接：没有可查询的数据库。
    pub fn is_scripted(&self) -> bool {
        matches!(self, Self::Scripted)
    }

    /// 执行一条只读语句，把每格转成文本。对象详情（列/索引/主键/注释）与补全外键读取共用，
    /// 各引擎的取值规则在各自适配器里。
    pub async fn text_rows(&self, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
        use crate::catalog::detail::failure;
        match self {
            Self::Scripted => Err(failure("这个连接没有连着可查询的数据库")),
            Self::Redis(_) => Err(failure("Redis 没有列或索引")),
            Self::Mongo(_) => Err(failure("MongoDB 没有 SQL 列或索引")),
            Self::Sqlite { pool, .. } => super::sqlite::catalog_read::text_rows(pool, sql).await,
            Self::Mysql(pool) => super::mysql::catalog_read::text_rows(pool, sql).await,
            Self::Postgres(client) => super::postgres::catalog_read::text_rows(client, sql).await,
            Self::Sqlserver(client) => super::sqlserver::catalog_read::text_rows(client, sql).await,
        }
    }

    /// 逐条执行结构变更，失败即停。由会话所属引擎的适配器执行（重建事务回滚、外键开关恢复各按方言）。
    pub async fn run_ddl_steps(&self, steps: &[Step], restore_foreign_keys: Option<bool>) -> Result<Vec<StepReceipt>, String> {
        match self {
            // 专项引擎没有结构编辑：误入时拒绝执行而不是生成无意义语句。
            Self::Redis(_) => Err("Redis 没有结构编辑。".to_owned()),
            Self::Mongo(_) => Err("MongoDB 没有结构编辑。".to_owned()),
            Self::Scripted => Err("这个连接没有连着可查询的数据库。".to_owned()),
            Self::Sqlite { pool, .. } => super::sqlite::ddl_read::run_steps(pool, steps, restore_foreign_keys).await,
            Self::Mysql(pool) => super::mysql::ddl_read::run_steps(pool, steps).await,
            Self::Postgres(client) => super::postgres::ddl_read::run_steps(client, steps).await,
            Self::Sqlserver(client) => super::sqlserver::ddl_read::run_steps(client, steps).await,
        }
    }
}

/// 一条已打开的会话。由适配器的 [`Connector`] 产出，连接服务按连接 id 持有 `Arc<dyn Session>`。
#[async_trait::async_trait]
pub trait Session: Send + Sync {
    /// 目录、对象树、补全、探活与其他元数据读取使用的句柄。`foreign_keys` 是连接阶段读到的外键开关
    /// （文件型引擎用）。它不加入用户事务：元数据读取不能误提交、回滚或阻塞业务会话（R2）。
    fn catalog(&self, foreign_keys: Option<bool>) -> CatalogSession;

    /// 关系型用户业务会话使用的句柄：SQL 查询、网格读取/写入与结构业务页面都从这里取得。
    /// 默认与目录句柄相同；需要业务/元数据分离的引擎返回自己的业务句柄（MySQL 执行池、
    /// PG/SQL Server 的业务连接、SQLite 的读写池），元数据连接只由 [`Session::catalog`] 返回。
    async fn query(&self, foreign_keys: Option<bool>) -> Result<CatalogSession, String> {
        Ok(self.catalog(foreign_keys))
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

/// Redis 会话的专属接口：键浏览主连接，以及拓扑、按节点建连与 Pub/Sub 专用连接的句柄。
pub trait RedisAccess: Send + Sync {
    fn browser(&self) -> Arc<tokio::sync::Mutex<crate::adapters::redis::conn::RedisConn>>;
    fn handle(&self) -> crate::adapters::redis::conn::RedisHandle;
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
