use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::pem::PemObject;

use mysql_async::prelude::Queryable;
use mysql_async::{OptsBuilder, Pool, SslOpts};
use tokio_postgres::NoTls;
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::connections::model::{CaMode, Engine, SavedConnection, SshAuthKind, SshSettings, TlsMode, TlsSettings};
use crate::connections::route::{self, MysqlTlsPlan, PostgresTlsPlan, Route};
use crate::connections::stages::{self, StageError};
use crate::connections::verify::EncryptWithoutVerify;

pub struct SqliteOpen {
    pub pool: sqlx::SqlitePool,
    pub version: String,
    pub foreign_keys: bool,
    pub schema_objects: usize,
}

pub async fn open_sqlite(path: &Path) -> Result<SqliteOpen, StageError> {
    if path.as_os_str().is_empty() {
        return Err(stages::fail("请选择数据库文件", "SQLite 只打开已有文件。"));
    }
    // 表编辑和目录元数据共用这一条已打开的会话，不再为写入另开只读连接。
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .busy_timeout(Duration::from_secs(5))
        .read_only(false);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|err| stages::fail(err.to_string(), "没有打开文件，后续阶段不会执行。"))?;
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type IN ('table', 'view') ORDER BY name")
        .fetch_all(&pool)
        .await
        .map_err(|err| stages::fail(err.to_string(), "文件已打开，但没有读到 schema。"))?;
    let version: String = sqlx::query_scalar("SELECT sqlite_version()")
        .fetch_one(&pool)
        .await
        .map_err(|err| stages::fail(err.to_string(), "文件已打开，但没有读到版本。"))?;
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&pool)
        .await
        .map_err(|err| stages::fail(err.to_string(), "没有读到 foreign_keys。"))?;
    Ok(SqliteOpen {
        pool,
        version,
        foreign_keys: foreign_keys != 0,
        schema_objects: names.len(),
    })
}

pub struct MysqlOpen {
    pub pool: Pool,
    pub version: String,
}

pub async fn open_mysql(config: &SavedConnection, route: &Route, password: &str) -> Result<MysqlOpen, StageError> {
    let plan = route::mysql_tls_plan(route);
    if plan.skip_domain_validation || plan.accept_invalid_certs {
        if route.tls_mode != TlsMode::Require {
            return Err(stages::fail(
                "TLS 计划不一致",
                "只有用户选择“加密但不验证”时才跳过证书校验。",
            ));
        }
    }
    let mut builder = OptsBuilder::default()
        .ip_or_hostname(plan.tcp_host.clone())
        .tcp_port(plan.tcp_port)
        .prefer_socket(false)
        .user(config.username.clone())
        .pass(Some(password.to_owned()));
    if let Some(database) = &config.database {
        builder = builder.db_name(Some(database.clone()));
    }
    // 目录和表浏览共用池里的这一条连接，查询时不再额外拨号。
    builder = builder.pool_opts(
        mysql_async::PoolOpts::new().with_constraints(mysql_async::PoolConstraints::new(0, 1).expect("mysql pool")),
    );
    if plan.ssl_opts_present {
        builder = builder.ssl_opts(Some(mysql_ssl(&plan, config.tls.as_ref())?));
    }
    let pool = Pool::new(builder);
    let mut conn = pool.get_conn().await.map_err(|err| db_stage(route.tls_mode, err.to_string()))?;
    let version: Option<(String,)> = conn
        .query_first("SELECT VERSION()")
        .await
        .map_err(|err| db_stage(route.tls_mode, err.to_string()))?;
    let version = version.map(|row| row.0).unwrap_or_else(|| "未知版本".to_owned());
    Ok(MysqlOpen {
        pool,
        version: version_note(config.engine, &version),
    })
}

