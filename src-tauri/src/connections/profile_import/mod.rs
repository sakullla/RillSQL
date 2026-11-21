//! Credential-free source rows. Keep the original row while editing a clone of its draft.

use crate::adapters::adapter;
use crate::connections::model::{
    normalize, CaMode, ConnectionDraft, Engine, FieldError, SshAuthKind, TlsMode, ENV_NONE,
};

mod dbeaver;
mod jdbc;

pub(crate) use dbeaver::parse_dbeaver;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProfileRow {
    pub name: String,
    pub draft: Option<ConnectionDraft>,
    pub blockers: Vec<String>,
    pub notes: Vec<String>,
    /// Set only when the source declares a TLS policy or certificate material.
    pub required_tls: Option<TlsMode>,
}

impl ProfileRow {
    fn block(&mut self, reason: &str) {
        if !self.blockers.iter().any(|existing| existing == reason) {
            self.blockers.push(reason.to_owned());
        }
    }

    /// Revalidate after editing, including immutable source restrictions.
    pub fn issues(&self, draft: &ConnectionDraft) -> Vec<FieldError> {
        let mut errors: Vec<_> = self
            .blockers
            .iter()
            .map(|reason| issue("source", reason))
            .collect();
        let Some(source) = &self.draft else {
            return errors;
        };
        if let Err(fields) = normalize(draft, String::new()) {
            errors.extend(fields);
        }
        if draft.id.is_some() || draft.save_password || source.engine != draft.engine {
            errors.push(issue("source", "导入必须使用原引擎新建无凭据配置"));
        }
        if source.env == "prod" && draft.env != "prod" {
            errors.push(issue("env", "必须保留生产环境标记"));
        }
        if source.ssh_on
            && (!draft.ssh_on
                || source.ssh_auth != draft.ssh_auth
                || (!source.ssh_host.is_empty() && source.ssh_host != draft.ssh_host)
                || (!source.ssh_port.is_empty() && source.ssh_port != draft.ssh_port)
                || (!source.ssh_user.is_empty() && source.ssh_user != draft.ssh_user))
        {
            errors.push(issue(
                "ssh_host",
                "必须保留来源 SSH 跳板、端口、用户及认证方式",
            ));
        }
        if draft.ssh_on && draft.ssh_auth == SshAuthKind::Key && !absolute_path(&draft.ssh_key) {
            errors.push(issue("ssh_key", "请定位 SSH 私钥文件"));
        }
        if self.required_tls.is_some_and(|mode| draft.tls_mode != mode) {
            errors.push(issue("tls_mode", "必须保留来源 TLS 校验模式"));
        }
        if source.ca_mode == CaMode::File
            && (draft.ca_mode != CaMode::File || !absolute_path(&draft.ca_file))
        {
            errors.push(issue("ca_file", "请定位来源指定的 CA 证书"));
        }
        if (!source.client_cert.is_empty() || !source.client_key.is_empty())
            && (!absolute_path(&draft.client_cert) || !absolute_path(&draft.client_key))
        {
            errors.push(issue("client_cert", "请定位客户端证书及私钥"));
        }
        if !draft.client_cert.is_empty() || !draft.client_key.is_empty() {
            match draft.engine {
                Engine::Postgres
                    if draft.tls_mode == TlsMode::Verify
                        && (draft.ca_mode != CaMode::File || !absolute_path(&draft.ca_file)) =>
                {
                    errors.push(issue(
                        "ca_file",
                        "PostgreSQL 客户端证书验证需要指定 CA 文件",
                    ));
                }
                Engine::Mongodb if draft.client_cert != draft.client_key => {
                    errors.push(issue(
                        "client_key",
                        "MongoDB 证书和私钥须指向同一合并 PEM 文件",
                    ));
                }
                _ => (),
            }
        }
        if draft.engine == Engine::Sqlite && !absolute_path(&draft.file_path) {
            errors.push(issue("file_path", "请定位 SQLite 数据库的绝对路径"));
        }
        errors
    }
}

fn issue(field: &str, message: &str) -> FieldError {
    FieldError {
        field: field.to_owned(),
        message: message.to_owned(),
    }
}

fn absolute_path(path: &str) -> bool {
    // Never resolve a source-relative name against RillSQL's current directory.
    std::path::Path::new(path).is_absolute() && !path.contains("${")
}

fn blank(engine: Engine, name: &str) -> ConnectionDraft {
    let meta = adapter(engine).meta();
    ConnectionDraft {
        id: None,
        name: name.to_owned(),
        engine,
        env: ENV_NONE.into(),
        host: String::new(),
        port: meta
            .default_port()
            .map(|port| port.to_string())
            .unwrap_or_default(),
        database: String::new(),
        username: String::new(),
        file_path: String::new(),
        replica_set: String::new(),
        tls_mode: if meta.defaults_to_local() || meta.file_based() {
            TlsMode::Off
        } else {
            TlsMode::Verify
        },
        ca_mode: CaMode::System,
        ca_file: String::new(),
        client_cert: String::new(),
        client_key: String::new(),
        ssh_on: false,
        ssh_host: String::new(),
        ssh_port: "22".into(),
        ssh_user: String::new(),
        ssh_auth: SshAuthKind::Password,
        ssh_key: String::new(),
        save_password: false,
    }
}

/// Strings that can safely be shown as individual fields, never raw connection strings.
fn plain(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    !value.chars().any(char::is_control)
        && !value.contains("://")
        && !lower.contains("jdbc:")
        && !value.contains("${")
        && !value
            .split_once('@')
            .is_some_and(|(user_info, _)| user_info.contains(':'))
        && !lower.contains("password=")
        && !lower.contains("pwd=")
        && !lower.contains("passphrase=")
}

fn valid_host(host: &str) -> bool {
    host.parse::<std::net::Ipv6Addr>().is_ok()
        || (!host.is_empty()
            && host
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_')))
}

#[cfg(test)]
mod tests;
