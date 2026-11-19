//! Redis 连接形态：单机多路复用或集群自动路由（同一个 [`RedisConn`]），以及会话句柄 [`RedisHandle`]：
//! 拓扑（节点表）、按节点建连（逐主节点 SCAN / 诊断）与 Pub/Sub 专用连接。
//!
//! 多路复用连接可以克隆后并发使用：键扫描、值读取、控制台各拿一份克隆，不再串行共用一把锁。

use std::sync::Arc;
use std::time::Duration;

use redis::aio::{ConnectionLike, MultiplexedConnection};
use redis::cluster_async::ClusterConnection;
use redis::{Cmd, ConnectionAddr, Pipeline, RedisFuture, Value};
use serde::Serialize;

use super::cluster::{self, ClusterNode};
use crate::connections::model::{SavedConnection, TlsMode};

/// 单机或集群连接。实现 `ConnectionLike`，命令写法与单机一致；集群按键自动路由。
#[derive(Clone)]
pub enum RedisConn {
    Single(MultiplexedConnection),
    Cluster(ClusterConnection),
}

impl RedisConn {
    pub fn is_cluster(&self) -> bool {
        matches!(self, Self::Cluster(_))
    }
}

impl ConnectionLike for RedisConn {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        match self {
            Self::Single(conn) => conn.req_packed_command(cmd),
            Self::Cluster(conn) => conn.req_packed_command(cmd),
        }
    }

    fn req_packed_commands<'a>(&'a mut self, cmd: &'a Pipeline, offset: usize, count: usize) -> RedisFuture<'a, Vec<Value>> {
        match self {
            Self::Single(conn) => conn.req_packed_commands(cmd, offset, count),
            Self::Cluster(conn) => conn.req_packed_commands(cmd, offset, count),
        }
    }

    fn get_db(&self) -> i64 {
        match self {
            Self::Single(conn) => conn.get_db(),
            Self::Cluster(conn) => conn.get_db(),
        }
    }
}

/// 拓扑：单机，或集群与节点表。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Topology {
    pub cluster: bool,
    pub nodes: Vec<ClusterNode>,
    /// 单机时的服务地址（展示用）。
    pub addr: String,
}

impl Topology {
    pub fn masters(&self) -> Vec<&ClusterNode> {
        self.nodes.iter().filter(|node| node.master).collect()
    }
}

/// 按地址建连所需的材料（认证、TLS 档位与证书）。只在会话内存里，不落盘。
#[derive(Clone)]
pub(crate) struct NodeFactory {
    pub config: SavedConnection,
    pub password: String,
    pub tls_mode: TlsMode,
}

impl NodeFactory {
    pub fn info(&self, host: &str, port: u16, db: i64) -> redis::ConnectionInfo {
        let addr = match self.tls_mode {
            TlsMode::Off => ConnectionAddr::Tcp(host.to_owned(), port),
            TlsMode::Require => ConnectionAddr::TcpTls { host: host.to_owned(), port, insecure: true, tls_params: None },
            TlsMode::Verify => ConnectionAddr::TcpTls { host: host.to_owned(), port, insecure: false, tls_params: None },
        };
        redis::ConnectionInfo {
            addr,
            redis: redis::RedisConnectionInfo {
                db,
                username: self.config.username.clone().filter(|user| !user.trim().is_empty()),
                password: if self.password.is_empty() { None } else { Some(self.password.clone()) },
                protocol: redis::ProtocolVersion::RESP2,
            },
        }
    }

    pub fn client(&self, host: &str, port: u16, db: i64) -> redis::RedisResult<redis::Client> {
        super::connect::build_redis_client(self.info(host, port, db), &self.config)
    }

    pub async fn connect(&self, host: &str, port: u16) -> redis::RedisResult<MultiplexedConnection> {
        let config = redis::AsyncConnectionConfig::new().set_connection_timeout(Duration::from_secs(15));
        self.client(host, port, 0)?.get_multiplexed_async_connection_with_config(&config).await
    }
}

