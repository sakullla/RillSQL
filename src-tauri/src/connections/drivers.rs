use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::pem::PemObject;

use mysql_async::prelude::Queryable;
use mysql_async::{OptsBuilder, Pool, SslOpts};
use redis::ConnectionAddr;
use tokio_postgres::NoTls;
use tokio_postgres_rustls::MakeRustlsConnect;
use tokio_util::compat::TokioAsyncReadCompatExt;

use crate::connections::model::{CaMode, Engine, SavedConnection, SshAuthKind, SshSettings, TlsMode, TlsSettings};
use crate::connections::route::{self, MysqlTlsPlan, PostgresTlsPlan, Route};
use crate::connections::stages::{self, StageError};
use crate::connections::verify::EncryptWithoutVerify;

/// tiberius 的连接句柄。futures-io 与 tokio 之间经 compat 桥接（tiberius 只发 futures 生态的流）。
pub type TdsClient = tiberius::Client<tokio_util::compat::Compat<tokio::net::TcpStream>>;

pub struct SqliteOpen {
    pub pool: sqlx::SqlitePool,
    pub version: String,
    pub foreign_keys: bool,
    pub schema_objects: usize,
}

pub async fn open_sqlite(path: &Path) -> Result<SqliteOpen, StageError> {
    if path.as_os_str().is_empty() {
        return Err(stages::fail("请选择数据库文件", "SQLite 只打开已有文件。"));
    }
    // 表编辑和目录元数据共用这一条已打开的会话，不再为写入另开只读连接。
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .busy_timeout(Duration::from_secs(5))
        .read_only(false);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|err| stages::fail(err.to_string(), "没有打开文件，后续阶段不会执行。"))?;
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type IN ('table', 'view') ORDER BY name")
        .fetch_all(&pool)
        .await
        .map_err(|err| stages::fail(err.to_string(), "文件已打开，但没有读到 schema。"))?;
    let version: String = sqlx::query_scalar("SELECT sqlite_version()")
        .fetch_one(&pool)
        .await
        .map_err(|err| stages::fail(err.to_string(), "文件已打开，但没有读到版本。"))?;
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&pool)
        .await
        .map_err(|err| stages::fail(err.to_string(), "没有读到 foreign_keys。"))?;
    Ok(SqliteOpen {
        pool,
        version,
        foreign_keys: foreign_keys != 0,
        schema_objects: names.len(),
    })
}

pub struct MysqlOpen {
    pub pool: Pool,
    pub version: String,
    /// 只在取消时另开一条连接。执行仍使用上面的池。
    pub kill_opts: mysql_async::Opts,
    pub connection_id: u64,
}

pub async fn open_mysql(config: &SavedConnection, route: &Route, password: &str) -> Result<MysqlOpen, StageError> {
    let plan = route::mysql_tls_plan(route);
    if plan.skip_domain_validation || plan.accept_invalid_certs {
        if route.tls_mode != TlsMode::Require {
            return Err(stages::fail(
                "加密连接设置前后不一致",
                "只有用户选择“加密但不验证”时才跳过证书校验。",
            ));
        }
    }
    let mut builder = OptsBuilder::default()
        .ip_or_hostname(plan.tcp_host.clone())
        .tcp_port(plan.tcp_port)
        .prefer_socket(false)
        .user(config.username.clone())
        .pass(Some(password.to_owned()));
    if let Some(database) = &config.database {
        builder = builder.db_name(Some(database.clone()));
    }
    // 目录、表浏览和查询共用池里这一条连接。空闲时留着它，归还时不发 COM_RESET_CONNECTION。
    builder = builder.pool_opts(
        mysql_async::PoolOpts::new()
            .with_constraints(mysql_async::PoolConstraints::new(0, 1).expect("mysql pool"))
            .with_reset_connection(false)
            .with_inactive_connection_ttl(Duration::from_secs(28_800)),
    );
    if plan.ssl_opts_present {
        builder = builder.ssl_opts(Some(mysql_ssl(&plan, config.tls.as_ref())?));
    }
    let opts = mysql_async::Opts::from(builder);
    let pool = Pool::new(opts.clone());
    let mut conn = pool.get_conn().await.map_err(|err| db_stage(route.tls_mode, err.to_string()))?;
    let version: Option<(String,)> = conn
        .query_first("SELECT VERSION()")
        .await
        .map_err(|err| db_stage(route.tls_mode, err.to_string()))?;
    let connection_id: Option<u64> = conn
        .query_first("SELECT CONNECTION_ID()")
        .await
        .map_err(|err| db_stage(route.tls_mode, err.to_string()))?;
    let version = version.map(|row| row.0).unwrap_or_else(|| "未知版本".to_owned());
    Ok(MysqlOpen {
        pool,
        version: version_note(config.engine, &version),
        kill_opts: opts,
        connection_id: connection_id.unwrap_or(0),
    })
}