pub(crate) fn mysql_ssl(plan: &MysqlTlsPlan, tls: Option<&TlsSettings>) -> Result<SslOpts, StageError> {
    let mut ssl = SslOpts::default()
        .with_danger_skip_domain_validation(plan.skip_domain_validation)
        .with_danger_accept_invalid_certs(plan.accept_invalid_certs)
        .with_danger_tls_hostname_override(plan.hostname_override.clone());
    if let Some(tls) = tls {
        ssl = install_mysql_trust(ssl, tls)?;
        if let (Some(cert), Some(key)) = (&tls.client_cert, &tls.client_key) {
            if !Path::new(cert).is_file() || !Path::new(key).is_file() {
                return Err(stages::fail("读不到客户端证书或私钥", "没有改用不验证的连接。"));
            }
            ssl = ssl.with_client_identity(Some(mysql_async::ClientIdentity::new(
                Path::new(cert).to_path_buf().into(),
                Path::new(key).to_path_buf().into(),
            )));
        }
    }
    Ok(ssl)
}

fn install_mysql_trust(ssl: SslOpts, tls: &TlsSettings) -> Result<SslOpts, StageError> {
    match tls.ca_mode {
        CaMode::File => {
            let path = tls.ca_file.clone().ok_or_else(|| stages::fail("请填写 CA 证书文件", "没有改用不验证的连接。"))?;
            if !Path::new(&path).is_file() {
                return Err(stages::fail(format!("读不到 CA 证书 {path}"), "没有改用不验证的连接。"));
            }
            // 只有指定 CA 文件时才不装操作系统信任库。
            Ok(ssl.with_root_certs(vec![PathBuf::from(path).into()]).with_disable_built_in_roots(true))
        }
        CaMode::System => {
            let mut roots = Vec::new();
            for anchor in os_trust_anchors()? {
                roots.push(anchor.into());
            }
            // mysql_async 的 built-in roots 是 webpki Mozilla 清单，不是操作系统信任库。
            Ok(ssl.with_root_certs(roots).with_disable_built_in_roots(true))
        }
    }
}

fn os_trust_anchors() -> Result<Vec<Vec<u8>>, StageError> {
    let loaded = rustls_native_certs::load_native_certs();
    if loaded.certs.is_empty() {
        let detail = if loaded.errors.is_empty() {
            "系统信任库里没有可用的根证书".to_owned()
        } else {
            loaded.errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
        };
        return Err(stages::fail(detail, "没有改用不验证的连接，也没有改用 Mozilla 内置根。"));
    }
    Ok(loaded.certs.into_iter().map(|cert| cert.as_ref().to_vec()).collect())
}

pub struct PostgresOpen {
    pub client: tokio_postgres::Client,
    pub version: String,
}

