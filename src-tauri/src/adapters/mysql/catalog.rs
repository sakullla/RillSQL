//! MySQL 家族（MySQL / MariaDB）目录方言：元数据 SQL、对象种类、列/索引/注释查询与读取入口。
//! MariaDB 复用同一实现，按 `engine` 区分序列与定义读取的差异。

use crate::adapters::catalog::{
    cell, diagram_session_error, group_index_rows, information_schema_primary_key_sql, literal, mismatch, mismatch_body, CatalogDialect,
};
use crate::catalog::hierarchy::IndexInfo;
use crate::catalog::model::{KindSql, MetadataSql, ObjectKind, ObjectRow};
use crate::catalog::objects::ExtrasBody;
use crate::catalog::read::{definition_failure, empty_page, read_failure, read_limit, ConnectionBody, ObjectPageBody, ObjectWindow};
use crate::connections::model::Engine;
use crate::connections::CatalogSession;
use crate::diagram::Diagram;

use super::catalog_read as read;

pub(in crate::adapters) struct MysqlCatalog {
    pub engine: Engine,
}

pub(in crate::adapters) static MYSQL_CATALOG: MysqlCatalog = MysqlCatalog { engine: Engine::Mysql };
pub(in crate::adapters) static MARIADB_CATALOG: MysqlCatalog = MysqlCatalog { engine: Engine::Mariadb };

pub(crate) fn metadata_sql() -> MetadataSql {
    MetadataSql {
        databases: "SELECT schema_name FROM information_schema.schemata WHERE schema_name NOT IN ('information_schema', 'mysql', 'performance_schema', 'sys') ORDER BY schema_name",
        schemas: None,
        objects: "SELECT table_name, CASE WHEN table_type = 'VIEW' THEN 'view' ELSE 'table' END, table_rows, '', '' FROM information_schema.tables WHERE table_schema = ? AND table_type IN ('BASE TABLE', 'VIEW') ORDER BY table_name LIMIT ? OFFSET ?",
        foreign_keys: None,
    }
}

pub(crate) fn listed_kinds(engine: Engine) -> &'static [ObjectKind] {
    // 事件在 MySQL 与 MariaDB 都存在，跟着库走（information_schema.EVENTS.EVENT_SCHEMA）。
    if engine == Engine::Mariadb {
        &[
            ObjectKind::Table,
            ObjectKind::View,
            ObjectKind::Function,
            ObjectKind::Procedure,
            ObjectKind::Trigger,
            ObjectKind::Sequence,
            ObjectKind::Event,
        ]
    } else {
        &[ObjectKind::Table, ObjectKind::View, ObjectKind::Function, ObjectKind::Procedure, ObjectKind::Trigger, ObjectKind::Event]
    }
}

/// 引擎没有的种类：恒空计数与恒空清单。SQL Server 沿用这条降级先例。
pub(in crate::adapters) fn empty_kind_sql() -> KindSql {
    KindSql {
        count: "SELECT 0",
        list: "SELECT table_name, 'table', NULL, '', '' FROM information_schema.tables WHERE 0",
    }
}