pub(crate) fn mysql_ssl(plan: &MysqlTlsPlan, tls: Option<&TlsSettings>) -> Result<SslOpts, StageError> {
    let mut ssl = SslOpts::default()
        .with_danger_skip_domain_validation(plan.skip_domain_validation)
        .with_danger_accept_invalid_certs(plan.accept_invalid_certs)
        .with_danger_tls_hostname_override(plan.hostname_override.clone());
    if let Some(tls) = tls {
        ssl = install_mysql_trust(ssl, tls)?;
        if let (Some(cert), Some(key)) = (&tls.client_cert, &tls.client_key) {
            if !Path::new(cert).is_file() || !Path::new(key).is_file() {
                return Err(stages::fail("读不到客户端证书或私钥", "没有改用不验证的连接。"));
            }
            ssl = ssl.with_client_identity(Some(mysql_async::ClientIdentity::new(
                Path::new(cert).to_path_buf().into(),
                Path::new(key).to_path_buf().into(),
            )));
        }
    }
    Ok(ssl)
}

fn install_mysql_trust(ssl: SslOpts, tls: &TlsSettings) -> Result<SslOpts, StageError> {
    match tls.ca_mode {
        CaMode::File => {
            let path = tls.ca_file.clone().ok_or_else(|| stages::fail("请填写 CA 证书文件", "没有改用不验证的连接。"))?;
            if !Path::new(&path).is_file() {
                return Err(stages::fail(format!("读不到 CA 证书 {path}"), "没有改用不验证的连接。"));
            }
            // 只有指定 CA 文件时才不装操作系统信任库。
            Ok(ssl.with_root_certs(vec![PathBuf::from(path).into()]).with_disable_built_in_roots(true))
        }
        CaMode::System => {
            let mut roots = Vec::new();
            for anchor in os_trust_anchors()? {
                roots.push(anchor.into());
            }
            // mysql_async 的 built-in roots 是 webpki Mozilla 清单，不是操作系统信任库。
            Ok(ssl.with_root_certs(roots).with_disable_built_in_roots(true))
        }
    }
}

fn os_trust_anchors() -> Result<Vec<Vec<u8>>, StageError> {
    let loaded = rustls_native_certs::load_native_certs();
    if loaded.certs.is_empty() {
        let detail = if loaded.errors.is_empty() {
            "系统信任库里没有可用的根证书".to_owned()
        } else {
            loaded.errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
        };
        return Err(stages::fail(detail, "没有改用不验证的连接，也没有改用 Mozilla 内置根。"));
    }
    Ok(loaded.certs.into_iter().map(|cert| cert.as_ref().to_vec()).collect())
}

pub struct PostgresOpen {
    pub client: tokio_postgres::Client,
    pub version: String,
    pub cancel: tokio_postgres::CancelToken,
    pub tls: Option<MakeRustlsConnect>,
}

pub struct SqlserverOpen {
    pub client: TdsClient,
    pub version: String,
    /// 会话的 SPID。取消时另开一条连接执行 KILL <spid>。
    pub spid: u32,
    /// 取消连接复用的配置：传输地址指向直连地址或 SSH 转发器，与登录时一致。
    pub kill_config: tiberius::Config,
}

/// SQL Server 2012 是方言下限：OFFSET/FETCH 分页与新版目录视图都以它为界。
pub const SQLSERVER_MIN_MAJOR: u32 = 11;

pub async fn open_sqlserver(config: &SavedConnection, route: &Route, password: &str) -> Result<SqlserverOpen, StageError> {
    let kill_config = sqlserver_tds_config(config, route, password)?;
    let tcp = tokio::net::TcpStream::connect(kill_config.get_addr())
        .await
        .map_err(|err| stages::fail(err.to_string(), "网络连接失败，没有改用加密连接重试。"))?;
    tcp.set_nodelay(true)
        .map_err(|err| stages::fail(err.to_string(), "网络连接失败，没有改用加密连接重试。"))?;
    let mut client = tiberius::Client::connect(kill_config.clone(), tcp.compat())
        .await
        .map_err(|err| sqlserver_stage(route.tls_mode, &err))?;
    let mut stream = client
        .simple_query("SELECT @@SPID AS spid, @@VERSION AS version")
        .await
        .map_err(|err| stages::fail(err.to_string(), "数据库认证已通过，但没有读到版本。"))?;
    let mut spid = 0u32;
    let mut banner = String::new();
    {
        use futures::TryStreamExt;
        while let Some(item) = stream.try_next().await.map_err(|err| stages::fail(err.to_string(), "数据库认证已通过，但没有读到版本。"))? {
            if let Some(row) = item.into_row() {
                spid = row.try_get::<i32, _>("spid").ok().flatten().map(|v| v as u32).unwrap_or(0);
                banner = row.try_get::<&str, _>("version").ok().flatten().unwrap_or_default().to_owned();
                break;
            }
        }
    }
    drop(stream);
    let major = sqlserver_product_major(&banner).unwrap_or_default();
    if major < SQLSERVER_MIN_MAJOR {
        return Err(stages::fail(
            format!("SQL Server 版本不受支持：{banner}"),
            "没有读到可支持的版本。方言按 2012（内部版本 11）及以上设计，低于 2012 的连接在测试阶段即停止。",
        ));
    }
    Ok(SqlserverOpen {
        client,
        version: banner.lines().next().unwrap_or("SQL Server").to_owned(),
        spid,
        kill_config,
    })
}

