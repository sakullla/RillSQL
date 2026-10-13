pub const DEFAULT_OBJECT_PAGE: usize = 100;

/// MySQL 与 MariaDB 共用库这一层，但仍是两种引擎。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Postgres,
    Mysql,
    Mariadb,
    Sqlite,
}

impl Engine {
    pub fn as_str(self) -> &'static str {
        match self {
            Engine::Postgres => "postgres",
            Engine::Mysql => "mysql",
            Engine::Mariadb => "mariadb",
            Engine::Sqlite => "sqlite",
        }
    }

    pub fn shows_databases(self) -> bool {
        !matches!(self, Engine::Sqlite)
    }

    pub fn shows_schemas(self) -> bool {
        matches!(self, Engine::Postgres)
    }

    pub fn shows_foreign_keys(self) -> bool {
        matches!(self, Engine::Sqlite)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Table,
    View,
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
}

impl ObjectRow {
    pub fn table(name: impl Into<String>, estimated_rows: Option<i64>) -> Self {
        Self {
            name: name.into(),
            kind: ObjectKind::Table,
            estimated_rows,
            stale: false,
        }
    }

    pub fn view(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: ObjectKind::View,
            estimated_rows: None,
            stale: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaNode {
    pub name: String,
    pub objects: Vec<ObjectRow>,
    pub total: usize,
    pub error: Option<String>,
}

impl SchemaNode {
    pub fn failed(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            objects: Vec::new(),
            total: 0,
            error: Some(message.into()),
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataSql {
    pub databases: &'static str,
    pub schemas: Option<&'static str>,
    pub objects: &'static str,
    pub foreign_keys: Option<&'static str>,
}

pub fn metadata_sql(engine: Engine) -> MetadataSql {
    match engine {
        Engine::Postgres => MetadataSql {
            databases: "SELECT datname FROM pg_database WHERE datallowconn AND NOT datistemplate ORDER BY datname",
            schemas: Some(
                "SELECT n.nspname FROM pg_namespace n WHERE n.nspname <> 'information_schema' AND n.nspname NOT LIKE 'pg_%' ORDER BY n.nspname",
            ),
            objects: "SELECT c.relname, CASE WHEN c.relkind IN ('v', 'm') THEN 'view' ELSE 'table' END, c.reltuples::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm') ORDER BY c.relname LIMIT $2 OFFSET $3",
            foreign_keys: None,
        },
        Engine::Mysql | Engine::Mariadb => MetadataSql {
            databases: "SELECT schema_name FROM information_schema.schemata WHERE schema_name NOT IN ('information_schema', 'mysql', 'performance_schema', 'sys') ORDER BY schema_name",
            schemas: None,
            objects: "SELECT table_name, CASE WHEN table_type = 'VIEW' THEN 'view' ELSE 'table' END, table_rows FROM information_schema.tables WHERE table_schema = ? ORDER BY table_name LIMIT ? OFFSET ?",
            foreign_keys: None,
        },
        Engine::Sqlite => MetadataSql {
            databases: "",
            schemas: None,
            objects: "SELECT name, type FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?1 OFFSET ?2",
            foreign_keys: Some("PRAGMA foreign_keys"),
        },
    }
}
