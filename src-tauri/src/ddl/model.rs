use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use crate::connections::model::Engine;

#[derive(Clone, Debug)]
pub struct Column {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub default_sql: Option<String>,
    pub primary: bool,
    pub auto: bool,
    pub generated: bool,
    /// SQL Server 的默认值是具名约束（sys.default_constraints）；改/删默认值需要这个名字。其余引擎为 None。
    pub default_constraint: Option<String>,
}

#[derive(Clone, Debug)]
pub struct IndexColumn {
    pub name: String,
    pub descending: bool,
}

#[derive(Clone, Debug)]
pub struct Index {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<IndexColumn>,
    pub protected: bool,
    pub reason: String,
    pub partial: bool,
    pub expression: bool,
    pub sql: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: String,
    pub on_update: String,
}

#[derive(Clone, Debug)]
pub struct IncomingForeignKey {
    pub name: String,
    pub table: String,
    pub schema: String,
    pub columns: Vec<String>,
    pub ref_columns: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct DependentSql {
    pub name: String,
    pub sql: String,
}

/// 序列参数。读不到的字段是 None：plan 不会用猜出来的当前值判断“没有变化”。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SequenceState {
    pub start: Option<i64>,
    pub increment: Option<i64>,
    pub min_value: Option<i64>,
    pub max_value: Option<i64>,
    pub cycle: Option<bool>,
}

/// 域的默认值与 NOT NULL。默认值为 None 表示这个域没有默认值。
#[derive(Clone, Debug, Default)]
pub struct DomainState {
    pub default_sql: Option<String>,
    pub not_null: bool,
}

/// SQLite 已附加的库。main 与 temp 也在列表里，它们的 path 为空。
#[derive(Clone, Debug, Default)]
pub struct AttachedDatabase {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub engine: Engine,
    pub database: String,
    pub schema: String,
    pub name: String,
    pub kind: String,
    pub storage: Option<String>,
    pub sqlite_version: String,
    pub recorded_foreign_keys: Option<bool>,
    pub live_foreign_keys: Option<bool>,
    pub columns: Vec<Column>,
    pub indexes: Vec<Index>,
    pub foreign_keys: Vec<ForeignKey>,
    pub incoming: Vec<IncomingForeignKey>,
    pub triggers: Vec<DependentSql>,
    pub views: Vec<DependentSql>,
    pub table_sql: String,
    pub has_check: bool,
    pub has_collate: bool,
    pub without_rowid: bool,
    pub strict: bool,
    pub autoincrement: bool,
    pub row_count: Option<i64>,
    pub null_counts: BTreeMap<String, i64>,
    pub object_exists: bool,
    pub reference: Option<Box<Snapshot>>,
    /// PostgreSQL 物化视图。替换会丢掉已经物化的数据，因此不能发送。
    pub materialized: bool,
    /// 视图原文读失败时的服务器错误。空字符串式的占位注释不能当作原文。
    pub definition_error: Option<String>,
    /// 索引查询失败。列已经读到，不能把整表当成没读到。
    pub index_error: Option<String>,
    /// 外键查询失败。列已经读到，不能把整表当成没读到。
    pub foreign_key_error: Option<String>,
    /// PostgreSQL 序列的现有参数。只有序列动作会读。
    pub sequence: Option<SequenceState>,
    /// PostgreSQL 枚举的现有取值，按定义顺序。空表示没读到或不是枚举。
    pub enum_values: Vec<String>,
    /// PostgreSQL 域的现有默认值与 NOT NULL。
    pub domain: Option<DomainState>,
    /// MySQL 事件的启用状态。None 表示没读到或事件不存在。
    pub event_enabled: Option<bool>,
    /// SQLite 的 foreign_keys / journal_mode / user_version 现值。
    pub pragmas: BTreeMap<String, String>,
    /// SQLite 已附加的库（含 main 与 temp）。
    pub attached: Vec<AttachedDatabase>,
}

impl Snapshot {
    pub fn bare(engine: Engine, database: &str, schema: &str, name: &str) -> Self {
        Self {
            engine,
            database: database.to_owned(),
            schema: schema.to_owned(),
            name: name.to_owned(),
            kind: "table".to_owned(),
            storage: None,
            sqlite_version: "3.51.3".to_owned(),
            recorded_foreign_keys: Some(false),
            live_foreign_keys: Some(false),
            columns: Vec::new(),
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            incoming: Vec::new(),
            triggers: Vec::new(),
            views: Vec::new(),
            table_sql: format!("CREATE TABLE {name} (id INTEGER)"),
            has_check: false,
            has_collate: false,
            without_rowid: false,
            strict: false,
            autoincrement: false,
            row_count: Some(0),
            null_counts: BTreeMap::new(),
            object_exists: true,
            reference: None,
            materialized: false,
            definition_error: None,
            index_error: None,
            foreign_key_error: None,
            sequence: None,
            enum_values: Vec::new(),
            domain: None,
            event_enabled: None,
            pragmas: BTreeMap::new(),
            attached: Vec::new(),
        }
    }
}

