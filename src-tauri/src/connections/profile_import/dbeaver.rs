use serde_json::{Map, Value};

use super::jdbc::{apply_url, property, Properties};
use super::{blank, plain, valid_host, ProfileRow};
use crate::connections::model::{
    parse_port, CaMode, ConnectionDraft, Engine, SshAuthKind, TlsMode,
};

type Object = Map<String, Value>;

pub(crate) fn parse_dbeaver(bytes: &[u8]) -> Result<Vec<ProfileRow>, String> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let root: Value =
        serde_json::from_slice(bytes).map_err(|_| "DBeaver 文件不是有效的 UTF-8 JSON")?;
    let connections = root
        .get("connections")
        .and_then(Value::as_object)
        .ok_or("文件缺少 DBeaver connections 对象")?;
    let types =
        optional_map(&root, "connection-types").map_err(|_| "DBeaver connection-types 格式无效")?;
    Ok(connections
        .values()
        .enumerate()
        .map(|(i, connection)| row(connection, i + 1, types))
        .collect())
}

fn row(entry: &Value, number: usize, types: Option<&Object>) -> ProfileRow {
    let name = string(entry, "name");
    let name = if name.is_empty() || !plain(name) {
        format!("连接 {number}")
    } else {
        name.to_owned()
    };
    let mut row = ProfileRow {
        name,
        draft: None,
        blockers: vec![],
        notes: vec![],
        required_tls: None,
    };
    if !entry.is_object() {
        row.block("连接条目格式无效");
        return row;
    }
    let Some(engine) = engine(string(entry, "provider"), string(entry, "driver")) else {
        let driver = string(entry, "driver");
        if !driver.is_empty()
            && driver.len() <= 80
            && driver
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'-' | b'.'))
        {
            row.block(&format!("不支持的来源驱动 {driver}"));
        } else {
            row.block("不支持此来源驱动或提供程序");
        }
        return row;
    };
    let config = &entry["configuration"];
    if !config.is_object() {
        row.block("缺少有效的 configuration 对象");
        return row;
    }
    let mut draft = blank(engine, &row.name);
    let mut props = Properties::new();
    let type_id = config.get("type").and_then(Value::as_str).unwrap_or("dev");
    let type_def = types.and_then(|types| types.get(type_id));
    if config.get("type").is_some_and(|v| !v.is_string())
        || type_def.is_some_and(|v| !v.is_object())
    {
        row.block("来源连接类型格式无效");
    }
    for settings in [Some(entry), Some(config), type_def].into_iter().flatten() {
        if enabled(&settings["read-only"]) != Some(false) && settings.get("read-only").is_some() {
            row.block("来源要求只读或只读字段无效，无法保证禁止写入");
        }
        if active(&settings["security"]) {
            match settings["security"].as_object() {
                Some(security)
                    if security.iter().all(|(k, v)| {
                        k == "permission-restrictions" && v.as_array().is_some_and(Vec::is_empty)
                    }) =>
                {
                    ()
                }
                _ => row.block("来源 security 权限限制无法还原"),
            }
        }
    }
    draft.env = match type_id {
        "prod" | "production" => "prod",
        "dev" => "dev",
        "test" => "test",
        _ if type_def.is_some_and(|definition| {
            string(definition, "name").eq_ignore_ascii_case("Production")
        }) =>
        {
            "prod"
        }
        _ => {
            row.block("无法确定自定义连接类型的环境与限制");
            "none"
        }
    }
    .to_owned();
    if ["config-profile", "config-profile-source", "auth-profile"]
        .iter()
        .any(|k| active(&config[k]))
    {
        row.block("引用的外部网络或认证配置无法还原");
    }
    if active(&entry["lockPassword"]) {
        row.block("无法保留来源的配置编辑锁");
    }
    if !matches!(string(config, "auth-model"), "" | "native") {
        row.block("来源认证方式无法通过用户名和密码还原");
    }
    if active(&config["bootstrap"]) {
        row.block("无法还原 bootstrap 的启动会话设置或目标库");
    }
    if active(&config["events"]) || active(&entry["events"]) {
        row.block("无法还原来源连接事件对会话的影响");
    }
    if let Err(reason) = read_properties(config, &mut props) {
        row.block(reason);
    }
    let user = [
        string(config, "user"),
        string(&config["auth-properties"], "userName"),
        string(&config["auth-properties"], "user"),
        string(entry, "user"),
    ]
    .into_iter()
    .find(|u| !u.is_empty())
    .unwrap_or("");
    draft.username = safe_field(user).to_owned();
    match config.get("url") {
        Some(Value::String(url)) if !url.trim().is_empty() => {
            let mut target = draft.clone();
            match apply_url(url.trim(), &mut target, &mut props) {
                Ok(()) if plain(&target.database) && plain(&target.username) => draft = target,
                Ok(()) => row.block("URL 中的目标字段无法安全显示"),
                Err(reason) => row.block(reason),
            }
        }
        Some(v) if !v.is_string() && !v.is_null() => row.block("URL 字段格式错误"),
        _ if string(config, "configurationType") == "URL" => {
            row.block("URL 模式缺少 URL，无法确定实际目标")
        }
        _ if engine == Engine::Sqlite => {
            let path = string(config, "database");
            if path.starts_with("file:") || path == ":memory:" || path.contains('?') {
                row.block("无法等价还原 SQLite 文件 URI 的访问模式或内存数据库");
            } else {
                draft.file_path = safe_field(path).to_owned();
            }
        }
        _ => {
            let host = string(config, "host");
            if valid_host(host) {
                draft.host = host.to_owned();
            }
            if let Some(port) = config.get("port").filter(|v| !v.is_null()) {
                draft.port = port_text(port);
            }
            let database = string(config, "database");
            if !plain(database)
                || config
                    .get("database")
                    .is_some_and(|v| !v.is_string() && !v.is_null())
            {
                row.block("无法安全还原目标数据库字段");
            } else {
                draft.database = database.to_owned();
            }
            if active(&config["server"]) {
                row.block("无法还原 server 指定的逻辑服务器");
            }
        }
    }
    if let Err(reason) = handlers(config, &mut draft, &mut props) {
        row.block(reason);
    }
    match driver_options(&props, &mut draft, &mut row.notes) {
        Ok(mode) => row.required_tls = mode,
        Err(reason) => row.block(reason),
    }
    if engine == Engine::Postgres
        && matches!(row.required_tls, Some(TlsMode::Require | TlsMode::Verify))
    {
        row.block(
            "来源要求 PostgreSQL TLS 加密或证书校验，当前驱动在服务器拒绝 TLS 时可能回退到明文",
        );
    }
    if draft.engine == Engine::Sqlite && draft.ssh_on {
        row.block("SQLite 文件连接无法应用 SSH 跳板");
    }
    if draft.ssh_on
        && draft.tls_mode == TlsMode::Verify
        && matches!(engine, Engine::Sqlserver | Engine::Redis | Engine::Mongodb)
    {
        row.block("此引擎经 SSH 转发时不能保证按原主机校验证书");
    }
    row.draft = Some(draft);
    row
}

