//! 引擎适配器层（ADR-0 / R22）。
//!
//! 每个引擎一个模块，按能力分组实现下列 trait；主代码只经 [`registry::adapter`] 取得
//! `&'static dyn EngineAdapter` 再调用能力组，不再对具体 `Engine` 变体分支。
//! `registry.rs` 是唯一允许对 `Engine` 穷举匹配、以及把引擎字符串解析成 `Engine` 的地方。
//!
//! 迁移分领域进行（A1 连接与会话 → A2 目录 → A3 DDL → A4 网格与查询 → A5 对比/转储/迁移/导出 →
//! A6 UI 元数据与收尾）。尚无实现的能力组在这里先定义接口骨架，由对应迁移任务填充；
//! 适配器在 [`Capabilities`] 里如实声明当前提供了哪些组，注册表完整性测试校验两者一致。

pub mod registry;
pub mod session;

mod mariadb;
pub(crate) mod mongodb;
pub(crate) mod mysql;
pub(crate) mod postgres;
pub(crate) mod redis;
pub(crate) mod sqlite;
pub(crate) mod sqlserver;

#[cfg(test)]
mod boundary_tests;
#[cfg(test)]
mod tests;

use crate::connections::model::{Engine, SavedConnection, TlsMode};

pub use registry::adapter;
pub use session::Connector;

/// 工作台类型：决定打开连接后主区用哪种 UI。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workbench {
    /// 关系型：SQL 编辑器 + 表网格 + 结构编辑。
    Relational,
    Redis,
    Mongo,
}

/// 适配器能力声明。取代散落各处的 `is_relational`、`shows_*`、`ddl_available` 等判断；
/// `*_group` 字段声明该适配器是否提供对应能力组（与 `EngineAdapter` 的访问器返回值必须一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    /// 有 SQL 执行、表网格、结构编辑、对比、导出等关系面。
    pub relational: bool,
    pub sql_syntax_group: bool,
    pub type_rules_group: bool,
    pub ddl_group: bool,
    pub catalog_group: bool,
    pub error_rules_group: bool,
    pub connector_group: bool,
    /// 会话绑定到单个库：切换库要改连，跨库读取要另开临时会话（不替换当前会话）。
    pub database_scoped_session: bool,
}

/// 连接表单“常规”分栏里远程引擎的字段，顺序即绘制与 Tab 顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Host,
    Port,
    Username,
    Password,
    Database,
    ReplicaSet,
    SavePassword,
}

/// 关系型远程引擎的默认字段顺序。
pub const DEFAULT_GENERAL_FIELDS: &[FormField] =
    &[FormField::Host, FormField::Port, FormField::Username, FormField::Password, FormField::Database, FormField::SavePassword];

/// 连接串的整体形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlStyle {
    /// `scheme://user:pass@host:port/db?k=v`。
    Authority,
    /// 同上，另接受 JDBC 风格 `;k=v` 参数，库名/用户/密码可写在参数里。
    Jdbc,
    /// 主机字段保存整串连接串（驱动自己解析），另记下原始主机名。
    FullUri,
    /// `scheme:path` 或 `scheme:///path`：本地文件路径。
    FilePath,
}

/// 一个连接串 scheme。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UrlScheme {
    /// 小写 scheme（不含冒号）。
    pub name: &'static str,
    /// 这个 scheme 本身就意味着加密（rediss、mongodb+srv）。
    pub implies_tls: bool,
    /// 端口由服务发现决定（mongodb+srv），不填默认端口。
    pub discovered_port: bool,
}

impl UrlScheme {
    pub const fn plain(name: &'static str) -> Self {
        Self { name, implies_tls: false, discovered_port: false }
    }
}

/// 引擎元数据：标识、显示名、默认端口、是否文件型、工作台类型。
/// A1 起补充默认用户/库、URL scheme、TLS 参数与连接表单分区；A6 补充徽标缩写与颜色。
pub trait EngineMeta: Sync {
    /// 适配器所服务的引擎。
    #[cfg_attr(not(test), allow(dead_code))] // 注册表完整性测试用它校验映射；A1 起主代码经它回到 Engine
    fn engine(&self) -> Engine;
    /// 持久化与 DTO 使用的稳定字符串 id。
    fn id(&self) -> &'static str;
    /// 显示名。
    fn label(&self) -> &'static str;
    /// 默认端口；文件型引擎没有。
    fn default_port(&self) -> Option<u16>;
    /// 文件型（本地文件即数据库，没有主机/端口）。
    fn file_based(&self) -> bool;
    fn workbench(&self) -> Workbench;

