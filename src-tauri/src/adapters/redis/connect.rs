//! Redis 的建连、会话、探活与连接错误分类。

use std::sync::Arc;
use std::time::Duration;

use redis::ConnectionAddr;

use super::cluster;
use super::conn::{ClusterFallback, NodeFactory, RedisConn, RedisHandle, Topology};
use crate::adapters::session::{CatalogSession, Connector, NoCancel, Opened, RedisAccess, Session};
use crate::connections::model::{SavedConnection, TlsMode};
use crate::connections::route::{self, Route};
use crate::connections::stages::{self, StageError};

pub(crate) struct RedisOpen {
    pub client: redis::Client,
    pub conn: redis::aio::MultiplexedConnection,
    pub version: String,
    /// `INFO cluster` 报告 cluster_enabled:1。
    pub cluster_enabled: bool,
}

/// Redis 连接：单条 MultiplexedConnection 会话（目录/键浏览/命令行共用）。
/// 密码与 ACL 用户名走 AUTH；库索引走 SELECT（都由 redis-rs 在建连时完成）。
/// TLS 三档：Off=明文 TCP；Require=rediss 但不校验证书；Verify=rediss 且按系统/CA 文件校验。任何一档失败都不降级重试。
pub(crate) async fn open_redis(config: &SavedConnection, route: &Route, password: &str) -> Result<RedisOpen, StageError> {
    let addr = match route.tls_mode {
        TlsMode::Off => ConnectionAddr::Tcp(route.transport_host.clone(), route.transport_port),
        TlsMode::Require => ConnectionAddr::TcpTls {
            host: route.transport_host.clone(),
            port: route.transport_port,
            insecure: true,
            tls_params: None,
        },
        TlsMode::Verify => ConnectionAddr::TcpTls {
            host: route.transport_host.clone(),
            port: route.transport_port,
            insecure: false,
            tls_params: None,
        },
    };
    let db = redis_db_index(config);
    let info = redis::ConnectionInfo {
        addr,
        redis: redis::RedisConnectionInfo {
            db,
            username: config.username.clone().filter(|user| !user.trim().is_empty()),
            password: if password.is_empty() { None } else { Some(password.to_owned()) },
            protocol: redis::ProtocolVersion::RESP2,
        },
    };
    let client = build_redis_client(info, config)
        .map_err(|err| redis_stage(route.tls_mode, &err))?;
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .map_err(|err| redis_stage(route.tls_mode, &err))?;
    // 建连后探活：PING 确认会话可用，INFO server 取版本横幅。
    let pong: String = redis::cmd("PING")
        .query_async(&mut conn)
        .await
        .map_err(|err| redis_stage(route.tls_mode, &err))?;
    if !pong.eq_ignore_ascii_case("pong") {
        return Err(stages::fail_code(crate::connections::error_codes::CONN_PROBE_FAILED, format!("PING 返回了 {pong}"), "探活没有通过，连接没有建立为可用会话。"));
    }
    let info_text: String = redis::cmd("INFO")
        .arg("server")
        .query_async(&mut conn)
        .await
        .map_err(|err| redis_stage(route.tls_mode, &err))?;
    // 自动识别集群：INFO cluster 的 cluster_enabled:1。读不到（被 ACL 禁用等）按单机处理。
    let cluster_text: String = redis::cmd("INFO").arg("cluster").query_async(&mut conn).await.unwrap_or_default();
    Ok(RedisOpen {
        client,
        conn,
        version: redis_server_version(&info_text),
        cluster_enabled: cluster_enabled(&cluster_text),
    })
}

/// `INFO cluster` 里 `cluster_enabled:1` 即为集群节点。
pub(crate) fn cluster_enabled(info: &str) -> bool {
    info.lines().any(|line| line.trim() == "cluster_enabled:1")
}

type ClusterOpen = (redis::cluster_async::ClusterConnection, redis::cluster::ClusterClient, Vec<cluster::ClusterNode>);

/// 建集群连接时单个节点的连接超时。种子节点已经连上，公告的节点连不上多半是地址不可达，不必久等。
const CLUSTER_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

/// 当前已建好的单节点连接与集群尝试的结果如何落地：
/// 不是集群 → 单机；经 SSH 隧道 → 不尝试，退回单节点；尝试成功 → 集群；尝试失败 → 退回单节点并记下原因。
/// 连接不会因为集群连接建不起来而整个打开失败。
pub(crate) enum ClusterSettle<T> {
    Single,
    Cluster(T),
    Fallback(ClusterFallback),
}