/// 一个扫描/诊断目标：节点地址与它的连接。
#[derive(Clone)]
pub struct NodeTarget {
    pub addr: String,
    pub conn: RedisConn,
}

/// 会话句柄：克隆即得，供应用层在后台任务里并发使用。
#[derive(Clone)]
pub struct RedisHandle {
    pub(crate) conn: RedisConn,
    pub(crate) topology: Arc<std::sync::RwLock<Topology>>,
    pub(crate) factory: Arc<NodeFactory>,
    /// 单机：建会话用的客户端（Pub/Sub 专用连接从它另开）。
    pub(crate) client: Option<redis::Client>,
}

impl RedisHandle {
    /// 主连接的一份克隆（单机多路复用 / 集群自动路由）。
    pub fn conn(&self) -> RedisConn {
        self.conn.clone()
    }

    pub fn topology(&self) -> Topology {
        self.topology.read().map(|topology| topology.clone()).unwrap_or_default()
    }

    pub fn is_cluster(&self) -> bool {
        self.conn.is_cluster()
    }

    /// 重读集群节点表（单机原样返回）。
    pub async fn refresh_topology(&self) -> redis::RedisResult<Topology> {
        if !self.is_cluster() {
            return Ok(self.topology());
        }
        let mut conn = self.conn();
        let text: String = redis::cmd("CLUSTER").arg("NODES").query_async(&mut conn).await?;
        let nodes = cluster::parse_cluster_nodes(&text);
        if let Ok(mut topology) = self.topology.write() {
            topology.nodes = nodes;
        }
        Ok(self.topology())
    }

    /// 逐节点操作的目标：单机为主连接本身；集群为每个主节点各开一条直连（SCAN / DBSIZE / 诊断都按节点发）。
    pub async fn node_targets(&self) -> redis::RedisResult<Vec<NodeTarget>> {
        if !self.is_cluster() {
            return Ok(vec![NodeTarget { addr: self.topology().addr, conn: self.conn() }]);
        }
        let topology = self.refresh_topology().await?;
        let mut targets = Vec::new();
        for node in topology.masters() {
            if node.status() == "fail" {
                continue;
            }
            let conn = self.factory.connect(&node.host, node.port).await?;
            targets.push(NodeTarget { addr: node.addr(), conn: RedisConn::Single(conn) });
        }
        Ok(targets)
    }

    /// 只连某一个节点（诊断页按节点查看）。空地址或单机时用主连接。
    pub async fn node_target(&self, addr: &str) -> redis::RedisResult<NodeTarget> {
        if !self.is_cluster() || addr.trim().is_empty() {
            return Ok(NodeTarget { addr: self.topology().addr, conn: self.conn() });
        }
        let (host, port) = addr
            .rsplit_once(':')
            .and_then(|(host, port)| port.parse::<u16>().ok().map(|port| (host.to_owned(), port)))
            .ok_or_else(|| redis::RedisError::from((redis::ErrorKind::InvalidClientConfig, "节点地址无效", addr.to_owned())))?;
        let conn = self.factory.connect(&host, port).await?;
        Ok(NodeTarget { addr: addr.to_owned(), conn: RedisConn::Single(conn) })
    }

    /// Pub/Sub 专用连接：订阅态的连接不能再发普通命令，所以每次另开。
    pub async fn pubsub(&self) -> redis::RedisResult<redis::aio::PubSub> {
        if let Some(client) = &self.client {
            return client.get_async_pubsub().await;
        }
        let topology = self.topology();
        let node = topology
            .masters()
            .into_iter()
            .next()
            .cloned()
            .ok_or_else(|| redis::RedisError::from((redis::ErrorKind::ClientError, "集群没有可用的主节点")))?;
        self.factory.client(&node.host, node.port, 0)?.get_async_pubsub().await
    }
}
