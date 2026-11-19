//! 对比、全库导出、迁移与导出的方言（ADR-0 / R22，A5）。
//!
//! 主代码（`compare/`、`dump/`、`migrate/`、`export/`）只经 [`transfer_of`] 取得方言再调用：脚本的排序、
//! 依赖与替换判断等算法留在主代码，各方言专属的改表写法、默认值、枚举/序列原地改写、SQLite 重建、
//! 范围校验与会话读取写在 `adapters/<engine>/transfer.rs`。没有关系面的引擎（Redis/MongoDB）不提供这一组，
//! 门面按通用写法兜底（对比/导出入口已按能力门控，不会走到）。

use crate::compare::model::{ObjectDef, Side, TableShape};
use crate::compare::structure::{primary_columns, primary_key_name};
use crate::connections::error_codes::{coded_string, DUMP_ENGINE_UNSUPPORTED, MIGRATE_MONGO_NOT_TARGET, MIGRATE_NOT_QUERYABLE, MIGRATE_REDIS_NOT_TARGET};
use crate::connections::model::Engine;
use crate::connections::CatalogSession;

use super::{adapter, mysql, postgres, qualified_object, quote_ident, sqlite, sqlserver};

/// 两侧引擎不同（同一家族也算不同）时对比拒绝生成脚本的说明。
pub const ENGINE_MISMATCH: &str = "两侧引擎不同。MySQL 与 MariaDB 是不同引擎，不生成脚本。";

/// 迁移预检：布尔写进数值列的提示（举出会拒绝的引擎）。
pub const BOOL_TO_NUMBER_NOTE: &str = "布尔写数值列在部分引擎（如 PostgreSQL）会被拒绝。";

/// 对比选择 schema：只有库下还有“模式”一层的引擎需要（按注册表列出显示名）。
pub fn schema_engines_note() -> String {
    format!("只有 {} 需要选择 schema", super::labels_with(|caps| caps.shows_schemas, " 和 "))
}

/// 把对照上的一张表改成基准的结构。
pub struct TableAlter<'a> {
    /// 脚本执行的一侧（对照）。
    pub side: &'a Side,
    /// 基准一侧（读基准上的类型定义用）。
    pub baseline_side: &'a Side,
    pub name: &'a str,
    pub baseline: &'a TableShape,
    pub target: &'a TableShape,
    /// 外键读全且没被阻断：重建时可以带上外键。
    pub with_fk: bool,
    /// 主键可信：可以增删主键。
    pub with_pk: bool,
}

impl TableAlter<'_> {
    /// 对照一侧这张表的限定名。
    pub fn table(&self, engine: Engine) -> String {
        qualify(engine, self.side, self.name)
    }
}

/// 序列定义里读到的参数（原地改写序列用）。空串表示没有写明。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SequenceSpec {
    pub data_type: String,
    pub increment: String,
    pub min_value: String,
    pub max_value: String,
    pub start: String,
    pub cache: String,
    pub cycle: bool,
}

/// 视图定义不同时能否就地替换（CREATE OR REPLACE VIEW）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewReplace {
    /// 没有就地替换，只能先摘依赖再删除重建。
    Never,
    /// 两边都不是物化视图就能替换。
    Plain,
    /// 还要能证明输出列兼容（新列清单以旧列清单开头）。
    MatchingColumns,
}

/// 类型与序列的原地改写：仍被列使用的枚举只加标签、序列只改参数，换类型时先把列转成文本再转回。
/// 只有支持这些写法的方言提供（`TransferDialect::in_place_types`）。名字参数都已限定，列名未引用。
pub trait InPlaceTypes: Sync {
    /// 给枚举加一个标签（可指定插在哪个已有标签之前）。
    fn enum_add_value(&self, type_name: &str, label: &str, before: Option<&str>) -> String;
    /// 按定义里的参数改写序列。
    fn sequence_alter(&self, sequence: &str, spec: &SequenceSpec) -> String;
    /// 把列临时转成文本，旧类型才能删除。
    fn detach_column_type(&self, table: &str, column: &str) -> String;
    /// 把列转回重建后的类型。
    fn restore_column_type(&self, table: &str, column: &str, type_name: &str) -> String;
    /// 序列重新归属到列。
    fn sequence_owned_by(&self, sequence: &str, owner_table: &str, column: &str) -> String;
    /// 删除被列拥有的序列之前先解除归属。
    fn sequence_disown(&self, sequence: &str) -> String;
}

