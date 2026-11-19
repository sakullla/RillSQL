//! 表级外键读取：补全的 ON 条件只有实际读到外键时才标成外键。
//!
//! 读取由会话所属引擎的适配器完成（与网格同一套按需读取）；键/文档引擎没有外键，空集不是失败。

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
    if table.is_empty() || !engine.is_relational() {
        return Ok(Vec::new());
    }
    let keys = session.completion_foreign_keys(database, schema, table).await?;
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

#[cfg(test)]
mod tests {
    use crate::adapters::sqlserver::grid_read::foreign_key_sql as sqlserver_keys_sql;

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
