use std::net::{IpAddr, Ipv4Addr};

use crate::connections::model::{Engine, SavedConnection, TlsMode};

/// 传输地址和 TLS 身份分开。回环地址只表示转发器接入点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub transport_host: String,
    pub transport_port: u16,
    pub tls_server_name: Option<String>,
    pub tls_mode: TlsMode,
    pub channel_target_host: Option<String>,
    pub channel_target_port: Option<u16>,
    pub bastion_host: Option<String>,
    pub bastion_port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MysqlTlsPlan {
    pub tcp_host: String,
    pub tcp_port: u16,
    pub ssl_opts_present: bool,
    pub hostname_override: Option<String>,
    pub skip_domain_validation: bool,
    pub accept_invalid_certs: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresTlsPlan {
    pub host: String,
    pub hostaddr: Option<IpAddr>,
    pub port: u16,
    pub use_tls: bool,
    pub verify_hostname: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retry {
    Stop { reason: &'static str },
}

pub fn resolve_route(config: &SavedConnection, forwarder_port: Option<u16>) -> Result<Route, String> {
    if config.engine == Engine::Sqlite {
        if config.tls.is_some() || config.ssh.is_some() {
            return Err("SQLite 没有 TLS 或 SSH".to_owned());
        }
        return Ok(Route {
            transport_host: String::new(),
            transport_port: 0,
            tls_server_name: None,
            tls_mode: TlsMode::Off,
            channel_target_host: None,
            channel_target_port: None,
            bastion_host: None,
            bastion_port: None,
        });
    }
    let host = config.host.clone().ok_or("缺少数据库主机")?;
    let port = config.port.ok_or("缺少数据库端口")?;
    let tls = config.tls.as_ref().ok_or("远程连接缺少 TLS 设置")?;
    if tls.server_name != host {
        return Err("证书校验名必须是数据库主机名".to_owned());
    }
    if let Some(ssh) = &config.ssh {
        let transport_port = forwarder_port.unwrap_or(0);
        return Ok(Route {
            transport_host: Ipv4Addr::LOCALHOST.to_string(),
            transport_port,
            tls_server_name: if tls.mode == TlsMode::Off {
                None
            } else {
                Some(tls.server_name.clone())
            },
            tls_mode: tls.mode,
            channel_target_host: Some(host),
            channel_target_port: Some(port),
            bastion_host: Some(ssh.host.clone()),
            bastion_port: Some(ssh.port),
        });
    }
    Ok(Route {
        transport_host: host.clone(),
        transport_port: port,
        tls_server_name: if tls.mode == TlsMode::Off {
            None
        } else {
            Some(tls.server_name.clone())
        },
        tls_mode: tls.mode,
        channel_target_host: None,
        channel_target_port: None,
        bastion_host: None,
        bastion_port: None,
    })
}

/// 校验名只用数据库主机名，不用传输地址，也不在失败后改口。
pub fn server_name_for(mode: TlsMode, tls_server_name: &str, transport_host: &str) -> Result<String, String> {
    if mode == TlsMode::Off {
        return Err("未要求 TLS".to_owned());
    }
    if tls_server_name.is_empty() {
        return Err("缺少证书校验名".to_owned());
    }
    if tls_server_name != transport_host && transport_host == Ipv4Addr::LOCALHOST.to_string() {
        return Ok(tls_server_name.to_owned());
    }
    if tls_server_name == transport_host {
        return Ok(tls_server_name.to_owned());
    }
    Ok(tls_server_name.to_owned())
}

pub fn mysql_tls_plan(route: &Route) -> MysqlTlsPlan {
    let encrypt = route.tls_mode != TlsMode::Off;
    let verify = route.tls_mode == TlsMode::Verify;
    let override_name = if verify && route.transport_host != route.tls_server_name.clone().unwrap_or_default() {
        route.tls_server_name.clone()
    } else {
        None
    };
    MysqlTlsPlan {
        tcp_host: route.transport_host.clone(),
        tcp_port: route.transport_port,
        ssl_opts_present: encrypt,
        hostname_override: override_name,
        skip_domain_validation: route.tls_mode == TlsMode::Require,
        accept_invalid_certs: route.tls_mode == TlsMode::Require,
    }
}

pub fn postgres_tls_plan(route: &Route) -> Result<PostgresTlsPlan, String> {
    let tls_name = if route.tls_mode == TlsMode::Off {
        route.transport_host.clone()
    } else {
        server_name_for(route.tls_mode, route.tls_server_name.as_deref().unwrap_or(""), &route.transport_host)?
    };
    let hostaddr = if route.bastion_host.is_some() {
        Some(IpAddr::V4(Ipv4Addr::LOCALHOST))
    } else {
        None
    };
    Ok(PostgresTlsPlan {
        host: tls_name,
        hostaddr,
        port: route.transport_port,
        use_tls: route.tls_mode != TlsMode::Off,
        verify_hostname: route.tls_mode == TlsMode::Verify,
    })
}

pub fn on_tls_failure(_plan_mode: TlsMode) -> Retry {
    Retry::Stop {
        reason: "不会改用 localhost，也不会关闭主机名验证或退回明文后重试",
    }
}

#[cfg(test)]
pub fn channel_target(route: &Route) -> Option<(String, u16)> {
    Some((
        route.channel_target_host.clone()?,
        route.channel_target_port?,
    ))
}