/// 把现有 TLS 三档映射到 TDS 加密级别：
/// Off → 只加密登录握手（TDS 协议底线）；Require → 全程加密但不核对证书；Verify → 全程加密并按 CA 校验。
/// 失败任何一步都不降级重试。
pub(crate) fn sqlserver_tds_config(config: &SavedConnection, route: &Route, password: &str) -> Result<tiberius::Config, StageError> {
    let mut tds = tiberius::Config::new();
    tds.host(&route.transport_host);
    tds.port(route.transport_port);
    if let Some(database) = &config.database {
        tds.database(database.clone());
    }
    tds.application_name("RillSQL");
    tds.authentication(tiberius::AuthMethod::sql_server(
        config.username.clone().unwrap_or_default(),
        password.to_owned(),
    ));
    let tls = config.tls.as_ref();
    let mode = tls.map(|tls| tls.mode).unwrap_or(TlsMode::Off);
    match mode {
        TlsMode::Off => {
            tds.encryption(tiberius::EncryptionLevel::Off);
        }
        TlsMode::Require => {
            tds.encryption(tiberius::EncryptionLevel::Required);
            tds.trust_cert();
        }
        TlsMode::Verify => {
            tds.encryption(tiberius::EncryptionLevel::Required);
            let tls = tls.ok_or_else(|| stages::fail("缺少 TLS 设置", "没有退回明文或跳过校验。"))?;
            match tls.ca_mode {
                CaMode::File => {
                    let path = tls.ca_file.clone().ok_or_else(|| stages::fail("请填写 CA 证书文件", "没有退回明文或跳过校验。"))?;
                    if !Path::new(&path).is_file() {
                        return Err(stages::fail(format!("读不到 CA 证书 {path}"), "没有退回明文或跳过校验。"));
                    }
                    tds.trust_cert_ca(path);
                }
                CaMode::System => {
                    // tiberius 的默认信任配置就是操作系统信任库。
                }
            }
        }
    }
    Ok(tds)
}

/// 从 @@VERSION 横幅解析主版本（2012=11）。横幅形如
/// "Microsoft SQL Server 2022 (RTM) - 16.0.1000.6 ..."，取年份映射。
pub(crate) fn sqlserver_product_major(banner: &str) -> Option<u32> {
    let year = banner
        .split_whitespace()
        .find_map(|token| token.trim_matches(|ch: char| !ch.is_ascii_digit()).parse::<u32>().ok().filter(|year| (2000..=2100).contains(year)))?;
    let major = match year {
        2008 => 10,
        2012 => 11,
        2014 => 12,
        2016 => 13,
        2017 => 14,
        2019 => 15,
        2022 => 16,
        2025 => 17,
        year if year > 2025 => year - 2006,
        // 2008 以前的横幅没有对应映射，按不支持处理。
        _ => 0,
    };
    Some(major)
}

/// 连接建立期的错误分类：TCP / 登录 / TLS 分开报，不自动降级。
pub(crate) fn sqlserver_stage(mode: TlsMode, err: &tiberius::error::Error) -> StageError {
    let text = err.to_string();
    let lowered = text.to_ascii_lowercase();
    if let tiberius::error::Error::Server(token) = err {
        if token.code() == 18456 || token.code() == 18452 || token.code() == 4060 {
            return stages::fail(text, "请检查数据库用户名和密码。");
        }
    }
    if lowered.contains("login failed") || lowered.contains("登录失败") {
        return stages::fail(text, "请检查数据库用户名和密码。");
    }
    if matches!(err, tiberius::error::Error::Tls(_)) || lowered.contains("certificate") || lowered.contains("tls") || lowered.contains("ssl") {
        let _ = route::on_tls_failure(mode);
        return stages::fail(text, "连接已终止。没有降级为不加密或不验证后重试。");
    }
    if lowered.contains("io") || lowered.contains("connection") || lowered.contains("timed out") || lowered.contains("refused") {
        return stages::fail(text, "网络连接失败，没有改用加密连接重试。");
    }
    stages::fail(text, "连接已终止，没有自动改用另一种加密方式重试。")
}

