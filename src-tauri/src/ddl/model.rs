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
    /// 默认值是具名约束的引擎（SQL Server）记下约束名；改/删默认值需要这个名字。其余引擎为 None。
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
    /// 改名的目标名是否已经被同一命名空间里的对象占用。None 表示没查过：改名计划不会当它没被占用。
    pub name_taken: Option<bool>,
    /// 表注释。只有读定义时才去读；None 表示没有注释或没读。
    pub table_comment: Option<String>,
    /// 列注释，按列名。只有读定义时才去读。
    pub column_comments: BTreeMap<String, String>,
    /// 注释查询失败。定义仍可重建，但要说明注释没读到。
    pub comment_error: Option<String>,
    /// 生成这份计划时同连接的用户事务状态。适配器据此把事务内不能执行或要改写边界的步骤
    /// 在计划里说明清楚（例如 SQLite 重建改用保存点）。默认 `Idle`，表示没有活动事务。
    pub transaction: crate::transaction::TransactionState,
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
            name_taken: None,
            table_comment: None,
            column_comments: BTreeMap::new(),
            comment_error: None,
            transaction: crate::transaction::TransactionState::Idle,
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

/// 列默认值的四种状态（ADR-7）。无默认值不写 DEFAULT；NULL 写 `DEFAULT NULL`；字面量由方言按列类型
/// 加引号与转义；表达式只接受白名单函数并原样写出。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value", rename_all = "camelCase")]
pub enum DefaultSpec {
    #[default]
    None,
    Null,
    Literal(String),
    Expression(String),
}

/// 默认值模式（界面下拉用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultMode {
    None,
    Null,
    Literal,
    Expression,
}

impl DefaultMode {
    pub const ALL: [DefaultMode; 4] = [DefaultMode::None, DefaultMode::Null, DefaultMode::Literal, DefaultMode::Expression];
}

impl DefaultSpec {
    pub fn mode(&self) -> DefaultMode {
        match self {
            DefaultSpec::None => DefaultMode::None,
            DefaultSpec::Null => DefaultMode::Null,
            DefaultSpec::Literal(_) => DefaultMode::Literal,
            DefaultSpec::Expression(_) => DefaultMode::Expression,
        }
    }

    /// 字面量或表达式的原文；其余为空串。
    pub fn text(&self) -> &str {
        match self {
            DefaultSpec::Literal(text) | DefaultSpec::Expression(text) => text,
            _ => "",
        }
    }

    /// 有非 NULL 的默认值（字面量或非空表达式）。
    pub fn has_value(&self) -> bool {
        match self {
            DefaultSpec::Literal(_) => true,
            DefaultSpec::Expression(text) => !text.trim().is_empty(),
            _ => false,
        }
    }

    /// 切换模式时保留已输入的值（字面量 ↔ 表达式）。
    pub fn with_mode(&self, mode: DefaultMode) -> Self {
        let text = self.text().to_owned();
        match mode {
            DefaultMode::None => DefaultSpec::None,
            DefaultMode::Null => DefaultSpec::Null,
            DefaultMode::Literal => DefaultSpec::Literal(text),
            DefaultMode::Expression => DefaultSpec::Expression(text),
        }
    }

    /// 改写值，模式不变；没有值的模式不受影响。返回是否改变。
    pub fn set_text(&mut self, value: String) -> bool {
        match self {
            DefaultSpec::Literal(text) | DefaultSpec::Expression(text) if *text != value => {
                *text = value;
                true
            }
            _ => false,
        }
    }

    /// 展示用（检索、变更说明）：无默认为空，NULL 为 `NULL`，其余为原文。
    pub fn label(&self) -> String {
        match self {
            DefaultSpec::None => String::new(),
            DefaultSpec::Null => "NULL".to_owned(),
            DefaultSpec::Literal(text) | DefaultSpec::Expression(text) => text.clone(),
        }
    }
}

/// 建表时的表级选项。哪些选项有意义、可选值由方言给出（见 `DdlDialect::table_option_spec`）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TableOptions {
    pub engine: String,
    pub charset: String,
    pub collation: String,
    pub comment: String,
    pub without_rowid: bool,
    pub strict: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDraft {
    pub original: Option<String>,
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    #[serde(default)]
    pub default: DefaultSpec,
    /// 字段注释。只有建表时写出（修改表的注释不在这里改）。
    #[serde(default)]
    pub comment: String,
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
    CreateTable {
        name: String,
        columns: Vec<ColumnDraft>,
        #[serde(default)]
        options: TableOptions,
    },
    AlterColumns { table: String, columns: Vec<ColumnDraft> },
    CreateIndex { table: String, name: String, unique: bool, columns: Vec<IndexColumnDraft> },
    DropIndex { table: String, name: String },
    CreateForeignKey { table: String, foreign_key: ForeignKeyDraft },
    DropForeignKey { table: String, name: String },
    DropTable { table: String },
    /// 删除一个视图（PostgreSQL 物化视图走 DROP MATERIALIZED VIEW）。不删除底层表的数据。
    DropView { view: String },
    /// 清空一张表的数据，保留结构。SQLite 没有 TRUNCATE，用 DELETE FROM。
    TruncateTable { table: String },
    /// 给一张表改名。新名字为空或已被占用时只给出阻断原因。
    RenameTable { table: String, new_name: String },
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

/// 活动外层事务里 SQLite 重建的保存点边界（ADR-4）。
///
/// 有外层事务时不能另开内部事务：执行层把重建计划里的 `BEGIN`/`COMMIT` 换成这两条语句，
/// 适配器的逐条执行据此只回滚到保存点，不撤销此前已经应用的修改。无外层事务时仍用原有重建事务。
pub(crate) const SQLITE_REBUILD_SAVEPOINT: &str = "rillsql_rebuild";

/// 重建保存点的开始语句（执行层与适配器共用同一份文本）。
pub(crate) const SQLITE_REBUILD_SAVEPOINT_SQL: &str = "SAVEPOINT rillsql_rebuild;";

/// 重建保存点的释放语句。
pub(crate) const SQLITE_REBUILD_RELEASE_SQL: &str = "RELEASE SAVEPOINT rillsql_rebuild;";

#[derive(Clone, Debug)]
pub struct StepReceipt {
    pub sql: String,
    pub label: String,
    pub status: StepStatus,
    pub message: String,
}

/// 引擎的外键引用动作（由适配器的类型规则给出；没有结构编辑的引擎为空）。
pub use crate::adapters::ddl::fk_actions;

/// 执行语义说明。`rebuild` 为真时按重建方式说明；没有结构编辑的引擎说明这一点。
pub fn rule_text(engine: Engine, rebuild: bool) -> String {
    match crate::adapters::ddl::ddl_of(engine) {
        Some(dialect) if rebuild => dialect.rebuild_rule_text().to_owned(),
        Some(dialect) => dialect.rule_text().to_owned(),
        None => crate::adapters::ddl::no_structure_editing(engine),
    }
}