pub(crate) fn settle_cluster<T>(cluster_enabled: bool, tunneled: bool, attempt: Option<Result<T, StageError>>) -> ClusterSettle<T> {
    if !cluster_enabled {
        return ClusterSettle::Single;
    }
    if tunneled {
        return ClusterSettle::Fallback(ClusterFallback::Tunnel);
    }
    match attempt {
        Some(Ok(opened)) => ClusterSettle::Cluster(opened),
        Some(Err(StageError::Fail { message, .. })) => ClusterSettle::Fallback(ClusterFallback::Unreachable(message)),
        Some(Err(_)) => ClusterSettle::Fallback(ClusterFallback::Unreachable(String::new())),
        None => ClusterSettle::Fallback(ClusterFallback::Unreachable(String::new())),
    }
}

/// 测试用：这些连接 id 建集群连接时把节点公告的地址换成不可达的本机端口，模拟 NAT / 容器内地址。
#[cfg(test)]
pub(crate) static UNREACHABLE_ANNOUNCE: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn announced(config: &SavedConnection, node: &cluster::ClusterNode) -> (String, u16) {
    #[cfg(test)]
    if UNREACHABLE_ANNOUNCE.lock().map(|ids| ids.contains(&config.id)).unwrap_or(false) {
        return ("127.0.0.1".to_owned(), 1);
    }
    let _ = config;
    (node.host.clone(), node.port)
}

/// 集群会话：从种子节点读节点表，再以全部主节点为初始节点建立自动路由的集群连接。
/// 失败由调用方退回单节点会话（见 [`settle_cluster`]）。
async fn open_cluster(seed: &mut redis::aio::MultiplexedConnection, factory: &NodeFactory, route: &Route) -> Result<ClusterOpen, StageError> {
    let text: String = redis::cmd("CLUSTER").arg("NODES").query_async(seed).await.map_err(|err| redis_stage(route.tls_mode, &err))?;
    let nodes = cluster::parse_cluster_nodes(&text);
    let mut initial: Vec<redis::ConnectionInfo> = nodes
        .iter()
        .filter(|node| node.master)
        .map(|node| {
            let (host, port) = announced(&factory.config, node);
            factory.info(&host, port, 0)
        })
        .collect();
    if initial.is_empty() {
        initial.push(factory.info(&route.transport_host, route.transport_port, 0));
    }
    let mut builder = redis::cluster::ClusterClientBuilder::new(initial)
        .connection_timeout(CLUSTER_CONNECT_TIMEOUT)
        .response_timeout(Duration::from_secs(60));
    if let Some(certs) = tls_certificates(&factory.config).map_err(|err| redis_stage(route.tls_mode, &err))? {
        builder = builder.certs(certs);
    }
    let client = builder.build().map_err(|err| redis_stage(route.tls_mode, &err))?;
    let conn = client.get_async_connection().await.map_err(|err| redis_stage(route.tls_mode, &err))?;
    Ok((conn, client, nodes))
}

/// Verify 档的 CA / 客户端证书材料（集群客户端要单独传入）。
fn tls_certificates(config: &SavedConnection) -> Result<Option<redis::TlsCertificates>, redis::RedisError> {
    let tls = config.tls.as_ref();
    let ca_file = tls.and_then(|tls| tls.ca_file.clone());
    let cert = tls.and_then(|tls| tls.client_cert.clone());
    let key = tls.and_then(|tls| tls.client_key.clone());
    if ca_file.is_none() && cert.is_none() {
        return Ok(None);
    }
    let read = |path: &str, what: &'static str| {
        std::fs::read(path).map_err(|_| redis::RedisError::from((redis::ErrorKind::InvalidClientConfig, what, path.to_owned())))
    };
    let root_cert = match ca_file {
        Some(path) => Some(read(&path, "读不到 CA 证书")?),
        None => None,
    };
    let client_tls = match (cert, key) {
        (Some(cert), Some(key)) => Some(redis::ClientTlsConfig { client_cert: read(&cert, "读不到客户端证书")?, client_key: read(&key, "读不到客户端私钥")? }),
        _ => None,
    };
    Ok(Some(redis::TlsCertificates { client_tls, root_cert }))
}

