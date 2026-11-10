//! 表级外键读取：补全的 ON 条件只有实际读到外键时才标成外键。
//!
//! 关系型引擎复用目录/网格已有的按需读取（SQLite、MySQL/MariaDB、PostgreSQL），
//! SQL Server 在这里补一条 sys 目录查询；键/文档引擎没有外键，空集不是失败。

use crate::catalog::Engine;
use crate::connections::CatalogSession;

/// 一个表级外键：来源列与目标列按顺序成对。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

/// 读一个表的外键。失败只影响这张表的 ON 条件，不影响已读到的对象和列。
pub async fn read_table_keys(
    session: &CatalogSession,
    engine: Engine,
    database: &str,
    schema: &str,
    table: &str,
) -> Result<Vec<TableKey>, String> {
    if table.is_empty() {
        return Ok(Vec::new());
    }
    match engine {
        Engine::Redis | Engine::Mongodb => Ok(Vec::new()),
        Engine::Sqlserver => sqlserver_keys(session, schema, table).await,
        Engine::Sqlite | Engine::Mysql | Engine::Mariadb | Engine::Postgres => {
            let keys = crate::grid::foreign_keys(session, database, schema, table, engine.as_str()).await?;
            Ok(keys
                .into_iter()
                .map(|key| TableKey {
                    name: key.name,
                    columns: key.columns,
                    ref_schema: key.ref_schema,
                    ref_table: key.ref_table,
                    ref_columns: key.ref_columns,
                })
                .collect())
        }
    }
}

fn literal(text: &str) -> String {
    format!("N'{}'", text.replace('\'', "''"))
}

/// SQL Server 的出向外键查询；只读 sys 目录，不碰用户表。
pub fn sqlserver_keys_sql(schema: &str, table: &str) -> String {
    let mut sql = String::from(
        "SELECT fk.name, pc.name, rs.name, rt.name, rc.name \
         FROM sys.foreign_key_columns fkc \
         JOIN sys.foreign_keys fk ON fk.object_id = fkc.constraint_object_id \
         JOIN sys.tables pt ON pt.object_id = fkc.parent_object_id \
         JOIN sys.schemas ps ON ps.schema_id = pt.schema_id \
         JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id \
         JOIN sys.tables rt ON rt.object_id = fkc.referenced_object_id \
         JOIN sys.schemas rs ON rs.schema_id = rt.schema_id \
         JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
         WHERE pt.name = ",
    );
    sql.push_str(&literal(table));
    if !schema.is_empty() {
        sql.push_str(" AND ps.name = ");
        sql.push_str(&literal(schema));
    }
    sql.push_str(" ORDER BY fk.name, fkc.constraint_column_id");
    sql
}

async fn sqlserver_keys(session: &CatalogSession, schema: &str, table: &str) -> Result<Vec<TableKey>, String> {
    let rows = crate::catalog::detail::rows_for(session, &sqlserver_keys_sql(schema, table)).await?;
    let mut keys: Vec<TableKey> = Vec::new();
    for row in rows {
        let cell = |index: usize| row.get(index).and_then(|value| value.clone()).unwrap_or_default();
        let name = cell(0);
        let column = cell(1);
        if name.is_empty() || column.is_empty() {
            continue;
        }
        let ref_schema = cell(2);
        let ref_table = cell(3);
        let ref_column = cell(4);
        if ref_table.is_empty() || ref_column.is_empty() {
            continue;
        }
        match keys.iter_mut().find(|key| key.name == name) {
            Some(existing) => {
                existing.columns.push(column);
                existing.ref_columns.push(ref_column);
            }
            None => keys.push(TableKey {
                name,
                columns: vec![column],
                ref_schema,
                ref_table,
                ref_columns: vec![ref_column],
            }),
        }
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlserver_query_reads_the_sys_catalog_without_user_tables() {
        let sql = sqlserver_keys_sql("dbo", "orders");
        assert!(sql.contains("sys.foreign_key_columns"), "{sql}");
        assert!(sql.contains("pt.name = N'orders'"), "{sql}");
        assert!(sql.contains("ps.name = N'dbo'"), "{sql}");
        assert!(sql.contains("referenced_column_id"), "{sql}");
        // 引号不能跑出字面量。
        let quoted = sqlserver_keys_sql("db'o", "o'rders");
        assert!(quoted.contains("N'db''o'"), "{quoted}");
        assert!(quoted.contains("N'o''rders'"), "{quoted}");
        // 没有模式时只按表名过滤。
        assert!(!sqlserver_keys_sql("", "orders").contains("ps.name = "));
    }
}