fn engine(provider: &str, driver: &str) -> Option<Engine> {
    match (provider, driver.to_ascii_lowercase().as_str()) {
        ("postgresql", "postgres-jdbc" | "postgresql") => Some(Engine::Postgres),
        ("mysql", "mysql8" | "mysql" | "mysql5") => Some(Engine::Mysql),
        ("mysql" | "mariadb", "maria_db" | "mariadb") => Some(Engine::Mariadb),
        ("generic" | "sqlite", "sqlite_jdbc" | "sqlite") => Some(Engine::Sqlite),
        ("sqlserver", "microsoft" | "mssql_jdbc_ms" | "mssql_jdbc_ms_new" | "sqlserver") => {
            Some(Engine::Sqlserver)
        }
        ("redis", "redis") => Some(Engine::Redis),
        ("mongodb", "mongo" | "mongodb") => Some(Engine::Mongodb),
        _ => None,
    }
}

fn optional_map<'a>(v: &'a Value, key: &str) -> Result<Option<&'a Object>, &'static str> {
    v.get(key)
        .map(|v| v.as_object().ok_or("配置应为对象"))
        .transpose()
}

fn string<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
}

fn safe_field(value: &str) -> &str {
    if plain(value) {
        value
    } else {
        ""
    }
}

fn enabled(v: &Value) -> Option<bool> {
    v.as_bool()
        .or_else(|| v.as_str().and_then(|s| s.to_ascii_lowercase().parse().ok()))
}