/// Verify 档带 CA 文件或客户端证书时，把 PEM 材料装进 TLS 参数；其余档位直接按 ConnectionInfo 建客户端。
pub(crate) fn build_redis_client(info: redis::ConnectionInfo, config: &SavedConnection) -> Result<redis::Client, redis::RedisError> {
    let tls = config.tls.as_ref();
    let ca_file = tls.and_then(|tls| tls.ca_file.clone());
    let client_cert = tls.and_then(|tls| tls.client_cert.clone());
    let client_key = tls.and_then(|tls| tls.client_key.clone());
    if ca_file.is_none() && client_cert.is_none() {
        return redis::Client::open(info);
    }
    let root_cert = match ca_file {
        Some(path) => Some(std::fs::read(&path).map_err(|_| {
            redis::RedisError::from((redis::ErrorKind::InvalidClientConfig, "读不到 CA 证书", path.to_string()))
        })?),
        None => None,
    };
    let client_tls = match (client_cert, client_key) {
        (Some(cert), Some(key)) => {
            let cert = std::fs::read(&cert).map_err(|_| {
                redis::RedisError::from((redis::ErrorKind::InvalidClientConfig, "读不到客户端证书", cert.to_string()))
            })?;
            let key = std::fs::read(&key).map_err(|_| {
                redis::RedisError::from((redis::ErrorKind::InvalidClientConfig, "读不到客户端私钥", key.to_string()))
            })?;
            Some(redis::ClientTlsConfig { client_cert: cert, client_key: key })
        }
        (None, None) => None,
        _ => {
            return Err(redis::RedisError::from((
                redis::ErrorKind::InvalidClientConfig,
                "客户端证书和私钥需要同时填写",
                String::new(),
            )))
        }
    };
    // 只有 Verify 档会走到这里（Off/Require 不读证书材料）。
    if !matches!(info.addr, ConnectionAddr::TcpTls { insecure: false, .. }) {
        return Err(redis::RedisError::from((
            redis::ErrorKind::InvalidClientConfig,
            "TLS 材料只在“加密连接并核对证书”档生效",
        )));
    }
    redis::Client::build_with_tls(info, redis::TlsCertificates { client_tls, root_cert })
}

/// 库索引来自连接配置的 database 字段；非法值在保存阶段已被拦截，这里兜底为 0。
pub(crate) fn redis_db_index(config: &SavedConnection) -> i64 {
    config
        .database
        .as_deref()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(0)
}

fn redis_server_version(info: &str) -> String {
    for line in info.lines() {
        if let Some(value) = line.strip_prefix("redis_version:") {
            return format!("Redis {}", value.trim());
        }
    }
    "Redis 未知版本".to_owned()
}

/// Redis 连接错误分类：TCP / AUTH / TLS 分开报，不降级重试（与 SQL Server 同纪律）。
pub(crate) fn redis_stage(mode: TlsMode, err: &redis::RedisError) -> StageError {
    let text = crate::error_text::chain(err);
    let lowered = text.to_ascii_lowercase();
    if matches!(err.kind(), redis::ErrorKind::AuthenticationFailed)
        || lowered.contains("noauth")
        || lowered.contains("wrongpass")
        || lowered.contains("invalid password")
        || lowered.contains("authentication")
        || text.contains("密码")
    {
        return stages::fail_code(crate::connections::error_codes::CONN_REDIS_AUTH_FAILED, text, "请检查 Redis 密码或 ACL 用户名。");
    }
    if lowered.contains("certificate") || lowered.contains("tls") || lowered.contains("ssl") || text.contains("证书") {
        let _ = route::on_tls_failure(mode);
        return stages::fail_code(crate::connections::error_codes::CONN_TLS_TERMINATED, text, "连接已终止。没有降级为不加密或不验证后重试。");
    }
    if lowered.contains("refused") || lowered.contains("timed out") || lowered.contains("timeout") || lowered.contains("io") || lowered.contains("connection") {
        return stages::fail_code(crate::connections::error_codes::CONN_NETWORK_FAILED, text, "网络连接失败，没有改用加密连接重试。");
    }
    stages::fail_code(crate::connections::error_codes::CONN_TLS_TERMINATED, text, "连接已终止，没有自动改用另一种加密方式重试。")
}

