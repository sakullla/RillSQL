//! 各引擎共用的连接基础设施：SSH 跳板隧道与 TLS/认证失败的通用分类。
//! 各引擎的建连、会话、取消与专属错误分类在 `adapters/<engine>/connect.rs`。

use std::sync::Arc;

use crate::connections::model::{SshAuthKind, SshSettings, TlsMode};
use crate::connections::route;
use crate::connections::stages::{self, StageError};

pub use crate::adapters::session::TdsClient;

/// 建连失败的通用分类（TLS / 认证 / 其他），供 MySQL 家族与 PostgreSQL 的 TLS 路径共用。
pub(crate) fn db_stage(mode: TlsMode, message: String) -> StageError {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("certificate") || lowered.contains("tls") || lowered.contains("ssl") || message.contains("证书") {
        let _ = route::on_tls_failure(mode);
        // 证书不是受信任机构签发的（本地/内网库的自签名证书最常见）：直接说出两条出路，而不只是“已终止”。
        if untrusted_certificate(&lowered) {
            return stages::fail_code(
                crate::connections::error_codes::CONN_TLS_TERMINATED,
                message,
                "服务器证书不是受信任机构签发的（自签名证书常见）。可在连接的“安全”里指定 CA 文件，或改为“加密”（不核对证书）。没有自动降级。",
            );
        }
        return stages::fail_code(crate::connections::error_codes::CONN_TLS_TERMINATED, message, "连接已终止。没有改用 localhost 身份校验，也没有降级为不验证证书或不加密后重试。");
    }
    if lowered.contains("password") || lowered.contains("access denied") || lowered.contains("authentication") || message.contains("认证") {
        return stages::fail_code(crate::connections::error_codes::CONN_AUTH_FAILED, message, "请检查数据库用户名和密码。");
    }
    stages::fail_code(crate::connections::error_codes::CONN_TLS_TERMINATED, message, "连接已终止，没有自动改用另一种加密方式重试。")
}

/// rustls / native-tls 对“证书链不可信”的几种写法。
fn untrusted_certificate(lowered: &str) -> bool {
    ["unknownissuer", "unknown issuer", "self signed", "self-signed", "unable to get local issuer", "certificate verify failed"]
        .iter()
        .any(|needle| lowered.contains(needle))
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
        Self(crate::error_text::chain(&err))
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
                .map_err(|err| crate::connections::forward::ForwardError::Channel(crate::error_text::chain(&err)))?
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
            return Err(TunnelFault::Identity(stages::fail_code(crate::connections::error_codes::CONN_SSH_AUTH_FAILED, err.0, "还没有尝试登录 SSH 跳板机或数据库。")));
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
                    return Err(TunnelFault::Authentication(stages::fail_code(crate::connections::error_codes::CONN_SSH_AUTH_FAILED,
                        "请填写 SSH 密码",
                        "这是登录跳板机失败，还没有尝试连接数据库。",
                    )))
                }
            };
            handle.authenticate_password(&ssh.username, password).await
        }
        SshAuthKind::Key => {
            let path = match ssh.key_path.clone() {
                Some(path) => path,
                None => {
                    return Err(TunnelFault::Authentication(stages::fail_code(crate::connections::error_codes::CONN_SSH_AUTH_FAILED,
                        "请填写私钥文件",
                        "这是登录跳板机失败，还没有尝试连接数据库。",
                    )))
                }
            };
            let key = match russh::keys::load_secret_key(&path, passphrase.filter(|item| !item.is_empty())) {
                Ok(key) => key,
                Err(err) => {
                    return Err(TunnelFault::Authentication(stages::fail_code(crate::connections::error_codes::CONN_SSH_KEY_UNREADABLE,
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
            return Err(TunnelFault::Authentication(stages::fail_code(
                crate::connections::error_codes::CONN_SSH_AUTH_FAILED,
                format!(
                    "Permission denied ({}) · 用户 {}",
                    if ssh.auth == SshAuthKind::Key { "publickey" } else { "password" },
                    ssh.username
                ),
                "这是登录跳板机失败，还没有尝试连接数据库。请检查 SSH 用户、私钥或口令。",
            )));
        }
        Err(err) => {
            return Err(TunnelFault::Authentication(stages::fail_code(crate::connections::error_codes::CONN_SSH_AUTH_FAILED,
                crate::error_text::chain(&err),
                "这是登录跳板机失败，还没有尝试连接数据库。",
            )));
        }
    }
    let forwarder = match crate::connections::forward::Forwarder::bind().await {
        Ok(forwarder) => forwarder,
        Err(err) => {
            return Err(TunnelFault::Tunnel(stages::fail_code(crate::connections::error_codes::CONN_TUNNEL_FAILED,
                crate::error_text::chain(&err),
                "已登录 SSH 跳板机，但到数据库的通道没有打通，还没有连接数据库。",
            )))
        }
    };
    let local_port = match forwarder.local_port() {
        Ok(port) => port,
        Err(err) => {
            return Err(TunnelFault::Tunnel(stages::fail_code(crate::connections::error_codes::CONN_TUNNEL_FAILED,
                crate::error_text::chain(&err),
                "已登录 SSH 跳板机，但到数据库的通道没有打通，还没有连接数据库。",
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
        crate::connections::hostkey::HostKeyDecision::Changed { presented, saved } => TunnelFault::Identity(stages::fail_code(crate::connections::error_codes::CONN_HOST_KEY_CHANGED,
            format!("服务器出示的密钥 {presented} 与已保存的 {saved} 不同"),
            "已阻止连接，没有发送任何凭据。如果确认服务器更换了密钥，请先通过可信渠道核实新指纹，然后忘记已保存的密钥再重新测试。",
        )),
        crate::connections::hostkey::HostKeyDecision::Trusted => {
            TunnelFault::Identity(stages::fail_code(crate::connections::error_codes::CONN_HOST_KEY_INCONSISTENT, "SSH 主机密钥状态不一致", "没有继续认证。"))
        }
    }
}

#[cfg(test)]
mod tls_hint_tests {
    use super::*;

    #[test]
    fn an_untrusted_certificate_says_how_to_get_through() {
        let stage = db_stage(TlsMode::Verify, "Input/output error: invalid peer certificate: UnknownIssuer".into());
        let StageError::Fail { hint, .. } = stage else { panic!("证书失败应当是失败阶段") };
        assert!(hint.contains("CA 文件") && hint.contains("不核对证书"), "{hint}");
        let other = db_stage(TlsMode::Verify, "TLS handshake eof".into());
        let StageError::Fail { hint, .. } = other else { panic!() };
        assert!(!hint.contains("CA 文件"), "其他 TLS 失败保持原说明：{hint}");
    }
}