pub struct RedisOpen {
    pub conn: redis::aio::MultiplexedConnection,
    pub version: String,
}

/// Redis 连接：单条 MultiplexedConnection 会话（目录/键浏览/命令行共用）。
/// 密码与 ACL 用户名走 AUTH；库索引走 SELECT（都由 redis-rs 在建连时完成）。
/// TLS 三档：Off=明文 TCP；Require=rediss 但不校验证书；Verify=rediss 且按系统/CA 文件校验。任何一档失败都不降级重试。
pub async fn open_redis(config: &SavedConnection, route: &Route, password: &str) -> Result<RedisOpen, StageError> {
    let addr = match route.tls_mode {
        TlsMode::Off => ConnectionAddr::Tcp(route.transport_host.clone(), route.transport_port),
        TlsMode::Require => ConnectionAddr::TcpTls {
            host: route.transport_host.clone(),
            port: route.transport_port,
            insecure: true,
            tls_params: None,
        },
        TlsMode::Verify => ConnectionAddr::TcpTls {
            host: route.transport_host.clone(),
            port: route.transport_port,
            insecure: false,
            tls_params: None,
        },
    };
    let db = redis_db_index(config);
    let info = redis::ConnectionInfo {
        addr,
        redis: redis::RedisConnectionInfo {
            db,
            username: config.username.clone().filter(|user| !user.trim().is_empty()),
            password: if password.is_empty() { None } else { Some(password.to_owned()) },
            protocol: redis::ProtocolVersion::RESP2,
        },
    };
    let client = build_redis_client(info, config)
        .map_err(|err| redis_stage(route.tls_mode, &err))?;
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .map_err(|err| redis_stage(route.tls_mode, &err))?;
    // 建连后探活：PING 确认会话可用，INFO server 取版本横幅。
    let pong: String = redis::cmd("PING")
        .query_async(&mut conn)
        .await
        .map_err(|err| redis_stage(route.tls_mode, &err))?;
    if !pong.eq_ignore_ascii_case("pong") {
        return Err(stages::fail(format!("PING 返回了 {pong}"), "探活没有通过，连接没有建立为可用会话。"));
    }
    let info_text: String = redis::cmd("INFO")
        .arg("server")
        .query_async(&mut conn)
        .await
        .map_err(|err| redis_stage(route.tls_mode, &err))?;
    Ok(RedisOpen {
        conn,
        version: redis_server_version(&info_text),
    })
}

/// Verify 档带 CA 文件或客户端证书时，把 PEM 材料装进 TLS 参数；其余档位直接按 ConnectionInfo 建客户端。
fn build_redis_client(info: redis::ConnectionInfo, config: &SavedConnection) -> Result<redis::Client, redis::RedisError> {
    let tls = config.tls.as_ref();
    let ca_file = tls.and_then(|tls| tls.ca_file.clone());
    let client_cert = tls.and_then(|tls| tls.client_cert.clone());
    let client_key = tls.and_then(|tls| tls.client_key.clone());
    if ca_file.is_none() && client_cert.is_none() {
        return redis::Client::open(info);
    }
    let root_cert = match ca_file {
        Some(path) => Some(std::fs::read(&path).map_err(|_| {
            redis::RedisError::from((redis::ErrorKind::InvalidClientConfig, "读不到 CA 证书", format!("{path}")))
        })?),
        None => None,
    };
    let client_tls = match (client_cert, client_key) {
        (Some(cert), Some(key)) => {
            let cert = std::fs::read(&cert).map_err(|_| {
                redis::RedisError::from((redis::ErrorKind::InvalidClientConfig, "读不到客户端证书", format!("{cert}")))
            })?;
            let key = std::fs::read(&key).map_err(|_| {
                redis::RedisError::from((redis::ErrorKind::InvalidClientConfig, "读不到客户端私钥", format!("{key}")))
            })?;
            Some(redis::ClientTlsConfig { client_cert: cert, client_key: key })
        }
        (None, None) => None,
        _ => {
            return Err(redis::RedisError::from((
                redis::ErrorKind::InvalidClientConfig,
                "客户端证书和私钥需要同时填写",
                String::new(),
            )))
        }
    };
    // 只有 Verify 档会走到这里（Off/Require 不读证书材料）。
    if !matches!(info.addr, ConnectionAddr::TcpTls { insecure: false, .. }) {
        return Err(redis::RedisError::from((
            redis::ErrorKind::InvalidClientConfig,
            "TLS 材料只在“加密连接并核对证书”档生效",
        )));
    }
    redis::Client::build_with_tls(info, redis::TlsCertificates { client_tls, root_cert })
}

/// 库索引来自连接配置的 database 字段；非法值在保存阶段已被拦截，这里兜底为 0。
pub(crate) fn redis_db_index(config: &SavedConnection) -> i64 {
    config
        .database
        .as_deref()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(0)
}

