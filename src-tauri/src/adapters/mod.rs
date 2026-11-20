//! 引擎适配器层（ADR-0 / R22）。
//!
//! 每个引擎一个模块，按能力分组实现下列 trait；主代码只经 [`registry::adapter`] 取得
//! `&'static dyn EngineAdapter` 再调用能力组，不再对具体 `Engine` 变体分支。
//! `registry.rs` 是唯一允许对 `Engine` 穷举匹配、以及把引擎字符串解析成 `Engine` 的地方。
//!
//! 迁移分领域进行（A1 连接与会话 → A2 目录 → A3 DDL → A4 网格与查询 → A5 对比/转储/迁移/导出 →
//! A6 UI 元数据与收尾）。尚无实现的能力组在这里先定义接口骨架，由对应迁移任务填充；
//! 适配器在 [`Capabilities`] 里如实声明当前提供了哪些组，注册表完整性测试校验两者一致。

pub mod catalog;
pub mod ddl;
pub mod exec;
pub mod grid;
pub mod query;
pub mod registry;
pub mod session;
pub mod transfer;

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
use crate::query::exec::{CappedSql, DatabaseGate};
use crate::query::functions::FunctionSig;
use crate::query::plan::PlanNode;

pub use catalog::{catalog_of, CatalogDialect};
pub use ddl::{DdlDialect, TypeRules};
pub use registry::adapter;
pub use session::Connector;
pub use transfer::TransferDialect;

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
    /// 对比、全库导出、迁移与导出的方言组（A5）。
    pub transfer_group: bool,
    /// 会话绑定到单个库：切换库要改连，跨库读取要另开临时会话（不替换当前会话）。
    pub database_scoped_session: bool,
    /// 对象树有“库”这一层（单文件引擎没有，对象直接挂在连接下）。
    pub shows_databases: bool,
    /// 对象树在库下还有“模式”这一层。
    pub shows_schemas: bool,
    /// 连接级显示外键约束开关（文件型引擎在连接阶段读到）。
    pub shows_foreign_keys: bool,
    /// 导航先只读库名（与当前库的模式名），对象按分组展开时再分页读取。
    pub outline_navigation: bool,
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