    /// 新建连接默认按本地开发处理：环境标签 local、TLS 关闭。
    fn defaults_to_local(&self) -> bool {
        false
    }

    /// 新建连接的默认库。
    fn default_database(&self) -> &'static str {
        ""
    }

    /// 用户名可以留空（密码认证或无认证直连）。
    fn username_optional(&self) -> bool {
        false
    }

    /// “数据库”字段是 0 起的库索引，不是库名（换引擎时两边不互相沿用）。
    fn database_is_index(&self) -> bool {
        false
    }

    /// 主机字段是不是整串连接串（此时端口不参与，也不能走 SSH 隧道）。
    fn host_is_uri(&self, _host: &str) -> bool {
        false
    }

    /// 连接表单有副本集字段。
    fn uses_replica_set(&self) -> bool {
        false
    }

    /// 远程引擎“常规”分栏的字段顺序。
    fn general_fields(&self) -> &'static [FormField] {
        DEFAULT_GENERAL_FIELDS
    }

    /// 没有密码时仍可建连（空密码不算凭据缺失）。
    fn password_optional(&self, _username: &str) -> bool {
        false
    }

    /// 自动重连要求凭据库里有保存的密码。
    fn reconnect_needs_saved_password(&self) -> bool {
        true
    }

    /// 数据库认证阶段成功时的说明。
    fn auth_stage_note(&self, config: &SavedConnection) -> String {
        format!("用户 {}", config.username.clone().unwrap_or_default())
    }

    /// 这个引擎认的连接串 scheme。
    fn url_schemes(&self) -> &'static [UrlScheme];

    fn url_style(&self) -> UrlStyle {
        UrlStyle::Authority
    }

    /// 通用参数（encrypt / sslmode）之外、该引擎专有的连接串 TLS 参数。`param` 按名（大小写不敏感）取值。
    fn url_tls(&self, _param: &dyn Fn(&str) -> Option<String>) -> Option<TlsMode> {
        None
    }
}

/// SQL 语法：标识符引号、保留字与限定名。A4 起补充字面量、分页、计数、超时、词法与 EXPLAIN。
///
/// 按需引号判定（保守：任何不确定都加引号）：仅 `[A-Za-z_][A-Za-z0-9_]*`（`allows_dollar` 时另允许 `$`），
/// 不是保留字，`bare_leading_underscore` 为假时下划线开头一律加引号，`folds_to_lowercase` 时要求全小写。
pub trait SqlSyntax: Sync {
    /// 总是加引号（方言引号字符，内部同字符加倍转义）。
    fn quote_always(&self, name: &str) -> String;

    /// 该方言的保留字表（大写、按字母序、无重复，供二分查找）。
    fn reserved_words(&self) -> &'static [&'static str];

    /// 未加引号的标识符能否含 `$`（非首字符）。
    fn allows_dollar(&self) -> bool {
        false
    }

    /// 以下划线开头的名字能否不加引号。
    fn bare_leading_underscore(&self) -> bool {
        true
    }

    /// 未加引号的标识符会被折叠成小写（含大写的名字必须加引号才能指回原对象）。
    fn folds_to_lowercase(&self) -> bool {
        false
    }

    /// 该方言的保留字（大小写不敏感）。
    fn is_reserved(&self, name: &str) -> bool {
        let upper = name.to_ascii_uppercase();
        self.reserved_words().binary_search(&upper.as_str()).is_ok()
    }

    /// 名字能否不加引号直接写进该方言的 SQL。
    fn is_bare(&self, name: &str) -> bool {
        let mut chars = name.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if !(first.is_ascii_alphabetic() || first == '_') {
            return false;
        }
        if first == '_' && !self.bare_leading_underscore() {
            return false;
        }
        let dollar = self.allows_dollar();
        if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || (dollar && ch == '$')) {
            return false;
        }
        if self.folds_to_lowercase() && name.chars().any(|ch| ch.is_ascii_uppercase()) {
            return false;
        }
        !self.is_reserved(name)
    }

    /// 按需引号：安全标识符原样返回，其余按方言加引号并转义。
    fn quote_ident(&self, name: &str) -> String {
        if self.is_bare(name) {
            name.to_owned()
        } else {
            self.quote_always(name)
        }
    }

    /// 限定名：每段按需引号后用 `.` 连接，空段跳过。
    fn qualified(&self, parts: &[&str]) -> String {
        parts
            .iter()
            .filter(|part| !part.is_empty())
            .map(|part| self.quote_ident(part))
            .collect::<Vec<_>>()
            .join(".")
    }
}

