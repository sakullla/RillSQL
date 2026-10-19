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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    Table,
    View,
    Function,
    Procedure,
    Trigger,
}

pub fn kind_name(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Table => "table",
        ObjectKind::View => "view",
        ObjectKind::Function => "function",
        ObjectKind::Procedure => "procedure",
        ObjectKind::Trigger => "trigger",
    }
}

pub fn parse_kind(kind: &str) -> ObjectKind {
    match kind.to_ascii_lowercase().as_str() {
        "view" => ObjectKind::View,
        "function" => ObjectKind::Function,
        "procedure" => ObjectKind::Procedure,
        "trigger" => ObjectKind::Trigger,
        _ => ObjectKind::Table,
    }
}

/// SQLite 没有函数和存储过程。其余引擎列出这五类。
pub fn listed_kinds(engine: Engine) -> &'static [ObjectKind] {
    match engine {
        Engine::Sqlite => &[ObjectKind::Table, ObjectKind::View, ObjectKind::Trigger],
        Engine::Postgres | Engine::Mysql | Engine::Mariadb => &[
            ObjectKind::Table,
            ObjectKind::View,
            ObjectKind::Function,
            ObjectKind::Procedure,
            ObjectKind::Trigger,
        ],
    }
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

pub fn metadata_sql(engine: Engine) -> MetadataSql {
    match engine {
        Engine::Postgres => MetadataSql {
            databases: "SELECT datname FROM pg_database WHERE datallowconn AND NOT datistemplate ORDER BY datname",
            schemas: Some(
                "SELECT n.nspname FROM pg_namespace n WHERE n.nspname <> 'information_schema' AND n.nspname NOT LIKE 'pg_%' ORDER BY n.nspname",
            ),
            objects: "SELECT c.relname, CASE WHEN c.relkind IN ('v', 'm') THEN 'view' ELSE 'table' END, c.reltuples::bigint, ''::text, ''::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm') ORDER BY c.relname LIMIT $2 OFFSET $3",
            foreign_keys: None,
        },
        Engine::Mysql | Engine::Mariadb => MetadataSql {
            databases: "SELECT schema_name FROM information_schema.schemata WHERE schema_name NOT IN ('information_schema', 'mysql', 'performance_schema', 'sys') ORDER BY schema_name",
            schemas: None,
            objects: "SELECT table_name, CASE WHEN table_type = 'VIEW' THEN 'view' ELSE 'table' END, table_rows, '', '' FROM information_schema.tables WHERE table_schema = ? AND table_type IN ('BASE TABLE', 'VIEW') ORDER BY table_name LIMIT ? OFFSET ?",
            foreign_keys: None,
        },
        Engine::Sqlite => MetadataSql {
            databases: "",
            schemas: None,
            objects: "SELECT name, type, '', '' FROM sqlite_master WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?1 OFFSET ?2",
            foreign_keys: Some("PRAGMA foreign_keys"),
        },
    }
}

pub fn object_count_sql(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "SELECT COUNT(*)::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm')",
        Engine::Mysql | Engine::Mariadb => "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = ? AND table_type IN ('BASE TABLE', 'VIEW')",
        Engine::Sqlite => "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%'",
    }
}

pub struct KindSql {
    pub count: &'static str,
    pub list: &'static str,
}

