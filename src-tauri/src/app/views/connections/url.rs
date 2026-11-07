//! 连接串解析。语义对齐现有 `src/connections/url.ts`，失败原因用既有字典键。

use crate::connections::model::{Engine, TlsMode};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedUrl {
    pub engine: Engine,
    pub host: String,
    pub port: String,
    pub database: String,
    pub username: String,
    pub password: String,
    pub tls_mode: Option<TlsMode>,
    pub file_path: Option<String>,
    pub original_host: Option<String>,
}

pub fn parse_connection_url(text: &str) -> Result<ParsedUrl, &'static str> {
    let trimmed = text.trim();
    let Some((scheme, rest)) = split_scheme(trimmed) else {
        return Err("connections.url.unsupported");
    };
    let engine = engine_of(scheme);
    if engine == Engine::Sqlite {
        return parse_sqlite(rest);
    }
    if !rest.starts_with("//") {
        return Err("connections.url.parseFailed");
    }
    let normalized = if engine == Engine::Sqlserver {
        normalize_sqlserver(rest)
    } else {
        rest.to_owned()
    };
    let (body, query) = normalized[2..].split_once('?').unwrap_or((&normalized[2..], ""));
    let (authority, path) = split_authority_path(body);
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((user, host)) => (user, host),
        None => ("", authority),
    };
    let (username, password) = split_userinfo(userinfo);
    let (host, port) = split_host_port(hostport)?;
    if host.is_empty() {
        return Err("connections.url.noHost");
    }
    let pairs = query_pairs(query);
    let mut database = path.trim_matches('/').to_owned();
    if engine == Engine::Sqlserver {
        if let Some(name) = param(&pairs, "databasename") {
            if !name.trim().is_empty() {
                database = name.trim().to_owned();
            }
        }
    }
    let username = if username.is_empty() && engine == Engine::Sqlserver {
        param(&pairs, "user").unwrap_or("").to_owned()
    } else {
        username
    };
    let password = if password.is_empty() && engine == Engine::Sqlserver {
        param(&pairs, "password").unwrap_or("").to_owned()
    } else {
        password
    };
    let default_port = default_port(engine);
    let port = if scheme == "mongodb+srv" {
        String::new()
    } else if port.is_empty() {
        default_port.to_owned()
    } else {
        port
    };
    let tls_mode = if scheme == "rediss" || scheme == "mongodb+srv" {
        Some(TlsMode::Require)
    } else {
        tls_from(engine, &pairs)
    };
    if engine == Engine::Mongodb {
        return Ok(ParsedUrl {
            engine,
            host: trimmed.to_owned(),
            port,
            database,
            username,
            password,
            tls_mode,
            file_path: None,
            original_host: Some(host),
        });
    }
    Ok(ParsedUrl {
        engine,
        host,
        port,
        database,
        username,
        password,
        tls_mode,
        file_path: None,
        original_host: None,
    })
}

fn parse_sqlite(rest: &str) -> Result<ParsedUrl, &'static str> {
    let mut path = rest.trim_start_matches("//").to_owned();
    if let Some(query) = path.find('?') {
        path.truncate(query);
    }
    path = decode_loose(path.trim());
    if path.starts_with('/') && path.as_bytes().get(2) == Some(&b':') && path.as_bytes().get(1).is_some_and(u8::is_ascii_alphabetic) {
        path.remove(0);
    }
    if path.is_empty() {
        return Err("connections.url.noHost");
    }
    Ok(ParsedUrl {
        engine: Engine::Sqlite,
        host: String::new(),
        port: String::new(),
        database: String::new(),
        username: String::new(),
        password: String::new(),
        tls_mode: None,
        file_path: Some(path),
        original_host: None,
    })
}