/// 对比/导出/迁移的方言。脚本生成是纯函数；读取在已打开的会话上进行，不新开连接。
#[async_trait::async_trait]
pub trait TransferDialect: Sync {
    // ---- 结构对比：表 ----

    /// 外键只能写在 CREATE TABLE 里（不能事后增删外键约束，差异只能重建表）。
    fn inline_foreign_keys(&self) -> bool {
        false
    }

    /// 主键来自列信息（索引没读到时主键仍然可信）。
    fn primary_key_from_columns(&self) -> bool {
        false
    }

    /// 列的类型/可空/默认值、主键或外键不同时必须重建整张表。
    fn needs_rebuild(&self, _baseline: &TableShape, _target: &TableShape, _with_fk: bool) -> bool {
        false
    }

    /// 把对照表改成基准表的语句（列增删改与主键）。
    fn alter_table(&self, alter: &TableAlter<'_>) -> Vec<String>;

    /// 删除索引。`table` 未限定。
    fn drop_index(&self, side: &Side, table: &str, index: &str) -> String;

    /// 删除顺序只看同一模式里的外键目标（引用别的模式的表不算本批依赖）。
    fn drop_order_same_schema_only(&self) -> bool {
        false
    }

    /// 删除触发器。
    fn drop_trigger(&self, side: &Side, object: &ObjectDef) -> String;

    /// 删除函数或过程。`word` 是 FUNCTION / PROCEDURE。
    fn drop_routine(&self, side: &Side, word: &str, object: &ObjectDef) -> String;

    /// 触发器名不能带限定（模式只写在 ON 后面的表上）。
    fn trigger_name_unqualified(&self) -> bool {
        false
    }

    /// 去掉列默认值。`table` 已限定，`column` 未引用。
    fn drop_default(&self, table: &str, column: &str) -> String;

    /// 设置列默认值。`table` 已限定，`column` 未引用。
    fn set_default(&self, table: &str, column: &str, expression: &str) -> String;

    /// 视图定义不同时能否就地替换。
    fn view_replace(&self) -> ViewReplace {
        ViewReplace::Never
    }

    /// 返回类型没变的函数/过程能用 CREATE OR REPLACE 就地替换。
    fn replaces_routine_in_place(&self) -> bool {
        false
    }

    /// 视图与例程互相引用成环时，先把例程换成不引用对方的桩再删或建。
    fn breaks_view_routine_cycles(&self) -> bool {
        false
    }

    /// 例程桩：保留语句头（签名与返回类型），正文不再引用任何对象。
    fn routine_stub(&self, _header: &str, _procedure: bool, _returns: &str) -> Option<String> {
        None
    }

    /// 枚举/域/序列的原地改写。
    fn in_place_types(&self) -> Option<&'static dyn InPlaceTypes> {
        None
    }

    /// 两侧的库/模式选择对这个方言不成立时的拒绝原因（不生成脚本）。
    fn compare_scope_refusal(&self, _baseline: &Side, _target: &Side) -> Option<String> {
        None
    }

    // ---- 全库导出 ----

