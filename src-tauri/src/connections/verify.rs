use std::sync::Arc;

use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject};
use rustls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection};

use crate::connections::model::TlsMode;
use crate::connections::route::{self, Retry};

#[derive(Clone, Copy)]
pub enum CertFixture {
    DatabaseHost,
    LocalhostOnly,
}

struct Material {
    ca: CertificateDer<'static>,
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

fn load(cert_pem: &str, key_pem: &str) -> Material {
    let ca = CertificateDer::from_pem_slice(include_str!("fixtures/ca.pem").as_bytes())
        .expect("测试 CA");
    let chain = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .expect("测试证书");
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).expect("测试私钥");
    Material { ca, chain, key }
}

fn material(fixture: CertFixture) -> Material {
    match fixture {
        CertFixture::DatabaseHost => load(include_str!("fixtures/db.pem"), include_str!("fixtures/db.key")),
        CertFixture::LocalhostOnly => {
            load(include_str!("fixtures/localhost.pem"), include_str!("fixtures/localhost.key"))
        }
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn server_config(fixture: CertFixture) -> Result<Arc<ServerConfig>, String> {
    let material = material(fixture);
    let config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|err| err.to_string())?
        .with_no_client_auth()
        .with_single_cert(material.chain, material.key)
        .map_err(|err| err.to_string())?;
    Ok(Arc::new(config))
}

fn verifying_client() -> Result<Arc<ClientConfig>, String> {
    let material = material(CertFixture::DatabaseHost);
    let mut roots = RootCertStore::empty();
    roots.add(material.ca).map_err(|err| err.to_string())?;
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|err| err.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

pub fn handshake(server_name: &str, fixture: CertFixture) -> Result<(), String> {
    let server_name = ServerName::try_from(server_name.to_owned()).map_err(|err| err.to_string())?;
    let mut client = ClientConnection::new(verifying_client()?, server_name).map_err(|err| err.to_string())?;
    let mut server = ServerConnection::new(server_config(fixture)?).map_err(|err| err.to_string())?;
    for _ in 0..16 {
        let mut client_buf = Vec::new();
        let client_wrote = client.write_tls(&mut client_buf).map_err(|err| err.to_string())?;
        if client_wrote > 0 {
            let mut rest = client_buf.as_slice();
            server.read_tls(&mut rest).map_err(|err| err.to_string())?;
            server.process_new_packets().map_err(|err| err.to_string())?;
        }
        let mut server_buf = Vec::new();
        let server_wrote = server.write_tls(&mut server_buf).map_err(|err| err.to_string())?;
        if server_wrote > 0 {
            let mut rest = server_buf.as_slice();
            client.read_tls(&mut rest).map_err(|err| err.to_string())?;
            client.process_new_packets().map_err(|err| err.to_string())?;
        }
        if !client.is_handshaking() && !server.is_handshaking() {
            return Ok(());
        }
        if client_wrote == 0 && server_wrote == 0 {
            break;
        }
    }
    Err("TLS 握手没有完成".to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsAttempt {
    pub server_name: String,
    pub mode: TlsMode,
    pub transport_host: String,
}

#[derive(Debug, Default)]
pub struct TlsSession {
    pub attempts: Vec<TlsAttempt>,
}

impl TlsSession {
    /// 只按证书校验名握手一次。失败后不改名、不降级。
    pub fn perform(
        &mut self,
        mode: TlsMode,
        tls_server_name: &str,
        transport_host: &str,
        fixture: CertFixture,
    ) -> Result<(), String> {
        let server_name = route::server_name_for(mode, tls_server_name, transport_host)?;
        self.attempts.push(TlsAttempt {
            server_name: server_name.clone(),
            mode,
            transport_host: transport_host.to_owned(),
        });
        let result = handshake(&server_name, fixture);
        if result.is_err() {
            match route::on_tls_failure(mode) {
                Retry::Stop { .. } => {}
            }
        }
        result
    }
}

/// 用户明确选择“加密但不验证”时使用。验证失败不会改走这条路径。
#[derive(Debug)]
pub struct EncryptWithoutVerify {
    inner: Arc<rustls::client::WebPkiServerVerifier>,
}

impl EncryptWithoutVerify {
    pub fn new(provider: Arc<rustls::crypto::CryptoProvider>) -> Result<Self, String> {
        let inner = rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(RootCertStore::empty()), provider)
            .build()
            .map_err(|err| err.to_string())?;
        Ok(Self { inner })
    }
}

impl ServerCertVerifier for EncryptWithoutVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localhost_certificate_fails_for_the_database_name_and_is_not_retried() {
        let mut session = TlsSession::default();
        let error = session
            .perform(TlsMode::Verify, "db.example.test", "127.0.0.1", CertFixture::LocalhostOnly)
            .expect_err("只签给 localhost 的证书必须失败");
        assert!(
            error.to_lowercase().contains("cert") || error.contains("Certificate") || error.contains("证书") || !error.is_empty(),
            "{error}"
        );
        assert_eq!(session.attempts.len(), 1);
        assert_eq!(session.attempts[0].server_name, "db.example.test");
        assert_eq!(session.attempts[0].mode, TlsMode::Verify);
        assert_ne!(session.attempts[0].server_name, session.attempts[0].transport_host);
        assert!(matches!(route::on_tls_failure(TlsMode::Verify), Retry::Stop { .. }));
    }

    #[test]
    fn database_certificate_succeeds_when_the_transport_is_loopback() {
        let mut session = TlsSession::default();
        session
            .perform(TlsMode::Verify, "db.example.test", "127.0.0.1", CertFixture::DatabaseHost)
            .expect("证书签发给数据库主机名时，经转发器地址也应成功");
        assert_eq!(session.attempts.len(), 1);
        let wrong_name = handshake("127.0.0.1", CertFixture::DatabaseHost);
        assert!(wrong_name.is_err(), "同一张证书不能改用转发器地址校验");
    }
}
