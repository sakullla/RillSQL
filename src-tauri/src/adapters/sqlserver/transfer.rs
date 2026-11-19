//! SQL Server 的对比/导出/迁移方言（A5）：ALTER COLUMN 不带默认值、默认值是具名约束、按模式限定，
//! 只对比/导出当前连接的库（不改连、不开临时会话）。

use crate::adapters::transfer::{compare_mismatch, constraint_primary_key, dump_mismatch, dump_session_gap, qualify, read_failure, TableAlter, TransferDialect};
use crate::adapters::{quote_ident, ErrorRules};
use crate::compare::model::{ColumnShape, ObjectDef, Side};
use crate::compare::structure::column_clause;
use crate::connections::model::Engine;
use crate::connections::CatalogSession;
use crate::grid::Cell;

use super::ddl::{sqlserver_add_default_sql, sqlserver_bare_table_name, sqlserver_drop_default_sql};
use super::grid_read::sqlserver_rows;

const ENGINE: Engine = Engine::Sqlserver;

fn quote(name: &str) -> String {
    quote_ident(ENGINE, name)
}

pub(in crate::adapters) struct SqlserverTransfer;

#[async_trait::async_trait]
impl TransferDialect for SqlserverTransfer {
    /// T-SQL 的表差异：ADD/DROP COLUMN，改类型或可空性用 ALTER COLUMN <def>，主键按约束增删。
    fn alter_table(&self, alter: &TableAlter<'_>) -> Vec<String> {
        let (name, baseline, target) = (alter.name, alter.baseline, alter.target);
        let table = alter.table(ENGINE);
        let mut statements = Vec::new();
        for column in &target.columns {
            if !baseline.columns.iter().any(|item| item.name == column.name) {
                statements.push(format!("ALTER TABLE {table} DROP COLUMN {}", quote(&column.name)));
            }
        }
        for column in &baseline.columns {
            match target.columns.iter().find(|item| item.name == column.name) {
                None => statements.push(format!("ALTER TABLE {table} ADD {}", column_clause(ENGINE, column))),
                Some(other) if !column.type_name.eq_ignore_ascii_case(&other.type_name) || column.nullable != other.nullable => {
                    // T-SQL 的 ALTER COLUMN 只接受列名/类型/NULL 性；默认值不能内联，走下面的约束分支。
                    statements.push(format!("ALTER TABLE {table} ALTER COLUMN {}", alter_column_clause(column)));
                }
                Some(other) if column.default_sql.trim() != other.default_sql.trim() => {
                    statements.extend(default_change(&table, name, column, other));
                }
                Some(_) => {}
            }
        }
        if alter.with_pk {
            constraint_primary_key(ENGINE, &table, name, baseline, target, &mut statements);
        }
        statements
    }

    fn drop_index(&self, side: &Side, table: &str, index: &str) -> String {
        format!("DROP INDEX {} ON {}", quote(index), qualify(ENGINE, side, table))
    }

    /// DDL 触发器之外，DML 触发器名挂在表上。
    fn drop_trigger(&self, _side: &Side, object: &ObjectDef) -> String {
        format!("DROP TRIGGER {}", quote(&object.name))
    }

    fn drop_routine(&self, side: &Side, word: &str, object: &ObjectDef) -> String {
        format!("DROP {word} {}", qualify(ENGINE, side, &object.name))
    }

    /// 默认值是具名约束：名字未知时按 sys.default_constraints 动态查找。
    fn drop_default(&self, table: &str, column: &str) -> String {
        sqlserver_drop_default_sql(table, column, "")
    }

    fn set_default(&self, table: &str, column: &str, expression: &str) -> String {
        sqlserver_add_default_sql(table, &sqlserver_bare_table_name(table), column, expression)
    }

    async fn dump_scope(&self, session: &CatalogSession, database: &str, schemas: &[String]) -> Result<(String, Vec<String>), String> {
        let current = current_database(session).await?;
        if !database.is_empty() && database != current {
            return Err(dump_session_gap(&current, database));
        }
        if !schemas.is_empty() {
            return Ok((current, schemas.to_vec()));
        }
        let sql = crate::catalog::metadata_sql(ENGINE).schemas.ok_or_else(|| read_failure("SQL Server 缺少 schema 查询"))?;
        let all = text_column(session, sql).await?;
        if all.is_empty() {
            return Err(read_failure("没有读到 schema"));
        }
        Ok((current, all))
    }

    /// 不改连也不开临时会话：对比只落在当前连接的库上，点名其他库直接说明。
    async fn compare_scope(&self, session: &CatalogSession, database: &str, schema: &str) -> Result<(), String> {
        let CatalogSession::Sqlserver(client) = session else {
            return Err(compare_mismatch());
        };
        let (_names, rows) = sqlserver_rows(client, "SELECT DB_NAME()").await.map_err(|err| err.to_string())?;
        let current = match rows.first().and_then(|row| row.first()) {
            Some(Cell::Text(text)) => text.clone(),
            _ => String::new(),
        };
        if current != database {
            return Err(format!("当前连接打开的是 {current}，没有另开连接读取 {database}"));
        }
        if schema.trim().is_empty() {
            return Err("SQL Server 需要指定 schema".to_owned());
        }
        let sql = format!("SELECT name FROM sys.schemas WHERE name = N'{}'", schema.trim().replace('\'', "''"));
        let (_names, rows) = sqlserver_rows(client, &sql).await.map_err(|err| err.to_string())?;
        if rows.is_empty() {
            return Err(format!("schema {schema} 不存在"));
        }
        Ok(())
    }