/// 表单字段占位：原样示例文字、i18n 键或不显示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldHint {
    None,
    Text(&'static str),
    Key(&'static str),
}

impl Workbench {
    /// 有文本查询编辑器（SQL 编辑器或命令控制台）；文档工作台没有。
    pub fn has_query_editor(self) -> bool {
        !matches!(self, Workbench::Mongo)
    }
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

    /// 徽标：两字母缩写与品牌色（0xRRGGBB）。只做识别，不做商标。
    fn mark(&self) -> (&'static str, u32);

    /// 显示层可识别的别名（小写，不含 id 本身），如旧数据或连接串里的另一种写法。
    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// 本地数据库文件的扩展名（小写、不含点）；只有文件型引擎有。
    fn file_extensions(&self) -> &'static [&'static str] {
        &[]
    }

    /// 本地数据库文件的文件头魔数；打开文件时优先于扩展名判断。只有文件型引擎有。
    fn file_signature(&self) -> Option<&'static [u8]> {
        None
    }

    /// 连接表单“连接串”占位示例。
    fn url_example(&self) -> &'static str;

    /// 连接表单“数据库”字段占位文案的 i18n 键。
    fn database_hint_key(&self) -> &'static str {
        "ui.conn.optional"
    }

    /// 连接表单“用户名”字段的占位。
    fn username_hint(&self) -> FieldHint {
        FieldHint::None
    }

    /// 只由这个引擎生成的说明句（计划风险/阻断/步骤、读取与执行错误、对象定义提示）的英文句型，
    /// 规则见 `ddl/english.rs`。共用句型不放这里。
    fn messages_en(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    /// 这个引擎专属的重建定义“未包含”项的英文。
    fn missing_items_en(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

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

/// SQL 语法：标识符引号、保留字与限定名；网格的字面量、分页、计数与投影；查询的词法、库切换、超时、
/// 行数上限、执行计划与函数表（A4）。主代码经 [`query`] 门面按引擎取用。
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

    /// 对象树“生成 SELECT”插入的取样语句（只写草稿，不执行）。`table` 已按方言引用。
    fn sample_select(&self, table: &str, limit: usize) -> String {
        default_sample_select(table, limit)
    }

    /// 结构页“查看数据”复制的单行取样语句：取前 `limit` 行。`table` 已按方言引用。
    fn select_first_rows(&self, table: &str, limit: usize) -> String {
        format!("SELECT * FROM {table} LIMIT {limit};")
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

    // ---- A4 网格：字面量、匹配、分页与投影 ----

    /// Unicode 字符串字面量的前缀（T-SQL 的 `N`）；字面量与 LIKE 模式共用。
    fn national_prefix(&self) -> &'static str {
        ""
    }

    /// 文本字面量：单引号翻倍。
    fn text_literal(&self, value: &str) -> String {
        format!("{}'{}'", self.national_prefix(), value.replace('\'', "''"))
    }

    /// 筛选“包含”与整表关键字搜索使用的匹配运算符（不区分大小写的方言给出专用运算符）。
    fn contains_operator(&self) -> &'static str {
        "LIKE"
    }

    /// 分页尾段。`ordered` 表示语句已有 ORDER BY。
    fn page_tail(&self, limit: u64, offset: u64, _ordered: bool) -> String {
        if offset == 0 {
            format!(" LIMIT {limit}")
        } else {
            format!(" LIMIT {limit} OFFSET {offset}")
        }
    }

    /// 没有任何列值的插入。`table` 已按方言引用。
    fn insert_defaults(&self, table: &str) -> String {
        format!("INSERT INTO {table} DEFAULT VALUES")
    }

    /// 计数投影（结果读成 64 位整数）。
    fn count_expression(&self) -> &'static str {
        "COUNT(*)"
    }

    /// 既没有排序也没有主键时，这种对象（`table` / `view`）的稳定行序列。
    fn implicit_order(&self, _kind: &str) -> Option<&'static str> {
        None
    }

    /// 把列转成文本形态（网格投影与关键字匹配）。`ident` 已引用。
    fn text_cast(&self, ident: &str) -> String;

    /// 超过 `cap` 字节的文本投影成预览信封 `\u{1}t:字节数:预览`。`plain` 是 `text_cast` 的结果。
    fn text_preview(&self, plain: &str, cap: usize) -> String;

    /// 二进制列投影成长度信封 `\u{1}b:字节数`。
    fn binary_length(&self, ident: &str) -> String;

    /// 二进制列投影成不带前缀的小写十六进制（外层 NULL 判断由调用方包）。
    fn binary_hex(&self, ident: &str) -> String;

    /// 十六进制文本写成二进制字面量。
    fn binary_literal(&self, hex: &str) -> String {
        format!("X'{hex}'")
    }

    // ---- A4 查询：词法、库切换、超时、行数上限、执行计划 ----

    /// 语句切分与词法扫描需要知道的方言特征。
    fn lexis(&self) -> Lexis {
        Lexis::default()
    }

    /// 单文件引擎唯一的库名；其余引擎没有。
    fn fixed_database(&self) -> Option<&'static str> {
        None
    }

    /// 请求的库与会话当前库不同（且不为空）时的处理。
    fn database_gate(&self, requested: &str) -> DatabaseGate {
        if self.fixed_database() == Some(requested) {
            DatabaseGate::Ready
        } else {
            DatabaseGate::Refuse
        }
    }

    /// 本批要下发的会话级语句超时设置（毫秒，大于 0）。
    fn timeout_statements(&self, _ms: u64) -> Vec<String> {
        Vec::new()
    }

    /// 整批结束后复位会话级超时的语句。
    fn timeout_reset_statements(&self) -> Vec<String> {
        Vec::new()
    }

    /// 行数上限改写：返回 None 表示走通用的 LIMIT 改写；方言自有写法（TOP）时给出结果。
    fn cap_rows(&self, _sql: &str, _cap: usize) -> Option<CappedSql> {
        None
    }

    /// 补全里该方言独有的关键字。
    fn dialect_words(&self) -> &'static [&'static str] {
        &[]
    }

    /// 把一条（已去掉末尾分号、非 ANALYZE 的）语句包成只看执行计划的语句。
    fn explain(&self, body: &str) -> String {
        if body.to_ascii_uppercase().starts_with("EXPLAIN") {
            body.to_owned()
        } else {
            format!("EXPLAIN {body}")
        }
    }

    /// 本方言中与 EXPLAIN 同义、后面跟着语句时就是查看执行计划的关键字（大写，如 MySQL 的 DESC/DESCRIBE）。
    /// 计划入口把这类语句改写成 EXPLAIN 原样发送，不再包一层；后面跟表名时是查看表结构，在本地拒绝。
    fn explain_synonyms(&self) -> &'static [&'static str] {
        &[]
    }

    /// 按服务器版本包装执行计划语句。默认与 [`SqlSyntax::explain`] 相同；
    /// 计划格式随服务器版本变化的方言（MySQL）覆盖这个方法，版本未知时用保守格式。
    fn explain_for(&self, body: &str, _server_version: Option<&str>) -> String {
        self.explain(body)
    }

    /// 解析执行计划结果；`explained` 表示语句以 EXPLAIN / SET SHOWPLAN 开头。认不出返回 None。
    fn parse_plan(&self, _sql: &str, _explained: bool, _columns: &[String], _rows: &[Vec<Option<String>>]) -> Option<Vec<PlanNode>> {
        None
    }

    /// 常用内置函数的签名表。
    fn functions(&self) -> &'static [FunctionSig] {
        &[]
    }

    /// 方言专有、不属于标准 SQL 的语句关键字（大写）。
    fn statement_keywords(&self) -> &'static [&'static str] {
        &[]
    }

    /// 出现在语句代码任意位置即视为可能写入或改变服务器状态的方言词（大写），
    /// 叠加在通用写入词之上，供“再次执行”等只读判定使用。宁可多列：误判只会让语句改为载入待确认。
    fn side_effect_words(&self) -> &'static [&'static str] {
        &[]
    }

    /// 系统内部对象的名字前缀（补全里排在业务表之后）。
    fn system_object_prefixes(&self) -> &'static [&'static str] {
        &[]
    }

    /// 查询编辑器经执行通道列出对象（名字、类型两列）的语句；None 表示走目录读取。
    fn editor_object_listing(&self) -> Option<&'static str> {
        None
    }

    /// 查询编辑器经执行通道读一张表的列（name/type/notnull/pk 列）的语句。`table` 未引用。
    fn editor_column_listing(&self, _table: &str) -> Option<String> {
        None
    }
}

