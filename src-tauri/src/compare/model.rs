//! 对比两侧已经读到的对象和行。读取失败只记在对应类别上。

use serde::{Deserialize, Serialize};

pub use crate::connections::model::Engine;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Table,
    View,
    Function,
    Procedure,
    Trigger,
    Sequence,
    Enum,
    Domain,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Table => "表",
            Self::View => "视图",
            Self::Function => "函数",
            Self::Procedure => "过程",
            Self::Trigger => "触发器",
            Self::Sequence => "序列",
            Self::Enum => "枚举",
            Self::Domain => "域",
        }
    }
}

/// 索引和外键不是目录种类，但读取失败时不能写进会创建或删除它们的脚本。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Table,
    View,
    Function,
    Procedure,
    Trigger,
    Sequence,
    Enum,
    Domain,
    Index,
    ForeignKey,
}

impl Category {
    pub fn from_kind(kind: Kind) -> Self {
        match kind {
            Kind::Table => Self::Table,
            Kind::View => Self::View,
            Kind::Function => Self::Function,
            Kind::Procedure => Self::Procedure,
            Kind::Trigger => Self::Trigger,
            Kind::Sequence => Self::Sequence,
            Kind::Enum => Self::Enum,
            Kind::Domain => Self::Domain,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Table => "表",
            Self::View => "视图",
            Self::Function => "函数",
            Self::Procedure => "过程",
            Self::Trigger => "触发器",
            Self::Sequence => "序列",
            Self::Enum => "枚举",
            Self::Domain => "域",
            Self::Index => "索引",
            Self::ForeignKey => "外键",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnShape {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub default_sql: String,
    pub primary: bool,
    /// SQL Server 的具名默认约束（sys.default_constraints）；其余引擎为空。
    pub default_constraint: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexShape {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<(String, bool)>,
    pub protected: bool,
    pub reason: String,
    pub partial: bool,
    pub expression: bool,
    pub sql: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignKeyShape {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: String,
    pub on_update: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableShape {
    pub columns: Vec<ColumnShape>,
    pub indexes: Vec<IndexShape>,
    pub foreign_keys: Vec<ForeignKeyShape>,
    /// 索引查询失败时为 false。不能把空列表当成没有索引或没有主键。
    pub indexes_read: bool,
    /// 外键查询失败时为 false。不能把空列表当成没有外键。
    pub foreign_keys_read: bool,
}

#[derive(Clone, Debug)]
pub struct ObjectDef {
    pub kind: Kind,
    pub name: String,
    pub signature: String,
    pub relation: String,
    pub definition: String,
    pub materialized: bool,
    pub table: Option<TableShape>,
    pub read_error: Option<String>,
}

impl ObjectDef {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn table(name: impl Into<String>, shape: TableShape) -> Self {
        Self {
            kind: Kind::Table,
            name: name.into(),
            signature: String::new(),
            relation: String::new(),
            definition: String::new(),
            materialized: false,
            table: Some(shape),
            read_error: None,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn defined(kind: Kind, name: impl Into<String>, definition: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
            signature: String::new(),
            relation: String::new(),
            definition: definition.into(),
            materialized: false,
            table: None,
            read_error: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Side {
    pub engine: String,
    pub database: String,
    pub schema: String,
    pub connected: bool,
    pub fatal: Option<String>,
    pub objects: Vec<ObjectDef>,
    pub category_errors: Vec<(Category, String)>,
    pub cancelled: bool,
    /// 对象名单没有读完时，不能把没看见的对象当成不存在而去删除。
    pub listing_complete: bool,
}

impl Side {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn ready(engine: impl Into<String>, database: impl Into<String>, schema: impl Into<String>, objects: Vec<ObjectDef>) -> Self {
        Self {
            engine: engine.into(),
            database: database.into(),
            schema: schema.into(),
            connected: true,
            fatal: None,
            objects,
            category_errors: Vec::new(),
            cancelled: false,
            listing_complete: true,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiffItem {
    pub kind: String,
    pub name: String,
    pub detail: String,
    /// 差异所属对象（索引/外键差异归到它们的表）：结果页按它找并排定义与同步 SQL。
    pub owner: String,
    /// 基准与对照一侧的定义文本（表按对照侧限定名重建，便于逐行对照）；没有这一侧时为空。
    pub baseline_definition: String,
    pub target_definition: String,
    /// 只让对照里这个对象变成基准的语句（不含依赖排序，整体脚本以 `script` 为准）。
    pub sql: String,
}

impl DiffItem {
    pub fn new(kind: impl Into<String>, name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { kind: kind.into(), name: name.into(), detail: detail.into(), ..Self::default() }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KeyChoice {
    pub name: String,
    pub columns: Vec<String>,
    pub primary: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TableChoice {
    pub name: String,
    pub on_both: bool,
    pub needs_key_choice: bool,
    pub keys: Vec<KeyChoice>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StructureReport {
    pub refused: Option<String>,
    pub script: String,
    pub incomplete: bool,
    pub notes: Vec<String>,
    pub only_baseline: Vec<DiffItem>,
    pub only_target: Vec<DiffItem>,
    pub changed: Vec<DiffItem>,
    /// 两侧都有且没有差异的对象；结果页关掉“仅差异”时列出。
    pub same: Vec<DiffItem>,
    pub tables: Vec<TableChoice>,
    /// 有读取超过超时设置：界面按失败处理并给重试。
    pub timed_out: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataColumn {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataKey {
    pub name: String,
    pub columns: Vec<String>,
    pub primary: bool,
}

#[derive(Clone, Debug)]
pub struct DataTable {
    pub name: String,
    pub qualify: String,
    pub columns: Vec<DataColumn>,
    pub keys: Vec<DataKey>,
    /// 索引没读到，且主键不来自列信息。不能写成没有主键或唯一键。
    pub keys_unknown: bool,
    pub chosen_key: Option<String>,
    pub baseline: Vec<std::collections::BTreeMap<String, crate::grid::Cell>>,
    pub target: Vec<std::collections::BTreeMap<String, crate::grid::Cell>>,
    pub read_error: Option<String>,
    /// 表只在一侧时不编造行。`baseline` 或 `target`。
    pub only_side: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DataInput {
    pub baseline_engine: String,
    pub target_engine: String,
    pub tables: Vec<DataTable>,
    pub cancelled: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DataDiff {
    pub table: String,
    pub action: String,
    pub key: String,
    pub detail: String,
    /// 行级字段差异：更新时是值不同的列；插入/删除时是整行（另一侧为空）。
    pub fields: Vec<FieldDiff>,
    /// 这一行对应的同步语句；拒绝或跳过时为空。
    pub sql: String,
}

impl DataDiff {
    pub fn skipped(table: &str, detail: impl Into<String>) -> Self {
        Self { table: table.to_owned(), action: "skipped".to_owned(), key: String::new(), detail: detail.into(), fields: Vec::new(), sql: String::new() }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FieldDiff {
    pub column: String,
    /// 基准一侧的值；`None` 表示这一侧没有这一行。SQL NULL 写成 `NULL`。
    pub baseline: Option<String>,
    pub target: Option<String>,
    /// 两侧值确实不同（插入/删除整行展示时，键列等相同列为 false）。
    pub changed: bool,
}

/// 一张表的数据对比摘要：结果页先按表列出，再下钻行级差异。
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DataTableSummary {
    pub table: String,
    pub inserts: usize,
    pub updates: usize,
    pub deletes: usize,
    /// 生成不了语句的行（字面量被拒绝、重复键）。
    pub rejected: usize,
    /// 整张表没有生成行变更时的原因（无主键、只在一侧、键未选定、读取失败…）。
    pub skipped: Option<String>,
    /// 用来对应行的键列；没有可用键时为空。
    pub key: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DataReport {
    pub refused: Option<String>,
    pub script: String,
    pub incomplete: bool,
    pub notes: Vec<String>,
    pub diffs: Vec<DataDiff>,
    pub tables: Vec<DataTableSummary>,
    pub timed_out: bool,
}

impl DataReport {
    pub fn refused(message: impl Into<String>) -> Self {
        Self { refused: Some(message.into()), script: String::new(), incomplete: false, notes: Vec::new(), diffs: Vec::new(), tables: Vec::new(), timed_out: false }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideRequest {
    pub connection_id: String,
    pub database: String,
    pub schema: String,
    pub engine: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructureRequest {
    pub id: String,
    pub baseline: SideRequest,
    pub target: SideRequest,
    /// 每次读取的超时（秒），取设置里的查询超时；0 时用目录读取的默认超时。
    #[serde(default)]
    pub timeout_secs: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataTableRequest {
    pub name: String,
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataRequest {
    pub id: String,
    pub baseline: SideRequest,
    pub target: SideRequest,
    pub tables: Vec<DataTableRequest>,
    #[serde(default)]
    pub timeout_secs: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaListRequest {
    pub connection_id: String,
    pub database: String,
}

/// 会话绑定单库的引擎（PostgreSQL）只有在要读的库不是当前会话库时才另开会话。
pub fn needs_ephemeral_postgres(engine: &str, current_database: &str, requested: &str) -> bool {
    let scoped = crate::adapters::registry::parse(engine).is_some_and(|engine| crate::adapters::adapter(engine).capabilities().database_scoped_session);
    scoped && !requested.is_empty() && current_database != requested
}