pub async fn open_postgres(config: &SavedConnection, route: &Route, password: &str) -> Result<PostgresOpen, StageError> {
    let plan = route::postgres_tls_plan(route).map_err(|err| stages::fail(err, "没有改用转发器地址做证书校验。"))?;
    if plan.verify_hostname && plan.host == "127.0.0.1" && route.tls_server_name.as_deref() != Some("127.0.0.1") {
        return Err(stages::fail(
            "TLS 计划把校验名改成了转发器地址",
            "证书仍应按数据库主机名校验。",
        ));
    }
    let mut pg = tokio_postgres::Config::new();
    if let Some(user) = &config.username {
        pg.user(user);
    }
    pg.password(password);
    if let Some(database) = &config.database {
        pg.dbname(database);
    }
    pg.application_name("RillSQL");
    pg.host(&plan.host);
    if let Some(addr) = plan.hostaddr {
        pg.hostaddr(addr);
    }
    pg.port(plan.port);
    pg.connect_timeout(Duration::from_secs(15));
    let client = if plan.use_tls {
        let tls = postgres_tls(config, &plan)?;
        let (client, connection) = pg.connect(tls).await.map_err(|err| db_stage(route.tls_mode, err.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    } else {
        let (client, connection) = pg.connect(NoTls).await.map_err(|err| stages::fail(err.to_string(), "网络连接失败，没有改用 TLS。"))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    };
    let messages = client
        .simple_query("SELECT version()")
        .await
        .map_err(|err| stages::fail(err.to_string(), "数据库认证已通过，但没有读到版本。"))?;
    let version = messages
        .iter()
        .find_map(|message| match message {
            tokio_postgres::SimpleQueryMessage::Row(row) => row.try_get(0).ok().flatten().map(str::to_owned),
            _ => None,
        })
        .unwrap_or_else(|| "未知版本".to_owned());
    Ok(PostgresOpen { client, version })
}

fn postgres_tls(config: &SavedConnection, plan: &PostgresTlsPlan) -> Result<MakeRustlsConnect, StageError> {
    let tls = config.tls.as_ref().ok_or_else(|| stages::fail("缺少 TLS 设置", "没有退回明文。"))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    if !plan.verify_hostname {
        let verifier = Arc::new(EncryptWithoutVerify::new(provider.clone()).map_err(|err| stages::fail(err, "没有退回明文。"))?);
        let builder = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|err| stages::fail(err.to_string(), "没有退回明文。"))?
            .dangerous()
            .with_custom_certificate_verifier(verifier);
        let client = finish_client_config(builder, tls)?;
        return Ok(MakeRustlsConnect::new(client));
    }
    if tls.ca_mode == CaMode::File {
        let path = tls.ca_file.clone().ok_or_else(|| stages::fail("请填写 CA 证书文件", "没有改用不验证的连接。"))?;
        let bytes = std::fs::read(&path).map_err(|_| stages::fail(format!("读不到 CA 证书 {path}"), "没有改用不验证的连接。"))?;
        let mut roots = rustls::RootCertStore::empty();
        let mut added = 0;
        for cert in rustls::pki_types::CertificateDer::pem_slice_iter(&bytes) {
            let cert = cert.map_err(|err| stages::fail(err.to_string(), "没有改用不验证的连接。"))?;
            roots.add(cert).map_err(|err| stages::fail(err.to_string(), "没有改用不验证的连接。"))?;
            added += 1;
        }
        if added == 0 {
            return Err(stages::fail("CA 文件里没有证书", "没有改用不验证的连接。"));
        }
        let builder = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|err| stages::fail(err.to_string(), "没有改用不验证的连接。"))?
            .with_root_certificates(roots);
        return Ok(MakeRustlsConnect::new(finish_client_config(builder, tls)?));
    }
    if tls.client_cert.is_some() || tls.client_key.is_some() {
        return Err(stages::fail(
            "使用客户端证书时请同时指定 CA 文件",
            "当前依赖没有公开系统根与客户端证书的组合接口，没有改为不验证。",
        ));
    }
    let (connector, errors) = MakeRustlsConnect::with_native_certs().map_err(|errors| {
        stages::fail(
            errors.iter().map(|err| err.to_string()).collect::<Vec<_>>().join("; "),
            "没有改用不验证的连接。",
        )
    })?;
    if !errors.is_empty() && connector_has_no_roots() {
        let _ = errors;
    }
    Ok(connector)
}

fn connector_has_no_roots() -> bool {
    false
}

fn finish_client_config(
    builder: rustls::ConfigBuilder<rustls::ClientConfig, rustls::client::WantsClientCert>,
    tls: &TlsSettings,
) -> Result<rustls::ClientConfig, StageError> {
    match (&tls.client_cert, &tls.client_key) {
        (Some(cert), Some(key)) => {
            let cert_bytes = std::fs::read(cert).map_err(|_| stages::fail(format!("读不到客户端证书 {cert}"), "没有改用不验证的连接。"))?;
            let key_bytes = std::fs::read(key).map_err(|_| stages::fail(format!("读不到客户端私钥 {key}"), "没有改用不验证的连接。"))?;
            let chain = rustls::pki_types::CertificateDer::pem_slice_iter(&cert_bytes)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| stages::fail(err.to_string(), "没有改用不验证的连接。"))?;
            let private = rustls::pki_types::PrivateKeyDer::from_pem_slice(&key_bytes)
                .map_err(|err| stages::fail(err.to_string(), "没有改用不验证的连接。"))?;
            builder
                .with_client_auth_cert(chain, private)
                .map_err(|err| stages::fail(err.to_string(), "没有改用不验证的连接。"))
        }
        (None, None) => Ok(builder.with_no_client_auth()),
        _ => Err(stages::fail("客户端证书和私钥需要同时填写", "没有改用不验证的连接。")),
    }
}