/// 类型规则：类型候选与校验、默认值候选与校验、默认值字面量、自增表达、FK 动作、常用字段类型映射。（A3 填充）
pub trait TypeRules: Sync {}

/// DDL 方言：create/alter/drop/rename/truncate/索引/外键/视图/重建、注释、序列与枚举生成及动作门控。（A3 填充）
pub trait DdlDialect: Sync {}

/// 目录方言：元数据/对象列表/搜索/定义/结构内省/外键/关系图/统计 SQL 与结果解析。（A2 填充）
pub trait CatalogDialect: Sync {}

/// 错误规则：错误码/位置解析、冲突错误识别、连接失败分类。（A1/A4 填充）
pub trait ErrorRules: Sync {}

/// 一个引擎的适配器：汇总各能力组。不支持的组返回 `None`，并在 [`Capabilities`] 里声明为 `false`。
pub trait EngineAdapter: Sync {
    fn meta(&self) -> &dyn EngineMeta;
    fn capabilities(&self) -> Capabilities;
    fn syntax(&self) -> Option<&dyn SqlSyntax> {
        None
    }
    #[cfg_attr(not(test), allow(dead_code))] // A3 填充并接入
    fn types(&self) -> Option<&dyn TypeRules> {
        None
    }
    #[cfg_attr(not(test), allow(dead_code))] // A3 填充并接入
    fn ddl(&self) -> Option<&dyn DdlDialect> {
        None
    }
    #[cfg_attr(not(test), allow(dead_code))] // A2 填充并接入
    fn catalog(&self) -> Option<&dyn CatalogDialect> {
        None
    }
    #[cfg_attr(not(test), allow(dead_code))] // A1/A4 填充并接入
    fn errors(&self) -> Option<&dyn ErrorRules> {
        None
    }
    fn connector(&self) -> Option<&dyn Connector> {
        None
    }
}

/// 主机字段是不是整串连接串（按引擎规则）。
pub fn host_is_uri(engine: Engine, host: &str) -> bool {
    adapter(engine).meta().host_is_uri(host)
}

/// 没有 SQL 语法组的引擎（Redis/MongoDB）没有 SQL 生成面；万一被调用，按标准 SQL 双引号总是引用。
fn standard_quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// 门面：按需引号。
pub fn quote_ident(engine: Engine, name: &str) -> String {
    match adapter(engine).syntax() {
        Some(syntax) => syntax.quote_ident(name),
        None => standard_quote(name),
    }
}

/// 门面：总是引号。
#[cfg_attr(not(test), allow(dead_code))] // A2 起目录/网格的“总是引号”实现改走这里
pub fn quote_always(engine: Engine, name: &str) -> String {
    match adapter(engine).syntax() {
        Some(syntax) => syntax.quote_always(name),
        None => standard_quote(name),
    }
}

/// 门面：限定名（每段按需引号，空段跳过）。
pub fn qualified(engine: Engine, parts: &[&str]) -> String {
    match adapter(engine).syntax() {
        Some(syntax) => syntax.qualified(parts),
        None => parts.iter().filter(|part| !part.is_empty()).map(|part| standard_quote(part)).collect::<Vec<_>>().join("."),
    }
}

/// 门面：引擎以字符串给出时的按需引号；未知引擎按标准 SQL 双引号总是引用。
pub fn quote_ident_named(engine: &str, name: &str) -> String {
    match registry::parse(engine) {
        Some(engine) => quote_ident(engine, name),
        None => standard_quote(name),
    }
}

/// 专项工作台（Redis / Mongo）顶栏徽标使用的引擎 id。
pub fn workbench_badge_id(workbench: Workbench) -> &'static str {
    registry::workbench_engine(workbench).map(|engine| adapter(engine).meta().id()).unwrap_or_default()
}
