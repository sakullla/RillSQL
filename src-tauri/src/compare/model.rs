//! 对比两侧已经读到的对象和行。读取失败只记在对应类别上。

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Engine {
    Postgres,
    Mysql,
    Mariadb,
    Sqlite,
}

impl Engine {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "postgres" => Ok(Self::Postgres),
            "mysql" => Ok(Self::Mysql),
            "mariadb" => Ok(Self::Mariadb),
            "sqlite" => Ok(Self::Sqlite),
            other => Err(format!("不支持的数据库 {other}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Mysql => "mysql",
            Self::Mariadb => "mariadb",
            Self::Sqlite => "sqlite",
        }
    }
}

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

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiffItem {
    pub kind: String,
    pub name: String,
    pub detail: String,
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
    pub tables: Vec<TableChoice>,
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
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DataReport {
    pub refused: Option<String>,
    pub script: String,
    pub incomplete: bool,
    pub notes: Vec<String>,
    pub diffs: Vec<DataDiff>,
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
}

/// PostgreSQL 只有在要读的库不是当前会话库时才另开会话。
pub fn needs_ephemeral_postgres(engine: &str, current_database: &str, requested: &str) -> bool {
    engine == "postgres" && !requested.is_empty() && current_database != requested
}