#[cfg(test)]
impl Snapshot {
    pub(crate) fn sample_index_failure() -> Self {
        let mut snapshot = Self::bare(Engine::Postgres, "app", "public", "orders");
        snapshot.columns.push(Column {
            name: "note".to_owned(),
            type_name: "text".to_owned(),
            nullable: true,
            default_sql: None,
            primary: false,
            auto: false,
            generated: false,
            default_constraint: None,
        });
        snapshot.index_error = Some("permission denied".to_owned());
        snapshot.foreign_key_error = Some("fk denied".to_owned());
        snapshot
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDraft {
    pub original: Option<String>,
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub default_sql: String,
    pub primary: bool,
    pub auto: bool,
    pub dropped: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexColumnDraft {
    pub name: String,
    pub descending: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyDraft {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: String,
    pub on_update: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Action {
    CreateTable { name: String, columns: Vec<ColumnDraft> },
    AlterColumns { table: String, columns: Vec<ColumnDraft> },
    CreateIndex { table: String, name: String, unique: bool, columns: Vec<IndexColumnDraft> },
    DropIndex { table: String, name: String },
    CreateForeignKey { table: String, foreign_key: ForeignKeyDraft },
    DropForeignKey { table: String, name: String },
    DropTable { table: String },
    /// 只改变当前 SQLite 连接的 foreign_keys。不清理数据，也不表示约束已经验证通过。
    SetForeignKeys { enabled: bool },
    /// 替换已有视图的定义。确认前只生成语句，不发送。
    ReplaceView { table: String, definition: String },
    /// 改 PostgreSQL 序列的参数。只有填了值的子句会进语句。
    AlterSequence { sequence: String, start: Option<i64>, increment: Option<i64>, min_value: Option<i64>, max_value: Option<i64>, cycle: Option<bool> },
    /// 给 PostgreSQL 枚举追加一个值。枚举值只能追加，不能删除或重排。
    AddEnumValue { enum_name: String, value: String, before: Option<String> },
    /// 改 PostgreSQL 域的默认值与 NOT NULL。默认值为空串表示删掉默认值。
    AlterDomain { domain: String, default_sql: Option<String>, not_null: Option<bool> },
    /// 启停 MySQL/MariaDB 事件。事件的事件体仍然只读。
    SetEventEnabled { event: String, enabled: bool },
    /// 改 SQLite 的 PRAGMA 值。foreign_keys 走 SetForeignKeys，这里不重复。
    SetPragma { pragma: String, value: String },
    /// 附加一个 SQLite 数据库文件。只对当前连接生效。
    AttachDatabase { path: String, name: String },
    /// SQLite 维护：整理数据库文件。确认前不写入。
    Vacuum,
    /// SQLite 维护：重建索引。不带名字时重建整个库的索引。
    Reindex { target: Option<String> },
}

#[derive(Clone, Debug)]
pub struct Request {
    pub database: String,
    pub schema: String,
    pub engine: Engine,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub sql: String,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct Risk {
    pub level: String,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub blocked: Vec<String>,
    pub risks: Vec<Risk>,
    pub steps: Vec<Step>,
    pub rule: String,
    pub rebuild: bool,
    pub target: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Ok,
    Failed,
    Skipped,
    Unknown,
    RolledBack,
}

#[derive(Clone, Debug)]
pub struct StepReceipt {
    pub sql: String,
    pub label: String,
    pub status: StepStatus,
    pub message: String,
}

pub fn fk_actions(engine: Engine) -> &'static [&'static str] {
    match engine {
        // Redis 没有结构编辑（capability 门控），空清单保持穷尽。
        Engine::Redis | Engine::Mongodb => &[],
        Engine::Mysql | Engine::Mariadb => &["RESTRICT", "CASCADE", "SET NULL", "NO ACTION"],
        Engine::Postgres | Engine::Sqlite | Engine::Sqlserver => &["RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT", "NO ACTION"],
    }
}

pub fn rule_text(engine: Engine, rebuild: bool) -> String {
    if rebuild {
        return "SQLite 按官方步骤重建：先在事务外关闭外键检查，再在一个事务里建新表、复制数据、删除旧表并改回原名，然后做 foreign_key_check（外键完整性检查）。这只属于这次重建，不是所有数据库都能回滚。事务内失败会回滚这次重建；事务外改过的 foreign_keys 会恢复为会话原来的值。".to_owned();
    }
    match engine {
        Engine::Mysql | Engine::Mariadb => "这些语句逐条执行，失败即停。MySQL 与 MariaDB 会立即提交每条结构变更语句，中途失败不会撤销已经成功的语句，不能声称已经全部回滚。".to_owned(),
        Engine::Postgres => "这些语句逐条执行，失败即停。已经成功的语句不撤销。PostgreSQL 虽然可以把结构变更放进事务，这里也不包成一个大事务，不能据此认为所有数据库都会回滚。".to_owned(),
        Engine::Sqlite => "这些语句逐条执行，失败即停。已经成功的语句不撤销，后面的语句不执行。".to_owned(),
        Engine::Sqlserver => "这些语句逐条执行，失败即停。已成功的语句不撤销；这里也不把整批包进一个事务，不能据此声称可以整体回滚。".to_owned(),
        // Redis 没有结构编辑（capability 门控）。
        Engine::Redis => "Redis 没有结构编辑。".to_owned(),
        Engine::Mongodb => "MongoDB 没有结构编辑。".to_owned(),
    }
}
