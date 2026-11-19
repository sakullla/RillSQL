pub const DEFAULT_OBJECT_PAGE: usize = 500;

pub use crate::connections::model::Engine;

/// 目录树的层级形状属于 catalog 语义，直接挂在正典 Engine 上；规则由各适配器的 Capabilities 声明。
impl Engine {
    pub fn shows_databases(self) -> bool {
        crate::adapters::adapter(self).capabilities().shows_databases
    }

    pub fn shows_schemas(self) -> bool {
        crate::adapters::adapter(self).capabilities().shows_schemas
    }

    pub fn shows_foreign_keys(self) -> bool {
        crate::adapters::adapter(self).capabilities().shows_foreign_keys
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    Table,
    View,
    Function,
    Procedure,
    Trigger,
    Sequence,
    Enum,
    Domain,
    /// PostgreSQL 扩展：装在某个 schema 里的独立对象，读取时带已安装版本。
    Extension,
    /// MySQL / MariaDB 事件（EVENT）：库级对象，读取时带启停状态。
    Event,
}

pub fn kind_name(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Table => "table",
        ObjectKind::View => "view",
        ObjectKind::Function => "function",
        ObjectKind::Procedure => "procedure",
        ObjectKind::Trigger => "trigger",
        ObjectKind::Sequence => "sequence",
        ObjectKind::Enum => "enum",
        ObjectKind::Domain => "domain",
        ObjectKind::Extension => "extension",
        ObjectKind::Event => "event",
    }
}

pub fn parse_kind(kind: &str) -> ObjectKind {
    match kind.to_ascii_lowercase().as_str() {
        "view" | "materialized" => ObjectKind::View,
        "function" => ObjectKind::Function,
        "procedure" => ObjectKind::Procedure,
        "trigger" => ObjectKind::Trigger,
        "sequence" => ObjectKind::Sequence,
        "enum" => ObjectKind::Enum,
        "domain" => ObjectKind::Domain,
        "extension" => ObjectKind::Extension,
        "event" => ObjectKind::Event,
        _ => ObjectKind::Table,
    }
}

/// 只列出引擎真正拥有的独立对象。列和索引不是这里的种类。
pub fn listed_kinds(engine: Engine) -> &'static [ObjectKind] {
    crate::adapters::catalog_of(engine).listed_kinds()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnStatus {
    Connected,
    Disconnected,
    Connecting,
    Error,
    Lost,
}

impl ConnStatus {
    pub fn is_connected(self) -> bool {
        matches!(self, ConnStatus::Connected)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRow {
    pub name: String,
    pub kind: ObjectKind,
    pub estimated_rows: Option<i64>,
    pub stale: bool,
    /// PostgreSQL 函数或过程的参数类型，不含括号。
    pub signature: String,
    /// 触发器所属的表。没有时为空。
    pub relation: String,
    /// PostgreSQL 物化视图仍记在视图组里，用这个标志和普通视图分开。
    pub materialized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindReport {
    pub kind: ObjectKind,
    pub total: usize,
    pub error: Option<String>,
}

/// 负数表示估算未知。PostgreSQL 的 `reltuples` 在从未 ANALYZE 时是 -1，不是行数。
pub(crate) fn known_row_estimate(value: Option<i64>) -> Option<i64> {
    value.filter(|rows| *rows >= 0)
}

impl ObjectRow {
    #[cfg(test)]
    pub fn fresh(name: impl Into<String>, kind: ObjectKind) -> Self {
        Self {
            name: name.into(),
            kind,
            estimated_rows: None,
            stale: false,
            signature: String::new(),
            relation: String::new(),
            materialized: false,
        }
    }

    #[cfg(test)]
    pub fn table(name: impl Into<String>, estimated_rows: Option<i64>) -> Self {
        Self {
            estimated_rows,
            ..Self::fresh(name, ObjectKind::Table)
        }
    }

    #[cfg(test)]
    pub fn view(name: impl Into<String>) -> Self {
        Self::fresh(name, ObjectKind::View)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaNode {
    pub name: String,
    pub objects: Vec<ObjectRow>,
    pub total: usize,
    pub error: Option<String>,
    pub kind_reports: Vec<KindReport>,
}

#[allow(dead_code)]
impl SchemaNode {
    pub fn empty(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            objects: Vec::new(),
            total: 0,
            error: None,
            kind_reports: Vec::new(),
        }
    }

    pub fn failed(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            objects: Vec::new(),
            total: 0,
            error: Some(message.into()),
            kind_reports: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaStatus {
    Ready,
    Empty,
    Failed { message: String },
}

pub fn schema_status(schema: &SchemaNode) -> SchemaStatus {
    match &schema.error {
        Some(message) => SchemaStatus::Failed {
            message: message.clone(),
        },
        None if schema.objects.is_empty() => SchemaStatus::Empty,
        None => SchemaStatus::Ready,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseNode {
    pub name: String,
    pub schemas: Vec<SchemaNode>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionNode {
    pub id: String,
    pub name: String,
    pub engine: Engine,
    pub status: ConnStatus,
    pub foreign_keys: Option<bool>,
    pub databases: Vec<DatabaseNode>,
    /// 库列表本身失败时填写。空的 databases 不能单独表示“没有库”。
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataSql {
    pub databases: &'static str,
    pub schemas: Option<&'static str>,
    pub objects: &'static str,
    pub foreign_keys: Option<&'static str>,
}

/// 引擎的元数据 SQL；没有元数据 SQL 的引擎（键值、文档）各项为空，它们的目录走专项通路。
pub fn metadata_sql(engine: Engine) -> MetadataSql {
    crate::adapters::catalog_of(engine).metadata_sql()
}

#[allow(dead_code)]
pub fn object_count_sql(engine: Engine) -> &'static str {
    crate::adapters::catalog_of(engine).object_count_sql()
}

pub struct KindSql {
    pub count: &'static str,
    pub list: &'static str,
}

/// 每一类单独计数、单独取窗口，这样某一类失败不会清掉其他类。
pub fn kind_sql(engine: Engine, kind: ObjectKind) -> KindSql {
    crate::adapters::catalog_of(engine).kind_sql(kind)
}