pub(crate) fn kind_sql(engine: Engine, kind: ObjectKind) -> KindSql {
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

#[async_trait::async_trait]
impl CatalogDialect for MysqlCatalog {
    fn listed_kinds(&self) -> &'static [ObjectKind] {
        listed_kinds(self.engine)
    }

    fn name_qualifier<'a>(&self, database: &'a str, _schema: &'a str) -> &'a str {
        database
    }

    fn qualifier_is_database(&self) -> bool {
        true
    }

    async fn read_completion_objects(&self, session: &CatalogSession, database: &str) -> Option<Result<Vec<ObjectRow>, String>> {
        match session {
            CatalogSession::Mysql(pool) => Some(super::catalog_read::read_completion_objects(pool, database).await),
            _ => None,
        }
    }

    fn definition_available(&self, kind: ObjectKind) -> bool {
        use ObjectKind::*;
        matches!(kind, Table | View | Function | Procedure | Trigger | Event) || (self.engine == Engine::Mariadb && kind == Sequence)
    }

    fn metadata_sql(&self) -> MetadataSql {
        metadata_sql()
    }

    fn object_count_sql(&self) -> &'static str {
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = ? AND table_type IN ('BASE TABLE', 'VIEW')"
    }

    fn kind_sql(&self, kind: ObjectKind) -> KindSql {
        kind_sql(self.engine, kind)
    }

    fn columns_sql(&self, database: &str, _schema: &str, table: &str) -> Option<String> {
        Some(format!(
            "SELECT column_name, column_type, is_nullable, column_default, column_comment FROM information_schema.columns WHERE table_schema = {} AND table_name = {} ORDER BY ordinal_position",
            literal(database),
            literal(table)
        ))
    }

    /// MySQL 视图的 table_comment 固定是 VIEW，不是注释，排除掉。
    fn table_comments_sql(&self, database: &str, _schema: &str) -> Option<String> {
        Some(format!(
            "SELECT table_name, table_comment FROM information_schema.tables WHERE table_schema = {} AND table_type <> 'VIEW' AND table_comment <> '' ORDER BY table_name",
            literal(database)
        ))
    }

    fn primary_key_sql(&self, database: &str, _schema: &str, table: &str) -> Option<String> {
        Some(information_schema_primary_key_sql(&literal(database), table))
    }

    /// 库清单：每个库的基表数量与数据+索引大小（information_schema.tables 的统计值）。
    fn database_stats_sql(&self) -> Option<String> {
        Some(
            "SELECT s.schema_name, CAST(COUNT(t.table_name) AS CHAR), CAST(SUM(COALESCE(t.data_length, 0) + COALESCE(t.index_length, 0)) AS CHAR) FROM information_schema.schemata s LEFT JOIN information_schema.tables t ON t.table_schema = s.schema_name AND t.table_type = 'BASE TABLE' WHERE s.schema_name NOT IN ('information_schema', 'mysql', 'performance_schema', 'sys') GROUP BY s.schema_name ORDER BY s.schema_name"
                .to_owned(),
        )
    }

    /// 表与视图：table_rows 是估算；视图的 table_comment 固定是 VIEW，不当注释。
    ///
    /// MySQL 8 按 information_schema_stats_expiry（默认 86400 秒）缓存 table_rows/data_length/index_length/update_time，
    /// 所以行数、大小与更新时间都可能滞后至多一天；概览摘要行如实提示统计可能滞后。
    fn object_stats_sql(&self, database: &str, _schema: &str) -> Option<String> {
        Some(format!(
            "SELECT table_name, CASE WHEN table_type = 'VIEW' THEN 'view' ELSE 'table' END, CAST(table_rows AS CHAR), CAST(data_length + index_length AS CHAR), CASE WHEN table_type = 'VIEW' THEN '' ELSE table_comment END, DATE_FORMAT(update_time, '%Y-%m-%d %H:%i:%s') FROM information_schema.tables WHERE table_schema = {} AND table_type IN ('BASE TABLE', 'VIEW', 'SYSTEM VERSIONED') ORDER BY table_name",
            literal(database)
        ))
    }

    /// MySQL 用 SHOW INDEX。
    fn indexes_sql(&self, database: &str, _schema: &str, table: &str) -> Option<String> {
        Some(format!("SHOW INDEX FROM {}", crate::adapters::qualified(self.engine, &[database, table])))
    }

    /// SHOW INDEX：table, non_unique, key_name, seq, column_name ...
    fn indexes_from_rows(&self, rows: &[Vec<Option<String>>]) -> Vec<IndexInfo> {
        group_index_rows((0..rows.len()).map(|index| {
            (
                cell(rows, index, 2),
                cell(rows, index, 4),
                cell(rows, index, 1).trim() == "0",
                cell(rows, index, 2).eq_ignore_ascii_case("PRIMARY"),
                cell(rows, index, 3).parse::<i64>().unwrap_or(0),
            )
        }))
    }

    async fn read_connection(&self, session: &CatalogSession, page: usize, windows: &[ObjectWindow]) -> ConnectionBody {
        let CatalogSession::Mysql(pool) = session else { return mismatch_body() };
        match read::read_mysql(pool, self.engine, page, windows).await {
            Ok(databases) => ConnectionBody { error: None, foreign_keys: None, databases },
            Err(message) => ConnectionBody { error: Some(message), foreign_keys: None, databases: Vec::new() },
        }
    }

    async fn read_outline(&self, session: &CatalogSession) -> ConnectionBody {
        let CatalogSession::Mysql(pool) = session else { return mismatch_body() };
        match read::read_mysql_outline(pool).await {
            Ok(databases) => ConnectionBody { error: None, foreign_keys: None, databases },
            Err(message) => ConnectionBody { error: Some(message), foreign_keys: None, databases: Vec::new() },
        }
    }

    async fn read_kind_page(&self, session: &CatalogSession, database: &str, _schema: &str, kind: ObjectKind, offset: usize, limit: usize) -> ObjectPageBody {
        let CatalogSession::Mysql(pool) = session else { return empty_page(mismatch()) };
        let mut conn = match pool.get_conn().await {
            Ok(conn) => conn,
            Err(err) => return empty_page(read_failure(err)),
        };
        match read::read_mysql_kind_slice(&mut conn, self.engine, database, kind, offset, limit).await {
            Ok((total, objects)) => ObjectPageBody { objects, total, error: None, kind_reports: Vec::new() },
            Err(message) => empty_page(message),
        }
    }

    /// 导航只需要一个窗口和是否还有下一页：不统计总数、不读 table_rows（旧版本/部分代理上会触发统计读取）。
    async fn read_navigation_kind_page(
        &self,
        session: &CatalogSession,
        database: &str,
        schema: &str,
        kind: ObjectKind,
        offset: usize,
        limit: usize,
    ) -> (ObjectPageBody, bool) {
        let CatalogSession::Mysql(pool) = session else {
            return (self.read_kind_page(session, database, schema, kind, offset, read_limit(limit, 0)).await, true);
        };
        read::read_navigation_page(pool, self.engine, database, kind, offset, limit).await
    }

    async fn read_object_page(&self, session: &CatalogSession, database: &str, _schema: &str, offset: usize, limit: usize) -> ObjectPageBody {
        let CatalogSession::Mysql(pool) = session else { return empty_page(mismatch()) };
        read::read_mysql_page(pool, self.engine, database, offset, limit).await
    }

    async fn search_objects(&self, session: &CatalogSession, pattern: &str) -> Vec<(String, String, ObjectRow)> {
        let CatalogSession::Mysql(pool) = session else { return Vec::new() };
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
        let CatalogSession::Mysql(pool) = session else {
            return Err(definition_failure("连接类型和引擎不一致"));
        };
        read::read_mysql_definition(pool, self.engine, database, name, kind).await
    }

    async fn read_extras(&self, session: &CatalogSession, database: &str, _schema: &str) -> ExtrasBody {
        let CatalogSession::Mysql(pool) = session else { return ExtrasBody::default() };
        super::extras::read_mysql_extras(pool, database).await
    }

    async fn read_diagram(&self, session: &CatalogSession, database: &str, _schema: &str) -> Result<Diagram, String> {
        if let Some(message) = super::diagram::empty_mysql_database(database) {
            return Err(message);
        }
        let CatalogSession::Mysql(pool) = session else {
            return Err(diagram_session_error(session));
        };
        super::diagram::read_mysql(pool, database).await
    }

    fn diagram_statements(&self) -> &'static [&'static str] {
        super::diagram::STATEMENTS
    }
}
