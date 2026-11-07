pub const DEFAULT_OBJECT_PAGE: usize = 100;

pub use crate::connections::model::Engine;

/// 目录树的层级形状属于 catalog 语义，直接挂在正典 Engine 上。
impl Engine {
    pub fn shows_databases(self) -> bool {
        !matches!(self, Engine::Sqlite)
    }

    pub fn shows_schemas(self) -> bool {
        matches!(self, Engine::Postgres | Engine::Sqlserver)
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
    match engine {
        // Redis 是键值专项引擎：没有关系型对象种类，键空间走 SCAN 专项通路。
        // Redis 没有关系型对象；MongoDB 只有集合（映射到 Table 一种，树按“集合”渲染）。
        Engine::Redis => &[],
        Engine::Mongodb => &[ObjectKind::Table],
        Engine::Sqlite => &[ObjectKind::Table, ObjectKind::View, ObjectKind::Trigger],
        // 事件在 MySQL 与 MariaDB 都存在，跟着库走（information_schema.EVENTS.EVENT_SCHEMA）。
        Engine::Mysql => &[
            ObjectKind::Table,
            ObjectKind::View,
            ObjectKind::Function,
            ObjectKind::Procedure,
            ObjectKind::Trigger,
            ObjectKind::Event,
        ],
        Engine::Mariadb => &[
            ObjectKind::Table,
            ObjectKind::View,
            ObjectKind::Function,
            ObjectKind::Procedure,
            ObjectKind::Trigger,
            ObjectKind::Sequence,
            ObjectKind::Event,
        ],
        Engine::Postgres => &[
            ObjectKind::Table,
            ObjectKind::View,
            ObjectKind::Function,
            ObjectKind::Procedure,
            ObjectKind::Trigger,
            ObjectKind::Sequence,
            ObjectKind::Enum,
            ObjectKind::Domain,
            // 扩展是装在某个 schema 里的对象：按 pg_extension.extnamespace 归到该 schema。
            ObjectKind::Extension,
        ],
        // SQL Server 有独立序列（2012+），没有独立枚举或域。
        Engine::Sqlserver => &[
            ObjectKind::Table,
            ObjectKind::View,
            ObjectKind::Function,
            ObjectKind::Procedure,
            ObjectKind::Trigger,
            ObjectKind::Sequence,
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

pub fn metadata_sql(engine: Engine) -> MetadataSql {
    match engine {
        // Redis 没有元数据 SQL；这行只保证穷尽，键空间浏览不经这里。
        Engine::Redis => MetadataSql {
            databases: "",
            schemas: None,
            objects: "",
            foreign_keys: None,
        },
        // MongoDB 的库/集合清单走 list_database_names/list_collection_names 专项通路。
        Engine::Mongodb => MetadataSql {
            databases: "",
            schemas: None,
            objects: "",
            foreign_keys: None,
        },
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
        Engine::Sqlserver => MetadataSql {
            databases: "SELECT name FROM sys.databases WHERE state = 0 AND name NOT IN ('master', 'tempdb', 'model', 'msdb') ORDER BY name",
            schemas: Some(
                "SELECT name FROM sys.schemas WHERE name NOT IN ('guest', 'INFORMATION_SCHEMA', 'sys') AND name NOT LIKE 'db[_]%' ORDER BY name",
            ),
            objects: "SELECT o.name, x.kind, NULL, '', '' FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id CROSS APPLY (SELECT CASE o.type WHEN 'U' THEN 'table' WHEN 'V' THEN 'view' WHEN 'FN' THEN 'function' WHEN 'IF' THEN 'function' WHEN 'TF' THEN 'function' WHEN 'AF' THEN 'function' WHEN 'FT' THEN 'function' WHEN 'P' THEN 'procedure' WHEN 'PC' THEN 'procedure' WHEN 'SO' THEN 'sequence' ELSE 'table' END AS kind) x WHERE s.name = @P1 AND o.type IN ('U', 'V', 'FN', 'IF', 'TF', 'AF', 'FT', 'P', 'PC', 'SO') ORDER BY o.name OFFSET 0 ROWS FETCH NEXT @P2 ROWS ONLY",
            foreign_keys: None,
        },
    }
}

#[allow(dead_code)]
pub fn object_count_sql(engine: Engine) -> &'static str {
    match engine {
        // Redis 没有对象计数 SQL；键计数经 DBSIZE/SCAN 专项通路。
        Engine::Redis => "",
        // MongoDB 没有对象计数 SQL；集合文档计数经 estimatedDocumentCount 专项通路。
        Engine::Mongodb => "",
        Engine::Postgres => "SELECT COUNT(*)::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm')",
        Engine::Mysql | Engine::Mariadb => "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = ? AND table_type IN ('BASE TABLE', 'VIEW')",
        Engine::Sqlite => "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%'",
        Engine::Sqlserver => "SELECT COUNT(*) FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = @P1 AND o.type IN ('U', 'V', 'FN', 'IF', 'TF', 'AF', 'FT', 'P', 'PC', 'SO')",
    }
}

pub struct KindSql {
    pub count: &'static str,
    pub list: &'static str,
}

/// 每一类单独计数、单独取窗口，这样某一类失败不会清掉其他类。
pub fn kind_sql(engine: Engine, kind: ObjectKind) -> KindSql {
    match engine {
        // Redis 没有分类对象 SQL；键空间浏览不经这里。MongoDB 的集合清单同样走专项通路。
        Engine::Redis | Engine::Mongodb => KindSql { count: "", list: "" },
        Engine::Postgres => match kind {
            ObjectKind::Table => KindSql {
                count: "SELECT COUNT(*)::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('r', 'p')",
                list: "SELECT c.relname, 'table', c.reltuples::bigint, ''::text, ''::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('r', 'p') ORDER BY c.relname LIMIT $2",
            },
            ObjectKind::View => KindSql {
                count: "SELECT COUNT(*)::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('v', 'm')",
                list: "SELECT c.relname, 'view', c.reltuples::bigint, CASE WHEN c.relkind = 'm' THEN 'materialized' ELSE ''::text END, ''::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('v', 'm') ORDER BY c.relname LIMIT $2",
            },
            ObjectKind::Sequence => KindSql {
                count: "SELECT COUNT(*)::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind = 'S'",
                list: "SELECT c.relname, 'sequence', NULL::bigint, ''::text, ''::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind = 'S' ORDER BY c.relname LIMIT $2",
            },
            ObjectKind::Enum => KindSql {
                count: "SELECT COUNT(*)::bigint FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname = $1 AND t.typtype = 'e'",
                list: "SELECT t.typname, 'enum', NULL::bigint, ''::text, ''::text FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname = $1 AND t.typtype = 'e' ORDER BY t.typname LIMIT $2",
            },
            ObjectKind::Domain => KindSql {
                count: "SELECT COUNT(*)::bigint FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname = $1 AND t.typtype = 'd'",
                list: "SELECT t.typname, 'domain', NULL::bigint, ''::text, ''::text FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname = $1 AND t.typtype = 'd' ORDER BY t.typname LIMIT $2",
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
            // 扩展的已安装版本放在 signature 列：树上因此能直接读到版本号。
            ObjectKind::Extension => KindSql {
                count: "SELECT COUNT(*)::bigint FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace WHERE n.nspname = $1",
                list: "SELECT e.extname, 'extension', NULL::bigint, e.extversion, ''::text FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace WHERE n.nspname = $1 ORDER BY e.extname LIMIT $2",
            },
            // 事件只属于 MySQL / MariaDB：PostgreSQL 不给这一组编造任何行。
            ObjectKind::Event => KindSql {
                count: "SELECT 0::bigint",
                list: "SELECT ''::text, 'event', NULL::bigint, ''::text, ''::text WHERE false",
            },
        },
        Engine::Mysql | Engine::Mariadb => mysql_kind_sql(engine, kind),
        Engine::Sqlserver => sqlserver_kind_sql(kind),
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
            ObjectKind::Function
            | ObjectKind::Procedure
            | ObjectKind::Sequence
            | ObjectKind::Enum
            | ObjectKind::Domain
            | ObjectKind::Extension
            | ObjectKind::Event => KindSql {
                count: "SELECT 0",
                list: "SELECT name, type, '', '' FROM sqlite_master WHERE 0",
            },
        },
    }
}

fn empty_kind_sql() -> KindSql {
    KindSql {
        count: "SELECT 0",
        list: "SELECT table_name, 'table', NULL, '', '' FROM information_schema.tables WHERE 0",
    }
}

/// SQL Server 的分类查询。@P1 是 schema 名，@P2 是窗口大小；OFFSET/FETCH 需要 ORDER BY，都按名称排序。
fn sqlserver_kind_sql(kind: ObjectKind) -> KindSql {
    match kind {
        ObjectKind::Table => KindSql {
            count: "SELECT COUNT(*) FROM sys.tables t JOIN sys.schemas s ON s.schema_id = t.schema_id WHERE s.name = @P1",
            list: "SELECT t.name, 'table', (SELECT SUM(p.rows) FROM sys.partitions p WHERE p.object_id = t.object_id AND p.index_id IN (0, 1)), '', '' FROM sys.tables t JOIN sys.schemas s ON s.schema_id = t.schema_id WHERE s.name = @P1 ORDER BY t.name OFFSET 0 ROWS FETCH NEXT @P2 ROWS ONLY",
        },
        ObjectKind::View => KindSql {
            count: "SELECT COUNT(*) FROM sys.views v JOIN sys.schemas s ON s.schema_id = v.schema_id WHERE s.name = @P1",
            list: "SELECT v.name, 'view', NULL, '', '' FROM sys.views v JOIN sys.schemas s ON s.schema_id = v.schema_id WHERE s.name = @P1 ORDER BY v.name OFFSET 0 ROWS FETCH NEXT @P2 ROWS ONLY",
        },
        ObjectKind::Function => KindSql {
            count: "SELECT COUNT(*) FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = @P1 AND o.type IN ('FN', 'IF', 'TF', 'AF', 'FT')",
            list: "SELECT o.name, 'function', NULL, '', '' FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = @P1 AND o.type IN ('FN', 'IF', 'TF', 'AF', 'FT') ORDER BY o.name OFFSET 0 ROWS FETCH NEXT @P2 ROWS ONLY",
        },
        ObjectKind::Procedure => KindSql {
            count: "SELECT COUNT(*) FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = @P1 AND o.type IN ('P', 'PC')",
            list: "SELECT o.name, 'procedure', NULL, '', '' FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = @P1 AND o.type IN ('P', 'PC') ORDER BY o.name OFFSET 0 ROWS FETCH NEXT @P2 ROWS ONLY",
        },
        ObjectKind::Trigger => KindSql {
            // sys.triggers 没有自己的 schema 列，跟随父对象的 schema。
            count: "SELECT COUNT(*) FROM sys.triggers t JOIN sys.objects p ON p.object_id = t.parent_id JOIN sys.schemas s ON s.schema_id = p.schema_id WHERE s.name = @P1",
            list: "SELECT t.name, 'trigger', NULL, '', OBJECT_NAME(t.parent_id) FROM sys.triggers t JOIN sys.objects p ON p.object_id = t.parent_id JOIN sys.schemas s ON s.schema_id = p.schema_id WHERE s.name = @P1 ORDER BY t.name OFFSET 0 ROWS FETCH NEXT @P2 ROWS ONLY",
        },
        ObjectKind::Sequence => KindSql {
            count: "SELECT COUNT(*) FROM sys.sequences q JOIN sys.schemas s ON s.schema_id = q.schema_id WHERE s.name = @P1",
            list: "SELECT q.name, 'sequence', NULL, '', '' FROM sys.sequences q JOIN sys.schemas s ON s.schema_id = q.schema_id WHERE s.name = @P1 ORDER BY q.name OFFSET 0 ROWS FETCH NEXT @P2 ROWS ONLY",
        },
        // SQL Server 没有独立的枚举、域、扩展或事件，沿用 MySQL 的空清单降级先例。
        ObjectKind::Enum | ObjectKind::Domain | ObjectKind::Extension | ObjectKind::Event => empty_kind_sql(),
    }
}

fn mysql_kind_sql(engine: Engine, kind: ObjectKind) -> KindSql {
    match kind {
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
        // MariaDB 11.4 没有 information_schema.SEQUENCES。序列从 10.3 起记在 TABLES，类型是 SEQUENCE。
        ObjectKind::Sequence if engine == Engine::Mariadb => KindSql {
            count: "SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_TYPE = 'SEQUENCE'",
            list: "SELECT TABLE_NAME, 'sequence', NULL, '', '' FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_TYPE = 'SEQUENCE' ORDER BY TABLE_NAME LIMIT ?",
        },
        // 事件跟着库走：EVENT_SCHEMA 就是 database 参数。STATUS 放在 signature 列，树上直接读状态。
        ObjectKind::Event => KindSql {
            count: "SELECT COUNT(*) FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ?",
            list: "SELECT EVENT_NAME, 'event', NULL, LOWER(STATUS), '' FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ? ORDER BY EVENT_NAME LIMIT ?",
        },
        ObjectKind::Extension | ObjectKind::Sequence | ObjectKind::Enum | ObjectKind::Domain => empty_kind_sql(),
    }
}

/// SQLite 的对象按「库」分开读：main 与每个 ATTACH 上来的库各有自己的 sqlite_master。
/// 名字用双引号括起来，内嵌双引号按 SQL 规则翻倍。
pub fn quote_sqlite(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub fn sqlite_kind_count_sql(database: &str) -> String {
    format!(
        "SELECT type, COUNT(*) FROM {}.sqlite_master WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%' GROUP BY type",
        quote_sqlite(database)
    )
}

pub fn sqlite_object_page_sql(database: &str) -> String {
    format!(
        "SELECT name, type, '', '' FROM {}.sqlite_master WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?1 OFFSET ?2",
        quote_sqlite(database)
    )
}