fn active(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.trim().is_empty(),
        Value::Object(map) => !map.is_empty(),
        Value::Array(arr) => !arr.is_empty(),
        Value::Number(_) => true,
    }
}

fn port_text(value: &Value) -> String {
    let port = match value {
        Value::String(s) => parse_port(s),
        Value::Number(n) => n
            .as_f64()
            .filter(|port| port.fract() == 0.0 && (1.0..=f64::from(u16::MAX)).contains(port))
            .map(|port| port as u16),
        _ => None,
    };
    port.map(|p| p.to_string()).unwrap_or_default()
}

fn read_properties(config: &Value, props: &mut Properties) -> Result<(), &'static str> {
    // DBeaver persists navigator visibility in each edited connection.
    if optional_map(config, "provider-properties")?
        .into_iter()
        .flatten()
        .any(|(key, value)| {
            !matches!(
                key.as_str(),
                "@dbeaver-show-non-default-db@"
                    | "@dbeaver-show-all-dbs@"
                    | "@dbeaver-show-unavailable-db@"
                    | "@dbeaver-show-template-db@"
            ) || enabled(value).is_none()
        })
    {
        return Err("暂不支持此提供程序的专用属性");
    }
    for (key, value) in optional_map(config, "properties")?.into_iter().flatten() {
        let val = match value {
            Value::String(text) => text.to_owned(),
            Value::Bool(_) | Value::Number(_) => value.to_string(),
            _ => return Err("驱动属性格式无效"),
        };
        property(props, key, &val)?;
    }
    Ok(())
}

fn handlers(
    config: &Value,
    draft: &mut ConnectionDraft,
    props: &mut Properties,
) -> Result<(), &'static str> {
    for (id, handler) in optional_map(config, "handlers")?.into_iter().flatten() {
        if !handler.is_object() {
            return Err("网络处理器格式无效");
        }
        match handler.get("enabled") {
            None => continue,
            Some(v) if enabled(v) == Some(false) => continue,
            Some(v) if enabled(v) == Some(true) => (),
            _ => return Err("网络处理器的启用状态无效"),
        }
        if draft.engine == Engine::Sqlite {
            return Err("SQLite 文件连接无法应用来源网络处理器");
        }
        match id.as_str() {
            "ssh_tunnel" => ssh(handler, draft)?,
            "postgre_ssl" if draft.engine == Engine::Postgres => ssl_handler(handler, props)?,
            "mysql_ssl" if matches!(draft.engine, Engine::Mysql | Engine::Mariadb) => {
                ssl_handler(handler, props)?
            }
            "mongo_ssl" if draft.engine == Engine::Mongodb => ssl_handler(handler, props)?,
            _ => return Err("启用的网络处理器无法还原，不能安全建立连接"),
        }
    }
    Ok(())
}

