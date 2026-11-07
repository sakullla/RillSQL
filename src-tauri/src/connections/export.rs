//! 连接配置导出/导入：无口令为明文 JSON，带口令则整体加密。
//! 文件头 `RILLSQLCX1` + salt + nonce + XChaCha20-Poly1305 密文；口令经 argon2id 派生密钥。
//! 凭据只存系统凭据库（keyring-only）：导出前把 password_saved 标志清零，机密字段结构上不存在。

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};

use crate::connections::model::SavedConnection;

/// 加密导出的文件头。明文导出是普通 JSON，不以此开头。
pub const MAGIC: &[u8; 10] = b"RILLSQLCX1";
pub const EXPORT_FORMAT: &str = "rillsql-connections";
pub const EXPORT_VERSION: u64 = 1;

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;
/// XChaCha20-Poly1305 的认证标签长度，用于校验密文段是否完整。
const TAG_LEN: usize = 16;

/// 解密后逐条解析时无法导入的条目（引擎不受支持、字段无效），进报告不阻塞其余条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedRow {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedImport {
    pub connections: Vec<SavedConnection>,
    pub skipped: Vec<SkippedRow>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ExportEnvelope {
    format: String,
    version: u64,
    connections: Vec<SavedConnection>,
}

/// 逐条松散解析用的信封：connections 保持原始 JSON，未知引擎的行才能进报告而不是炸掉整个文件。
#[derive(Debug, Deserialize)]
struct RawEnvelope {
    format: String,
    version: u64,
    connections: Vec<serde_json::Value>,
}

/// 导出与导入都过这一步：文件里没有任何机密，导入后密码需重填。
fn strip_credentials(connection: &SavedConnection) -> SavedConnection {
    let mut clean = connection.clone();
    clean.password_saved = false;
    clean.ssh_password_saved = false;
    clean
}

fn envelope(connections: &[SavedConnection]) -> ExportEnvelope {
    ExportEnvelope {
        format: EXPORT_FORMAT.to_owned(),
        version: EXPORT_VERSION,
        connections: connections.iter().map(strip_credentials).collect(),
    }
}

pub fn encode_export(connections: &[SavedConnection], passphrase: Option<&str>) -> Result<Vec<u8>, String> {
    let envelope = envelope(connections);
    match passphrase.map(str::trim).filter(|value| !value.is_empty()) {
        None => serde_json::to_vec_pretty(&envelope).map_err(|err| format!("序列化导出内容失败：{err}")),
        Some(passphrase) => encrypt(&envelope, passphrase),
    }
}

/// OS CSPRNG（v4 UUID）取 40 字节：16 字节 salt + 24 字节 nonce。
fn random_seed() -> [u8; SALT_LEN + NONCE_LEN] {
    let mut seed = [0u8; SALT_LEN + NONCE_LEN];
    let blocks = [
        uuid::Uuid::new_v4().as_u128().to_le_bytes(),
        uuid::Uuid::new_v4().as_u128().to_le_bytes(),
        uuid::Uuid::new_v4().as_u128().to_le_bytes(),
    ];
    for (index, block) in blocks.iter().enumerate() {
        let start = index * 16;
        let end = (start + 16).min(seed.len());
        if start < end {
            seed[start..end].copy_from_slice(&block[..end - start]);
        }
    }
    seed
}

