//! Redis 的建连、会话、探活与连接错误分类。

use std::sync::Arc;
use std::time::Duration;

use redis::ConnectionAddr;

use crate::adapters::session::{CatalogSession, Connector, NoCancel, Opened, RedisAccess, Session};
use crate::connections::model::{SavedConnection, TlsMode};
use crate::connections::route::{self, Route};
use crate::connections::stages::{self, StageError};

pub(crate) struct RedisOpen {
    pub client: redis::Client,
    pub conn: redis::aio::MultiplexedConnection,
    pub version: String,
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
    Ok(RedisOpen {
        client,
        conn,
        version: redis_server_version(&info_text),
    })
}

/// Verify 档带 CA 文件或客户端证书时，把 PEM 材料装进 TLS 参数；其余档位直接按 ConnectionInfo 建客户端。
fn build_redis_client(info: redis::ConnectionInfo, config: &SavedConnection) -> Result<redis::Client, redis::RedisError> {
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
        let opened = open_redis(config, route, password).await?;
        Ok(Opened {
            version: opened.version,
            session: Arc::new(RedisSession { browser: Arc::new(tokio::sync::Mutex::new(opened.conn)), client: opened.client }),
            canceller: Arc::new(NoCancel),
            schema_note: None,
            foreign_keys: None,
        })
    }
}

/// browser 是键浏览、值读取与命令行共用的多路复用连接；client 用来给查询批次另开独占连接。
struct RedisSession {
    browser: Arc<tokio::sync::Mutex<redis::aio::MultiplexedConnection>>,
    client: redis::Client,
}

#[async_trait::async_trait]
impl Session for RedisSession {
    fn catalog(&self, _foreign_keys: Option<bool>) -> CatalogSession {
        CatalogSession::Redis(Arc::clone(&self.browser))
    }

    /// SELECT 作用于整个网络连接。查询批次独占一条命令连接，避免和键浏览器互相切库。
    async fn query(&self, _foreign_keys: Option<bool>) -> Result<CatalogSession, String> {
        let config = redis::AsyncConnectionConfig::new().set_connection_timeout(Duration::from_secs(15));
        let conn = self.client.get_multiplexed_async_connection_with_config(&config).await.map_err(|err| err.to_string())?;
        Ok(CatalogSession::Redis(Arc::new(tokio::sync::Mutex::new(conn))))
    }

    async fn ping(&self) -> Result<(), String> {
        let mut conn = self.browser.lock().await;
        let pong: String = redis::cmd("PING").query_async(&mut *conn).await.map_err(|err| crate::error_text::chain(&err))?;
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
    fn browser(&self) -> Arc<tokio::sync::Mutex<redis::aio::MultiplexedConnection>> {
        Arc::clone(&self.browser)
    }
}
