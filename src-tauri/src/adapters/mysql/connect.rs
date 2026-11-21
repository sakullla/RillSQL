//! MySQL 家族（MySQL / MariaDB 共用）的建连、会话、探活与取消。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mysql_async::prelude::Queryable;
use mysql_async::{OptsBuilder, Pool, SslOpts};

use crate::adapters::session::{Canceller, CatalogSession, Connector, Opened, Session};
use crate::connections::drivers::db_stage;
use crate::connections::model::{CaMode, Engine, SavedConnection, TlsMode, TlsSettings};
use crate::connections::route::{self, MysqlTlsPlan, Route};
use crate::connections::stages::{self, StageError};

pub(crate) struct MysqlOpen {
    /// 查询执行专用：固定一条连接，USE、会话变量与事务在多次执行之间保持。
    pub pool: Pool,
    /// 目录、结构、表浏览与补全元数据专用的小池子：慢的元数据读取不会卡住查询，
    /// 查询中的长语句也不会卡住资源管理器。
    pub meta_pool: Pool,
    pub version: String,
    /// 只在取消时另开一条连接。执行仍使用上面的池。
    pub kill_opts: mysql_async::Opts,
    pub connection_id: u64,
}

/// 元数据池的连接上限。目录读取本身已按连接限流到 2 个，保留 2 条短生命周期空闲连接即可，
/// 避免占满受限账号的 max_user_connections 或 SSH 跳板机的 MaxSessions。
pub const MYSQL_META_CONNECTIONS: usize = 2;

fn mysql_pool_opts(max: usize, idle_ttl: Duration) -> mysql_async::PoolOpts {
    mysql_async::PoolOpts::new()
        .with_constraints(mysql_async::PoolConstraints::new(0, max.max(1)).expect("mysql pool"))
        .with_reset_connection(false)
        .with_inactive_connection_ttl(idle_ttl)
}

