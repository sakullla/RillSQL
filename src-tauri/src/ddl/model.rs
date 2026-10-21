use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug)]
pub struct Column {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub default_sql: Option<String>,
    pub primary: bool,
    pub auto: bool,
    pub generated: bool,
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
        Engine::Mysql | Engine::Mariadb => &["RESTRICT", "CASCADE", "SET NULL", "NO ACTION"],
        Engine::Postgres | Engine::Sqlite => &["RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT", "NO ACTION"],
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
    }
}
