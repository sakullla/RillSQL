use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// MySQL 与 MariaDB 是两种连接，不能因为协议相近而合并。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Mysql,
    Mariadb,
    Postgres,
    Sqlite,
}

impl Engine {
    pub fn as_str(self) -> &'static str {
        match self {
            Engine::Mysql => "mysql",
            Engine::Mariadb => "mariadb",
            Engine::Postgres => "postgres",
            Engine::Sqlite => "sqlite",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "mysql" => Some(Engine::Mysql),
            "mariadb" => Some(Engine::Mariadb),
            "postgres" => Some(Engine::Postgres),
            "sqlite" => Some(Engine::Sqlite),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn default_port(self) -> Option<u16> {
        match self {
            Engine::Mysql | Engine::Mariadb => Some(3306),
            Engine::Postgres => Some(5432),
            Engine::Sqlite => None,
        }
    }

    #[cfg(test)]
    pub fn is_remote(self) -> bool {
        !matches!(self, Engine::Sqlite)
    }

    pub fn label_zh(self) -> &'static str {
        match self {
            Engine::Mysql => "MySQL",
            Engine::Mariadb => "MariaDB",
            Engine::Postgres => "PostgreSQL",
            Engine::Sqlite => "SQLite",
        }
    }
}

pub const ENV_NONE: &str = "none";

/// 用户自己维护的环境标签。连接上保存的是 id；`none` 不进这张表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvTag {
    pub id: String,
    pub label: String,
    pub tone: String,
    pub position: i64,
}

pub fn normalize_env_id(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        ENV_NONE.to_owned()
    } else {
        trimmed.to_owned()
    }
}

pub fn parse_tone(value: &str) -> Option<&'static str> {
    match value.trim() {
        "dev" => Some("dev"),
        "test" => Some("test"),
        "prod" => Some("prod"),
        "local" => Some("local"),
        _ => None,
    }
}

pub fn validate_tag_label(label: &str) -> Result<String, String> {
    let trimmed = label.trim();
    let count = trimmed.chars().count();
    if count == 0 {
        return Err("请填写标签名称".to_owned());
    }
    if count > 24 {
        return Err("标签名称最多 24 个字".to_owned());
    }
    if trimmed == "无标签" {
        return Err("无标签表示不使用环境标签".to_owned());
    }
    Ok(trimmed.to_owned())
}

/// `Off` 表示用户关闭 TLS，不是握手失败后的退路。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    Off,
    Require,
    Verify,
}

impl TlsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            TlsMode::Off => "off",
            TlsMode::Require => "require",
            TlsMode::Verify => "verify",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(TlsMode::Off),
            "require" => Some(TlsMode::Require),
            "verify" => Some(TlsMode::Verify),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaMode {
    System,
    File,
}

impl CaMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CaMode::System => "system",
            CaMode::File => "file",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(CaMode::System),
            "file" => Some(CaMode::File),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshAuthKind {
    Password,
    Key,
}

impl SshAuthKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SshAuthKind::Password => "password",
            SshAuthKind::Key => "key",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "password" => Some(SshAuthKind::Password),
            "key" => Some(SshAuthKind::Key),
            _ => None,
        }
    }
}

/// 远程 TLS 材料。不含密码。`server_name` 是数据库主机名，不是传输地址。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TlsSettings {
    pub mode: TlsMode,
    pub server_name: String,
    pub ca_mode: CaMode,
    pub ca_file: Option<String>,
    pub client_cert: Option<String>,
    pub client_key: Option<String>,
}

/// SSH 跳板。不含密码和私钥口令。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshSettings {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SshAuthKind,
    pub key_path: Option<String>,
}

/// 写入普通配置的连接。结构上没有密码、私钥口令字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedConnection {
    pub id: String,
    pub name: String,
    pub engine: Engine,
    pub env: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub username: Option<String>,
    pub file_path: Option<String>,
    pub tls: Option<TlsSettings>,
    pub ssh: Option<SshSettings>,
    pub password_saved: bool,
    pub ssh_password_saved: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Clone)]
pub struct SecretInput {
    pub password: Option<Secret>,
    pub ssh_password: Option<Secret>,
    pub ssh_passphrase: Option<Secret>,
    pub save_password: bool,
}

impl fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretInput")
            .field("password", &self.password)
            .field("ssh_password", &self.ssh_password)
            .field("ssh_passphrase", &self.ssh_passphrase)
            .field("save_password", &self.save_password)
            .finish()
    }
}

impl Default for SecretInput {
    fn default() -> Self {
        Self {
            password: None,
            ssh_password: None,
            ssh_passphrase: None,
            save_password: false,
        }
    }
}

/// 界面草稿。密码不在这个结构里，避免和普通配置一起序列化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionDraft {
    pub id: Option<String>,
    pub name: String,
    pub engine: Engine,
    pub env: String,
    pub host: String,
    pub port: String,
    pub database: String,
    pub username: String,
    pub file_path: String,
    pub tls_mode: TlsMode,
    pub ca_mode: CaMode,
    pub ca_file: String,
    pub client_cert: String,
    pub client_key: String,
    pub ssh_on: bool,
    pub ssh_host: String,
    pub ssh_port: String,
    pub ssh_user: String,
    pub ssh_auth: SshAuthKind,
    pub ssh_key: String,
    pub save_password: bool,
}

