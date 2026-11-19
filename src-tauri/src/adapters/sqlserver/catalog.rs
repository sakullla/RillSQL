//! SQL Server 目录方言：库 → 模式 → 对象，元数据走 sys 目录视图；读别的库用三段名，不发 USE。

use crate::adapters::catalog::{diagram_session_error, information_schema_primary_key_sql, literal, mismatch, mismatch_body, CatalogDialect};
use crate::catalog::model::{KindSql, MetadataSql, ObjectKind, ObjectRow, SchemaNode};
use crate::catalog::read::{definition_failure, empty_page, other_database_message, read_failure, ConnectionBody, ObjectPageBody, ObjectWindow};
use crate::connections::CatalogSession;
use crate::diagram::Diagram;

use super::catalog_read as read;

pub(in crate::adapters) struct SqlserverCatalog;

// SQL Server 有独立序列（2012+），没有独立枚举或域。
const LISTED: &[ObjectKind] = &[
    ObjectKind::Table,
    ObjectKind::View,
    ObjectKind::Function,
    ObjectKind::Procedure,
    ObjectKind::Trigger,
    ObjectKind::Sequence,
];

pub(crate) fn listed_kinds() -> &'static [ObjectKind] {
    LISTED
}

pub(crate) fn metadata_sql() -> MetadataSql {
    MetadataSql {
        databases: "SELECT name FROM sys.databases WHERE state = 0 AND name NOT IN ('master', 'tempdb', 'model', 'msdb') ORDER BY name",
        schemas: Some(
            "SELECT name FROM sys.schemas WHERE name NOT IN ('guest', 'INFORMATION_SCHEMA', 'sys') AND name NOT LIKE 'db[_]%' ORDER BY name",
        ),
        objects: "SELECT o.name, x.kind, NULL, '', '' FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id CROSS APPLY (SELECT CASE o.type WHEN 'U' THEN 'table' WHEN 'V' THEN 'view' WHEN 'FN' THEN 'function' WHEN 'IF' THEN 'function' WHEN 'TF' THEN 'function' WHEN 'AF' THEN 'function' WHEN 'FT' THEN 'function' WHEN 'P' THEN 'procedure' WHEN 'PC' THEN 'procedure' WHEN 'SO' THEN 'sequence' ELSE 'table' END AS kind) x WHERE s.name = @P1 AND o.type IN ('U', 'V', 'FN', 'IF', 'TF', 'AF', 'FT', 'P', 'PC', 'SO') ORDER BY o.name OFFSET 0 ROWS FETCH NEXT @P2 ROWS ONLY",
        foreign_keys: None,
    }
}

/// SQL Server 的分类查询。@P1 是 schema 名，@P2 是窗口大小；OFFSET/FETCH 需要 ORDER BY，都按名称排序。
pub(crate) fn kind_sql(kind: ObjectKind) -> KindSql {
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
        ObjectKind::Enum | ObjectKind::Domain | ObjectKind::Extension | ObjectKind::Event => crate::adapters::mysql::catalog::empty_kind_sql(),
    }
}

/// SQL Server 的三段名限定：目录视图与“只在当前库生效”的函数都换成目标库内的写法。
/// 在同一个会话上读别的库，不发 USE、不改当前库、也不另开连接。
pub(crate) fn sqlserver_qualified(sql: &str, database: &str) -> String {
    let database = database.trim();
    if database.is_empty() {
        return sql.to_owned();
    }
    let name = format!("[{}]", database.replace(']', "]]"));
    sql.replace("sys.", &format!("{name}.sys."))
        .replace("information_schema.", &format!("{name}.information_schema."))
        // OBJECT_NAME 只认当前库；三段名读取时按目标库查父对象名。
        .replace("OBJECT_NAME(t.parent_id)", &format!("(SELECT name FROM {name}.sys.objects WHERE object_id = t.parent_id)"))
}

