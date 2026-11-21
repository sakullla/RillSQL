//! PostgreSQL 的建连、会话、探活、取消与连接错误分类。

use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use tokio_postgres::NoTls;
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::adapters::session::{Canceller, CatalogSession, Connector, Opened, Session};
use crate::connections::drivers::db_stage;
use crate::connections::model::{CaMode, SavedConnection, TlsMode, TlsSettings};
use crate::connections::route::{self, PostgresTlsPlan, Route};
use crate::connections::stages::{self, StageError};
use crate::connections::verify::EncryptWithoutVerify;

pub(crate) struct PostgresOpen {
    pub client: tokio_postgres::Client,
    /// 目录、补全与探活专用的独立连接：不加入用户事务（ADR-1）。
    pub meta_client: tokio_postgres::Client,
    pub version: String,
    pub cancel: tokio_postgres::CancelToken,
    pub tls: Option<MakeRustlsConnect>,
}

pub(crate) async fn open_postgres(config: &SavedConnection, route: &Route, password: &str) -> Result<PostgresOpen, StageError> {
    let plan = route::postgres_tls_plan(route).map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_ROUTE_RESOLVE_FAILED, err, "没有改用转发器地址做证书校验。"))?;
    if plan.verify_hostname && plan.host == "127.0.0.1" && route.tls_server_name.as_deref() != Some("127.0.0.1") {
        return Err(stages::fail_code(crate::connections::error_codes::CONN_ROUTE_RESOLVE_FAILED,
            "加密设置错把转发器地址当成了证书校验名",
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
    let tls_for_cancel = if plan.use_tls { Some(postgres_tls(config, &plan)?) } else { None };
    let client = connect_client(&pg, tls_for_cancel.clone(), route.tls_mode).await?;
    // 元数据会话是同一服务器上的第二条物理连接：目录读取、补全与探活不进入用户事务。
    let meta_client = connect_client(&pg, tls_for_cancel.clone(), route.tls_mode).await?;
    let cancel = client.cancel_token();
    let messages = client
        .simple_query("SELECT version()")
        .await
        .map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_VERSION_READ_FAILED, crate::error_text::chain(&err), "数据库认证已通过，但没有读到版本。"))?;
    let version = messages
        .iter()
        .find_map(|message| match message {
            tokio_postgres::SimpleQueryMessage::Row(row) => row.try_get(0).ok().flatten().map(str::to_owned),
            _ => None,
        })
        .unwrap_or_else(|| "未知版本".to_owned());
    Ok(PostgresOpen {
        client,
        meta_client,
        version,
        cancel,
        tls: tls_for_cancel,
    })
}

/// 打开一条 PostgreSQL 物理连接；连接任务在后台运行到连接结束。
async fn connect_client(
    config: &tokio_postgres::Config,
    tls: Option<MakeRustlsConnect>,
    mode: TlsMode,
) -> Result<tokio_postgres::Client, StageError> {
    let client = if let Some(tls) = tls {
        let (client, connection) = config.connect(tls).await.map_err(|err| postgres_connect_error(&err, mode, true))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    } else {
        let (client, connection) = config.connect(NoTls).await.map_err(|err| postgres_connect_error(&err, mode, false))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    };
    Ok(client)
}

