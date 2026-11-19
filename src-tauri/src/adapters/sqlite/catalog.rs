//! SQLite 目录方言：没有库/模式层（main 与 ATTACH 上来的库各有自己的 sqlite_master），列与索引走 PRAGMA。

use crate::adapters::catalog::{cell, diagram_session_error, group_index_rows, mismatch, mismatch_body, yes, CatalogDialect};
use crate::catalog::hierarchy::{ColumnInfo, IndexInfo};
use crate::catalog::model::{KindSql, MetadataSql, ObjectKind, ObjectRow};
use crate::catalog::objects::ExtrasBody;
use crate::catalog::read::{definition_failure, empty_page, ConnectionBody, ObjectPageBody, ObjectWindow};
use crate::connections::CatalogSession;
use crate::diagram::Diagram;
use crate::query::assist::ColumnMeta;

use super::catalog_read as read;

pub(in crate::adapters) struct SqliteCatalog;

const LISTED: &[ObjectKind] = &[ObjectKind::Table, ObjectKind::View, ObjectKind::Trigger];

pub(crate) fn listed_kinds() -> &'static [ObjectKind] {
    LISTED
}

pub(crate) fn kind_sql(kind: ObjectKind) -> KindSql {
    match kind {
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
    }
}

/// SQLite 的对象按「库」分开读：main 与每个 ATTACH 上来的库各有自己的 sqlite_master。
/// 名字用双引号括起来，内嵌双引号按 SQL 规则翻倍。
pub(crate) fn quote_sqlite(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub(crate) fn sqlite_kind_count_sql(database: &str) -> String {
    format!(
        "SELECT type, COUNT(*) FROM {}.sqlite_master WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%' GROUP BY type",
        quote_sqlite(database)
    )
}

/// 只读一种对象的一页；种类名来自固定表，不拼用户输入。
pub(crate) fn sqlite_kind_page_sql(database: &str, kind: &str) -> String {
    format!(
        "SELECT name, type, '', '' FROM {}.sqlite_master WHERE type = '{}' AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?1 OFFSET ?2",
        quote_sqlite(database),
        kind
    )
}

pub(crate) fn sqlite_object_page_sql(database: &str) -> String {
    format!(
        "SELECT name, type, '', '' FROM {}.sqlite_master WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?1 OFFSET ?2",
        quote_sqlite(database)
    )
}

fn quote(name: &str) -> String {
    use crate::adapters::SqlSyntax;
    super::syntax::SqliteSyntax.quote_ident(name)
}

#[async_trait::async_trait]
impl CatalogDialect for SqliteCatalog {
    fn listed_kinds(&self) -> &'static [ObjectKind] {
        LISTED
    }

    fn name_qualifier<'a>(&self, _database: &'a str, _schema: &'a str) -> &'a str {
        ""
    }

    fn definition_available(&self, kind: ObjectKind) -> bool {
        matches!(kind, ObjectKind::Table | ObjectKind::View | ObjectKind::Trigger)
    }

    fn metadata_sql(&self) -> MetadataSql {
        MetadataSql {
            databases: "",
            schemas: None,
            objects: "SELECT name, type, '', '' FROM sqlite_master WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?1 OFFSET ?2",
            foreign_keys: Some("PRAGMA foreign_keys"),
        }
    }

    fn object_count_sql(&self) -> &'static str {
        "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%'"
    }

    fn kind_sql(&self, kind: ObjectKind) -> KindSql {
        kind_sql(kind)
    }

    fn columns_sql(&self, _database: &str, _schema: &str, table: &str) -> Option<String> {
        Some(format!("PRAGMA table_info({})", quote(table)))
    }

    fn default_database(&self) -> Option<&'static str> {
        Some("main")
    }

    /// 表与视图：SQLite 没有行数估算、注释与更新时间；大小来自 dbstat 虚表（表自己与它的索引的页）。
    /// 没有编译 dbstat 时整条查询失败，概览退回名称清单并标注统计缺失。
    fn object_stats_sql(&self, database: &str, _schema: &str) -> Option<String> {
        let schema = quote_sqlite(database);
        let name = crate::adapters::catalog::literal(database);
        Some(format!(
            "SELECT m.name, m.type, NULL, CASE WHEN m.type = 'table' THEN (SELECT SUM(d.pgsize) FROM dbstat({name}) d WHERE d.name IN (SELECT i.name FROM {schema}.sqlite_master i WHERE i.tbl_name = m.name AND i.type IN ('table', 'index'))) END, '', '' FROM {schema}.sqlite_master m WHERE m.type IN ('table', 'view') AND m.name NOT LIKE 'sqlite_%' ORDER BY m.name"
        ))
    }

    fn indexes_sql(&self, _database: &str, _schema: &str, table: &str) -> Option<String> {
        Some(format!("PRAGMA index_list({})", quote(table)))
    }

    fn index_columns_sql(&self, index: &str) -> Option<String> {
        Some(format!("PRAGMA index_info({})", quote(index)))
    }

    /// PRAGMA table_info：cid, name, type, notnull, default, pk。
    fn columns_from_rows(&self, rows: &[Vec<Option<String>>]) -> Vec<ColumnInfo> {
        (0..rows.len())
            .map(|index| {
                let notnull = cell(rows, index, 3).trim() == "1";
                let pk = cell(rows, index, 5);
                ColumnInfo {
                    name: cell(rows, index, 1),
                    data_type: cell(rows, index, 2),
                    nullable: !notnull,
                    primary: !pk.is_empty() && pk.trim() != "0",
                }
            })
            .filter(|column| !column.name.is_empty())
            .collect()
    }

    /// SQLite 没有列注释，主键在 PRAGMA 的 pk 列。
    fn column_meta_from_rows(&self, rows: &[Vec<Option<String>>]) -> Vec<ColumnMeta> {
        (0..rows.len())
            .filter_map(|index| {
                let name = cell(rows, index, 1);
                if name.is_empty() {
                    return None;
                }
                let nullable = match cell(rows, index, 3).trim() {
                    "" => None,
                    flag => Some(flag != "1"),
                };
                let pk = cell(rows, index, 5);
                Some(ColumnMeta {
                    name,
                    type_name: cell(rows, index, 2),
                    nullable,
                    comment: String::new(),
                    primary: !pk.trim().is_empty() && pk.trim() != "0",
                    foreign_key: false,
                })
            })
            .collect()
    }

    /// PRAGMA index_list：seq, name, unique, ...；列由 `index_columns_sql` 逐个读取。
    fn indexes_from_rows(&self, rows: &[Vec<Option<String>>]) -> Vec<IndexInfo> {
        group_index_rows((0..rows.len()).map(|index| (cell(rows, index, 1), String::new(), yes(&cell(rows, index, 2)), false, 0)))
    }

    async fn read_connection(&self, session: &CatalogSession, page: usize, windows: &[ObjectWindow]) -> ConnectionBody {
        let CatalogSession::Sqlite { pool, foreign_keys } = session else { return mismatch_body() };
        read::read_sqlite(pool, *foreign_keys, page, windows).await
    }

    /// SQLite 的“库”就是 main 与每个 ATTACH 上来的库：ATTACH 之后它们同样出现在树上。
    async fn read_outline(&self, session: &CatalogSession) -> ConnectionBody {
        let CatalogSession::Sqlite { pool, foreign_keys } = session else { return mismatch_body() };
        match read::sqlite_database_names(pool).await {
            Ok(databases) => ConnectionBody { error: None, foreign_keys: *foreign_keys, databases },
            Err(message) => ConnectionBody { error: Some(message), foreign_keys: *foreign_keys, databases: Vec::new() },
        }
    }

    async fn read_kind_page(&self, session: &CatalogSession, database: &str, _schema: &str, kind: ObjectKind, offset: usize, limit: usize) -> ObjectPageBody {
        let CatalogSession::Sqlite { pool, .. } = session else { return empty_page(mismatch()) };
        read::read_kind_page(pool, database, kind, offset, limit).await
    }

    async fn read_object_page(&self, session: &CatalogSession, database: &str, _schema: &str, offset: usize, limit: usize) -> ObjectPageBody {
        let CatalogSession::Sqlite { pool, .. } = session else { return empty_page(mismatch()) };
        match read::read_sqlite_objects(pool, database, offset, limit).await {
            Ok(schema_node) => ObjectPageBody {
                objects: schema_node.objects,
                total: schema_node.total,
                error: schema_node.error,
                kind_reports: schema_node.kind_reports,
            },
            Err(message) => empty_page(message),
        }
    }

    async fn search_objects(&self, session: &CatalogSession, pattern: &str) -> Vec<(String, String, ObjectRow)> {
        let CatalogSession::Sqlite { pool, .. } = session else { return Vec::new() };
        read::search(pool, pattern).await
    }

    async fn read_definition(
        &self,
        session: &CatalogSession,
        database: &str,
        _schema: &str,
        name: &str,
        kind: &str,
        _signature: &str,
        _relation: &str,
    ) -> Result<String, String> {
        let CatalogSession::Sqlite { pool, .. } = session else {
            return Err(definition_failure("连接类型和引擎不一致"));
        };
        read::read_sqlite_definition(pool, database, name, kind).await
    }

    async fn read_extras(&self, session: &CatalogSession, _database: &str, _schema: &str) -> ExtrasBody {
        let CatalogSession::Sqlite { pool, .. } = session else { return ExtrasBody::default() };
        super::extras::read_sqlite_extras(pool).await
    }

    async fn read_diagram(&self, session: &CatalogSession, _database: &str, _schema: &str) -> Result<Diagram, String> {
        let CatalogSession::Sqlite { pool, .. } = session else {
            return Err(diagram_session_error(session));
        };
        super::diagram::read_sqlite(pool).await
    }

    fn diagram_statements(&self) -> &'static [&'static str] {
        super::diagram::STATEMENTS
    }
}