fn db_stage(mode: TlsMode, message: String) -> StageError {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("certificate") || lowered.contains("tls") || lowered.contains("ssl") || message.contains("证书") {
        let _ = route::on_tls_failure(mode);
        return stages::fail(message, "连接已终止。没有改用 localhost 身份校验，也没有降级为不验证证书或不加密后重试。");
    }
    if lowered.contains("password") || lowered.contains("access denied") || lowered.contains("authentication") || message.contains("认证") {
        return stages::fail(message, "请检查数据库用户名和密码。");
    }
    stages::fail(message, "连接已终止，没有自动改用另一种加密方式重试。")
}

pub fn version_note(engine: Engine, version: &str) -> String {
    let maria = version.to_ascii_lowercase().contains("mariadb");
    match (engine, maria) {
        (Engine::Mysql, true) => format!("{version} · 服务器横幅是 MariaDB，这条连接仍保持为 MySQL"),
        (Engine::Mariadb, false) => format!("{version} · 服务器横幅不像 MariaDB，这条连接仍保持为 MariaDB"),
        _ => version.to_owned(),
    }
}

#[derive(Debug, Clone)]
pub enum TunnelFault {
    Identity(StageError),
    Authentication(StageError),
    Tunnel(StageError),
}

pub struct StartedTunnel {
    pub local_port: u16,
    pub shutdown: tokio::sync::oneshot::Sender<()>,
}

struct KeyHandler {
    saved: Option<String>,
    outcome: Arc<std::sync::Mutex<Option<crate::connections::hostkey::HostKeyDecision>>>,
}

#[derive(Debug)]
struct HandlerErr(String);

impl From<russh::Error> for HandlerErr {
    fn from(err: russh::Error) -> Self {
        Self(err.to_string())
    }
}

impl russh::client::Handler for KeyHandler {
    type Error = HandlerErr;

    fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
        let key = server_public_key.public_key();
        let fingerprint = key.fingerprint(russh::keys::HashAlg::Sha256).to_string();
        let algorithm = format!("{}", key.algorithm());
        let decision = crate::connections::hostkey::decide_host_key(self.saved.as_deref(), &algorithm, &fingerprint);
        if let Ok(mut slot) = self.outcome.lock() {
            *slot = Some(decision.clone());
        }
        let allow = crate::connections::hostkey::credentials_allowed(&decision);
        async move { Ok(allow) }
    }
}

struct HandleOpener {
    handle: Arc<tokio::sync::Mutex<russh::client::Handle<KeyHandler>>>,
}

#[async_trait::async_trait]
impl crate::connections::forward::DirectTcpOpener for HandleOpener {
    async fn open_direct_tcpip(&self, host: &str, port: u16) -> Result<crate::connections::forward::Pipe, crate::connections::forward::ForwardError> {
        let channel = {
            let handle = self.handle.lock().await;
            handle
                .channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0)
                .await
                .map_err(|err| crate::connections::forward::ForwardError::Channel(err.to_string()))?
        };
        Ok(crate::connections::forward::Pipe::new(channel.into_stream()))
    }
}