    /// 自增列在建表语句里的子句。
    fn dump_auto_clause(&self) -> &'static str {
        ""
    }

    /// 数据库保存的建表原文最保真（外键内联在原文里），导出直接用它。
    fn keeps_table_source(&self) -> bool {
        false
    }

    /// 解析导出范围：(限定用的库, 模式列表)。只认当前会话的库，模式缺省为全部。
    async fn dump_scope(&self, session: &CatalogSession, database: &str, schemas: &[String]) -> Result<(String, Vec<String>), String>;

    // ---- 对比读取 ----

    /// 校验对比要读的库/模式在当前会话上存在。
    async fn compare_scope(&self, session: &CatalogSession, database: &str, schema: &str) -> Result<(), String>;

    /// 会话当前连着的库（只有会话绑定单库的方言需要；其余为空串）。
    async fn session_database(&self, _session: &CatalogSession) -> Result<String, String> {
        Ok(String::new())
    }

    /// 执行目录的模式清单语句，读出模式名。
    async fn schema_names(&self, _session: &CatalogSession, _sql: &str) -> Result<Vec<String>, String> {
        Err("这个引擎没有模式层".to_owned())
    }

    /// 序列被哪张表的哪一列拥有：`表` 或 `表\u{1}列`。
    async fn sequence_owner(&self, _session: &CatalogSession, _schema: &str, _name: &str) -> Option<String> {
        None
    }
}

/// 对象在一侧的限定名（按引擎层级取库或模式作容器段，每段按需引号）。
pub fn qualify(engine: Engine, side: &Side, name: &str) -> String {
    qualified_object(engine, &side.database, &side.schema, name)
}

/// 没有对比/导出面的引擎（Redis/MongoDB，也没有 SQL 语法组）的兜底：按标准 SQL 双引号写，读取一律拒绝。
/// 入口已按能力门控，正常走不到这里。
struct GenericTransfer;

fn generic_quote(name: &str) -> String {
    super::standard_quote(name)
}

#[async_trait::async_trait]
impl TransferDialect for GenericTransfer {
    fn alter_table(&self, _alter: &TableAlter<'_>) -> Vec<String> {
        Vec::new()
    }

    fn drop_index(&self, _side: &Side, _table: &str, index: &str) -> String {
        format!("DROP INDEX {}", generic_quote(index))
    }

    fn drop_trigger(&self, _side: &Side, object: &ObjectDef) -> String {
        format!("DROP TRIGGER {}", generic_quote(&object.name))
    }

    fn drop_routine(&self, _side: &Side, word: &str, object: &ObjectDef) -> String {
        format!("DROP {word} {}", generic_quote(&object.name))
    }

    fn drop_default(&self, table: &str, column: &str) -> String {
        format!("ALTER TABLE {table} ALTER COLUMN {} DROP DEFAULT", generic_quote(column))
    }

    fn set_default(&self, table: &str, column: &str, expression: &str) -> String {
        format!("ALTER TABLE {table} ALTER COLUMN {} SET DEFAULT {expression}", generic_quote(column))
    }

    async fn dump_scope(&self, _session: &CatalogSession, _database: &str, _schemas: &[String]) -> Result<(String, Vec<String>), String> {
        Err(coded_string(DUMP_ENGINE_UNSUPPORTED, "这个引擎没有整库导出。"))
    }

    async fn compare_scope(&self, _session: &CatalogSession, _database: &str, _schema: &str) -> Result<(), String> {
        Err("这个引擎没有结构对比".to_owned())
    }
}

/// 门面：引擎的对比/导出方言；没有这一组的引擎按通用写法兜底。
pub fn transfer_of(engine: Engine) -> &'static dyn TransferDialect {
    adapter(engine).transfer().unwrap_or(&GenericTransfer)
}

/// 门面：按引擎 id 取有关系面的引擎（对比、导出只认这些）。
pub fn relational_engine(id: &str) -> Result<Engine, String> {
    super::registry::parse(id).filter(|engine| adapter(*engine).capabilities().relational).ok_or_else(|| format!("不支持的数据库 {id}"))
}