pub(crate) async fn open_mysql(config: &SavedConnection, route: &Route, password: &str) -> Result<MysqlOpen, StageError> {
    let plan = route::mysql_tls_plan(route);
    if (plan.skip_domain_validation || plan.accept_invalid_certs)
        && route.tls_mode != TlsMode::Require {
            return Err(stages::fail_code(crate::connections::error_codes::CONN_TLS_NOT_CONFIGURED,
                "加密连接设置前后不一致",
                "只有用户选择“加密但不验证”时才跳过证书校验。",
            ));
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
    // 查询执行独占池里这一条连接。空闲时留着它，归还时不发 COM_RESET_CONNECTION。
    builder = builder.pool_opts(mysql_pool_opts(1, Duration::from_secs(28_800)));
    if plan.ssl_opts_present {
        builder = builder.ssl_opts(Some(mysql_ssl(&plan, config.tls.as_ref())?));
    }
    let meta_opts = mysql_async::Opts::from(
        builder
            .clone()
            .pool_opts(mysql_pool_opts(MYSQL_META_CONNECTIONS, Duration::from_secs(45))),
    );
    let opts = mysql_async::Opts::from(builder);
    let pool = Pool::new(opts.clone());
    let meta_pool = Pool::new(meta_opts);
    // 建连验证直接使用目录池；连接成功后库列表复用这条已认证连接。
    // 执行池仍独占会话，在第一次执行时创建，避免首屏串行做两次握手。
    let mut conn = match tokio::time::timeout(Duration::from_secs(15), meta_pool.get_conn()).await {
        Ok(Ok(conn)) => conn,
        Ok(Err(err)) => return Err(db_stage(route.tls_mode, crate::error_text::chain(&err))),
        Err(_) => return Err(db_stage(route.tls_mode, "连接 MySQL 服务器超时（15 秒未完成握手）".to_owned())),
    };
    let version: Option<(String,)> = conn
        .query_first("SELECT VERSION()")
        .await
        .map_err(|err| db_stage(route.tls_mode, crate::error_text::chain(&err)))?;
    let version = version.map(|row| row.0).unwrap_or_else(|| "未知版本".to_owned());
    drop(conn);
    // 让池的 recycler 接回已认证连接，紧接着的目录请求就能复用它。
    tokio::task::yield_now().await;
    Ok(MysqlOpen {
        pool,
        meta_pool,
        version: version_note(config.engine, &version),
        kill_opts: opts,
        // 执行器拿到专用连接后用握手 id 更新取消目标，不能取消目录连接。
        connection_id: 0,
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
                return Err(stages::fail_code(crate::connections::error_codes::CONN_TLS_CERT_UNREADABLE, "读不到客户端证书或私钥", "没有改用不验证的连接。"));
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
            let path = tls.ca_file.clone().ok_or_else(|| stages::fail_code(crate::connections::error_codes::CONN_TLS_CA_MISSING, "请填写 CA 证书文件", "没有改用不验证的连接。"))?;
            if !Path::new(&path).is_file() {
                return Err(stages::fail_code(crate::connections::error_codes::CONN_TLS_DOWNGRADE_REFUSED, format!("读不到 CA 证书 {path}"), "没有改用不验证的连接。"));
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
        return Err(stages::fail_code(crate::connections::error_codes::CONN_TLS_DOWNGRADE_REFUSED, detail, "没有改用不验证的连接，也没有改用 Mozilla 内置根。"));
    }
    Ok(loaded.certs.into_iter().map(|cert| cert.as_ref().to_vec()).collect())
}

/// 服务器横幅与连接所选引擎不一致时只提示，不改连接类型。
pub fn version_note(engine: Engine, version: &str) -> String {
    let maria = version.to_ascii_lowercase().contains("mariadb");
    match (engine, maria) {
        (Engine::Mysql, true) => format!("{version} · 服务器横幅是 MariaDB，这条连接仍保持为 MySQL"),
        (Engine::Mariadb, false) => format!("{version} · 服务器横幅不像 MariaDB，这条连接仍保持为 MariaDB"),
        _ => version.to_owned(),
    }
}

/// MySQL 家族的连接串 TLS 参数：`ssl-mode` / `sslMode` 与 `useSSL`。
pub(in crate::adapters) fn url_tls(param: &dyn Fn(&str) -> Option<String>) -> Option<TlsMode> {
    if let Some(mode) = param("ssl-mode").or_else(|| param("sslmode")).map(|value| value.to_ascii_lowercase()) {
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
    if let Some(flag) = param("usessl").map(|value| value.to_ascii_lowercase()) {
        return Some(if flag == "false" || flag == "no" { TlsMode::Off } else { TlsMode::Require });
    }
    None
}

/// MySQL 家族连接器（MariaDB 复用）。
pub(in crate::adapters) struct MysqlConnector;

#[async_trait::async_trait]
impl Connector for MysqlConnector {
    async fn open_remote(&self, config: &SavedConnection, route: &Route, password: &str) -> Result<Opened, StageError> {
        let opened = open_mysql(config, route, password).await?;
        Ok(Opened {
            version: opened.version,
            session: Arc::new(MysqlSession { exec: opened.pool, meta: opened.meta_pool }),
            canceller: Arc::new(MysqlKill {
                connection_id: AtomicU64::new(opened.connection_id),
                opts: opened.kill_opts,
                via_ssh: config.ssh.is_some(),
            }),
            schema_note: None,
            foreign_keys: None,
        })
    }
}

/// exec 只给 SQL 执行（单连接，会话状态跨执行保留）；meta 给目录、结构、表浏览与补全。
struct MysqlSession {
    exec: Pool,
    meta: Pool,
}

#[async_trait::async_trait]
impl Session for MysqlSession {
    fn catalog(&self, _foreign_keys: Option<bool>) -> CatalogSession {
        CatalogSession::Mysql(self.meta.clone())
    }

    async fn query(&self, _foreign_keys: Option<bool>) -> Result<CatalogSession, String> {
        Ok(CatalogSession::Mysql(self.exec.clone()))
    }

    async fn ping(&self) -> Result<(), String> {
        let mut conn = self.meta.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
        conn.ping().await.map_err(|err| crate::error_text::chain(&err))
    }
}

/// 取消：另开一条连接执行 `KILL QUERY <id>`。
struct MysqlKill {
    connection_id: AtomicU64,
    opts: mysql_async::Opts,
    via_ssh: bool,
}

#[async_trait::async_trait]
impl Canceller for MysqlKill {
    fn via_ssh(&self) -> bool {
        self.via_ssh
    }

    fn note_server_session(&self, id: u64) {
        self.connection_id.store(id, Ordering::SeqCst);
    }

    async fn cancel(&self) -> Result<bool, String> {
        let id = self.connection_id.load(Ordering::SeqCst);
        if id == 0 {
            return Err("还没有读到正在执行的连接编号，取消失败。查询仍在执行。".to_owned());
        }
        // 新的池会再拨一次传输地址。经 SSH 时该地址是本地转发器，因此是另一次 accept / direct-tcpip。
        let pool = mysql_async::Pool::new(self.opts.clone());
        let via_ssh = self.via_ssh;
        let mut conn = pool.get_conn().await.map_err(|err| {
            if via_ssh {
                format!("经 SSH 另开取消连接失败：{err}。查询仍在执行。")
            } else {
                format!("取消失败：{err}。查询仍在执行。")
            }
        })?;
        let statement = format!("KILL QUERY {id}");
        conn.query_drop(statement).await.map_err(|err| format!("取消失败：{err}。查询仍在执行。"))?;
        drop(conn);
        let _ = pool.disconnect().await;
        Ok(true)
    }
}