/// 每一类单独计数、单独取窗口，这样某一类失败不会清掉其他类。
pub fn kind_sql(engine: Engine, kind: ObjectKind) -> KindSql {
    match engine {
        Engine::Postgres => match kind {
            ObjectKind::Table => KindSql {
                count: "SELECT COUNT(*)::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('r', 'p')",
                list: "SELECT c.relname, 'table', c.reltuples::bigint, ''::text, ''::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('r', 'p') ORDER BY c.relname LIMIT $2",
            },
            ObjectKind::View => KindSql {
                count: "SELECT COUNT(*)::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('v', 'm')",
                list: "SELECT c.relname, 'view', c.reltuples::bigint, ''::text, ''::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('v', 'm') ORDER BY c.relname LIMIT $2",
            },
            ObjectKind::Function => KindSql {
                count: "SELECT COUNT(*)::bigint FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 AND p.prokind = 'f'",
                list: "SELECT p.proname, 'function', NULL::bigint, pg_get_function_identity_arguments(p.oid), ''::text FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 AND p.prokind = 'f' ORDER BY p.proname, 4 LIMIT $2",
            },
            ObjectKind::Procedure => KindSql {
                count: "SELECT COUNT(*)::bigint FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 AND p.prokind = 'p'",
                list: "SELECT p.proname, 'procedure', NULL::bigint, pg_get_function_identity_arguments(p.oid), ''::text FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 AND p.prokind = 'p' ORDER BY p.proname, 4 LIMIT $2",
            },
            ObjectKind::Trigger => KindSql {
                count: "SELECT COUNT(*)::bigint FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND NOT t.tgisinternal",
                list: "SELECT t.tgname, 'trigger', NULL::bigint, ''::text, c.relname FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND NOT t.tgisinternal ORDER BY t.tgname, c.relname LIMIT $2",
            },
        },
        Engine::Mysql | Engine::Mariadb => match kind {
            ObjectKind::Table => KindSql {
                count: "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = ? AND table_type = 'BASE TABLE'",
                list: "SELECT table_name, 'table', table_rows, '', '' FROM information_schema.tables WHERE table_schema = ? AND table_type = 'BASE TABLE' ORDER BY table_name LIMIT ?",
            },
            ObjectKind::View => KindSql {
                count: "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = ? AND table_type = 'VIEW'",
                list: "SELECT table_name, 'view', NULL, '', '' FROM information_schema.tables WHERE table_schema = ? AND table_type = 'VIEW' ORDER BY table_name LIMIT ?",
            },
            ObjectKind::Function => KindSql {
                count: "SELECT COUNT(*) FROM information_schema.routines WHERE routine_schema = ? AND routine_type = 'FUNCTION'",
                list: "SELECT routine_name, 'function', NULL, '', '' FROM information_schema.routines WHERE routine_schema = ? AND routine_type = 'FUNCTION' ORDER BY routine_name LIMIT ?",
            },
            ObjectKind::Procedure => KindSql {
                count: "SELECT COUNT(*) FROM information_schema.routines WHERE routine_schema = ? AND routine_type = 'PROCEDURE'",
                list: "SELECT routine_name, 'procedure', NULL, '', '' FROM information_schema.routines WHERE routine_schema = ? AND routine_type = 'PROCEDURE' ORDER BY routine_name LIMIT ?",
            },
            ObjectKind::Trigger => KindSql {
                count: "SELECT COUNT(*) FROM information_schema.triggers WHERE trigger_schema = ?",
                list: "SELECT trigger_name, 'trigger', NULL, '', event_object_table FROM information_schema.triggers WHERE trigger_schema = ? ORDER BY trigger_name, event_object_table LIMIT ?",
            },
        },
        Engine::Sqlite => match kind {
            ObjectKind::Table => KindSql {
                count: "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                list: "SELECT name, 'table', '', '' FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?1",
            },
            ObjectKind::View => KindSql {
                count: "SELECT COUNT(*) FROM sqlite_master WHERE type = 'view' AND name NOT LIKE 'sqlite_%'",
                list: "SELECT name, 'view', '', '' FROM sqlite_master WHERE type = 'view' AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?1",
            },
            ObjectKind::Trigger => KindSql {
                count: "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name NOT LIKE 'sqlite_%'",
                list: "SELECT name, 'trigger', '', '' FROM sqlite_master WHERE type = 'trigger' AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?1",
            },
            ObjectKind::Function | ObjectKind::Procedure => KindSql {
                count: "SELECT 0",
                list: "SELECT name, type, '', '' FROM sqlite_master WHERE 0",
            },
        },
    }
}

pub fn sqlite_kind_count_sql() -> &'static str {
    "SELECT type, COUNT(*) FROM sqlite_master WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%' GROUP BY type"
}