fn postgres_tls(config: &SavedConnection, plan: &PostgresTlsPlan) -> Result<MakeRustlsConnect, StageError> {
    let tls = config.tls.as_ref().ok_or_else(|| stages::fail_code(crate::connections::error_codes::CONN_TLS_NOT_CONFIGURED, "缺少 TLS 设置", "没有退回明文。"))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    if !plan.verify_hostname {
        let verifier = Arc::new(EncryptWithoutVerify::new(provider.clone()).map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_TLS_DOWNGRADE_REFUSED, err, "没有退回明文。"))?);
        let builder = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_TLS_DOWNGRADE_REFUSED, crate::error_text::chain(&err), "没有退回明文。"))?
            .dangerous()
            .with_custom_certificate_verifier(verifier);
        let client = finish_client_config(builder, tls)?;
        return Ok(MakeRustlsConnect::new(client));
    }
    if tls.ca_mode == CaMode::File {
        let path = tls.ca_file.clone().ok_or_else(|| stages::fail_code(crate::connections::error_codes::CONN_TLS_CA_MISSING, "请填写 CA 证书文件", "没有改用不验证的连接。"))?;
        let bytes = std::fs::read(&path).map_err(|_| stages::fail_code(crate::connections::error_codes::CONN_TLS_DOWNGRADE_REFUSED, format!("读不到 CA 证书 {path}"), "没有改用不验证的连接。"))?;
        let mut roots = rustls::RootCertStore::empty();
        let mut added = 0;
        for cert in rustls::pki_types::CertificateDer::pem_slice_iter(&bytes) {
            let cert = cert.map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_TLS_DOWNGRADE_REFUSED, crate::error_text::chain(&err), "没有改用不验证的连接。"))?;
            roots.add(cert).map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_TLS_DOWNGRADE_REFUSED, crate::error_text::chain(&err), "没有改用不验证的连接。"))?;
            added += 1;
        }
        if added == 0 {
            return Err(stages::fail_code(crate::connections::error_codes::CONN_TLS_CA_UNREADABLE, "CA 文件里没有证书", "没有改用不验证的连接。"));
        }
        let builder = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_TLS_DOWNGRADE_REFUSED, crate::error_text::chain(&err), "没有改用不验证的连接。"))?
            .with_root_certificates(roots);
        return Ok(MakeRustlsConnect::new(finish_client_config(builder, tls)?));
    }
    if tls.client_cert.is_some() || tls.client_key.is_some() {
        return Err(stages::fail_code(crate::connections::error_codes::CONN_TLS_CA_MISSING,
            "使用客户端证书时请同时指定 CA 文件",
            "当前依赖没有公开系统根与客户端证书的组合接口，没有改为不验证。",
        ));
    }
    let (connector, errors) = MakeRustlsConnect::with_native_certs().map_err(|errors| {
        stages::fail_code(crate::connections::error_codes::CONN_TLS_DOWNGRADE_REFUSED,
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
            let cert_bytes = std::fs::read(cert).map_err(|_| stages::fail_code(crate::connections::error_codes::CONN_TLS_DOWNGRADE_REFUSED, format!("读不到客户端证书 {cert}"), "没有改用不验证的连接。"))?;
            let key_bytes = std::fs::read(key).map_err(|_| stages::fail_code(crate::connections::error_codes::CONN_TLS_DOWNGRADE_REFUSED, format!("读不到客户端私钥 {key}"), "没有改用不验证的连接。"))?;
            let chain = rustls::pki_types::CertificateDer::pem_slice_iter(&cert_bytes)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_TLS_DOWNGRADE_REFUSED, crate::error_text::chain(&err), "没有改用不验证的连接。"))?;
            let private = rustls::pki_types::PrivateKeyDer::from_pem_slice(&key_bytes)
                .map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_TLS_DOWNGRADE_REFUSED, crate::error_text::chain(&err), "没有改用不验证的连接。"))?;
            builder
                .with_client_auth_cert(chain, private)
                .map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_TLS_DOWNGRADE_REFUSED, crate::error_text::chain(&err), "没有改用不验证的连接。"))
        }
        (None, None) => Ok(builder.with_no_client_auth()),
        _ => Err(stages::fail_code(crate::connections::error_codes::CONN_TLS_CERT_MISSING, "客户端证书和私钥需要同时填写", "没有改用不验证的连接。")),
    }
}

/// PostgreSQL 建连失败。服务端回了 SQLSTATE 就说明网络和握手都通了，按它分类：
/// 28xxx 是认证失败（密码错误、用户不存在），3D000 是库不存在；
/// 只有没有 SQLSTATE 的传输层错误才算网络或加密失败。明文连接以前把密码错误也说成“网络连接失败”。
fn postgres_connect_error(err: &tokio_postgres::Error, mode: TlsMode, tls: bool) -> StageError {
    postgres_failure(err.code().map(|code| code.code()), crate::error_text::chain(err), mode, tls)
}

fn postgres_failure(sqlstate: Option<&str>, text: String, mode: TlsMode, tls: bool) -> StageError {
    use crate::connections::error_codes;
    match sqlstate {
        Some(code) if code.starts_with("28") => stages::fail_code(error_codes::CONN_AUTH_FAILED, text, "请检查数据库用户名和密码。"),
        Some("3D000") => stages::fail_code(error_codes::CONN_STAGE_FAILED, text, "服务器上没有这个数据库，请检查连接里的数据库名。"),
        Some(_) => stages::fail_code(error_codes::CONN_STAGE_FAILED, text, "服务器拒绝了这次连接，原因见上。"),
        None if tls => db_stage(mode, text),
        None => stages::fail_code(error_codes::CONN_NETWORK_FAILED, text, "网络连接失败，没有改走加密连接重试。"),
    }
}

