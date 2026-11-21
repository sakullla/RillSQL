use super::{plain, valid_host};
use crate::connections::model::{parse_port, ConnectionDraft, Engine};
use std::collections::BTreeMap;

pub(super) type Properties = BTreeMap<String, String>;

pub(super) fn property(props: &mut Properties, key: &str, value: &str) -> Result<(), &'static str> {
    let key = key.to_ascii_lowercase();
    if matches!(
        key.as_str(),
        "password" | "pwd" | "passphrase" | "sslpassword"
    ) {
        return Ok(());
    }
    if props
        .insert(key, value.to_owned())
        .is_some_and(|prev| prev != value)
    {
        return Err("驱动属性与 URL 参数互相冲突");
    }
    Ok(())
}

/// Parse only single-target URL forms. No password or raw URL leaves this function.
pub(super) fn apply_url(
    url: &str,
    draft: &mut ConnectionDraft,
    props: &mut Properties,
) -> Result<(), &'static str> {
    if url.chars().any(char::is_control) || url.contains("${") || url.contains('#') {
        return Err("URL 包含无法还原的变量、片段或控制字符");
    }
    let body = url.strip_prefix("jdbc:").unwrap_or(url);
    let (scheme, address) = body.split_once(':').ok_or("无法识别 JDBC URL 结构")?;
    let scheme_engine = match scheme {
        "postgresql" => Engine::Postgres,
        "mysql" => Engine::Mysql,
        "mariadb" => Engine::Mariadb,
        "sqlite" => Engine::Sqlite,
        "sqlserver" => Engine::Sqlserver,
        "redis" | "rediss" => Engine::Redis,
        "mongodb" => Engine::Mongodb,
        _ => return Err("暂不支持此 URL 协议或目标发现方式"),
    };
    if draft.engine != scheme_engine {
        return Err("URL 协议和来源驱动不一致");
    }
    if draft.engine == Engine::Sqlite {
        if address.starts_with("file:")
            || address == ":memory:"
            || address.is_empty()
            || address.contains(['?', ';'])
        {
            return Err("暂不支持 SQLite URI 参数或内存数据库");
        }
        if !plain(address) {
            return Err("无法安全识别 SQLite URL 的文件路径");
        }
        draft.file_path = address.to_owned();
        return Ok(());
    }
    let body = address.strip_prefix("//").ok_or("URL 缺少明确的主机")?;
    let sqlserver = draft.engine == Engine::Sqlserver;
    let (target, parameters) = if sqlserver {
        if body.contains(['?', '{', '}']) {
            return Err("暂不支持此 SQL Server URL 参数写法");
        }
        body.split_once(';').unwrap_or((body, ""))
    } else {
        if body.contains(';') {
            return Err("暂不支持此 URL 参数分隔符");
        }
        body.split_once('?').unwrap_or((body, ""))
    };
    let (authority, path) = target.split_once('/').unwrap_or((target, ""));
    if path.contains('/') || (sqlserver && !path.is_empty()) {
        return Err("无法还原 URL 中的数据库路径");
    }
    let (credentials, host_port) = authority.rsplit_once('@').unwrap_or(("", authority));
    if !credentials.is_empty() {
        let user = credentials.split_once(':').map_or(credentials, |(u, _)| u);
        draft.username = decode(user, false)?;
    }
    let (host, port) = if let Some(ipv6) = host_port.strip_prefix('[') {
        let (host, suffix) = ipv6.split_once(']').ok_or("URL 的 IPv6 主机格式错误")?;
        (
            host,
            if suffix.is_empty() {
                None
            } else {
                Some(suffix.strip_prefix(':').ok_or("URL 端口格式错误")?)
            },
        )
    } else {
        match host_port.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (host_port, None),
        }
    };
    if !valid_host(host) {
        return Err("URL 包含多个主机或无法还原的目标");
    }
    if let Some(port) = port {
        draft.port = parse_port(port)
            .ok_or("URL 端口超出范围或格式错误")?
            .to_string();
    }
    draft.host = host.to_owned();
    // pgJDBC uses Java URLDecoder for database names and query values.
    let form_encoded = draft.engine == Engine::Postgres;
    draft.database = decode(path, form_encoded)?;
    if draft.database.trim() != draft.database {
        return Err("URL 数据库名含首尾空白，保存时无法保留原名");
    }
    if scheme == "rediss" {
        property(props, "tls", "true")?;
    }
    for pair in parameters
        .split(if sqlserver { ';' } else { '&' })
        .filter(|p| !p.is_empty())
    {
        let (key, value) = pair.split_once('=').ok_or("URL 参数缺少值")?;
        let key = if form_encoded {
            key.to_owned()
        } else {
            decode(key, false)?
        };
        if matches!(
            key.to_ascii_lowercase().as_str(),
            "password" | "pwd" | "passphrase" | "sslpassword"
        ) {
            continue;
        }
        property(props, &key, &decode(value, form_encoded)?)?;
    }
    Ok(())
}

fn decode(text: &str, form_encoded: bool) -> Result<String, &'static str> {
    let mut bytes = text.as_bytes().iter().copied();
    let mut decoded = Vec::with_capacity(text.len());
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let mut digit = || {
                bytes
                    .next()
                    .and_then(|ch| char::from(ch).to_digit(16))
                    .ok_or("URL 百分号编码无效")
            };
            decoded.push((digit()? * 16 + digit()?) as u8);
        } else if form_encoded && byte == b'+' {
            decoded.push(b' ');
        } else {
            decoded.push(byte);
        }
    }
    String::from_utf8(decoded).map_err(|_| "URL 字段编码无效")
}