fn derive_key(passphrase: &str, salt: &[u8]) -> Result<[u8; KEY_LEN], String> {
    let params = Params::new(19456, 2, 1, Some(KEY_LEN)).map_err(|err| format!("argon2 参数无效：{err}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; KEY_LEN];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|err| format!("口令派生失败：{err}"))?;
    Ok(key)
}

#[allow(deprecated)] // XNonce 仍来自 generic-array 0.14，24 字节 nonce 和密文格式不变。
fn seal(plain: &[u8], passphrase: &str) -> Result<Vec<u8>, String> {
    let seed = random_seed();
    let (salt, nonce) = seed.split_at(SALT_LEN);
    let key = derive_key(passphrase, salt)?;
    let cipher = XChaCha20Poly1305::new((&key).into());
    let sealed = cipher
        .encrypt(
            XNonce::from_slice(nonce),
            Payload { msg: plain, aad: MAGIC },
        )
        .map_err(|_| "加密导出内容失败。".to_owned())?;
    let mut file = Vec::with_capacity(MAGIC.len() + seed.len() + sealed.len());
    file.extend_from_slice(MAGIC);
    file.extend_from_slice(&seed);
    file.extend_from_slice(&sealed);
    Ok(file)
}

fn encrypt(envelope: &ExportEnvelope, passphrase: &str) -> Result<Vec<u8>, String> {
    let plain = serde_json::to_vec(envelope).map_err(|err| format!("序列化导出内容失败：{err}"))?;
    seal(&plain, passphrase)
}

/// 口令不正确、文件被改动或截断都走同一拒绝：AEAD 校验失败无法区分，整体不导入。
#[allow(deprecated)] // 与 seal 同一套 XNonce，解密必须读回原来的 24 字节 nonce。
fn open(file: &[u8], passphrase: &str) -> Result<Vec<u8>, String> {
    let body = &file[MAGIC.len()..];
    if body.len() < SALT_LEN + NONCE_LEN + TAG_LEN {
        return Err(crate::connections::error_codes::coded_string(crate::connections::error_codes::IMPORT_UNRECOGNIZED, "这份文件不是完整的 RillSQL 加密导出，已整体拒绝。"));
    }
    let (salt, rest) = body.split_at(SALT_LEN);
    let (nonce, sealed) = rest.split_at(NONCE_LEN);
    let key = derive_key(passphrase, salt)?;
    let cipher = XChaCha20Poly1305::new((&key).into());
    cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload { msg: sealed, aad: MAGIC },
        )
        .map_err(|_| crate::connections::error_codes::coded_string(crate::connections::error_codes::IMPORT_BAD_PASSPHRASE, "口令不正确或文件已损坏，已整体拒绝导入。"))
}

pub fn parse_import(file: &[u8], passphrase: Option<&str>) -> Result<ParsedImport, String> {
    let payload = if file.starts_with(MAGIC) {
        let Some(passphrase) = passphrase.map(str::trim).filter(|value| !value.is_empty()) else {
            return Err(crate::connections::error_codes::coded_string(crate::connections::error_codes::IMPORT_PASSPHRASE_REQUIRED, "这份文件带口令加密，请输入导出时设置的口令。"));
        };
        open(file, passphrase)?
    } else {
        String::from_utf8(file.to_vec())
            .map_err(|_| crate::connections::error_codes::coded_string(crate::connections::error_codes::IMPORT_UNRECOGNIZED, "无法识别这份文件：既不是 RillSQL 明文导出，也不是加密导出。"))?
            .into_bytes()
    };
    parse_payload(&payload)
}

fn parse_payload(payload: &[u8]) -> Result<ParsedImport, String> {
    let raw: RawEnvelope = serde_json::from_slice(payload)
        .map_err(|_| crate::connections::error_codes::coded_string(crate::connections::error_codes::IMPORT_UNRECOGNIZED, "文件内容无法识别为 RillSQL 连接导出，已整体拒绝。"))?;
    if raw.format != EXPORT_FORMAT {
        return Err("这不是 RillSQL 导出的连接文件，已整体拒绝。".to_owned());
    }
    if raw.version != EXPORT_VERSION {
        return Err(format!("不支持的导出版本 {}，已整体拒绝。", raw.version));
    }
    let mut connections = Vec::with_capacity(raw.connections.len());
    let mut skipped = Vec::new();
    for row in raw.connections {
        let name = row
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("未命名连接")
            .to_owned();
        match serde_json::from_value::<SavedConnection>(row.clone()) {
            Ok(connection) => connections.push(strip_credentials(&connection)),
            Err(_) => skipped.push(SkippedRow {
                name,
                reason: skip_reason(&row),
            }),
        }
    }
    Ok(ParsedImport { connections, skipped })
}

fn skip_reason(row: &serde_json::Value) -> String {
    if let Some(engine) = row.get("engine").and_then(|value| value.as_str()) {
        if crate::connections::model::Engine::parse(engine).is_err() {
            return format!("不支持的数据库引擎 {engine}。");
        }
    }
    "字段缺失或无效。".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::model::{CaMode, Engine, SshAuthKind, SshSettings, TlsMode, TlsSettings};

    fn postgres_row(id: &str) -> SavedConnection {
        SavedConnection {
            id: id.to_owned(),
            name: "生产库".to_owned(),
            engine: Engine::Postgres,
            env: "prod".to_owned(),
            host: Some("db.example.test".to_owned()),
            port: Some(5432),
            database: Some("analytics".to_owned()),
            username: Some("analyst".to_owned()),
            file_path: None,
            tls: Some(TlsSettings {
                mode: TlsMode::Verify,
                server_name: "db.example.test".to_owned(),
                ca_mode: CaMode::File,
                ca_file: Some("C:/certs/ca.pem".to_owned()),
                client_cert: Some("C:/certs/client.pem".to_owned()),
                client_key: Some("C:/certs/client.key".to_owned()),
            }),
            ssh: Some(SshSettings {
                host: "bastion.example.test".to_owned(),
                port: 22,
                username: "jump".to_owned(),
                auth: SshAuthKind::Key,
                key_path: Some("C:/ssh/id_ed25519".to_owned()),
            }),
            replica_set: None,
            // 已保存密码的连接：导出后必须既没有值也没有标志。
            password_saved: true,
            ssh_password_saved: true,
        }
    }

    fn sqlite_row(id: &str) -> SavedConnection {
        SavedConnection {
            id: id.to_owned(),
            name: "本地笔记".to_owned(),
            engine: Engine::Sqlite,
            env: "local".to_owned(),
            host: None,
            port: None,
            database: None,
            username: None,
            file_path: Some("C:/data/notes.sqlite".to_owned()),
            tls: None,
            ssh: None,
            replica_set: None,
            password_saved: false,
            ssh_password_saved: false,
        }
    }

    fn sample() -> Vec<SavedConnection> {
        vec![postgres_row("pg-1"), sqlite_row("lite-1")]
    }

    fn decrypted_export(connections: &[SavedConnection], passphrase: &str) -> Vec<SavedConnection> {
        let file = encode_export(connections, Some(passphrase)).unwrap();
        parse_import(&file, Some(passphrase)).unwrap().connections
    }

    #[test]
    fn encrypted_roundtrip_restores_non_secret_config_without_credentials() {
        let restored = decrypted_export(&sample(), "口令-正确");
        assert_eq!(restored.len(), 2);
        let pg = &restored[0];
        assert_eq!(pg.id, "pg-1");
        assert_eq!(pg.engine, Engine::Postgres);
        assert_eq!(pg.env, "prod");
        assert_eq!(pg.host.as_deref(), Some("db.example.test"));
        assert_eq!(pg.port, Some(5432));
        assert_eq!(pg.database.as_deref(), Some("analytics"));
        assert_eq!(pg.username.as_deref(), Some("analyst"));
        let tls = pg.tls.as_ref().unwrap();
        assert_eq!(tls.mode, TlsMode::Verify);
        assert_eq!(tls.ca_mode, CaMode::File);
        assert_eq!(tls.ca_file.as_deref(), Some("C:/certs/ca.pem"));
        assert_eq!(tls.client_cert.as_deref(), Some("C:/certs/client.pem"));
        assert_eq!(tls.client_key.as_deref(), Some("C:/certs/client.key"));
        let ssh = pg.ssh.as_ref().unwrap();
        assert_eq!(ssh.host, "bastion.example.test");
        assert_eq!(ssh.port, 22);
        assert_eq!(ssh.username, "jump");
        assert_eq!(ssh.auth, SshAuthKind::Key);
        assert_eq!(ssh.key_path.as_deref(), Some("C:/ssh/id_ed25519"));
        assert!(!pg.password_saved);
        assert!(!pg.ssh_password_saved);
        assert_eq!(restored[1].file_path.as_deref(), Some("C:/data/notes.sqlite"));
        assert_eq!(restored[1].engine, Engine::Sqlite);
    }

    #[test]
    fn wrong_passphrase_is_rejected_wholesale() {
        let file = encode_export(&sample(), Some("正确口令")).unwrap();
        let error = parse_import(&file, Some("错误口令")).unwrap_err();
        assert!(error.contains("口令不正确"), "{error}");
        // 篡改密文同样整体拒绝，不会部分导入。
        let mut tampered = file.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        let tampered_error = parse_import(&tampered, Some("正确口令")).unwrap_err();
        assert!(tampered_error.contains("已整体拒绝"), "{tampered_error}");
    }

    #[test]
    fn magic_is_recognized_and_requires_a_passphrase() {
        let file = encode_export(&sample(), Some("秘密")).unwrap();
        assert!(file.starts_with(MAGIC));
        let error = parse_import(&file, None).unwrap_err();
        assert!(error.contains("带口令加密"), "{error}");
        assert!(error.contains("口令"), "{error}");
        // 空口令视同未提供。
        let blank = parse_import(&file, Some("   ")).unwrap_err();
        assert!(blank.contains("带口令加密"), "{blank}");
    }

    #[test]
    fn plain_export_is_readable_json_without_secrets() {
        let file = encode_export(&sample(), None).unwrap();
        assert!(!file.starts_with(MAGIC));
        let text = String::from_utf8(file.clone()).unwrap();
        // 明文导出是可读 JSON：格式与版本可直读。
        assert!(text.contains("\"format\": \"rillsql-connections\""));
        assert!(text.contains("\"version\": 1"));
        // 结构上就没有密码/私钥内容字段；saved 标志也清零。
        assert!(!text.contains("\"password\""));
        assert!(!text.contains("\"sshPassword\""));
        assert!(!text.contains("\"passphrase\""));
        assert!(!text.contains("\"passwordSaved\": true"));
        assert!(!text.contains("\"sshPasswordSaved\": true"));
        // 往返：明文导入同样恢复配置且不带凭据。
        let restored = parse_import(&file, None).unwrap().connections;
        assert_eq!(restored.len(), 2);
        assert!(!restored[0].password_saved);
        assert!(!restored[0].ssh_password_saved);
    }

    #[test]
    fn unsupported_engine_rows_are_skipped_not_fatal() {
        let foreign = format!(
            "{{\"format\":\"{EXPORT_FORMAT}\",\"version\":1,\"connections\":[{},{{\"id\":\"x2\",\"name\":\"报表库\",\"engine\":\"clickhouse\",\"env\":\"none\",\"passwordSaved\":false,\"sshPasswordSaved\":false}}]}}",
            serde_json::to_string(&postgres_row("x1")).unwrap()
        );
        let parsed = parse_import(foreign.as_bytes(), None).unwrap();
        assert_eq!(parsed.connections.len(), 1);
        assert_eq!(parsed.connections[0].id, "x1");
        assert_eq!(parsed.skipped.len(), 1);
        assert_eq!(parsed.skipped[0].name, "报表库");
        assert!(parsed.skipped[0].reason.contains("clickhouse"), "{}", parsed.skipped[0].reason);
    }

    #[test]
    fn foreign_formats_are_rejected_wholesale() {
        // DBeaver 文件没有 format 字段：整体拒绝，不落到逐条报告。
        let dbeaver = "{\"connections\":{\"pg-1\":{\"provider\":\"postgresql\",\"name\":\"生产库\",\"configuration\":{\"host\":\"h\",\"user\":\"u\"}}}}";
        let error = parse_import(dbeaver.as_bytes(), None).unwrap_err();
        assert!(error.contains("整体拒绝"), "{error}");
        // 版本不认识：整体拒绝。
        let future = format!("{{\"format\":\"{EXPORT_FORMAT}\",\"version\":2,\"connections\":[]}}");
        let version_error = parse_import(future.as_bytes(), None).unwrap_err();
        assert!(version_error.contains("版本"), "{version_error}");
        // 非 UTF-8 也不是加密导出：整体拒绝。
        let binary = parse_import(&[0xff, 0xfe, 0x00, 0x01], None).unwrap_err();
        assert!(binary.contains("无法识别"), "{binary}");
    }
}