/// PostgreSQL 连接器。
pub(in crate::adapters) struct PostgresConnector;

#[async_trait::async_trait]
impl Connector for PostgresConnector {
    async fn open_remote(&self, config: &SavedConnection, route: &Route, password: &str) -> Result<Opened, StageError> {
        let opened = open_postgres(config, route, password).await?;
        Ok(Opened {
            version: opened.version,
            session: Arc::new(PostgresSession { exec: Arc::new(opened.client), meta: Arc::new(opened.meta_client) }),
            canceller: Arc::new(PostgresCancel { token: opened.cancel, tls: opened.tls, via_ssh: config.ssh.is_some() }),
            schema_note: None,
            foreign_keys: None,
        })
    }
}

struct PostgresSession {
    /// 用户事务会话：SQL 查询、网格写入与结构业务页面共用同一条物理连接。
    exec: Arc<tokio_postgres::Client>,
    /// 元数据会话：目录、补全与探活，不进入用户事务。
    meta: Arc<tokio_postgres::Client>,
}

#[async_trait::async_trait]
impl Session for PostgresSession {
    fn catalog(&self, _foreign_keys: Option<bool>) -> CatalogSession {
        CatalogSession::Postgres(Arc::clone(&self.meta))
    }

    async fn query(&self, _foreign_keys: Option<bool>) -> Result<CatalogSession, String> {
        Ok(CatalogSession::Postgres(Arc::clone(&self.exec)))
    }

    /// 探活走元数据会话：用户事务中的语句不会让探活误判，探活也不会进入用户事务。
    async fn ping(&self) -> Result<(), String> {
        self.meta.simple_query("SELECT 1").await.map(|_| ()).map_err(|err| crate::error_text::chain(&err))
    }
}

/// 取消：`cancel_query` 自己另开一条连接，因此不会复用正在执行的流。
struct PostgresCancel {
    token: tokio_postgres::CancelToken,
    tls: Option<MakeRustlsConnect>,
    via_ssh: bool,
}

#[async_trait::async_trait]
impl Canceller for PostgresCancel {
    fn via_ssh(&self) -> bool {
        self.via_ssh
    }

    async fn cancel(&self) -> Result<bool, String> {
        let result = match &self.tls {
            Some(tls) => self.token.cancel_query(tls.clone()).await,
            None => self.token.cancel_query(NoTls).await,
        };
        result.map_err(|err| format!("取消失败：{err}。查询仍在执行。"))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_of(stage: StageError) -> (&'static str, String) {
        let StageError::Fail { code, hint, .. } = stage else { panic!("应当是失败阶段") };
        (code, hint)
    }

    #[test]
    fn a_rejected_postgres_password_is_an_auth_failure_not_a_network_one() {
        // 明文与加密两条路径：服务端回了 28P01 就是认证失败，不能说成网络连接失败。
        for tls in [false, true] {
            let (code, hint) = code_of(postgres_failure(Some("28P01"), "password authentication failed for user \"postgres\"".into(), TlsMode::Off, tls));
            assert_eq!(code, crate::connections::error_codes::CONN_AUTH_FAILED);
            assert!(hint.contains("用户名和密码"), "{hint}");
        }
        let (code, _) = code_of(postgres_failure(Some("28000"), "role \"nobody\" does not exist".into(), TlsMode::Off, false));
        assert_eq!(code, crate::connections::error_codes::CONN_AUTH_FAILED);
        let (_, hint) = code_of(postgres_failure(Some("3D000"), "database \"nope\" does not exist".into(), TlsMode::Off, false));
        assert!(hint.contains("数据库名"), "{hint}");
        // 没有 SQLSTATE 的才是传输层错误。
        let (code, hint) = code_of(postgres_failure(None, "Connection refused (os error 111)".into(), TlsMode::Off, false));
        assert_eq!(code, crate::connections::error_codes::CONN_NETWORK_FAILED);
        assert!(hint.contains("网络连接失败"), "{hint}");
    }
}