/// Redis 连接器。命令没有服务端取消通道（探活/断线走健康句柄）。
pub(in crate::adapters) struct RedisConnector;

#[async_trait::async_trait]
impl Connector for RedisConnector {
    async fn open_remote(&self, config: &SavedConnection, route: &Route, password: &str) -> Result<Opened, StageError> {
        let mut opened = open_redis(config, route, password).await?;
        let factory = NodeFactory { config: config.clone(), password: password.to_owned(), tls_mode: route.tls_mode };
        let addr = format!("{}:{}", config.host.clone().unwrap_or_default(), config.port.unwrap_or(6379));
        let tunneled = config.ssh.is_some();
        let attempt = if opened.cluster_enabled && !tunneled { Some(open_cluster(&mut opened.conn, &factory, route).await) } else { None };
        let (conn, cluster_client, topology, version) = match settle_cluster(opened.cluster_enabled, tunneled, attempt) {
            ClusterSettle::Cluster((conn, client, nodes)) => {
                let version = format!("{} · Cluster", opened.version);
                (RedisConn::Cluster(conn), Some(client), Topology { cluster: true, nodes, addr, fallback: None }, version)
            }
            ClusterSettle::Single => (RedisConn::Single(opened.conn), None, Topology { cluster: false, nodes: Vec::new(), addr, fallback: None }, opened.version),
            ClusterSettle::Fallback(reason) => {
                // 退回原因由界面按语言显示（Topology.fallback），版本串保持与语言无关。
                let version = format!("{} · Cluster", opened.version);
                (RedisConn::Single(opened.conn), None, Topology { cluster: false, nodes: Vec::new(), addr, fallback: Some(reason) }, version)
            }
        };
        let handle = RedisHandle {
            conn: conn.clone(),
            topology: Arc::new(std::sync::RwLock::new(topology)),
            factory: Arc::new(factory),
            client: if cluster_client.is_some() { None } else { Some(opened.client.clone()) },
        };
        Ok(Opened {
            version,
            session: Arc::new(RedisSession { browser: Arc::new(tokio::sync::Mutex::new(conn)), client: opened.client, cluster: cluster_client, handle }),
            canceller: Arc::new(NoCancel),
            schema_note: None,
            foreign_keys: None,
        })
    }
}

/// browser 是键浏览、值读取共用的主连接（多路复用，克隆后可并发）；client/cluster 用来给查询批次另开独占连接。
struct RedisSession {
    browser: Arc<tokio::sync::Mutex<RedisConn>>,
    client: redis::Client,
    cluster: Option<redis::cluster::ClusterClient>,
    handle: RedisHandle,
}

#[async_trait::async_trait]
impl Session for RedisSession {
    fn catalog(&self, _foreign_keys: Option<bool>) -> CatalogSession {
        CatalogSession::Redis(Arc::clone(&self.browser))
    }

    /// SELECT 作用于整个网络连接。查询批次独占一条命令连接，避免和键浏览器互相切库。
    async fn query(&self, _foreign_keys: Option<bool>) -> Result<CatalogSession, String> {
        let conn = match &self.cluster {
            Some(cluster) => RedisConn::Cluster(cluster.get_async_connection().await.map_err(|err| err.to_string())?),
            None => {
                let config = redis::AsyncConnectionConfig::new().set_connection_timeout(Duration::from_secs(15));
                RedisConn::Single(self.client.get_multiplexed_async_connection_with_config(&config).await.map_err(|err| err.to_string())?)
            }
        };
        Ok(CatalogSession::Redis(Arc::new(tokio::sync::Mutex::new(conn))))
    }

    async fn ping(&self) -> Result<(), String> {
        let mut conn = self.browser.lock().await.clone();
        let pong: String = redis::cmd("PING").query_async(&mut conn).await.map_err(|err| crate::error_text::chain(&err))?;
        if pong.eq_ignore_ascii_case("pong") {
            Ok(())
        } else {
            Err(format!("PING 返回了 {pong}"))
        }
    }

    fn as_redis(&self) -> Option<&dyn RedisAccess> {
        Some(self)
    }
}

impl RedisAccess for RedisSession {
    fn browser(&self) -> Arc<tokio::sync::Mutex<RedisConn>> {
        Arc::clone(&self.browser)
    }

    fn handle(&self) -> RedisHandle {
        self.handle.clone()
    }
}