fn ssh(handler: &Value, draft: &mut ConnectionDraft) -> Result<(), &'static str> {
    draft.ssh_on = true;
    let settings = &handler["properties"];
    if !settings.is_null() && !settings.is_object() {
        return Err("SSH 属性格式无效");
    }
    draft.ssh_host = string(settings, "host").to_owned();
    if !valid_host(&draft.ssh_host) {
        draft.ssh_host.clear();
    }
    if let Some(p) = settings.get("port") {
        draft.ssh_port = port_text(p);
    }
    let user = [
        string(handler, "user"),
        string(settings, "userName"),
        string(settings, "user"),
        string(settings, "username"),
    ]
    .into_iter()
    .find(|u| !u.is_empty())
    .unwrap_or("");
    draft.ssh_user = safe_field(user).to_owned();
    draft.ssh_auth = match string(settings, "authType") {
        "PUBLIC_KEY" => SshAuthKind::Key,
        "PASSWORD" => SshAuthKind::Password,
        _ => return Err("SSH 认证方式缺失或不受支持"),
    };
    draft.ssh_key = safe_field(string(settings, "keyPath")).to_owned();
    for (key, val) in settings.as_object().into_iter().flatten() {
        match key.as_str() {
            "host" | "port" | "user" | "userName" | "username" | "password" | "passphrase"
            | "authType" | "keyPath" | "implementation" | "aliveInterval" | "sshConnectTimeout"
            | "shareTunnels" => (),
            "bypassHostVerification" if enabled(val) == Some(false) => (),
            "jumpServer.count" if val.as_f64() == Some(0.0) || val.as_str() == Some("0") => (),
            _ if !active(val) => (),
            _ => return Err("SSH 额外跳板、转发或主机校验设置无法等价还原"),
        }
    }
    Ok(())
}

fn ssl_handler(handler: &Value, props: &mut Properties) -> Result<(), &'static str> {
    let settings = optional_map(handler, "properties")?.ok_or("SSL 处理器缺少校验配置")?;
    for (key, val) in settings {
        match key.as_str() {
            "ssl.method" if val.as_str() == Some("CERTIFICATES") => (),
            "ssl.sslMode" => property(props, "sslmode", val.as_str().ok_or("SSL 模式无效")?)?,
            "ssl.ca.cert" | "ssl.client.cert" | "ssl.client.key" => {
                property(props, key, val.as_str().ok_or("SSL 证书路径无效")?)?;
            }
            "ssl.client.key.password" => (),
            _ if !active(val) => (),
            _ => return Err("无法还原 SSL 处理器的证书材料或校验选项"),
        }
    }
    if !settings.contains_key("ssl.sslMode") {
        return Err("启用的 SSL 处理器未声明可识别的校验模式");
    }
    Ok(())
}

