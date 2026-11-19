//! PostgreSQL 目录方言：库 → 模式 → 对象，元数据走 pg_catalog；会话只连着一个库。

use crate::adapters::catalog::{diagram_session_error, information_schema_primary_key_sql, literal, mismatch, mismatch_body, CatalogDialect};
use crate::catalog::model::{KindSql, MetadataSql, ObjectKind, ObjectRow, SchemaNode};
use crate::catalog::objects::ExtrasBody;
use crate::catalog::read::{definition_failure, empty_page, other_database_message, read_failure, ConnectionBody, ObjectPageBody, ObjectWindow};
use crate::connections::CatalogSession;
use crate::diagram::Diagram;

use super::catalog_read as read;

pub(in crate::adapters) struct PostgresCatalog;

const LISTED: &[ObjectKind] = &[
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
];

pub(crate) fn metadata_sql() -> MetadataSql {
    MetadataSql {
        databases: "SELECT datname FROM pg_database WHERE datallowconn AND NOT datistemplate ORDER BY datname",
        schemas: Some(
            "SELECT n.nspname FROM pg_namespace n WHERE n.nspname <> 'information_schema' AND n.nspname NOT LIKE 'pg_%' ORDER BY n.nspname",
        ),
        objects: "SELECT c.relname, CASE WHEN c.relkind IN ('v', 'm') THEN 'view' ELSE 'table' END, c.reltuples::bigint, ''::text, ''::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm') ORDER BY c.relname LIMIT $2 OFFSET $3",
        foreign_keys: None,
    }
}

pub(crate) fn kind_sql(kind: ObjectKind) -> KindSql {
    match kind {
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
    }
}

pub(crate) fn listed_kinds() -> &'static [ObjectKind] {
    LISTED
}