/// 主键按约束增删（PostgreSQL、SQL Server）：先删旧主键约束（名字没读到时按 `<表>_pkey`），再加新主键。
pub(crate) fn constraint_primary_key(engine: Engine, table: &str, name: &str, baseline: &TableShape, target: &TableShape, statements: &mut Vec<String>) {
    let wanted = primary_columns(baseline);
    let current = primary_columns(target);
    if wanted == current {
        return;
    }
    if !current.is_empty() {
        let constraint = primary_key_name(target).unwrap_or_else(|| format!("{name}_pkey"));
        statements.push(format!("ALTER TABLE {table} DROP CONSTRAINT {}", quote_ident(engine, &constraint)));
    }
    if !wanted.is_empty() {
        let names = wanted.iter().map(|column| quote_ident(engine, column)).collect::<Vec<_>>().join(", ");
        statements.push(format!("ALTER TABLE {table} ADD PRIMARY KEY ({names})"));
    }
}

/// 导出读取失败的统一前缀。
pub(crate) fn read_failure(err: impl Into<crate::error_text::Reason>) -> String {
    let err = err.into();
    format!("读取失败：{err}")
}

/// 会话与引擎对不上（导出）。
pub(crate) fn dump_mismatch() -> String {
    coded_string(crate::connections::error_codes::DUMP_CONNECTION_MISMATCH, "连接类型和引擎不一致")
}

/// 会话当前库与要导出的库不同：不另开连接。
pub(crate) fn dump_session_gap(current: &str, database: &str) -> String {
    crate::connections::error_codes::coded_string_params(
        crate::connections::error_codes::DUMP_SESSION_GAP,
        &[("current", current), ("database", database)],
        format!("当前连接打开的是 {current}，没有另开连接读取 {database}"),
    )
}

/// 会话与引擎对不上（对比）。
pub(crate) fn compare_mismatch() -> String {
    "会话类型和目标引擎不一致".to_owned()
}

impl CatalogSession {
    /// 迁移写入：一批放进一个事务，整批成功才提交；失败即回滚并返回错误（调用方降级逐行重放）。
    pub async fn insert_in_transaction(&self, statement: &str) -> Result<(), String> {
        match self {
            Self::Scripted => Err(coded_string(MIGRATE_NOT_QUERYABLE, "这个连接没有连着可查询的数据库")),
            // Redis 不在迁移目标面（capability 门控）；误入时拒绝写入。
            Self::Redis(_) => Err(coded_string(MIGRATE_REDIS_NOT_TARGET, "Redis 不是迁移目标；数据经键命令写入。")),
            Self::Mongo(_) => Err(coded_string(MIGRATE_MONGO_NOT_TARGET, "MongoDB 不是迁移目标；文档经集合浏览器写入。")),
            Self::Sqlite { pool, .. } => sqlite::transfer::insert_in_transaction(pool, statement).await,
            Self::Mysql(pool) => mysql::transfer::insert_in_transaction(pool, statement).await,
            Self::Postgres(client) => postgres::transfer::insert_in_transaction(client, statement).await,
            Self::Sqlserver(client) => sqlserver::transfer::insert_in_transaction(client, statement).await,
        }
    }

    /// 迁移逐行重放的单条自动提交语句（降级路径专用）。
    pub async fn execute_autocommit(&self, statement: &str) -> Result<(), String> {
        match self {
            Self::Scripted => Err(coded_string(MIGRATE_NOT_QUERYABLE, "这个连接没有连着可查询的数据库")),
            Self::Redis(_) => Err(coded_string(MIGRATE_REDIS_NOT_TARGET, "Redis 不是迁移目标；数据经键命令写入。")),
            Self::Mongo(_) => Err(coded_string(MIGRATE_MONGO_NOT_TARGET, "MongoDB 不是迁移目标；文档经集合浏览器写入。")),
            Self::Sqlite { pool, .. } => sqlite::transfer::execute(pool, statement).await,
            Self::Mysql(pool) => mysql::transfer::execute(pool, statement).await,
            Self::Postgres(client) => postgres::transfer::execute(client, statement).await,
            Self::Sqlserver(client) => sqlserver::transfer::execute(client, statement).await,
        }
    }
}