fn driver_options(
    props: &Properties,
    draft: &mut ConnectionDraft,
    notes: &mut Vec<String>,
) -> Result<Option<TlsMode>, &'static str> {
    let mut tls: Option<TlsMode> = None;
    let mut mode = |m| {
        if tls.replace(m).is_some_and(|previous| previous != m) {
            Err("来源 TLS 参数互相冲突")
        } else {
            Ok(())
        }
    };
    for (key, value) in props {
        match key.as_str() {
            "user" if !value.is_empty() && plain(value) => draft.username = value.clone(),
            "databasename" if draft.engine == Engine::Sqlserver && plain(value) => {
                draft.database = value.clone()
            }
            "replicaset" if draft.engine == Engine::Mongodb && plain(value) => {
                draft.replica_set = value.clone()
            }
            "read_only" | "readonly" if value.eq_ignore_ascii_case("true") => {
                return Err("来源驱动要求只读，无法保证禁止写入")
            }
            "read_only" | "readonly" if value.eq_ignore_ascii_case("false") => (),
            "usessl"
                if matches!(draft.engine, Engine::Mysql | Engine::Mariadb)
                    && value.eq_ignore_ascii_case("false") =>
            {
                mode(TlsMode::Off)?
            }
            "sslmode" if draft.engine == Engine::Postgres => {
                mode(match value.to_ascii_lowercase().as_str() {
                    "disable" => TlsMode::Off,
                    "require" => TlsMode::Require,
                    "verify-ca" | "verify-full" => TlsMode::Verify,
                    _ => return Err("无法等价还原 PostgreSQL sslmode 的校验或回退行为"),
                })?
            }
            "sslmode" | "ssl-mode"
                if matches!(
                    draft.engine,
                    Engine::Mysql | Engine::Mariadb | Engine::Mongodb
                ) =>
            {
                mode(match value.to_ascii_lowercase().as_str() {
                    "disable" | "disabled" => TlsMode::Off,
                    "require" | "required" | "trust" => TlsMode::Require,
                    "verify-full" | "verify_identity" => TlsMode::Verify,
                    _ => return Err("无法等价还原来源 sslMode 的校验或回退行为"),
                })?
            }
            "ssl" | "tls"
                if matches!(
                    draft.engine,
                    Engine::Postgres | Engine::Redis | Engine::Mongodb
                ) =>
            {
                mode(match value.to_ascii_lowercase().as_str() {
                    "true" => TlsMode::Verify,
                    "false" if draft.engine != Engine::Postgres => TlsMode::Off,
                    _ => return Err("无法等价还原来源 ssl/tls 参数"),
                })?
            }
            "encrypt" | "trustservercertificate" if draft.engine == Engine::Sqlserver => {
                let encrypted = props.get("encrypt").map(String::as_str).unwrap_or("true");
                let trust = props
                    .get("trustservercertificate")
                    .map(String::as_str)
                    .unwrap_or("false");
                mode(
                    match (
                        encrypted.to_ascii_lowercase().as_str(),
                        trust.to_ascii_lowercase().as_str(),
                    ) {
                        ("false", "false" | "true") => TlsMode::Off,
                        ("true", "true") => TlsMode::Require,
                        ("true", "false") => TlsMode::Verify,
                        _ => return Err("无法还原 SQL Server 加密模式"),
                    },
                )?;
            }
            "ssl.ca.cert" | "sslrootcert" if !value.is_empty() => {
                draft.ca_mode = CaMode::File;
                draft.ca_file = safe_field(value).to_owned();
            }
            "ssl.client.cert" | "sslcert" if !value.is_empty() && plain(value) => {
                draft.client_cert = value.clone()
            }
            "ssl.client.key" | "sslkey" if !value.is_empty() && plain(value) => {
                draft.client_key = value.clone()
            }
            "connecttimeout" | "logintimeout" | "sockettimeout" | "applicationname" => (),
            _ => return Err("URL 或驱动含未映射的连接属性，无法确定有效访问条件"),
        }
    }
    if let Some(mode) = tls {
        draft.tls_mode = mode;
    } else if draft.engine == Engine::Postgres {
        notes.push(
            "来源未要求 PostgreSQL TLS。启用 TLS 时，当前驱动在服务器拒绝 TLS 时可能回退到明文"
                .into(),
        );
    } else if draft.engine != Engine::Sqlite {
        notes.push("来源未显式声明 TLS 模式，预览显示 RillSQL 将采用的设置".into());
    }
    if draft.ca_mode == CaMode::File && draft.tls_mode != TlsMode::Verify {
        return Err("来源指定了 CA 文件，但 TLS 模式不能保证验证该证书");
    }
    if (!draft.client_cert.is_empty() || !draft.client_key.is_empty())
        && (matches!(draft.engine, Engine::Sqlserver | Engine::Sqlite)
            || draft.tls_mode == TlsMode::Off
            || (draft.engine == Engine::Mongodb && draft.tls_mode != TlsMode::Verify))
    {
        return Err("此引擎或 TLS 模式无法执行来源客户端证书认证");
    }
    Ok(
        if draft.ca_mode == CaMode::File
            || !draft.client_cert.is_empty()
            || !draft.client_key.is_empty()
        {
            Some(draft.tls_mode)
        } else {
            tls
        },
    )
}
