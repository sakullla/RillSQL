//! MongoDB 的建连、会话、探活与连接错误分类（R12）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::adapters::session::{CatalogSession, Connector, MongoAccess, NoCancel, Opened, Session};
use crate::connections::model::{CaMode, SavedConnection, TlsMode};
use crate::connections::route::{self, Route};
use crate::connections::stages::{self, StageError};

/// 主机字段也可以直接填 mongodb:// / mongodb+srv:// 连接串；此时端口字段不参与。
pub(crate) fn is_uri(host: &str) -> bool {
    let lowered = host.trim().to_ascii_lowercase();
    lowered.starts_with("mongodb://") || lowered.starts_with("mongodb+srv://")
}

pub(crate) struct MongoOpen {
    pub client: mongodb::Client,
    pub version: String,
}

/// MongoDB 连接（R12）。主机字段两种形态：
/// · host/port：装配 ClientOptions（SCRAM 凭据、authSource=认证库、replicaSet 参数、TLS 三档）；
/// · mongodb:// 或 mongodb+srv:// 整串：交给驱动解析，表单的认证库/副本集/TLS 档仅在串里没写对应参数时补齐，
///   串里带用户名而没带密码时用 keyring 密码补上（密码永远不落连接串之外的配置）。
/// 建连后探活：ping 确认会话可用，buildInfo 取版本横幅。任何一档失败都不降级重试。
pub(crate) async fn open_mongo(config: &SavedConnection, route: &Route, password: &str) -> Result<MongoOpen, StageError> {
    let mut options = mongo_client_options(config, route, password).await?;
    options.server_selection_timeout = Some(Duration::from_secs(15));
    let client = mongodb::Client::with_options(options.clone()).map_err(|err| mongo_stage(route.tls_mode, &err))?;
    let admin = client.database("admin");
    let pong = admin
        .run_command(mongodb::bson::doc! { "ping": 1 })
        .await
        .map_err(|err| mongo_stage(route.tls_mode, &err))?;
    if pong.get_i32("ok") != Ok(1) && pong.get_f64("ok") != Ok(1.0) {
        return Err(stages::fail_code(crate::connections::error_codes::CONN_PROBE_FAILED, format!("ping 返回了 {pong:?}"), "探活没有通过，连接没有建立为可用会话。"));
    }
    let build = client
        .database("admin")
        .run_command(mongodb::bson::doc! { "buildInfo": 1 })
        .await
        .map_err(|err| mongo_stage(route.tls_mode, &err))?;
    let version = build
        .get_str("version")
        .map(|value| format!("MongoDB {value}"))
        .unwrap_or_else(|_| "MongoDB 未知版本".to_owned());
    Ok(MongoOpen { client, version })
}