fn redis_server_version(info: &str) -> String {
    for line in info.lines() {
        if let Some(value) = line.strip_prefix("redis_version:") {
            return format!("Redis {}", value.trim());
        }
    }
    "Redis 未知版本".to_owned()
}

/// Redis 连接错误分类：TCP / AUTH / TLS 分开报，不降级重试（与 sqlserver_stage 同纪律）。
pub(crate) fn redis_stage(mode: TlsMode, err: &redis::RedisError) -> StageError {
    let text = err.to_string();
    let lowered = text.to_ascii_lowercase();
    if matches!(err.kind(), redis::ErrorKind::AuthenticationFailed)
        || lowered.contains("noauth")
        || lowered.contains("wrongpass")
        || lowered.contains("invalid password")
        || lowered.contains("authentication")
        || text.contains("密码")
    {
        return stages::fail(text, "请检查 Redis 密码或 ACL 用户名。");
    }
    if lowered.contains("certificate") || lowered.contains("tls") || lowered.contains("ssl") || text.contains("证书") {
        let _ = route::on_tls_failure(mode);
        return stages::fail(text, "连接已终止。没有降级为不加密或不验证后重试。");
    }
    if lowered.contains("refused") || lowered.contains("timed out") || lowered.contains("timeout") || lowered.contains("io") || lowered.contains("connection") {
        return stages::fail(text, "网络连接失败，没有改用加密连接重试。");
    }
    stages::fail(text, "连接已终止，没有自动改用另一种加密方式重试。")
}

pub struct MongoOpen {
    pub client: mongodb::Client,
    pub version: String,
}

/// MongoDB 连接（R12）。主机字段两种形态：
/// · host/port：装配 ClientOptions（SCRAM 凭据、authSource=认证库、replicaSet 参数、TLS 三档）；
/// · mongodb:// 或 mongodb+srv:// 整串：交给驱动解析，表单的认证库/副本集/TLS 档仅在串里没写对应参数时补齐，
///   串里带用户名而没带密码时用 keyring 密码补上（密码永远不落连接串之外的配置）。
/// 建连后探活：ping 确认会话可用，buildInfo 取版本横幅。任何一档失败都不降级重试。
pub async fn open_mongo(config: &SavedConnection, route: &Route, password: &str) -> Result<MongoOpen, StageError> {
    let mut options = mongo_client_options(config, route, password).await?;
    options.server_selection_timeout = Some(Duration::from_secs(15));
    let client = mongodb::Client::with_options(options.clone()).map_err(|err| mongo_stage(route.tls_mode, &err))?;
    let admin = client.database("admin");
    let pong = admin
        .run_command(mongodb::bson::doc! { "ping": 1 })
        .await
        .map_err(|err| mongo_stage(route.tls_mode, &err))?;
    if pong.get_i32("ok") != Ok(1) && pong.get_f64("ok") != Ok(1.0) {
        return Err(stages::fail(format!("ping 返回了 {pong:?}"), "探活没有通过，连接没有建立为可用会话。"));
    }
    let build = client
        .database("admin")
        .run_command(mongodb::bson::doc! { "buildInfo": 1 })
        .await
        .map_err(|err| mongo_stage(route.tls_mode, &err))?;
    let version = build
        .get_str("version")
        .map(|value| format!("MongoDB {value}"))
        .unwrap_or_else(|_| "MongoDB 未知版本".to_owned());
    Ok(MongoOpen { client, version })
}