pub async fn start_tunnel(
    ssh: &SshSettings,
    saved_fingerprint: Option<String>,
    password: Option<&str>,
    passphrase: Option<&str>,
    database_host: &str,
    database_port: u16,
    tls_server_name: &str,
) -> Result<StartedTunnel, TunnelFault> {
    let outcome = Arc::new(std::sync::Mutex::new(None));
    let handler = KeyHandler {
        saved: saved_fingerprint,
        outcome: outcome.clone(),
    };
    let config = Arc::new(russh::client::Config::default());
    let mut handle = match russh::client::connect(config, format!("{}:{}", ssh.host, ssh.port), handler).await {
        Ok(handle) => handle,
        Err(err) => {
            let decision = outcome.lock().ok().and_then(|slot| slot.clone());
            if let Some(decision) = decision {
                return Err(decision_error(decision));
            }
            return Err(TunnelFault::Identity(stages::fail(err.0, "尚未尝试 SSH 认证或数据库认证。")));
        }
    };
    let decision = outcome.lock().ok().and_then(|slot| slot.clone());
    if let Some(decision) = decision.as_ref() {
        if !crate::connections::hostkey::credentials_allowed(decision) {
            return Err(decision_error(decision.clone()));
        }
    }
    let auth = match ssh.auth {
        SshAuthKind::Password => {
            let password = match password {
                Some(password) => password,
                None => {
                    return Err(TunnelFault::Authentication(stages::fail(
                        "请填写 SSH 密码",
                        "这是跳板机认证失败，尚未尝试连接数据库。",
                    )))
                }
            };
            handle.authenticate_password(&ssh.username, password).await
        }
        SshAuthKind::Key => {
            let path = match ssh.key_path.clone() {
                Some(path) => path,
                None => {
                    return Err(TunnelFault::Authentication(stages::fail(
                        "请填写私钥文件",
                        "这是跳板机认证失败，尚未尝试连接数据库。",
                    )))
                }
            };
            let key = match russh::keys::load_secret_key(&path, passphrase.filter(|item| !item.is_empty())) {
                Ok(key) => key,
                Err(err) => {
                    return Err(TunnelFault::Authentication(stages::fail(
                        format!("读不到或无法解锁私钥：{err}"),
                        "私钥口令只用于解锁，没有写入配置。尚未尝试连接数据库。",
                    )))
                }
            };
            let key = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), None);
            handle.authenticate_publickey(&ssh.username, key).await
        }
    };
    match auth {
        Ok(russh::client::AuthResult::Success) => {}
        Ok(russh::client::AuthResult::Failure { .. }) => {
            return Err(TunnelFault::Authentication(stages::fail(
                format!(
                    "Permission denied ({}) · 用户 {}",
                    if ssh.auth == SshAuthKind::Key { "publickey" } else { "password" },
                    ssh.username
                ),
                "这是跳板机认证失败，尚未尝试连接数据库。请检查 SSH 用户、私钥或口令。",
            )));
        }
        Err(err) => {
            return Err(TunnelFault::Authentication(stages::fail(
                err.to_string(),
                "这是跳板机认证失败，尚未尝试连接数据库。",
            )));
        }
    }
    let forwarder = match crate::connections::forward::Forwarder::bind().await {
        Ok(forwarder) => forwarder,
        Err(err) => {
            return Err(TunnelFault::Tunnel(stages::fail(
                err.to_string(),
                "SSH 已认证，但隧道没有建立。尚未连接数据库。",
            )))
        }
    };
    let local_port = match forwarder.local_port() {
        Ok(port) => port,
        Err(err) => {
            return Err(TunnelFault::Tunnel(stages::fail(
                err.to_string(),
                "SSH 已认证，但隧道没有建立。尚未连接数据库。",
            )))
        }
    };
    let handle = Arc::new(tokio::sync::Mutex::new(handle));
    let (shutdown, stop) = tokio::sync::oneshot::channel();
    let opener = HandleOpener { handle };
    let channel_host = database_host.to_owned();
    let tls_name = tls_server_name.to_owned();
    let bastion = ssh.host.clone();
    tokio::spawn(async move {
        let _ = forwarder
            .serve_until(Arc::new(opener), channel_host, database_port, tls_name, bastion, stop)
            .await;
    });
    Ok(StartedTunnel { local_port, shutdown })
}

fn decision_error(decision: crate::connections::hostkey::HostKeyDecision) -> TunnelFault {
    match decision {
        crate::connections::hostkey::HostKeyDecision::Unconfirmed { algorithm, fingerprint } => {
            TunnelFault::Identity(StageError::WaitHostKey { algorithm, fingerprint })
        }
        crate::connections::hostkey::HostKeyDecision::Changed { presented, saved } => TunnelFault::Identity(stages::fail(
            format!("服务器出示的密钥 {presented} 与已保存的 {saved} 不同"),
            "已阻止连接，没有发送任何凭据。如果确认服务器更换了密钥，请先通过可信渠道核实新指纹，然后忘记已保存的密钥再重新测试。",
        )),
        crate::connections::hostkey::HostKeyDecision::Trusted => {
            TunnelFault::Identity(stages::fail("SSH 主机密钥状态不一致", "没有继续认证。"))
        }
    }
}
