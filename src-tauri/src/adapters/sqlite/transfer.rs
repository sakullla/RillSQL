//! SQLite 的对比/导出/迁移方言（A5）：外键只能内联、改列靠重建表、只认连接的 main。

use crate::compare::model::{ObjectDef, Side, TableShape};
use crate::compare::structure::{column_clause, create_table, foreign_keys_differ, primary_columns};
use crate::connections::model::Engine;
use crate::connections::CatalogSession;

use crate::adapters::transfer::{qualify, TableAlter, TransferDialect};
use crate::adapters::{quote_ident, ErrorRules};

const ENGINE: Engine = Engine::Sqlite;

fn quote(name: &str) -> String {
    quote_ident(ENGINE, name)
}

pub(in crate::adapters) struct SqliteTransfer;

#[async_trait::async_trait]
impl TransferDialect for SqliteTransfer {
    /// SQLite 不能事后加外键，只能写在 CREATE TABLE 里，而且允许被引用表尚未存在。
    fn inline_foreign_keys(&self) -> bool {
        true
    }

    /// SQLite 的主键来自列信息。
    fn primary_key_from_columns(&self) -> bool {
        true
    }

    fn needs_rebuild(&self, baseline: &TableShape, target: &TableShape, with_fk: bool) -> bool {
        primary_columns(baseline) != primary_columns(target)
            || baseline.columns.iter().any(|column| {
                target.columns.iter().any(|other| {
                    other.name == column.name
                        && (!column.type_name.eq_ignore_ascii_case(&other.type_name) || column.nullable != other.nullable || column.default_sql.trim() != other.default_sql.trim())
                })
            })
            || (with_fk && foreign_keys_differ(baseline, target))
    }

    /// 只增删列时直接 ALTER；类型、可空、默认值、主键或外键不同就建新表、搬数据、删旧表、改名。
    fn alter_table(&self, alter: &TableAlter<'_>) -> Vec<String> {
        let (side, name, baseline, target, with_fk) = (alter.side, alter.name, alter.baseline, alter.target, alter.with_fk);
        if !self.needs_rebuild(baseline, target, with_fk) {
            let table = qualify(ENGINE, side, name);
            let mut statements = Vec::new();
            for column in &target.columns {
                if !baseline.columns.iter().any(|item| item.name == column.name) {
                    statements.push(format!("ALTER TABLE {table} DROP COLUMN {}", quote(&column.name)));
                }
            }
            for column in &baseline.columns {
                if !target.columns.iter().any(|item| item.name == column.name) {
                    statements.push(format!("ALTER TABLE {table} ADD COLUMN {}", column_clause(ENGINE, column)));
                }
            }
            return statements;
        }
        let fresh = format!("{name}__rillsql_new");
        let shared = baseline
            .columns
            .iter()
            .filter(|column| target.columns.iter().any(|other| other.name == column.name))
            .map(|column| quote(&column.name))
            .collect::<Vec<_>>()
            .join(", ");
        let mut statements = vec![create_table(ENGINE, side, &fresh, baseline, with_fk)];
        if !shared.is_empty() {
            statements.push(format!("INSERT INTO {} ({shared}) SELECT {shared} FROM {}", quote(&fresh), quote(name)));
        }
        statements.push(format!("DROP TABLE {}", quote(name)));
        statements.push(format!("ALTER TABLE {} RENAME TO {}", quote(&fresh), quote(name)));
        statements
    }

    fn drop_index(&self, _side: &Side, _table: &str, index: &str) -> String {
        format!("DROP INDEX {}", quote(index))
    }

    fn drop_trigger(&self, _side: &Side, object: &ObjectDef) -> String {
        format!("DROP TRIGGER {}", quote(&object.name))
    }

    fn drop_routine(&self, _side: &Side, word: &str, object: &ObjectDef) -> String {
        format!("DROP {word} {}", quote(&object.name))
    }

    fn drop_default(&self, table: &str, column: &str) -> String {
        format!("ALTER TABLE {table} ALTER COLUMN {} DROP DEFAULT", quote(column))
    }

    fn set_default(&self, table: &str, column: &str, expression: &str) -> String {
        format!("ALTER TABLE {table} ALTER COLUMN {} SET DEFAULT {expression}", quote(column))
    }

    fn compare_scope_refusal(&self, baseline: &Side, target: &Side) -> Option<String> {
        let other = |side: &Side| !side.database.is_empty() && side.database != "main";
        (other(baseline) || other(target)).then(|| "SQLite 只对比该连接的 main，不附加其他文件，不生成脚本。".to_owned())
    }

    /// CREATE TABLE 原文带内联外键，原文最保真。
    fn keeps_table_source(&self) -> bool {
        true
    }

    async fn dump_scope(&self, _session: &CatalogSession, database: &str, _schemas: &[String]) -> Result<(String, Vec<String>), String> {
        if !database.is_empty() && database != "main" {
            return Err(crate::connections::error_codes::coded_string_params(
                crate::connections::error_codes::DUMP_SESSION_GAP_MAIN,
                &[("database", database)],
                format!("当前连接打开的是 main，没有另开连接读取 {database}"),
            ));
        }
        Ok((String::new(), vec![String::new()]))
    }

    async fn compare_scope(&self, _session: &CatalogSession, database: &str, _schema: &str) -> Result<(), String> {
        if !database.is_empty() && database != "main" {
            return Err("SQLite 只对比该连接的 main，不附加其他文件".to_owned());
        }
        Ok(())
    }
}

/// SQLite 的写入冲突措辞：`UNIQUE constraint failed: t.id`。
pub(in crate::adapters) struct SqliteErrors;

impl ErrorRules for SqliteErrors {
    fn is_conflict(&self, lower: &str) -> bool {
        lower.contains("unique constraint")
    }
}

pub(in crate::adapters) async fn execute(pool: &sqlx::SqlitePool, statement: &str) -> Result<(), String> {
    sqlx::query(sqlx::AssertSqlSafe(statement.to_owned()))
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(|err| crate::error_text::chain(&err))
}

/// SQLite 会话的池固定单连接，BEGIN/COMMIT 语句与数据语句落在同一条连接上。
pub(in crate::adapters) async fn insert_in_transaction(pool: &sqlx::SqlitePool, statement: &str) -> Result<(), String> {
    execute(pool, "BEGIN").await?;
    match execute(pool, statement).await {
        Ok(()) => execute(pool, "COMMIT").await,
        Err(error) => {
            let _ = execute(pool, "ROLLBACK").await;
            Err(error)
        }
    }
}