#[async_trait::async_trait]
impl CatalogDialect for PostgresCatalog {
    fn listed_kinds(&self) -> &'static [ObjectKind] {
        LISTED
    }

    fn definition_available(&self, kind: ObjectKind) -> bool {
        use ObjectKind::*;
        matches!(kind, Table | View | Function | Procedure | Trigger | Sequence | Enum | Domain)
    }

    fn table_is_rebuilt(&self) -> bool {
        true
    }

    fn metadata_sql(&self) -> MetadataSql {
        metadata_sql()
    }

    fn object_count_sql(&self) -> &'static str {
        "SELECT COUNT(*)::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm')"
    }

    fn kind_sql(&self, kind: ObjectKind) -> KindSql {
        kind_sql(kind)
    }

    /// information_schema 的位置列 name, type, nullable, default, comment；注释用左连接读，读不到就是 NULL。
    fn columns_sql(&self, _database: &str, schema: &str, table: &str) -> Option<String> {
        Some(format!(
            "SELECT c.column_name, COALESCE(pg_catalog.format_type(a.atttypid, a.atttypmod), c.data_type), c.is_nullable, c.column_default, d.description FROM information_schema.columns c LEFT JOIN pg_catalog.pg_namespace n ON n.nspname = c.table_schema LEFT JOIN pg_catalog.pg_class t ON t.relnamespace = n.oid AND t.relname = c.table_name LEFT JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = c.ordinal_position::int AND NOT a.attisdropped LEFT JOIN pg_catalog.pg_description d ON d.objoid = t.oid AND d.classoid = 'pg_catalog.pg_class'::regclass AND d.objsubid = c.ordinal_position::int WHERE c.table_schema = {} AND c.table_name = {} ORDER BY c.ordinal_position",
            literal(schema),
            literal(table)
        ))
    }

    fn table_comments_sql(&self, _database: &str, schema: &str) -> Option<String> {
        Some(format!(
            "SELECT t.relname, d.description FROM pg_catalog.pg_class t JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace JOIN pg_catalog.pg_description d ON d.objoid = t.oid AND d.classoid = 'pg_catalog.pg_class'::regclass AND d.objsubid = 0 WHERE n.nspname = {} AND t.relkind IN ('r', 'p', 'v', 'm', 'f') ORDER BY t.relname",
            literal(schema)
        ))
    }

    fn table_comment_name_column(&self) -> &'static str {
        "t.relname"
    }

    fn primary_key_sql(&self, _database: &str, schema: &str, table: &str) -> Option<String> {
        Some(information_schema_primary_key_sql(&literal(schema), table))
    }

    fn indexes_sql(&self, _database: &str, schema: &str, table: &str) -> Option<String> {
        Some(format!(
            "SELECT i.relname, a.attname, ix.indisunique::text, ix.indisprimary::text, k.ord::text FROM pg_index ix JOIN pg_class i ON i.oid = ix.indexrelid JOIN pg_class t ON t.oid = ix.indrelid JOIN pg_namespace n ON n.oid = t.relnamespace LEFT JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS k(attnum, ord) ON true LEFT JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum WHERE n.nspname = {} AND t.relname = {} ORDER BY i.relname, k.ord",
            literal(schema),
            literal(table)
        ))
    }

    async fn read_connection(&self, session: &CatalogSession, page: usize, windows: &[ObjectWindow]) -> ConnectionBody {
        let CatalogSession::Postgres(client) = session else { return mismatch_body() };
        match read::read_postgres(client, page, windows).await {
            Ok(databases) => ConnectionBody { error: None, foreign_keys: None, databases },
            Err(message) => ConnectionBody { error: Some(message), foreign_keys: None, databases: Vec::new() },
        }
    }

    async fn read_outline(&self, session: &CatalogSession) -> ConnectionBody {
        let CatalogSession::Postgres(client) = session else { return mismatch_body() };
        match read::read_postgres_outline(client).await {
            Ok(databases) => ConnectionBody { error: None, foreign_keys: None, databases },
            Err(message) => ConnectionBody { error: Some(message), foreign_keys: None, databases: Vec::new() },
        }
    }

    /// 会话必须已经连到目标库（调用方另开临时会话，见 Service::open_ephemeral_postgres）。
    async fn read_database_outline(&self, session: &CatalogSession, database: &str) -> Result<Vec<SchemaNode>, String> {
        let CatalogSession::Postgres(client) = session else {
            return Err(read_failure("这个引擎的库层级已经随连接一次读出，不需要按库再读"));
        };
        read::read_database_schemas(client, database).await
    }

    async fn read_kind_page(&self, session: &CatalogSession, _database: &str, schema: &str, kind: ObjectKind, offset: usize, limit: usize) -> ObjectPageBody {
        let CatalogSession::Postgres(client) = session else { return empty_page(mismatch()) };
        match read::read_postgres_kind_slice(client, schema, kind, offset, limit).await {
            Ok((total, objects)) => ObjectPageBody { objects, total, error: None, kind_reports: Vec::new() },
            Err(message) => empty_page(message),
        }
    }

    async fn read_object_page(&self, session: &CatalogSession, database: &str, schema: &str, offset: usize, limit: usize) -> ObjectPageBody {
        let CatalogSession::Postgres(client) = session else { return empty_page(mismatch()) };
        read::read_postgres_page(client, database, schema, offset, limit).await
    }

    async fn search_objects(&self, session: &CatalogSession, pattern: &str) -> Vec<(String, String, ObjectRow)> {
        let CatalogSession::Postgres(client) = session else { return Vec::new() };
        read::search(client, pattern).await
    }

    /// pg_catalog 只覆盖会话自己的库：读别的库的定义会安静地读到同名的另一个对象，宁可报出原因，也不返回错误的 DDL。
    async fn read_definition(
        &self,
        session: &CatalogSession,
        database: &str,
        schema: &str,
        name: &str,
        kind: &str,
        signature: &str,
        relation: &str,
    ) -> Result<String, String> {
        let CatalogSession::Postgres(client) = session else {
            return Err(definition_failure("连接类型和引擎不一致"));
        };
        let current = read::current_database(client).await?;
        if database != current {
            return Err(definition_failure(other_database_message(&current, database)));
        }
        read::read_postgres_definition(client, schema, name, kind, signature, relation).await
    }

    async fn read_extras(&self, session: &CatalogSession, _database: &str, schema: &str) -> ExtrasBody {
        let CatalogSession::Postgres(client) = session else { return ExtrasBody::default() };
        super::extras::read_postgres_extras(client, schema).await
    }

    async fn read_diagram(&self, session: &CatalogSession, database: &str, schema: &str) -> Result<Diagram, String> {
        if let Some(message) = crate::diagram::model::empty_postgres_schema(schema) {
            return Err(message);
        }
        let CatalogSession::Postgres(client) = session else {
            return Err(diagram_session_error(session));
        };
        super::diagram::read_postgres(client, database, schema).await
    }

    fn diagram_statements(&self) -> &'static [&'static str] {
        super::diagram::STATEMENTS
    }
}