fn split_scheme(text: &str) -> Option<(&str, &str)> {
    let lower = text.to_ascii_lowercase();
    let (body, lower_body) = if let Some(stripped) = lower.strip_prefix("jdbc:") {
        (&text[5..], stripped)
    } else {
        (text, lower.as_str())
    };
    const SCHEMES: &[&str] = &[
        "mongodb+srv",
        "postgresql",
        "postgres",
        "mariadb",
        "sqlserver",
        "mongodb",
        "sqlite",
        "rediss",
        "mysql",
        "mssql",
        "redis",
    ];
    for scheme in SCHEMES {
        let prefix = format!("{scheme}:");
        if lower_body.starts_with(&prefix) {
            return Some((*scheme, &body[prefix.len()..]));
        }
    }
    None
}

fn engine_of(scheme: &str) -> Engine {
    match scheme {
        "mysql" => Engine::Mysql,
        "mariadb" => Engine::Mariadb,
        "sqlserver" | "mssql" => Engine::Sqlserver,
        "sqlite" => Engine::Sqlite,
        "redis" | "rediss" => Engine::Redis,
        "mongodb" | "mongodb+srv" => Engine::Mongodb,
        _ => Engine::Postgres,
    }
}

pub fn default_port(engine: Engine) -> &'static str {
    match engine {
        Engine::Mysql | Engine::Mariadb => "3306",
        Engine::Postgres => "5432",
        Engine::Sqlserver => "1433",
        Engine::Redis => "6379",
        Engine::Mongodb => "27017",
        Engine::Sqlite => "",
    }
}

fn normalize_sqlserver(rest: &str) -> String {
    match rest.find(';') {
        Some(at) => {
            let (head, tail) = rest.split_at(at);
            format!("{head}?{}", tail[1..].replace(';', "&"))
        }
        None => rest.to_owned(),
    }
}

fn split_authority_path(body: &str) -> (&str, &str) {
    if let Some(open) = body.find('[') {
        if let Some(rel) = body[open..].find(']') {
            let end = open + rel;
            if let Some(slash) = body[end..].find('/') {
                let at = end + slash;
                return (&body[..at], &body[at + 1..]);
            }
            return (body, "");
        }
    }
    match body.find('/') {
        Some(at) => (&body[..at], &body[at + 1..]),
        None => (body, ""),
    }
}

fn split_userinfo(userinfo: &str) -> (String, String) {
    if userinfo.is_empty() {
        return (String::new(), String::new());
    }
    match userinfo.split_once(':') {
        Some((user, pass)) => (decode_loose(user), decode_loose(pass)),
        None => (decode_loose(userinfo), String::new()),
    }
}

fn split_host_port(hostport: &str) -> Result<(String, String), &'static str> {
    if let Some(rest) = hostport.strip_prefix('[') {
        let Some(end) = rest.find(']') else {
            return Err("connections.url.parseFailed");
        };
        let host = rest[..end].to_owned();
        let after = &rest[end + 1..];
        if after.is_empty() {
            return Ok((host, String::new()));
        }
        let Some(port) = after.strip_prefix(':') else {
            return Err("connections.url.parseFailed");
        };
        if !port.is_empty() && !port.bytes().all(|b| b.is_ascii_digit()) {
            return Err("connections.url.parseFailed");
        }
        return Ok((host, port.to_owned()));
    }
    if let Some((host, port)) = hostport.rsplit_once(':') {
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return Err("connections.url.parseFailed");
        }
        return Ok((decode_loose(host), port.to_owned()));
    }
    Ok((decode_loose(hostport), String::new()))
}

fn query_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| match part.split_once('=') {
            Some((key, value)) => (decode_loose(key), decode_loose(value)),
            None => (decode_loose(part), String::new()),
        })
        .collect()
}

fn param<'a>(pairs: &'a [(String, String)], wanted: &str) -> Option<&'a str> {
    pairs.iter().find(|(key, _)| key.eq_ignore_ascii_case(wanted)).map(|(_, value)| value.as_str())
}