/// 装配 MongoDB ClientOptions。装配函数（不联网，plain mongodb:// 串解析也不发 DNS 查询），
/// 供单测核对主机/凭据/副本集/TLS 三档。
/// 整串 URI 与 SSH 隧道互斥：URI 的地址直接进驱动 hosts，route 的转发器地址没有入口，
/// 组合会把隧道静默绕过——保存阶段已拒绝，这里再拦一道（不建隧道、不连接）。
pub(crate) async fn mongo_client_options(
    config: &SavedConnection,
    route: &Route,
    password: &str,
) -> Result<mongodb::options::ClientOptions, StageError> {
    if Engine::host_is_mongo_uri(config.host.as_deref().unwrap_or_default()) {
        if config.ssh.is_some() {
            return Err(stages::fail(
                "整串 URI 暂不支持 SSH 隧道，请改用 host/port 表单",
                "mongodb:// 连接串的地址会直接交给驱动，SSH 转发器地址没有入口，组合会静默绕过隧道。把主机拆成 host/port 填写后再开启跳板机。",
            ));
        }
        let mut options = mongodb::options::ClientOptions::parse(config.host.clone().unwrap_or_default())
            .await
            .map_err(|err| mongo_stage(route.tls_mode, &err))?;
        // 串里带了用户名但没带密码：用 keyring/本次输入的密码补上，密码不回写连接串。
        let needs_password = options
            .credential
            .as_ref()
            .is_some_and(|credential| credential.username.is_some() && credential.password.is_none());
        if needs_password && !password.is_empty() {
            if let Some(credential) = options.credential.as_mut() {
                credential.password = Some(password.to_owned());
            }
        }
        // 表单的 TLS 三档只在串里没有 tls 参数时生效（连接串写过的参数优先）。
        if options.tls.is_none() {
            options.tls = mongo_tls(config)?;
        }
        if options.repl_set_name.is_none() {
            options.repl_set_name = config.replica_set.clone().filter(|name| !name.trim().is_empty());
        }
        return Ok(options);
    }
    let mut options = mongodb::options::ClientOptions::builder().build();
    options.hosts = vec![mongodb::options::ServerAddress::Tcp {
        host: route.transport_host.clone(),
        port: Some(route.transport_port),
    }];
    options.app_name = Some("RillSQL".to_owned());
    if let Some(replica_set) = config.replica_set.as_deref().filter(|name| !name.trim().is_empty()) {
        options.repl_set_name = Some(replica_set.trim().to_owned());
    }
    let username = config.username.as_deref().unwrap_or("").trim();
    if !username.is_empty() {
        // SCRAM 凭据：机制留空由服务器协商（SCRAM-SHA-256 优先），认证库默认 admin。
        let mut credential = mongodb::options::Credential::default();
        credential.username = Some(username.to_owned());
        credential.password = (!password.is_empty()).then(|| password.to_owned());
        credential.source = config
            .database
            .as_deref()
            .map(str::trim)
            .filter(|source| !source.is_empty())
            .map(|source| source.to_owned());
        options.credential = Some(credential);
    }
    options.tls = mongo_tls(config)?;
    Ok(options)
}

/// TLS 三档映射：Off=不启用；Require=启用但不核对证书；Verify=启用并核对（CA 文件或 webpki 内置根）。
/// 驱动把客户端证书与私钥放在同一个 PEM 文件（cert_key_file_path）：两栏填不同路径时报明确错误，
/// 填同一路径即合并后的 PEM；只填一栏时按“该文件就是合并 PEM”处理。
fn mongo_tls(config: &SavedConnection) -> Result<Option<mongodb::options::Tls>, StageError> {
    let mode = config.tls.as_ref().map(|tls| tls.mode).unwrap_or(TlsMode::Off);
    match mode {
        TlsMode::Off => Ok(None),
        TlsMode::Require => Ok(Some(mongodb::options::Tls::Enabled(
            mongodb::options::TlsOptions::builder().allow_invalid_certificates(true).build(),
        ))),
        TlsMode::Verify => {
            let mut tls_options = mongodb::options::TlsOptions::builder().allow_invalid_certificates(false).build();
            if let Some(tls) = config.tls.as_ref() {
                if tls.ca_mode == CaMode::File {
                    if let Some(path) = tls.ca_file.as_deref().filter(|path| !path.trim().is_empty()) {
                        tls_options.ca_file_path = Some(PathBuf::from(path));
                    }
                }
                let cert = tls.client_cert.as_deref().map(str::trim).filter(|path| !path.is_empty());
                let key = tls.client_key.as_deref().map(str::trim).filter(|path| !path.is_empty());
                match (cert, key) {
                    (Some(cert), Some(key)) => {
                        if cert != key {
                            return Err(stages::fail(
                                "MongoDB 客户端证书需合并 PEM：证书与私钥需同一文件",
                                "mongodb 驱动只接受一个证书+私钥合并的 PEM 文件。请先把两份材料合并到一个文件，再在两栏填同一路径。",
                            ));
                        }
                        // 同一路径 = 合并后的 PEM。
                        tls_options.cert_key_file_path = Some(PathBuf::from(cert));
                    }
                    // 只填一栏：该文件即合并 PEM（其余引擎的两栏语义由各自驱动处理）。
                    (Some(path), None) | (None, Some(path)) => {
                        tls_options.cert_key_file_path = Some(PathBuf::from(path));
                    }
                    (None, None) => {}
                }
            }
            Ok(Some(mongodb::options::Tls::Enabled(tls_options)))
        }
    }
}