#[async_trait::async_trait]
impl CatalogDialect for SqlserverCatalog {
    fn listed_kinds(&self) -> &'static [ObjectKind] {
        LISTED
    }

    fn default_schema(&self) -> Option<&'static str> {
        Some("dbo")
    }

    fn definition_available(&self, kind: ObjectKind) -> bool {
        use ObjectKind::*;
        matches!(kind, Table | View | Function | Procedure | Trigger | Sequence)
    }

    fn table_is_rebuilt(&self) -> bool {
        true
    }

    fn metadata_sql(&self) -> MetadataSql {
        metadata_sql()
    }

    fn object_count_sql(&self) -> &'static str {
        "SELECT COUNT(*) FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = @P1 AND o.type IN ('U', 'V', 'FN', 'IF', 'TF', 'AF', 'FT', 'P', 'PC', 'SO')"
    }

    fn kind_sql(&self, kind: ObjectKind) -> KindSql {
        kind_sql(kind)
    }

    fn columns_sql(&self, _database: &str, schema: &str, table: &str) -> Option<String> {
        Some(format!(
            "SELECT c.column_name, CASE WHEN c.data_type IN ('varchar','nvarchar','char','nchar','varbinary','binary') AND c.character_maximum_length IS NOT NULL THEN c.data_type + '(' + CASE WHEN c.character_maximum_length = -1 THEN 'MAX' ELSE CAST(c.character_maximum_length AS varchar(16)) END + ')' WHEN c.data_type IN ('decimal','numeric') AND c.numeric_precision IS NOT NULL THEN c.data_type + '(' + CAST(c.numeric_precision AS varchar(16)) + ',' + CAST(ISNULL(c.numeric_scale, 0) AS varchar(16)) + ')' ELSE c.data_type END, c.is_nullable, c.column_default, CAST(ep.value AS nvarchar(4000)) FROM information_schema.columns c LEFT JOIN sys.extended_properties ep ON ep.class = 1 AND ep.name = 'MS_Description' AND ep.major_id = OBJECT_ID(QUOTENAME(c.table_schema) + '.' + QUOTENAME(c.table_name)) AND ep.minor_id = COLUMNPROPERTY(OBJECT_ID(QUOTENAME(c.table_schema) + '.' + QUOTENAME(c.table_name)), c.column_name, 'ColumnId') WHERE c.table_schema = {} AND c.table_name = {} ORDER BY c.ordinal_position",
            literal(schema),
            literal(table)
        ))
    }

    fn table_comments_sql(&self, _database: &str, schema: &str) -> Option<String> {
        Some(format!(
            "SELECT o.name, CAST(ep.value AS nvarchar(4000)) FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id JOIN sys.extended_properties ep ON ep.class = 1 AND ep.major_id = o.object_id AND ep.minor_id = 0 AND ep.name = 'MS_Description' WHERE s.name = {} AND o.type IN ('U', 'V') ORDER BY o.name",
            literal(schema)
        ))
    }

    fn table_comment_name_column(&self) -> &'static str {
        "o.name"
    }

    fn primary_key_sql(&self, _database: &str, schema: &str, table: &str) -> Option<String> {
        Some(information_schema_primary_key_sql(&literal(schema), table))
    }

    fn indexes_sql(&self, _database: &str, schema: &str, table: &str) -> Option<String> {
        Some(format!(
            "SELECT i.name, c.name, i.is_unique, i.is_primary_key FROM sys.indexes i JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id JOIN sys.columns c ON c.object_id = i.object_id AND c.column_id = ic.column_id JOIN sys.tables t ON t.object_id = i.object_id JOIN sys.schemas s ON s.schema_id = t.schema_id WHERE s.name = {} AND t.name = {} AND i.name IS NOT NULL ORDER BY i.name, ic.key_ordinal",
            literal(schema),
            literal(table)
        ))
    }

    /// 列/索引/主键查询里既有目录视图也有只在当前库生效的函数：读非当前库时按目标库三段名限定。
    fn scope_sql(&self, database: &str, sql: &str) -> String {
        sqlserver_qualified(sql, database)
    }

    async fn read_connection(&self, session: &CatalogSession, page: usize, windows: &[ObjectWindow]) -> ConnectionBody {
        let CatalogSession::Sqlserver(client) = session else { return mismatch_body() };
        match read::read_sqlserver(client, page, windows).await {
            Ok(databases) => ConnectionBody { error: None, foreign_keys: None, databases },
            Err(message) => ConnectionBody { error: Some(message), foreign_keys: None, databases: Vec::new() },
        }
    }

    async fn read_outline(&self, session: &CatalogSession) -> ConnectionBody {
        let CatalogSession::Sqlserver(client) = session else { return mismatch_body() };
        match read::read_sqlserver_outline(client).await {
            Ok(databases) => ConnectionBody { error: None, foreign_keys: None, databases },
            Err(message) => ConnectionBody { error: Some(message), foreign_keys: None, databases: Vec::new() },
        }
    }

    /// 在原会话上用三段名读目标库的 schema。
    async fn read_database_outline(&self, session: &CatalogSession, database: &str) -> Result<Vec<SchemaNode>, String> {
        let CatalogSession::Sqlserver(client) = session else {
            return Err(read_failure("这个引擎的库层级已经随连接一次读出，不需要按库再读"));
        };
        read::read_database_schemas(client, database).await
    }

    async fn read_kind_page(&self, session: &CatalogSession, database: &str, schema: &str, kind: ObjectKind, offset: usize, limit: usize) -> ObjectPageBody {
        let CatalogSession::Sqlserver(client) = session else { return empty_page(mismatch()) };
        match read::read_sqlserver_kind_slice(client, database, schema, kind, offset, limit).await {
            Ok((total, objects)) => ObjectPageBody { objects, total, error: None, kind_reports: Vec::new() },
            Err(message) => empty_page(message),
        }
    }

    async fn read_object_page(&self, session: &CatalogSession, database: &str, schema: &str, offset: usize, limit: usize) -> ObjectPageBody {
        let CatalogSession::Sqlserver(client) = session else { return empty_page(mismatch()) };
        read::read_sqlserver_page(client, database, schema, offset, limit).await
    }

    async fn search_objects(&self, session: &CatalogSession, pattern: &str) -> Vec<(String, String, ObjectRow)> {
        let CatalogSession::Sqlserver(client) = session else { return Vec::new() };
        read::search(client, pattern).await
    }

    async fn read_definition(
        &self,
        session: &CatalogSession,
        database: &str,
        schema: &str,
        name: &str,
        kind: &str,
        _signature: &str,
        _relation: &str,
    ) -> Result<String, String> {
        let CatalogSession::Sqlserver(client) = session else {
            return Err(definition_failure("连接类型和引擎不一致"));
        };
        let current = read::sqlserver_current_database(client).await?;
        if database != current {
            return Err(definition_failure(other_database_message(&current, database)));
        }
        read::read_sqlserver_definition(client, schema, name, kind).await
    }

    async fn read_diagram(&self, session: &CatalogSession, database: &str, schema: &str) -> Result<Diagram, String> {
        if let Some(message) = crate::diagram::model::empty_sqlserver_schema(schema) {
            return Err(message);
        }
        let CatalogSession::Sqlserver(client) = session else {
            return Err(diagram_session_error(session));
        };
        super::diagram::read_sqlserver(client, database, schema).await
    }

    fn diagram_statements(&self) -> &'static [&'static str] {
        super::diagram::STATEMENTS
    }
}