/// 语句切分与词法扫描的方言特征。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lexis {
    /// `#` 起行注释（MySQL 家族）。
    pub hash_comments: bool,
    /// T-SQL：`[name]` 括号标识符、独占一行的 GO 批次分隔与 BEGIN…END 块。
    pub transact: bool,
    /// 认 `DELIMITER` 客户端指令；不认的方言跳过它并提示。
    pub delimiter_command: bool,
    /// `/*! ... */`（及 MariaDB `/*M! ... */`）是服务器会执行的代码，不是注释（MySQL 家族）。
    pub executable_comments: bool,
    /// 字符串里的反斜杠转义下一个字符（MySQL 家族默认 sql_mode）：`'a\'b'` 是一个字符串。
    pub backslash_escapes: bool,
    /// 字符串里的反斜杠是否转义取决于会话设置或字符串前缀：MySQL 家族的 NO_BACKSLASH_ESCAPES/ANSI_QUOTES，
    /// PostgreSQL 的 E'…' 与 standard_conforming_strings。为 true 时，含反斜杠的字符串让
    /// 只读判定与单表证明保守拒绝（同一文本在不同设置下引号边界不同）。
    pub backslash_ambiguous: bool,
}

impl Default for Lexis {
    /// 标准 SQL：没有 `#` 注释与 T-SQL 写法，也不认 DELIMITER。
    fn default() -> Self {
        Self { hash_comments: false, transact: false, delimiter_command: false, executable_comments: false, backslash_escapes: false, backslash_ambiguous: false }
    }
}

impl Lexis {
    /// 引擎未知（或不是 SQL 引擎）时：保守地照常处理 DELIMITER。
    /// 可执行注释也按代码检查，只读判定因此更保守。
    pub const UNKNOWN: Lexis = Lexis { hash_comments: false, transact: false, delimiter_command: true, executable_comments: true, backslash_escapes: false, backslash_ambiguous: true };
}