/// MongoDB 连接错误分类：TCP / 认证 / TLS 分开报，不降级重试（与 redis_stage 同纪律）。
pub(crate) fn mongo_stage(mode: TlsMode, err: &mongodb::error::Error) -> StageError {
    let text = err.to_string();
    let lowered = text.to_ascii_lowercase();
    let is_auth = matches!(&*err.kind, mongodb::error::ErrorKind::Authentication { .. })
        || matches!(&*err.kind, mongodb::error::ErrorKind::Command(command) if command.code == 18 || command.code == 13 || command.code == 33)
        || lowered.contains("authentication")
        || lowered.contains("auth failed")
        || lowered.contains("scram")
        || text.contains("认证");
    if is_auth {
        return stages::fail(text, "请检查 MongoDB 用户名和密码（认证库默认 admin）。");
    }
    if matches!(&*err.kind, mongodb::error::ErrorKind::InvalidTlsConfig { .. })
        || lowered.contains("certificate")
        || lowered.contains("tls")
        || lowered.contains("ssl")
        || text.contains("证书")
    {
        let _ = route::on_tls_failure(mode);
        return stages::fail(text, "连接已终止。没有降级为不加密或不验证后重试。");
    }
    if matches!(
        &*err.kind,
        mongodb::error::ErrorKind::Io(_)
            | mongodb::error::ErrorKind::ServerSelection { .. }
            | mongodb::error::ErrorKind::ConnectionPoolCleared { .. }
            | mongodb::error::ErrorKind::DnsResolve { .. }
    ) || lowered.contains("timed out")
        || lowered.contains("timeout")
        || lowered.contains("refused")
        || lowered.contains("connection")
    {
        return stages::fail(text, "网络连接失败，没有改用加密连接重试。");
    }
    stages::fail(text, "连接已终止，没有自动改用另一种加密方式重试。")
}

