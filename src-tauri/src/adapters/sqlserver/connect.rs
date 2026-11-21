//! SQL Server 的建连、会话、探活、取消（KILL spid）与连接错误分类。

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use tokio_util::compat::TokioAsyncReadCompatExt;

use crate::adapters::session::{Canceller, CatalogSession, Connector, Opened, Session, TdsClient};
use crate::connections::model::{CaMode, SavedConnection, TlsMode};
use crate::connections::route::{self, Route};
use crate::connections::stages::{self, StageError};

pub(crate) struct SqlserverOpen {
    pub client: TdsClient,
    /// 目录、补全与探活专用的独立连接：不加入用户事务（ADR-1）。
    pub meta_client: TdsClient,
    pub version: String,
    /// 会话的 SPID。取消时另开一条连接执行 KILL <spid>。
    pub spid: u32,
    /// 取消连接复用的配置：传输地址指向直连地址或 SSH 转发器，与登录时一致。
    pub kill_config: tiberius::Config,
}

/// SQL Server 2012 是方言下限：OFFSET/FETCH 分页与新版目录视图都以它为界。
pub const SQLSERVER_MIN_MAJOR: u32 = 11;

pub(crate) async fn open_sqlserver(config: &SavedConnection, route: &Route, password: &str) -> Result<SqlserverOpen, StageError> {
    let kill_config = sqlserver_tds_config(config, route, password)?;
    let mut client = connect_client(kill_config.clone(), route.tls_mode).await?;
    // 元数据会话是同一服务器上的第二条物理连接：目录读取、补全与探活不进入用户事务。
    let meta_client = connect_client(kill_config.clone(), route.tls_mode).await?;
    let mut stream = client
        .simple_query("SELECT @@SPID AS spid, @@VERSION AS version")
        .await
        .map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_VERSION_READ_FAILED, crate::error_text::chain(&err), "数据库认证已通过，但没有读到版本。"))?;
    let mut spid = 0u32;
    let mut banner = String::new();
    {
        use futures::TryStreamExt;
        while let Some(item) = stream.try_next().await.map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_VERSION_READ_FAILED, crate::error_text::chain(&err), "数据库认证已通过，但没有读到版本。"))? {
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
        return Err(stages::fail_code(crate::connections::error_codes::CONN_VERSION_UNSUPPORTED,
            format!("SQL Server 版本不受支持：{banner}"),
            "没有读到可支持的版本。方言按 2012（内部版本 11）及以上设计，低于 2012 的连接在测试阶段即停止。",
        ));
    }
    Ok(SqlserverOpen {
        client,
        meta_client,
        version: banner.lines().next().unwrap_or("SQL Server").to_owned(),
        spid,
        kill_config,
    })
}