/// 错误规则：错误码/位置解析、冲突错误识别、连接失败分类。（A5 起提供冲突识别）
pub trait ErrorRules: Sync {
    /// 写入失败是主键/唯一键冲突（迁移按“跳过”计，而不是失败）。各引擎措辞不同，`lower` 已转成小写。
    fn is_conflict(&self, lower: &str) -> bool;
}

/// 一个引擎的适配器：汇总各能力组。不支持的组返回 `None`，并在 [`Capabilities`] 里声明为 `false`。
pub trait EngineAdapter: Sync {
    fn meta(&self) -> &dyn EngineMeta;
    fn capabilities(&self) -> Capabilities;
    fn syntax(&self) -> Option<&dyn SqlSyntax> {
        None
    }
    fn types(&self) -> Option<&'static dyn TypeRules> {
        None
    }
    fn ddl(&self) -> Option<&'static dyn DdlDialect> {
        None
    }
    fn catalog(&self) -> Option<&dyn CatalogDialect> {
        None
    }
    fn errors(&self) -> Option<&dyn ErrorRules> {
        None
    }
    fn connector(&self) -> Option<&dyn Connector> {
        None
    }
    fn transfer(&self) -> Option<&'static dyn TransferDialect> {
        None
    }
}

/// 门面：主键/唯一键冲突的识别。任一已注册引擎的错误规则认出即算（写入目标的措辞由各自适配器给出）。
pub fn is_conflict_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    registry::ENGINES.into_iter().filter_map(|engine| adapter(engine).errors()).any(|rules| rules.is_conflict(&lower))
}

/// 门面：会话句柄属于这个引擎（取引擎的结构方言判断；没有结构方言的引擎不匹配任何关系会话）。
pub fn session_matches(engine: Engine, session: &crate::connections::CatalogSession) -> bool {
    adapter(engine).ddl().is_some_and(|dialect| dialect.matches_session(session))
}

/// 门面：会话句柄所属的第一个引擎（共用会话形态的引擎取注册表顺序里靠前者）。
pub fn engine_of_session(session: &crate::connections::CatalogSession) -> Option<Engine> {
    registry::ENGINES.into_iter().find(|engine| session_matches(*engine, session))
}

/// 门面：单文件引擎唯一的库名（没有“库”这一层的引擎）；按引擎 id 查，解析不出或有库层时为 None。
pub fn fixed_database_named(engine: &str) -> Option<&'static str> {
    let engine = registry::parse(engine)?;
    if adapter(engine).capabilities().shows_databases {
        return None;
    }
    adapter(engine).syntax().and_then(|syntax| syntax.fixed_database())
}

/// 门面：引擎在库下还有“模式”一层（对比、导出要先选模式）。按引擎 id 查。
pub fn shows_schemas_named(engine: &str) -> bool {
    registry::parse(engine).is_some_and(|engine| adapter(engine).capabilities().shows_schemas)
}

/// 门面：引擎显示名；解析不出时原样返回 id。
pub fn label_named(engine: &str) -> String {
    registry::parse(engine).map(|engine| adapter(engine).meta().label().to_owned()).unwrap_or_else(|| engine.to_owned())
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

fn default_sample_select(table: &str, limit: usize) -> String {
    format!("SELECT *\nFROM {table}\nLIMIT {limit};")
}

/// 门面：对象树“生成 SELECT”的取样语句。没有 SQL 语法组的引擎按通用写法。
pub fn sample_select(engine: Engine, table: &str, limit: usize) -> String {
    match adapter(engine).syntax() {
        Some(syntax) => syntax.sample_select(table, limit),
        None => default_sample_select(table, limit),
    }
}

/// 各方言的标识符引号（开、闭）的缓存。词法扫描在热循环里逐字符判断，不能每次重新收集。
fn cached_identifier_quotes() -> &'static [(char, char)] {
    static QUOTES: std::sync::OnceLock<Vec<(char, char)>> = std::sync::OnceLock::new();
    QUOTES.get_or_init(identifier_quotes)
}

/// 这个字符开始一个被引号引起来的标识符时，返回它的闭合字符。
pub fn identifier_quote_close(open: char) -> Option<char> {
    cached_identifier_quotes().iter().find(|(start, _)| *start == open).map(|(_, close)| *close)
}