impl ConnectionDraft {
    #[cfg(test)]
    pub fn blank(engine: Engine) -> Self {
        let env = if engine == Engine::Sqlite {
            "local".to_owned()
        } else {
            "dev".to_owned()
        };
        Self {
            id: None,
            name: String::new(),
            engine,
            env,
            host: String::new(),
            port: engine
                .default_port()
                .map(|port| port.to_string())
                .unwrap_or_default(),
            database: String::new(),
            username: String::new(),
            file_path: String::new(),
            tls_mode: if engine.is_remote() {
                TlsMode::Verify
            } else {
                TlsMode::Off
            },
            ca_mode: CaMode::System,
            ca_file: String::new(),
            client_cert: String::new(),
            client_key: String::new(),
            ssh_on: false,
            ssh_host: String::new(),
            ssh_port: "22".to_owned(),
            ssh_user: String::new(),
            ssh_auth: SshAuthKind::Key,
            ssh_key: String::new(),
            save_password: engine.is_remote(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

pub fn normalize(draft: &ConnectionDraft, id: String) -> Result<SavedConnection, Vec<FieldError>> {
    if draft.engine == Engine::Sqlite {
        return normalize_sqlite(draft, id);
    }
    normalize_remote(draft, id)
}

fn normalize_sqlite(draft: &ConnectionDraft, id: String) -> Result<SavedConnection, Vec<FieldError>> {
    let file_path = draft.file_path.trim();
    if file_path.is_empty() {
        return Err(vec![FieldError {
            field: "file_path".to_owned(),
            message: "请选择数据库文件".to_owned(),
        }]);
    }
    let name = chosen_name(&draft.name, Path::new(file_path).file_name().and_then(|n| n.to_str()).unwrap_or(file_path));
    Ok(SavedConnection {
        id,
        name,
        engine: Engine::Sqlite,
        env: normalize_env_id(&draft.env),
        host: None,
        port: None,
        database: None,
        username: None,
        file_path: Some(file_path.to_owned()),
        tls: None,
        ssh: None,
        password_saved: false,
        ssh_password_saved: false,
    })
}

fn normalize_remote(draft: &ConnectionDraft, id: String) -> Result<SavedConnection, Vec<FieldError>> {
    let mut errors = Vec::new();
    let host = draft.host.trim();
    if host.is_empty() {
        errors.push(FieldError {
            field: "host".to_owned(),
            message: "请填写主机".to_owned(),
        });
    }
    let port = match parse_port(&draft.port) {
        Some(port) => Some(port),
        None => {
            errors.push(FieldError {
                field: "port".to_owned(),
                message: "端口应为 1–65535".to_owned(),
            });
            None
        }
    };
    let username = draft.username.trim();
    if username.is_empty() {
        errors.push(FieldError {
            field: "username".to_owned(),
            message: "请填写用户名".to_owned(),
        });
    }
    let mut ssh = None;
    if draft.ssh_on {
        let ssh_host = draft.ssh_host.trim();
        if ssh_host.is_empty() {
            errors.push(FieldError {
                field: "ssh_host".to_owned(),
                message: "请填写跳板机主机".to_owned(),
            });
        }
        let ssh_user = draft.ssh_user.trim();
        if ssh_user.is_empty() {
            errors.push(FieldError {
                field: "ssh_user".to_owned(),
                message: "请填写 SSH 用户".to_owned(),
            });
        }
        let ssh_port = match parse_port(&draft.ssh_port) {
            Some(port) => Some(port),
            None => {
                errors.push(FieldError {
                    field: "ssh_port".to_owned(),
                    message: "端口应为 1–65535".to_owned(),
                });
                None
            }
        };
        if ssh_host.is_empty() || ssh_user.is_empty() || ssh_port.is_none() {
            // 字段错误已记录。
        } else if let Some(ssh_port) = ssh_port {
            ssh = Some(SshSettings {
                host: ssh_host.to_owned(),
                port: ssh_port,
                username: ssh_user.to_owned(),
                auth: draft.ssh_auth,
                key_path: nonempty(&draft.ssh_key),
            });
        }
    }
    if draft.tls_mode != TlsMode::Off && draft.ca_mode == CaMode::File && draft.ca_file.trim().is_empty() {
        errors.push(FieldError {
            field: "ca_file".to_owned(),
            message: "请填写 CA 证书文件".to_owned(),
        });
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let host = host.to_owned();
    let tls = Some(TlsSettings {
        mode: draft.tls_mode,
        server_name: host.clone(),
        ca_mode: draft.ca_mode,
        ca_file: nonempty(&draft.ca_file),
        client_cert: nonempty(&draft.client_cert),
        client_key: nonempty(&draft.client_key),
    });
    Ok(SavedConnection {
        id,
        name: chosen_name(&draft.name, &host),
        engine: draft.engine,
        env: normalize_env_id(&draft.env),
        host: Some(host),
        port,
        database: nonempty(&draft.database),
        username: Some(username.to_owned()),
        file_path: None,
        tls,
        ssh,
        password_saved: false,
        ssh_password_saved: false,
    })
}

fn chosen_name(name: &str, fallback: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

pub fn parse_port(value: &str) -> Option<u16> {
    let parsed: u16 = value.trim().parse().ok()?;
    if parsed == 0 { None } else { Some(parsed) }
}

pub fn password_account(id: &str) -> String {
    format!("conn:{id}:password")
}

pub fn ssh_password_account(id: &str) -> String {
    format!("conn:{id}:ssh-password")
}

/// 序列化后的普通配置不得包含这些机密。私钥口令永远不入库。
#[cfg(test)]
pub fn leaked_secret(blob: &str, secrets: &SecretInput) -> Option<&'static str> {
    for (label, secret) in [
        ("password", secrets.password.as_ref()),
        ("ssh_password", secrets.ssh_password.as_ref()),
        ("ssh_passphrase", secrets.ssh_passphrase.as_ref()),
    ] {
        if let Some(secret) = secret {
            if !secret.is_empty() && blob.contains(secret.expose()) {
                return Some(label);
            }
        }
    }
    None
}