/// 装配 MongoDB ClientOptions。装配函数（不联网，plain mongodb:// 串解析也不发 DNS 查询），
/// 供单测核对主机/凭据/副本集/TLS 三档。
/// 整串 URI 与 SSH 隧道互斥：URI 的地址直接进驱动 hosts，route 的转发器地址没有入口，
/// 组合会把隧道静默绕过——保存阶段已拒绝，这里再拦一道（不建隧道、不连接）。
pub(crate) async fn mongo_client_options(
    config: &SavedConnection,
    route: &Route,
    password: &str,
) -> Result<mongodb::options::ClientOptions, StageError> {
    if is_uri(config.host.as_deref().unwrap_or_default()) {
        if config.ssh.is_some() {
            return Err(stages::fail_code(
                crate::connections::error_codes::CONN_ROUTE_RESOLVE_FAILED,
                "整串 URI 暂不支持 SSH 隧道，请改用 host/port 表单",
                "mongodb:// 连接串的地址会直接交给驱动，SSH 转发器地址没有入口，组合会静默绕过隧道。把主机拆成 host/port 填写后再开启跳板机。",
            ));
        }
        let mut options = mongodb::options::ClientOptions::parse(config.host.clone().unwrap_or_default())
            .await
            .map_err(|err| mongo_stage(route.tls_mode, &err))?;
        // 串里带了用户名但没带密码：用 keyring/本次输入的密码补上，密码不回写连接串。
        let needs_password = options
            .credential
            .as_ref()
            .is_some_and(|credential| credential.username.is_some() && credential.password.is_none());
        if needs_password && !password.is_empty() {
            if let Some(credential) = options.credential.as_mut() {
                credential.password = Some(password.to_owned());
            }
        }
        // 表单的 TLS 三档只在串里没有 tls 参数时生效（连接串写过的参数优先）。
        if options.tls.is_none() {
            options.tls = mongo_tls(config)?;
        }
        if options.repl_set_name.is_none() {
            options.repl_set_name = config.replica_set.clone().filter(|name| !name.trim().is_empty());
        }
        return Ok(options);
    }
    let mut options = mongodb::options::ClientOptions::builder().build();
    options.hosts = vec![mongodb::options::ServerAddress::Tcp {
        host: route.transport_host.clone(),
        port: Some(route.transport_port),
    }];
    options.app_name = Some("RillSQL".to_owned());
    if let Some(replica_set) = config.replica_set.as_deref().filter(|name| !name.trim().is_empty()) {
        options.repl_set_name = Some(replica_set.trim().to_owned());
    }
    let username = config.username.as_deref().unwrap_or("").trim();
    if !username.is_empty() {
        // SCRAM 凭据：机制留空由服务器协商（SCRAM-SHA-256 优先），认证库默认 admin。
        let mut credential = mongodb::options::Credential::default();
        credential.username = Some(username.to_owned());
        credential.password = (!password.is_empty()).then(|| password.to_owned());
        credential.source = config
            .database
            .as_deref()
            .map(str::trim)
            .filter(|source| !source.is_empty())
            .map(|source| source.to_owned());
        options.credential = Some(credential);
    }
    options.tls = mongo_tls(config)?;
    Ok(options)
}

/// TLS 三档映射：Off=不启用；Require=启用但不核对证书；Verify=启用并核对（CA 文件或 webpki 内置根）。
/// 驱动把客户端证书与私钥放在同一个 PEM 文件（cert_key_file_path）：两栏填不同路径时报明确错误，
/// 填同一路径即合并后的 PEM；只填一栏时按“该文件就是合并 PEM”处理。
fn mongo_tls(config: &SavedConnection) -> Result<Option<mongodb::options::Tls>, StageError> {
    let mode = config.tls.as_ref().map(|tls| tls.mode).unwrap_or(TlsMode::Off);
    match mode {
        TlsMode::Off => Ok(None),
        TlsMode::Require => Ok(Some(mongodb::options::Tls::Enabled(
            mongodb::options::TlsOptions::builder().allow_invalid_certificates(true).build(),
        ))),
        TlsMode::Verify => {
            let mut tls_options = mongodb::options::TlsOptions::builder().allow_invalid_certificates(false).build();
            if let Some(tls) = config.tls.as_ref() {
                if tls.ca_mode == CaMode::File {
                    if let Some(path) = tls.ca_file.as_deref().filter(|path| !path.trim().is_empty()) {
                        tls_options.ca_file_path = Some(PathBuf::from(path));
                    }
                }
                let cert = tls.client_cert.as_deref().map(str::trim).filter(|path| !path.is_empty());
                let key = tls.client_key.as_deref().map(str::trim).filter(|path| !path.is_empty());
                match (cert, key) {
                    (Some(cert), Some(key)) => {
                        if cert != key {
                            return Err(stages::fail_code(crate::connections::error_codes::CONN_TLS_CERT_MISSING,
                                "MongoDB 客户端证书需合并 PEM：证书与私钥需同一文件",
                                "mongodb 驱动只接受一个证书+私钥合并的 PEM 文件。请先把两份材料合并到一个文件，再在两栏填同一路径。",
                            ));
                        }
                        // 同一路径 = 合并后的 PEM。
                        tls_options.cert_key_file_path = Some(PathBuf::from(cert));
                    }
                    // 只填一栏：该文件即合并 PEM（其余引擎的两栏语义由各自驱动处理）。
                    (Some(path), None) | (None, Some(path)) => {
                        tls_options.cert_key_file_path = Some(PathBuf::from(path));
                    }
                    (None, None) => {}
                }
            }
            Ok(Some(mongodb::options::Tls::Enabled(tls_options)))
        }
    }
}