pub async fn open_postgres(config: &SavedConnection, route: &Route, password: &str) -> Result<PostgresOpen, StageError> {
    let plan = route::postgres_tls_plan(route).map_err(|err| stages::fail(err, "没有改用转发器地址做证书校验。"))?;
    if plan.verify_hostname && plan.host == "127.0.0.1" && route.tls_server_name.as_deref() != Some("127.0.0.1") {
        return Err(stages::fail(
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
    let client = if let Some(tls) = tls_for_cancel.clone() {
        let (client, connection) = pg.connect(tls).await.map_err(|err| db_stage(route.tls_mode, err.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    } else {
        let (client, connection) = pg.connect(NoTls).await.map_err(|err| stages::fail(err.to_string(), "网络连接失败，没有改走加密连接重试。"))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    };
    let cancel = client.cancel_token();
    let messages = client
        .simple_query("SELECT version()")
        .await
        .map_err(|err| stages::fail(err.to_string(), "数据库认证已通过，但没有读到版本。"))?;
    let version = messages
        .iter()
        .find_map(|message| match message {
            tokio_postgres::SimpleQueryMessage::Row(row) => row.try_get(0).ok().flatten().map(str::to_owned),
            _ => None,
        })
        .unwrap_or_else(|| "未知版本".to_owned());
    Ok(PostgresOpen {
        client,
        version,
        cancel,
        tls: tls_for_cancel,
    })
}

fn postgres_tls(config: &SavedConnection, plan: &PostgresTlsPlan) -> Result<MakeRustlsConnect, StageError> {
    let tls = config.tls.as_ref().ok_or_else(|| stages::fail("缺少 TLS 设置", "没有退回明文。"))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    if !plan.verify_hostname {
        let verifier = Arc::new(EncryptWithoutVerify::new(provider.clone()).map_err(|err| stages::fail(err, "没有退回明文。"))?);
        let builder = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|err| stages::fail(err.to_string(), "没有退回明文。"))?
            .dangerous()
            .with_custom_certificate_verifier(verifier);
        let client = finish_client_config(builder, tls)?;
        return Ok(MakeRustlsConnect::new(client));
    }
    if tls.ca_mode == CaMode::File {
        let path = tls.ca_file.clone().ok_or_else(|| stages::fail("请填写 CA 证书文件", "没有改用不验证的连接。"))?;
        let bytes = std::fs::read(&path).map_err(|_| stages::fail(format!("读不到 CA 证书 {path}"), "没有改用不验证的连接。"))?;
        let mut roots = rustls::RootCertStore::empty();
        let mut added = 0;
        for cert in rustls::pki_types::CertificateDer::pem_slice_iter(&bytes) {
            let cert = cert.map_err(|err| stages::fail(err.to_string(), "没有改用不验证的连接。"))?;
            roots.add(cert).map_err(|err| stages::fail(err.to_string(), "没有改用不验证的连接。"))?;
            added += 1;
        }
        if added == 0 {
            return Err(stages::fail("CA 文件里没有证书", "没有改用不验证的连接。"));
        }
        let builder = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|err| stages::fail(err.to_string(), "没有改用不验证的连接。"))?
            .with_root_certificates(roots);
        return Ok(MakeRustlsConnect::new(finish_client_config(builder, tls)?));
    }
    if tls.client_cert.is_some() || tls.client_key.is_some() {
        return Err(stages::fail(
            "使用客户端证书时请同时指定 CA 文件",
            "当前依赖没有公开系统根与客户端证书的组合接口，没有改为不验证。",
        ));
    }
    let (connector, errors) = MakeRustlsConnect::with_native_certs().map_err(|errors| {
        stages::fail(
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
            let cert_bytes = std::fs::read(cert).map_err(|_| stages::fail(format!("读不到客户端证书 {cert}"), "没有改用不验证的连接。"))?;
            let key_bytes = std::fs::read(key).map_err(|_| stages::fail(format!("读不到客户端私钥 {key}"), "没有改用不验证的连接。"))?;
            let chain = rustls::pki_types::CertificateDer::pem_slice_iter(&cert_bytes)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| stages::fail(err.to_string(), "没有改用不验证的连接。"))?;
            let private = rustls::pki_types::PrivateKeyDer::from_pem_slice(&key_bytes)
                .map_err(|err| stages::fail(err.to_string(), "没有改用不验证的连接。"))?;
            builder
                .with_client_auth_cert(chain, private)
                .map_err(|err| stages::fail(err.to_string(), "没有改用不验证的连接。"))
        }
        (None, None) => Ok(builder.with_no_client_auth()),
        _ => Err(stages::fail("客户端证书和私钥需要同时填写", "没有改用不验证的连接。")),
    }
}

fn db_stage(mode: TlsMode, message: String) -> StageError {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("certificate") || lowered.contains("tls") || lowered.contains("ssl") || message.contains("证书") {
        let _ = route::on_tls_failure(mode);
        return stages::fail(message, "连接已终止。没有改用 localhost 身份校验，也没有降级为不验证证书或不加密后重试。");
    }
    if lowered.contains("password") || lowered.contains("access denied") || lowered.contains("authentication") || message.contains("认证") {
        return stages::fail(message, "请检查数据库用户名和密码。");
    }
    stages::fail(message, "连接已终止，没有自动改用另一种加密方式重试。")
}

pub fn version_note(engine: Engine, version: &str) -> String {
    let maria = version.to_ascii_lowercase().contains("mariadb");
    match (engine, maria) {
        (Engine::Mysql, true) => format!("{version} · 服务器横幅是 MariaDB，这条连接仍保持为 MySQL"),
        (Engine::Mariadb, false) => format!("{version} · 服务器横幅不像 MariaDB，这条连接仍保持为 MariaDB"),
        _ => version.to_owned(),
    }
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
        Self(err.to_string())
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
                .map_err(|err| crate::connections::forward::ForwardError::Channel(err.to_string()))?
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
            return Err(TunnelFault::Identity(stages::fail(err.0, "还没有尝试登录 SSH 跳板机或数据库。")));
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
                    return Err(TunnelFault::Authentication(stages::fail(
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
                    return Err(TunnelFault::Authentication(stages::fail(
                        "请填写私钥文件",
                        "这是登录跳板机失败，还没有尝试连接数据库。",
                    )))
                }
            };
            let key = match russh::keys::load_secret_key(&path, passphrase.filter(|item| !item.is_empty())) {
                Ok(key) => key,
                Err(err) => {
                    return Err(TunnelFault::Authentication(stages::fail(
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
            return Err(TunnelFault::Authentication(stages::fail(
                format!(
                    "Permission denied ({}) · 用户 {}",
                    if ssh.auth == SshAuthKind::Key { "publickey" } else { "password" },
                    ssh.username
                ),
                "这是登录跳板机失败，还没有尝试连接数据库。请检查 SSH 用户、私钥或口令。",
            )));
        }
        Err(err) => {
            return Err(TunnelFault::Authentication(stages::fail(
                err.to_string(),
                "这是登录跳板机失败，还没有尝试连接数据库。",
            )));
        }
    }
    let forwarder = match crate::connections::forward::Forwarder::bind().await {
        Ok(forwarder) => forwarder,
        Err(err) => {
            return Err(TunnelFault::Tunnel(stages::fail(
                err.to_string(),
                "已登录 SSH 跳板机，但到数据库的通道没有打通，还没有连接数据库。",
            )))
        }
    };
    let local_port = match forwarder.local_port() {
        Ok(port) => port,
        Err(err) => {
            return Err(TunnelFault::Tunnel(stages::fail(
                err.to_string(),
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
        crate::connections::hostkey::HostKeyDecision::Changed { presented, saved } => TunnelFault::Identity(stages::fail(
            format!("服务器出示的密钥 {presented} 与已保存的 {saved} 不同"),
            "已阻止连接，没有发送任何凭据。如果确认服务器更换了密钥，请先通过可信渠道核实新指纹，然后忘记已保存的密钥再重新测试。",
        )),
        crate::connections::hostkey::HostKeyDecision::Trusted => {
            TunnelFault::Identity(stages::fail("SSH 主机密钥状态不一致", "没有继续认证。"))
        }
    }
}
