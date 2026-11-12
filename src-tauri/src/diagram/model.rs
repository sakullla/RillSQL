use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyPart {
    pub name: String,
    pub column: String,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_column: String,
    pub ordinal: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OutgoingKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

/// 列名、数据类型和是否主键。只有名字时类型留空、主键为 false，旧测试仍能按名字比较。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagramColumn {
    pub name: String,
    pub type_name: String,
    pub primary: bool,
}

impl DiagramColumn {
    pub(crate) fn plain(name: String) -> Self {
        Self { name, type_name: String::new(), primary: false }
    }
}

impl PartialEq<&str> for DiagramColumn {
    fn eq(&self, other: &&str) -> bool {
        self.name == *other
    }
}

/// 读取到的列。`FromIterator<String>` 让只给列名的测试仍能构造。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReadColumns(pub Vec<DiagramColumn>);

impl ReadColumns {
    pub(crate) fn into_inner(self) -> Vec<DiagramColumn> {
        self.0
    }
}

impl FromIterator<String> for ReadColumns {
    fn from_iter<T: IntoIterator<Item = String>>(iter: T) -> Self {
        Self(iter.into_iter().map(DiagramColumn::plain).collect())
    }
}

/// 一张基表的读取结果。列或外键失败时整张表不进入图。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TableAttempt {
    pub name: String,
    pub columns: Result<ReadColumns, String>,
    pub foreign_keys: Result<Vec<OutgoingKey>, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RangeScope {
    pub database: String,
    pub schema: String,
    pub fold_names: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagramTable {
    pub name: String,
    pub columns: Vec<DiagramColumn>,
    pub outside: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagramEdge {
    pub name: String,
    pub from_table: String,
    pub from_columns: Vec<String>,
    pub to_table: String,
    pub to_columns: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OmittedTable {
    pub name: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagram {
    pub database: String,
    pub schema: String,
    pub tables: Vec<DiagramTable>,
    pub edges: Vec<DiagramEdge>,
    pub omitted: Vec<OmittedTable>,
    pub notice: Option<String>,
}

pub(crate) fn empty_postgres_schema(schema: &str) -> Option<String> {
    if schema.trim().is_empty() {
        Some("PostgreSQL 关系图需要一个 schema。".to_owned())
    } else {
        None
    }
}

pub(crate) fn empty_sqlserver_schema(schema: &str) -> Option<String> {
    if schema.trim().is_empty() {
        Some("SQL Server 关系图需要一个 schema。".to_owned())
    } else {
        None
    }
}

pub(crate) fn other_postgres_database(current: &str, database: &str) -> Option<String> {
    if database == current {
        None
    } else {
        Some(format!("当前会话连在 {current}，没有另开连接读取 {database}"))
    }
}

pub(crate) fn empty_mysql_database(database: &str) -> Option<String> {
    if database.trim().is_empty() {
        Some("MySQL 和 MariaDB 的关系图需要一个数据库。".to_owned())
    } else {
        None
    }
}