    async fn schema_names(&self, session: &CatalogSession, sql: &str) -> Result<Vec<String>, String> {
        let CatalogSession::Sqlserver(client) = session else {
            return Err(compare_mismatch());
        };
        let (_names, rows) = sqlserver_rows(client, sql).await.map_err(|err| err.to_string())?;
        let mut names = Vec::new();
        for row in &rows {
            if let Some(Cell::Text(name)) = row.first() {
                if !name.is_empty() {
                    names.push(name.clone());
                }
            }
        }
        Ok(names)
    }
}

/// T-SQL 的 ALTER COLUMN 列定义：不含默认值（内联 DEFAULT 在 ALTER COLUMN 里非法）。
fn alter_column_clause(column: &ColumnShape) -> String {
    let mut line = format!("{} {}", quote(&column.name), column.type_name.trim());
    if !column.nullable {
        line.push_str(" NOT NULL");
    }
    line
}

/// T-SQL 的默认值变更：具名约束的 DROP CONSTRAINT / ADD CONSTRAINT ... DEFAULT ... FOR。
/// 删除时优先用从 sys.default_constraints 读到的约束名；名字缺失时动态查找，不猜 DF_ 前缀名。
/// 新增用生成的 DF_<表>_<列> 名（与 SQL Server 自动命名习惯一致，重名冲突由执行报错暴露）。
fn default_change(table: &str, table_name: &str, wanted: &ColumnShape, current: &ColumnShape) -> Vec<String> {
    let mut statements = Vec::new();
    let wanted_default = wanted.default_sql.trim();
    let current_default = current.default_sql.trim();
    if wanted_default.is_empty() && current_default.is_empty() {
        return statements;
    }
    if !current_default.is_empty() {
        statements.push(sqlserver_drop_default_sql(table, &wanted.name, &current.default_constraint));
    }
    if !wanted_default.is_empty() {
        statements.push(sqlserver_add_default_sql(table, table_name, &wanted.name, wanted_default));
    }
    statements
}

/// SQL Server 的写入冲突措辞：`Violation of PRIMARY KEY constraint` / `Violation of UNIQUE KEY constraint`
/// 与唯一索引的 `Cannot insert duplicate key row`。
pub(in crate::adapters) struct SqlserverErrors;

impl ErrorRules for SqlserverErrors {
    fn is_conflict(&self, lower: &str) -> bool {
        lower.contains("violation of primary key constraint") || lower.contains("violation of unique key constraint") || lower.contains("duplicate key")
    }
}

async fn current_database(session: &CatalogSession) -> Result<String, String> {
    let CatalogSession::Sqlserver(client) = session else {
        return Err(dump_mismatch());
    };
    let (_names, rows) = sqlserver_rows(client, "SELECT DB_NAME()").await.map_err(read_failure)?;
    match rows.first().and_then(|row| row.first()) {
        Some(Cell::Text(text)) => Ok(text.clone()),
        _ => Err(read_failure("没有当前数据库")),
    }
}

async fn text_column(session: &CatalogSession, sql: &str) -> Result<Vec<String>, String> {
    let CatalogSession::Sqlserver(client) = session else {
        return Err(dump_mismatch());
    };
    let (_names, rows) = sqlserver_rows(client, sql).await.map_err(read_failure)?;
    rows.iter()
        .map(|row| match row.first() {
            Some(Cell::Text(text)) => Ok(text.clone()),
            _ => Err(read_failure("元数据列是空值")),
        })
        .collect()
}

type TdsHandle = std::sync::Arc<tokio::sync::Mutex<crate::adapters::session::TdsClient>>;

pub(in crate::adapters) async fn execute(client: &TdsHandle, statement: &str) -> Result<(), String> {
    let mut locked = client.lock().await;
    locked.execute(statement.to_owned(), &[]).await.map(|_| ()).map_err(|err| crate::error_text::chain(&err))
}

/// 同一条 TDS 连接上的显式事务；失败回滚后由调用方降级逐行重放。
pub(in crate::adapters) async fn insert_in_transaction(client: &TdsHandle, statement: &str) -> Result<(), String> {
    let mut locked = client.lock().await;
    locked.execute("BEGIN TRANSACTION", &[]).await.map_err(|err| crate::error_text::chain(&err))?;
    match locked.execute(statement.to_owned(), &[]).await {
        Ok(_) => locked.execute("COMMIT", &[]).await.map(|_| ()).map_err(|err| crate::error_text::chain(&err)),
        Err(error) => {
            let _ = locked.execute("ROLLBACK", &[]).await;
            Err(error.to_string())
        }
    }
}