fn tls_from(engine: Engine, pairs: &[(String, String)]) -> Option<TlsMode> {
    if let Some(encrypt) = param(pairs, "encrypt").map(|value| value.to_ascii_lowercase()) {
        if encrypt == "false" || encrypt == "no" {
            return Some(TlsMode::Off);
        }
        let trust = param(pairs, "trustservercertificate").unwrap_or("").to_ascii_lowercase();
        if trust == "true" || trust == "yes" {
            return Some(TlsMode::Require);
        }
        if matches!(encrypt.as_str(), "true" | "yes" | "strict" | "mandatory") {
            return Some(TlsMode::Verify);
        }
    }
    if let Some(sslmode) = param(pairs, "sslmode").map(|value| value.to_ascii_lowercase()) {
        if sslmode == "disable" {
            return Some(TlsMode::Off);
        }
        if matches!(sslmode.as_str(), "require" | "prefer" | "allow") {
            return Some(TlsMode::Require);
        }
        if sslmode == "verify-full" || sslmode == "verify-ca" {
            return Some(TlsMode::Verify);
        }
    }
    if matches!(engine, Engine::Mysql | Engine::Mariadb) {
        if let Some(mode) = param(pairs, "ssl-mode").or_else(|| param(pairs, "sslmode")).map(|value| value.to_ascii_lowercase()) {
            if mode == "disabled" || mode == "disable" {
                return Some(TlsMode::Off);
            }
            if mode == "verify_ca" || mode == "verify_identity" {
                return Some(TlsMode::Verify);
            }
            if mode == "required" || mode == "preferred" {
                return Some(TlsMode::Require);
            }
        }
        if let Some(flag) = param(pairs, "usessl").map(|value| value.to_ascii_lowercase()) {
            return Some(if flag == "false" || flag == "no" { TlsMode::Off } else { TlsMode::Require });
        }
    }
    if matches!(engine, Engine::Mongodb | Engine::Redis) {
        if let Some(flag) = param(pairs, "tls").or_else(|| param(pairs, "ssl")).map(|value| value.to_ascii_lowercase()) {
            return Some(if matches!(flag.as_str(), "true" | "yes" | "1") { TlsMode::Require } else { TlsMode::Off });
        }
    }
    None
}