/// MongoDB 连接错误分类：TCP / 认证 / TLS 分开报，不降级重试（与 Redis 同纪律）。
pub(crate) fn mongo_stage(mode: TlsMode, err: &mongodb::error::Error) -> StageError {
    let text = crate::error_text::chain(err);
    let lowered = text.to_ascii_lowercase();
    let is_auth = matches!(&*err.kind, mongodb::error::ErrorKind::Authentication { .. })
        || matches!(&*err.kind, mongodb::error::ErrorKind::Command(command) if command.code == 18 || command.code == 13 || command.code == 33)
        || lowered.contains("authentication")
        || lowered.contains("auth failed")
        || lowered.contains("scram")
        || text.contains("认证");
    if is_auth {
        return stages::fail_code(crate::connections::error_codes::CONN_AUTH_FAILED, text, "请检查 MongoDB 用户名和密码（认证库默认 admin）。");
    }
    if matches!(&*err.kind, mongodb::error::ErrorKind::InvalidTlsConfig { .. })
        || lowered.contains("certificate")
        || lowered.contains("tls")
        || lowered.contains("ssl")
        || text.contains("证书")
    {
        let _ = route::on_tls_failure(mode);
        return stages::fail_code(crate::connections::error_codes::CONN_TLS_TERMINATED, text, "连接已终止。没有降级为不加密或不验证后重试。");
    }
    if matches!(
        &*err.kind,
        mongodb::error::ErrorKind::Io(_)
            | mongodb::error::ErrorKind::ServerSelection { .. }
            | mongodb::error::ErrorKind::ConnectionPoolCleared { .. }
            | mongodb::error::ErrorKind::DnsResolve { .. }
    ) || lowered.contains("timed out")
        || lowered.contains("timeout")
        || lowered.contains("refused")
        || lowered.contains("connection")
    {
        return stages::fail_code(crate::connections::error_codes::CONN_NETWORK_FAILED, text, "网络连接失败，没有改用加密连接重试。");
    }
    stages::fail_code(crate::connections::error_codes::CONN_TLS_TERMINATED, text, "连接已终止，没有自动改用另一种加密方式重试。")
}

/// MongoDB 连接器。文档操作没有会话级取消通道（探活/断线走健康句柄）。
pub(in crate::adapters) struct MongoConnector;

#[async_trait::async_trait]
impl Connector for MongoConnector {
    async fn open_remote(&self, config: &SavedConnection, route: &Route, password: &str) -> Result<Opened, StageError> {
        let opened = open_mongo(config, route, password).await?;
        Ok(Opened {
            version: opened.version,
            session: Arc::new(MongoSession(opened.client)),
            canceller: Arc::new(NoCancel),
            schema_note: None,
            foreign_keys: None,
        })
    }
}

/// mongodb::Client 内部共享连接池，克隆廉价；库/集合/文档操作共用这一个句柄。
struct MongoSession(mongodb::Client);

#[async_trait::async_trait]
impl Session for MongoSession {
    fn catalog(&self, _foreign_keys: Option<bool>) -> CatalogSession {
        CatalogSession::Mongo(self.0.clone())
    }

    async fn ping(&self) -> Result<(), String> {
        let reply = self
            .0
            .database("admin")
            .run_command(mongodb::bson::doc! { "ping": 1 })
            .await
            .map_err(|err| crate::error_text::chain(&err))?;
        if reply.get_f64("ok") == Ok(1.0) || reply.get_i32("ok") == Ok(1) {
            Ok(())
        } else {
            Err(format!("ping 返回了 {reply:?}"))
        }
    }

    fn as_mongo(&self) -> Option<&dyn MongoAccess> {
        Some(self)
    }
}

impl MongoAccess for MongoSession {
    fn client(&self) -> mongodb::Client {
        self.0.clone()
    }
}