/// 拨一条 TDS 连接。错误分类与连接建立阶段一致：TCP / 登录 / TLS 分开报，不自动降级。
async fn connect_client(config: tiberius::Config, mode: TlsMode) -> Result<TdsClient, StageError> {
    let tcp = tokio::net::TcpStream::connect(config.get_addr())
        .await
        .map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_NETWORK_FAILED, crate::error_text::chain(&err), "网络连接失败，没有改用加密连接重试。"))?;
    tcp.set_nodelay(true)
        .map_err(|err| stages::fail_code(crate::connections::error_codes::CONN_NETWORK_FAILED, crate::error_text::chain(&err), "网络连接失败，没有改用加密连接重试。"))?;
    tiberius::Client::connect(config, tcp.compat())
        .await
        .map_err(|err| sqlserver_stage(mode, &err))
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
            let tls = tls.ok_or_else(|| stages::fail_code(crate::connections::error_codes::CONN_TLS_NOT_CONFIGURED, "缺少 TLS 设置", "没有退回明文或跳过校验。"))?;
            match tls.ca_mode {
                CaMode::File => {
                    let path = tls.ca_file.clone().ok_or_else(|| stages::fail_code(crate::connections::error_codes::CONN_TLS_CA_MISSING, "请填写 CA 证书文件", "没有退回明文或跳过校验。"))?;
                    if !Path::new(&path).is_file() {
                        return Err(stages::fail_code(crate::connections::error_codes::CONN_TLS_DOWNGRADE_REFUSED, format!("读不到 CA 证书 {path}"), "没有退回明文或跳过校验。"));
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
    let text = crate::error_text::chain(err);
    let lowered = text.to_ascii_lowercase();
    if let tiberius::error::Error::Server(token) = err {
        if token.code() == 18456 || token.code() == 18452 || token.code() == 4060 {
            return stages::fail_code(crate::connections::error_codes::CONN_AUTH_FAILED, text, "请检查数据库用户名和密码。");
        }
    }
    if lowered.contains("login failed") || lowered.contains("登录失败") {
        return stages::fail_code(crate::connections::error_codes::CONN_AUTH_FAILED, text, "请检查数据库用户名和密码。");
    }
    if matches!(err, tiberius::error::Error::Tls(_)) || lowered.contains("certificate") || lowered.contains("tls") || lowered.contains("ssl") {
        let _ = route::on_tls_failure(mode);
        return stages::fail_code(crate::connections::error_codes::CONN_TLS_TERMINATED, text, "连接已终止。没有降级为不加密或不验证后重试。");
    }
    if lowered.contains("io") || lowered.contains("connection") || lowered.contains("timed out") || lowered.contains("refused") {
        return stages::fail_code(crate::connections::error_codes::CONN_NETWORK_FAILED, text, "网络连接失败，没有改用加密连接重试。");
    }
    stages::fail_code(crate::connections::error_codes::CONN_TLS_TERMINATED, text, "连接已终止，没有自动改用另一种加密方式重试。")
}

/// SQL Server 连接器。
pub(in crate::adapters) struct SqlserverConnector;

#[async_trait::async_trait]
impl Connector for SqlserverConnector {
    async fn open_remote(&self, config: &SavedConnection, route: &Route, password: &str) -> Result<Opened, StageError> {
        let opened = open_sqlserver(config, route, password).await?;
        Ok(Opened {
            version: opened.version,
            session: Arc::new(SqlserverSession {
                exec: Arc::new(tokio::sync::Mutex::new(opened.client)),
                meta: Arc::new(tokio::sync::Mutex::new(opened.meta_client)),
            }),
            canceller: Arc::new(SqlserverKill {
                spid: AtomicU32::new(opened.spid),
                config: opened.kill_config,
                via_ssh: config.ssh.is_some(),
            }),
            schema_note: None,
            foreign_keys: None,
        })
    }
}

struct SqlserverSession {
    /// 用户事务会话：SQL 查询、网格写入与结构业务页面共用这一条物理连接。
    exec: Arc<tokio::sync::Mutex<TdsClient>>,
    /// 元数据会话：目录、补全与探活，不进入用户事务。
    meta: Arc<tokio::sync::Mutex<TdsClient>>,
}

#[async_trait::async_trait]
impl Session for SqlserverSession {
    fn catalog(&self, _foreign_keys: Option<bool>) -> CatalogSession {
        CatalogSession::Sqlserver(Arc::clone(&self.meta))
    }

    async fn query(&self, _foreign_keys: Option<bool>) -> Result<CatalogSession, String> {
        Ok(CatalogSession::Sqlserver(Arc::clone(&self.exec)))
    }

    /// 探活走元数据会话：用户事务中的长语句不会让探活误判，探活也不进入用户事务。
    async fn ping(&self) -> Result<(), String> {
        let mut client = self.meta.lock().await;
        use futures::TryStreamExt;
        let mut stream = client.simple_query("SELECT 1").await.map_err(|err| crate::error_text::chain(&err))?;
        while let Some(item) = stream.try_next().await.map_err(|err| crate::error_text::chain(&err))? {
            if item.into_row().is_some() {
                return Ok(());
            }
        }
        Ok(())
    }
}

/// 取消语句的 SQL。spid 是连接建立时读到的会话编号，纯函数供单测核对。
pub fn kill_sql(spid: u32) -> String {
    format!("KILL {spid}")
}

/// 取消：另开一条传输（经 SSH 时指向本地转发器，是新的 direct-tcpip）执行 `KILL <spid>`。
struct SqlserverKill {
    spid: AtomicU32,
    config: tiberius::Config,
    via_ssh: bool,
}

#[async_trait::async_trait]
impl Canceller for SqlserverKill {
    fn via_ssh(&self) -> bool {
        self.via_ssh
    }

    async fn cancel(&self) -> Result<bool, String> {
        let id = self.spid.load(Ordering::SeqCst);
        if id == 0 {
            return Err("还没有读到正在执行的会话编号，取消失败。查询仍在执行。".to_owned());
        }
        let via_ssh = self.via_ssh;
        let tcp = tokio::net::TcpStream::connect(self.config.get_addr()).await.map_err(|err| {
            if via_ssh {
                format!("经 SSH 另开取消连接失败：{err}。查询仍在执行。")
            } else {
                format!("取消失败：{err}。查询仍在执行。")
            }
        })?;
        tcp.set_nodelay(true).map_err(|err| format!("取消失败：{err}。查询仍在执行。"))?;
        let mut client = tiberius::Client::connect(self.config.clone(), tcp.compat())
            .await
            .map_err(|err| format!("取消失败：{err}。查询仍在执行。"))?;
        client
            .execute(kill_sql(id), &[])
            .await
            .map_err(|err| format!("取消失败：{err}。查询仍在执行。"))?;
        Ok(true)
    }
}