fn decode_loose(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return value.to_owned();
            }
            let hex = &value[index + 1..index + 3];
            match u8::from_str_radix(hex, 16) {
                Ok(byte) => out.push(byte),
                Err(_) => return value.to_owned(),
            }
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_seven_engine_url_shapes() {
        let pg = parse_connection_url("postgres://app_user:secret@db.example.internal:5432/appdb").unwrap();
        assert_eq!(pg.engine, Engine::Postgres);
        assert_eq!(pg.host, "db.example.internal");
        assert_eq!(pg.port, "5432");
        assert_eq!(pg.database, "appdb");
        assert_eq!(pg.username, "app_user");
        assert_eq!(pg.password, "secret");
        assert_eq!(pg.tls_mode, None);

        let mysql = parse_connection_url("MYSQL://root@127.0.0.1:3307/inventory").unwrap();
        assert_eq!(mysql.engine, Engine::Mysql);
        assert_eq!(mysql.port, "3307");
        assert_eq!(parse_connection_url("mariadb://u@h/db").unwrap().port, "3306");
        assert_eq!(parse_connection_url("postgres://u@h/db").unwrap().port, "5432");
        assert_eq!(parse_connection_url("postgres://u@[::1]/db").unwrap().host, "::1");

        let decoded = parse_connection_url("postgres://u:p%40ss%21@h/db").unwrap();
        assert_eq!(decoded.password, "p@ss!");
        assert_eq!(parse_connection_url("postgres://u:pa%zz@h/db").unwrap().password, "pa%zz");

        assert_eq!(parse_connection_url("postgres://u@h/db?sslmode=disable").unwrap().tls_mode, Some(TlsMode::Off));
        assert_eq!(parse_connection_url("postgres://u@h/db?sslmode=prefer").unwrap().tls_mode, Some(TlsMode::Require));
        assert_eq!(parse_connection_url("postgres://u@h/db?sslmode=verify-full").unwrap().tls_mode, Some(TlsMode::Verify));
        assert_eq!(parse_connection_url("mysql://u@h/db?ssl-mode=REQUIRED").unwrap().tls_mode, Some(TlsMode::Require));
        assert_eq!(parse_connection_url("mysql://u@h/db?sslMode=VERIFY_IDENTITY").unwrap().tls_mode, Some(TlsMode::Verify));
        assert_eq!(parse_connection_url("mariadb://u@h/db?useSSL=false").unwrap().tls_mode, Some(TlsMode::Off));

        let jdbc = parse_connection_url("jdbc:postgresql://u:p@db.internal:5432/app?sslmode=verify-full").unwrap();
        assert_eq!(jdbc.engine, Engine::Postgres);
        assert_eq!(jdbc.tls_mode, Some(TlsMode::Verify));
        let mssql = parse_connection_url("jdbc:sqlserver://sql.internal:1433;databaseName=rillsql;user=sa;password=secret;encrypt=true;trustServerCertificate=true").unwrap();
        assert_eq!(mssql.engine, Engine::Sqlserver);
        assert_eq!(mssql.host, "sql.internal");
        assert_eq!(mssql.database, "rillsql");
        assert_eq!(mssql.username, "sa");
        assert_eq!(mssql.password, "secret");
        assert_eq!(mssql.tls_mode, Some(TlsMode::Require));
        assert_eq!(parse_connection_url("sqlserver://sql.internal;databaseName=rillsql;user=sa;encrypt=true").unwrap().tls_mode, Some(TlsMode::Verify));
        assert_eq!(parse_connection_url("mssql://sa@db.example.test/reports").unwrap().port, "1433");

        let file = parse_connection_url("sqlite:///C:/data/notes.db").unwrap();
        assert_eq!(file.file_path.as_deref(), Some("C:/data/notes.db"));
        assert_eq!(parse_connection_url("jdbc:sqlite:C:/data/notes.db").unwrap().file_path.as_deref(), Some("C:/data/notes.db"));
        assert_eq!(parse_connection_url("sqlite:data/app.db").unwrap().file_path.as_deref(), Some("data/app.db"));

        let redis = parse_connection_url("redis://cache.example.test:6380/2").unwrap();
        assert_eq!(redis.engine, Engine::Redis);
        assert_eq!(redis.database, "2");
        assert_eq!(redis.port, "6380");
        assert_eq!(parse_connection_url("redis://127.0.0.1").unwrap().port, "6379");
        assert_eq!(parse_connection_url("rediss://:secret@cache.example.test/0").unwrap().tls_mode, Some(TlsMode::Require));
        assert_eq!(parse_connection_url("redis://h:6379?ssl=false").unwrap().tls_mode, Some(TlsMode::Off));

        let mongo = parse_connection_url("mongodb://root:secret@mongo.example.test:27018/shop?replicaSet=rs0").unwrap();
        assert_eq!(mongo.engine, Engine::Mongodb);
        assert_eq!(mongo.host, "mongodb://root:secret@mongo.example.test:27018/shop?replicaSet=rs0");
        assert_eq!(mongo.original_host.as_deref(), Some("mongo.example.test"));
        assert_eq!(mongo.database, "shop");
        assert_eq!(mongo.tls_mode, None);
        let srv = parse_connection_url("mongodb+srv://cluster.example.test/shop").unwrap();
        assert_eq!(srv.port, "");
        assert_eq!(srv.tls_mode, Some(TlsMode::Require));
        assert_eq!(parse_connection_url("mongodb://u:p@h:27017/app?tls=true").unwrap().tls_mode, Some(TlsMode::Require));
    }

    #[test]
    fn rejects_unknown_and_broken_urls_with_dictionary_keys() {
        assert_eq!(parse_connection_url("oracle://u@h/db").unwrap_err(), "connections.url.unsupported");
        assert_eq!(parse_connection_url("http://example.com").unwrap_err(), "connections.url.unsupported");
        assert_eq!(parse_connection_url("   ").unwrap_err(), "connections.url.unsupported");
        assert_eq!(parse_connection_url("postgres://").unwrap_err(), "connections.url.noHost");
        assert_eq!(parse_connection_url("sqlite://").unwrap_err(), "connections.url.noHost");
        assert_eq!(parse_connection_url("postgres://h:notaport/db").unwrap_err(), "connections.url.parseFailed");
        assert_eq!(parse_connection_url("postgres:h").unwrap_err(), "connections.url.parseFailed");
    }
}