/// 开闭相同的标识符引号（双引号、反引号）：按“同字符加倍转义”扫描。
pub fn is_symmetric_identifier_quote(ch: char) -> bool {
    cached_identifier_quotes().iter().any(|(open, close)| *open == ch && *close == ch)
}

/// 各方言的标识符引号（开、闭），去重。SQL 文本的通用词法（切语句、读视图头）据此认出被引起来的名字。
pub fn identifier_quotes() -> Vec<(char, char)> {
    let mut quotes = Vec::new();
    for syntax in registry::ENGINES.into_iter().filter_map(|engine| adapter(engine).syntax()) {
        let empty = syntax.quote_always("");
        let mut chars = empty.chars();
        if let (Some(open), Some(close)) = (chars.next(), chars.next()) {
            if !quotes.contains(&(open, close)) {
                quotes.push((open, close));
            }
        }
    }
    quotes
}

/// 门面：结构页“查看数据”的取样语句。没有 SQL 语法组的引擎按通用写法。
pub fn first_rows_select(engine: Engine, table: &str, limit: usize) -> String {
    match adapter(engine).syntax() {
        Some(syntax) => syntax.select_first_rows(table, limit),
        None => format!("SELECT * FROM {table} LIMIT {limit};"),
    }
}

/// 门面：对象的限定名（按引擎层级取库或模式作容器段，每段按需引号）。
pub fn qualified_object(engine: Engine, database: &str, schema: &str, name: &str) -> String {
    qualified(engine, &[catalog_of(engine).name_qualifier(database, schema), name])
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

/// 门面：按引擎名（id 或别名，大小写不敏感）判断是否关系型；认不出的名字按关系型处理。
pub fn is_relational_named(engine: &str) -> bool {
    registry::parse_loose(engine).is_none_or(|engine| adapter(engine).capabilities().relational)
}

/// 门面：按引擎名（id 或别名）取工作台类型；认不出时为 None。
pub fn workbench_loose(engine: &str) -> Option<Workbench> {
    registry::parse_loose(engine).map(|engine| adapter(engine).meta().workbench())
}

/// 门面：文件开头的字节属于哪种文件型引擎的数据库文件（按文件头魔数）。
pub fn engine_for_file_header(header: &[u8]) -> Option<Engine> {
    registry::ENGINES
        .into_iter()
        .find(|engine| adapter(*engine).meta().file_signature().is_some_and(|signature| header.starts_with(signature)))
}

/// 门面：判断文件头需要读取的最大字节数。
pub fn file_signature_len() -> usize {
    registry::ENGINES.into_iter().filter_map(|engine| adapter(engine).meta().file_signature()).map(<[u8]>::len).max().unwrap_or(0)
}

/// 门面：扩展名（不含点，大小写不敏感）属于哪种文件型引擎。
pub fn engine_for_file_extension(ext: &str) -> Option<Engine> {
    let ext = ext.to_ascii_lowercase();
    registry::ENGINES.into_iter().find(|engine| adapter(*engine).meta().file_extensions().contains(&ext.as_str()))
}

/// 门面：具备某项能力的引擎显示名（注册表顺序），用 `separator` 连接。用于“只有 X 需要…”一类说明，
/// 说明里不写死引擎名。
pub fn labels_with(capability: impl Fn(Capabilities) -> bool, separator: &str) -> String {
    registry::ENGINES
        .into_iter()
        .filter(|engine| capability(adapter(*engine).capabilities()))
        .map(|engine| adapter(engine).meta().label())
        .collect::<Vec<_>>()
        .join(separator)
}

/// 门面：各引擎专属说明句的英文句型表（注册表顺序）。
pub fn engine_messages_en() -> impl Iterator<Item = &'static [(&'static str, &'static str)]> {
    registry::ENGINES.into_iter().map(|engine| adapter(engine).meta().messages_en())
}

/// 门面：各引擎专属的重建定义“未包含”项英文表（注册表顺序）。
pub fn engine_missing_items_en() -> impl Iterator<Item = &'static [(&'static str, &'static str)]> {
    registry::ENGINES.into_iter().map(|engine| adapter(engine).meta().missing_items_en())
}
