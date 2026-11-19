//! MySQL 家族的对比/导出/迁移方言（A5）：MODIFY COLUMN 改列、按库限定、DROP PRIMARY KEY。
//! MariaDB 复用同一实现，只是引号/保留字随引擎，且函数/过程支持 CREATE OR REPLACE。

use mysql_async::prelude::Queryable;

use crate::adapters::transfer::{compare_mismatch, qualify, TableAlter, TransferDialect, ViewReplace};
use crate::adapters::{quote_ident, ErrorRules};
use crate::compare::model::{ObjectDef, Side};
use crate::compare::structure::{column_clause, primary_columns};
use crate::connections::model::Engine;
use crate::connections::CatalogSession;

pub(in crate::adapters) struct MysqlTransfer {
    pub engine: Engine,
    /// 返回类型没变的函数/过程能 CREATE OR REPLACE（MariaDB 有，MySQL 没有）。
    pub routine_or_replace: bool,
}

pub(in crate::adapters) static MYSQL_TRANSFER: MysqlTransfer = MysqlTransfer { engine: Engine::Mysql, routine_or_replace: false };
pub(in crate::adapters) static MARIADB_TRANSFER: MysqlTransfer = MysqlTransfer { engine: Engine::Mariadb, routine_or_replace: true };

impl MysqlTransfer {
    fn quote(&self, name: &str) -> String {
        quote_ident(self.engine, name)
    }
}

#[async_trait::async_trait]
impl TransferDialect for MysqlTransfer {
    fn alter_table(&self, alter: &TableAlter<'_>) -> Vec<String> {
        let engine = self.engine;
        let (baseline, target) = (alter.baseline, alter.target);
        let table = alter.table(engine);
        let mut statements = Vec::new();
        for column in &target.columns {
            if !baseline.columns.iter().any(|item| item.name == column.name) {
                statements.push(format!("ALTER TABLE {table} DROP COLUMN {}", self.quote(&column.name)));
            }
        }
        for column in &baseline.columns {
            match target.columns.iter().find(|item| item.name == column.name) {
                None => statements.push(format!("ALTER TABLE {table} ADD COLUMN {}", column_clause(engine, column))),
                Some(other)
                    if !column.type_name.eq_ignore_ascii_case(&other.type_name) || column.nullable != other.nullable || column.default_sql.trim() != other.default_sql.trim() =>
                {
                    statements.push(format!("ALTER TABLE {table} MODIFY COLUMN {}", column_clause(engine, column)));
                }
                _ => {}
            }
        }
        if alter.with_pk {
            let wanted = primary_columns(baseline);
            let current = primary_columns(target);
            if wanted != current {
                let names = wanted.iter().map(|column| self.quote(column)).collect::<Vec<_>>().join(", ");
                match (!current.is_empty(), !wanted.is_empty()) {
                    (true, true) => statements.push(format!("ALTER TABLE {table} DROP PRIMARY KEY, ADD PRIMARY KEY ({names})")),
                    (true, false) => statements.push(format!("ALTER TABLE {table} DROP PRIMARY KEY")),
                    (false, true) => statements.push(format!("ALTER TABLE {table} ADD PRIMARY KEY ({names})")),
                    (false, false) => {}
                }
            }
        }
        statements
    }

    fn drop_index(&self, side: &Side, table: &str, index: &str) -> String {
        format!("DROP INDEX {} ON {}", self.quote(index), qualify(self.engine, side, table))
    }

    fn drop_trigger(&self, side: &Side, object: &ObjectDef) -> String {
        format!("DROP TRIGGER {}", qualify(self.engine, side, &object.name))
    }

    fn drop_routine(&self, side: &Side, word: &str, object: &ObjectDef) -> String {
        format!("DROP {word} {}", qualify(self.engine, side, &object.name))
    }

    fn drop_default(&self, table: &str, column: &str) -> String {
        format!("ALTER TABLE {table} ALTER {} DROP DEFAULT", self.quote(column))
    }

    fn set_default(&self, table: &str, column: &str, expression: &str) -> String {
        format!("ALTER TABLE {table} ALTER {} SET DEFAULT {expression}", self.quote(column))
    }

    fn view_replace(&self) -> ViewReplace {
        ViewReplace::Plain
    }

    fn replaces_routine_in_place(&self) -> bool {
        self.routine_or_replace
    }

    fn dump_auto_clause(&self) -> &'static str {
        " AUTO_INCREMENT"
    }

    async fn dump_scope(&self, _session: &CatalogSession, database: &str, _schemas: &[String]) -> Result<(String, Vec<String>), String> {
        if database.trim().is_empty() {
            return Err(crate::connections::error_codes::coded_string(crate::connections::error_codes::DUMP_DATABASE_REQUIRED, "需要数据库名，没有另开连接。"));
        }
        Ok((database.to_owned(), vec![String::new()]))
    }

    async fn compare_scope(&self, session: &CatalogSession, database: &str, _schema: &str) -> Result<(), String> {
        let CatalogSession::Mysql(pool) = session else {
            return Err(compare_mismatch());
        };
        let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
        let row: Option<mysql_async::Row> = conn
            .exec_first("SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?", (database.to_owned(),))
            .await
            .map_err(|err| crate::error_text::chain(&err))?;
        if row.is_none() {
            return Err(format!("数据库 {database} 不存在"));
        }
        Ok(())
    }
}

/// MySQL 家族的写入冲突措辞：`Duplicate entry '1' for key 'PRIMARY'`。
pub(in crate::adapters) struct MysqlErrors;

impl ErrorRules for MysqlErrors {
    fn is_conflict(&self, lower: &str) -> bool {
        lower.contains("duplicate entry")
    }
}

pub(in crate::adapters) async fn execute(pool: &mysql_async::Pool, statement: &str) -> Result<(), String> {
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    conn.query_drop(statement.to_owned()).await.map_err(|err| crate::error_text::chain(&err))
}

pub(in crate::adapters) async fn insert_in_transaction(pool: &mysql_async::Pool, statement: &str) -> Result<(), String> {
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    conn.query_drop("BEGIN".to_owned()).await.map_err(|err| crate::error_text::chain(&err))?;
    match conn.query_drop(statement.to_owned()).await {
        Ok(()) => conn.query_drop("COMMIT".to_owned()).await.map_err(|err| crate::error_text::chain(&err)),
        Err(error) => {
            let _ = conn.query_drop("ROLLBACK".to_owned()).await;
            Err(error.to_string())
        }
    }
}
